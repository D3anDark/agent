use serde_json::Value;
use std::collections::BTreeMap;

/// At start, reinstalls the headroom service when its manifest lacks a value from `env`.
/// Under a lock, so agents started together reinstall once: the next one reads the manifest the first one wrote.
pub fn sync(home: &std::path::Path, env: &BTreeMap<String, String>) -> Result<(), String> {
    crate::api::accounts::locked(&home.join(".agents/headroom.json"), || {
        let manifest = std::fs::read_to_string(home.join(".headroom/deploy/default/manifest.json")).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or(Value::Null);
        let Some(args) = apply_args(env, &manifest)? else { return Ok(()) };
        install(&args)
    })
}

fn install(args: &[String]) -> Result<(), String> {
    // headroom 0.41.0 once failed with "runtime remained alive after SIGTERM" and left the proxy stopped; a second apply started it.
    let mut error = String::new();
    for _ in 0..2 {
        let output = crate::process::command("headroom").args(args).output().map_err(|e| e.to_string())?;
        if output.status.success() {
            return Ok(());
        }
        error = String::from_utf8_lossy(&output.stderr).trim_end().to_string();
    }
    Err(error)
}

/// Arguments for `headroom install apply` when the installed deployment lacks a value from `env`, else `None`.
/// `install apply` rebuilds the service env from defaults plus `--env`, so every value goes along, not only the changed one.
pub fn apply_args(env: &BTreeMap<String, String>, manifest: &Value) -> Result<Option<Vec<String>>, String> {
    if let Some(key) = ["HEADROOM_HOST", "HEADROOM_PORT"].into_iter().find(|key| env.contains_key(*key)) {
        return Err(format!("headroom_env cannot set {key}: the agent sends requests to {}.", crate::api::HEADROOM_URL));
    }
    if env.iter().all(|(key, value)| manifest["base_env"][key].as_str() == Some(value)) {
        return Ok(None);
    }
    let mut args: Vec<String> = ["install", "apply", "--providers", "manual", "--no-rate-limit"].map(String::from).to_vec();
    for (key, value) in env {
        args.extend(["--env".to_string(), format!("{key}={value}")]);
    }
    Ok(Some(args))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_a_missing_or_different_value_reinstalls() {
        let env = BTreeMap::from([("HEADROOM_DISABLE_KOMPRESS".to_string(), "1".to_string())]);
        let installed = json!({"base_env": {"HEADROOM_PORT": "8787", "HEADROOM_DISABLE_KOMPRESS": "1"}});
        assert_eq!(apply_args(&env, &installed), Ok(None));
        assert_eq!(apply_args(&BTreeMap::new(), &Value::Null), Ok(None));
        let expected = ["install", "apply", "--providers", "manual", "--no-rate-limit", "--env", "HEADROOM_DISABLE_KOMPRESS=1"].map(String::from).to_vec();
        assert_eq!(apply_args(&env, &json!({"base_env": {"HEADROOM_DISABLE_KOMPRESS": "0"}})), Ok(Some(expected.clone())));
        assert_eq!(apply_args(&env, &Value::Null), Ok(Some(expected)));
    }

    #[test]
    fn an_env_that_moves_the_proxy_off_the_agents_address_is_refused() {
        let env = BTreeMap::from([("HEADROOM_PORT".to_string(), "9000".to_string())]);
        assert!(apply_args(&env, &Value::Null).unwrap_err().contains("HEADROOM_PORT"));
    }
}
