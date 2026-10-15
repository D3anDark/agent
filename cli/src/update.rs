use std::path::Path;
use std::process::Command;
use std::time::Duration;

const RELEASES: &str = "https://github.com/rokartur/agent/releases";
const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// Only CI release builds update themselves, so a local `cargo build` never gets replaced.
pub fn enabled() -> bool {
    option_env!("AGENT_RELEASE").is_some()
}

/// Replaces the running binary with the latest release. Returns the installed version.
pub fn install_latest() -> Result<Option<String>, String> {
    // Offline start is normal: no network means no update, not an error.
    let Some(latest) = latest_version() else { return Ok(None) };
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

fn latest_version() -> Option<String> {
    let http: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .max_redirects(0)
        .build()
        .into();
    let response = http.get(&format!("{RELEASES}/latest")).call().ok()?;
    let location = response.headers().get("location")?.to_str().ok()?;
    tag_version(location).map(str::to_owned)
}

fn download(version: &str, path: &Path) -> Result<(), String> {
    let asset = format!("agent-{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let url = format!("{RELEASES}/download/v{version}/{asset}");
    let http: ureq::Agent =
        ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(300))).build().into();
    let bytes = http
        .get(&url)
        .call()
        .and_then(|mut response| response.body_mut().with_config().limit(100 << 20).read_to_vec())
        .map_err(|e| format!("Cannot download {url}: {e}"))?;
    std::fs::write(path, bytes).map_err(|e| format!("Cannot write {}: {e}", path.display()))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("Cannot make {} executable: {e}", path.display()))
}

/// A truncated or wrong download must never replace a working binary.
fn verify(path: &Path, version: &str) -> Result<(), String> {
    let output = Command::new(path)
        .arg("--version")
        .output()
        .map_err(|e| format!("Cannot run the downloaded agent: {e}"))?;
    let reported = String::from_utf8_lossy(&output.stdout);
    if reported.trim() != format!("agent {version}") {
        return Err(format!("The downloaded agent reports {:?}, expected {version}.", reported.trim()));
    }
    Ok(())
}

fn is_newer(latest: &str, current: &str) -> bool {
    let parse = |version: &str| version.split('.').map(str::parse::<u32>).collect::<Result<Vec<_>, _>>();
    match (parse(latest), parse(current)) {
        (Ok(latest), Ok(current)) => latest > current,
        _ => false,
    }
}

fn tag_version(location: &str) -> Option<&str> {
    location.rsplit_once("/tag/v").map(|(_, version)| version)
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
        assert!(!is_newer("0.3.0-beta", "0.2.0"));
    }

    #[test]
    fn version_comes_from_the_latest_release_redirect() {
        let location = "https://github.com/rokartur/agent/releases/tag/v0.2.0";
        assert_eq!(tag_version(location), Some("0.2.0"));
        assert_eq!(tag_version("https://github.com/rokartur/agent/releases"), None);
    }
}
