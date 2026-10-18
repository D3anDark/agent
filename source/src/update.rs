use std::path::Path;
use std::time::Duration;

use serde_json::Value;

const RELEASES: &str = "https://github.com/rokartur/agent/releases";
const RELEASES_API: &str = "https://api.github.com/repos/rokartur/agent/releases?per_page=20";
const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// `latest` follows stable releases only, `pre-release` also takes pre-releases.
pub const CHANNELS: [&str; 2] = ["latest", "pre-release"];

/// Only CI release builds update themselves, so a local `cargo build` never gets replaced.
pub fn enabled() -> bool {
    option_env!("AGENT_RELEASE").is_some()
}

/// Replaces the running binary with the newest release on `channel`. Returns the installed version.
pub fn install_latest(channel: &str) -> Result<Option<String>, String> {
    // Offline start is normal: no network means no update, not an error.
    let Some(latest) = latest_version(channel == "pre-release") else { return Ok(None) };
    if !is_newer(&latest, CURRENT) {
        return Ok(None);
    }
    let exe = std::env::current_exe().map_err(|e| format!("Cannot find the agent binary: {e}"))?;
    let staged = exe.with_file_name(format!(".agent-update-{}", std::process::id()));
    let result = download(&latest, &staged).and_then(|()| verify(&staged, &latest));
    if let Err(error) = result {
        let _ = std::fs::remove_file(&staged);
        return Err(error);
    }
    std::fs::rename(&staged, &exe).map_err(|e| format!("Cannot replace {}: {e}", exe.display()))?;
    Ok(Some(latest))
}

fn latest_version(include_prereleases: bool) -> Option<String> {
    let http: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(10))).build().into();
    let body = http.get(RELEASES_API).call().ok()?.body_mut().read_to_string().ok()?;
    let releases: Value = serde_json::from_str(&body).ok()?;
    newest(&releases, include_prereleases).map(str::to_owned)
}

/// Unauthenticated API calls never list drafts.
fn newest(releases: &Value, include_prereleases: bool) -> Option<&str> {
    releases
        .as_array()?
        .iter()
        .filter(|release| include_prereleases || release["prerelease"] == false)
        .filter_map(|release| release["tag_name"].as_str()?.strip_prefix('v'))
        .max_by_key(|version| precedence(version))
}

fn download(version: &str, path: &Path) -> Result<(), String> {
    let asset = format!("agent-{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let url = format!("{RELEASES}/download/v{version}/{asset}");
    let http: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(300))).build().into();
    let bytes = http.get(&url).call().and_then(|mut response| response.body_mut().with_config().limit(100 << 20).read_to_vec()).map_err(|e| format!("Cannot download {url}: {e}"))?;
    std::fs::write(path, bytes).map_err(|e| format!("Cannot write {}: {e}", path.display()))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).map_err(|e| format!("Cannot make {} executable: {e}", path.display()))
}

/// A truncated or wrong download must never replace a working binary.
fn verify(path: &Path, version: &str) -> Result<(), String> {
    let output = crate::process::output_within(crate::process::command(path).arg("--version"), std::time::Duration::from_secs(10))
        .map_err(|e| format!("Cannot run the downloaded agent: {e}"))?
        .ok_or("The downloaded agent did not answer --version within 10 s.")?;
    let reported = String::from_utf8_lossy(&output.stdout);
    if reported.trim() != format!("agent {version}") {
        return Err(format!("The downloaded agent reports {:?}, expected {version}.", reported.trim()));
    }
    Ok(())
}

pub(crate) fn is_newer(latest: &str, current: &str) -> bool {
    match (precedence(latest), precedence(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum Identifier {
    Number(u64),
    Text(String),
}

/// Semver order: 0.3.0-beta.2 < 0.3.0-beta.10 < 0.3.0-rc.1 < 0.3.0.
fn precedence(version: &str) -> Option<(Vec<u64>, bool, Vec<Identifier>)> {
    let (core, pre) = match version.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (version, None),
    };
    let core = core.split('.').map(|n| n.parse().ok()).collect::<Option<Vec<u64>>>()?;
    let identifiers = pre.into_iter().flat_map(|pre| pre.split('.')).map(|id| id.parse().map_or_else(|_| Identifier::Text(id.to_string()), Identifier::Number)).collect();
    Some((core, pre.is_none(), identifiers))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_higher_release_counts_as_newer() {
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(!is_newer("0.1.x", "0.1.0"));
    }

    #[test]
    fn pre_releases_sort_below_their_release() {
        assert!(is_newer("0.3.0-beta.1", "0.2.0"));
        assert!(is_newer("0.3.0-beta.10", "0.3.0-beta.2"));
        assert!(is_newer("0.3.0-rc.1", "0.3.0-beta.10"));
        assert!(is_newer("0.3.0", "0.3.0-rc.1"));
        assert!(!is_newer("0.3.0-rc.1", "0.3.0"));
    }

    #[test]
    fn channel_picks_the_newest_release_it_follows() {
        let releases = serde_json::json!([
            {"tag_name": "v0.3.0-beta.1", "prerelease": true},
            {"tag_name": "v0.2.1", "prerelease": false},
            {"tag_name": "v0.2.0", "prerelease": false},
        ]);
        assert_eq!(newest(&releases, false), Some("0.2.1"));
        assert_eq!(newest(&releases, true), Some("0.3.0-beta.1"));
    }
}
