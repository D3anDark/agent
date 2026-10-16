//! `mcpServers` from `~/.agents/mcp.json`, then `<cwd>/.mcp.json`, then `<cwd>/.agents/mcp.json`.
//! A later file overrides an earlier one field by field, so `{"enabled": false}` turns a global server off.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Map, Value};

use super::client::Launch;

#[derive(Clone, Debug, PartialEq)]
pub struct Server {
    pub name: String,
    pub launch: Launch,
    pub enabled: bool,
}

/// The servers in name order, or the first unreadable file's error.
pub fn load(home: &Path, cwd: &Path) -> Result<Vec<Server>, String> {
    let files = [home.join(".agents/mcp.json"), cwd.join(".mcp.json"), cwd.join(".agents/mcp.json")];
    let mut merged: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        let config: Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", file.display()))?;
        for (name, entry) in config["mcpServers"].as_object().into_iter().flatten() {
            let slot = merged.entry(name.clone()).or_default();
            for (key, value) in entry.as_object().into_iter().flatten() {
                slot.insert(key.clone(), value.clone());
            }
        }
    }
    merged.into_iter().map(|(name, entry)| server(&name, &Value::Object(entry), cwd, &|key| std::env::var(key).ok())).collect()
}

fn server(name: &str, entry: &Value, cwd: &Path, env: &dyn Fn(&str) -> Option<String>) -> Result<Server, String> {
    let pairs = |key: &str| -> Vec<(String, String)> {
        let values = entry[key].as_object().into_iter().flatten();
        values.map(|(name, value)| (name.clone(), expand(value.as_str().unwrap_or_default(), env))).collect()
    };
    let launch = match (entry["command"].as_str(), entry["url"].as_str()) {
        (Some(command), _) => Launch::Stdio {
            command: command.into(),
            args: entry["args"].as_array().into_iter().flatten().filter_map(Value::as_str).map(String::from).collect(),
            env: pairs("env"),
            cwd: entry["cwd"].as_str().map_or_else(|| cwd.to_path_buf(), |dir| cwd.join(dir)),
        },
        (None, Some(url)) => Launch::Http { url: expand(url, env), headers: pairs("headers") },
        (None, None) => return Err(format!("MCP server {name} needs a command or a url.")),
    };
    Ok(Server { name: name.into(), launch, enabled: entry["enabled"] != false })
}

/// Replaces `${NAME}` with the environment variable, or with nothing when it is unset.
fn expand(text: &str, env: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        let Some(end) = rest[start..].find('}') else { break };
        out.push_str(&rest[..start]);
        out.push_str(&env(&rest[start + 2..start + end]).unwrap_or_default());
        rest = &rest[start + end + 1..];
    }
    out + rest
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn write(path: PathBuf, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn project_files_override_global_servers_by_name() {
        let root = std::env::temp_dir().join(format!("agent-mcp-config-{}", std::process::id()));
        let (home, cwd) = (root.join("home"), root.join("project"));
        write(home.join(".agents/mcp.json"), r#"{"mcpServers": {"fs": {"command": "fs-server", "args": ["--ro"]}, "web": {"url": "https://a.example/mcp"}}}"#);
        write(cwd.join(".mcp.json"), r#"{"mcpServers": {"web": {"url": "https://b.example/mcp"}, "local": {"command": "x", "cwd": "tools"}}}"#);
        write(cwd.join(".agents/mcp.json"), r#"{"mcpServers": {"fs": {"enabled": false}}}"#);
        let servers = load(&home, &cwd).unwrap();
        std::fs::remove_dir_all(&root).unwrap();

        let names: Vec<_> = servers.iter().map(|server| server.name.as_str()).collect();
        assert_eq!(names, ["fs", "local", "web"]);
        assert!(!servers[0].enabled);
        assert_eq!(servers[0].launch, Launch::Stdio { command: "fs-server".into(), args: vec!["--ro".into()], env: vec![], cwd: cwd.clone() });
        assert_eq!(servers[1].launch, Launch::Stdio { command: "x".into(), args: vec![], env: vec![], cwd: cwd.join("tools") });
        assert_eq!(servers[2].launch, Launch::Http { url: "https://b.example/mcp".into(), headers: vec![] });
    }

    #[test]
    fn headers_expand_environment_variables() {
        let env = |key: &str| (key == "TOKEN").then(|| "s3cret".to_string());
        let entry = serde_json::json!({"url": "https://x/mcp", "headers": {"Authorization": "Bearer ${TOKEN}", "X-Missing": "${NOPE}!"}});
        let parsed = server("x", &entry, Path::new("/"), &env).unwrap();
        let headers = vec![("Authorization".into(), "Bearer s3cret".into()), ("X-Missing".into(), "!".into())];
        assert_eq!(parsed.launch, Launch::Http { url: "https://x/mcp".into(), headers });
        assert!(server("bad", &serde_json::json!({}), Path::new("/"), &env).is_err());
    }
}
