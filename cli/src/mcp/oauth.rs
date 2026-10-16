//! OAuth for HTTP servers, per the MCP authorization spec: metadata discovery, dynamic client
//! registration, PKCE through the browser, and refresh. Tokens live in `~/.agents/mcp-auth.json`.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::api::oauth::{REFRESH_MARGIN_MS, callback_code, form, open_browser, pkce, post, random_hex};
use crate::now_ms;

/// The access token stored for `url`, refreshed first when it is about to expire.
pub fn bearer(home: &Path, url: &str) -> Result<Option<String>, String> {
    let mut store = read(home);
    let Some(entry) = store.get(url).cloned() else { return Ok(None) };
    if entry["expires"].as_u64().is_none_or(|expires| now_ms() + REFRESH_MARGIN_MS < expires) {
        return Ok(entry["access"].as_str().map(String::from));
    }
    let Some(refresh) = entry["refresh"].as_str() else { return Ok(entry["access"].as_str().map(String::from)) };
    let client_id = entry["client_id"].as_str().unwrap_or_default();
    let mut fields = vec![("grant_type", "refresh_token"), ("refresh_token", refresh), ("client_id", client_id), ("resource", url)];
    if let Some(secret) = entry["client_secret"].as_str() {
        fields.push(("client_secret", secret));
    }
    let answer = post(entry["token_endpoint"].as_str().unwrap_or_default(), "application/x-www-form-urlencoded", &form(&fields))
        .map_err(|e| format!("Refreshing the login failed ({e}). Press l in /mcp to log in again."))?;
    let updated = with_tokens(entry, &answer)?;
    let access = updated["access"].as_str().map(String::from);
    store.insert(url.into(), updated);
    save(home, &store)?;
    Ok(access)
}

/// Whether `url` has a stored login.
pub fn logged_in(home: &Path, url: &str) -> bool {
    read(home).contains_key(url)
}

/// Runs the browser login for the server at `url` and stores its tokens.
pub fn login(home: &Path, url: &str, abort: &AtomicBool) -> Result<(), String> {
    let resource = fetch_first(&metadata_urls(url, "oauth-protected-resource"));
    let issuer = resource.as_ref().and_then(|meta| meta["authorization_servers"][0].as_str()).unwrap_or(url).to_string();
    let mut urls = metadata_urls(&issuer, "oauth-authorization-server");
    urls.extend(metadata_urls(&issuer, "openid-configuration"));
    let endpoints = Endpoints::new(fetch_first(&urls), &issuer);

    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("Cannot listen for the login callback: {e}"))?;
    let redirect = format!("http://127.0.0.1:{}/callback", listener.local_addr().map_err(|e| e.to_string())?.port());
    let client = register(&endpoints, &redirect)?;
    let (verifier, challenge) = pkce();
    let state = random_hex(16);
    let scope = resource.as_ref().map(scopes).unwrap_or_default();
    let client_id = client["client_id"].as_str().unwrap_or_default();
    open_browser(&authorize_url(&endpoints.authorization, &[("client_id", client_id), ("redirect_uri", &redirect), ("code_challenge", &challenge), ("state", &state), ("resource", url), ("scope", &scope)]));
    let code = callback_code(&listener, "/callback?", &state, abort)?;

    let mut fields = vec![("grant_type", "authorization_code"), ("code", &code), ("redirect_uri", &redirect), ("client_id", client_id), ("code_verifier", &verifier), ("resource", url)];
    if let Some(secret) = client["client_secret"].as_str() {
        fields.push(("client_secret", secret));
    }
    let answer = post(&endpoints.token, "application/x-www-form-urlencoded", &form(&fields))?;
    let mut entry = client.clone();
    entry["token_endpoint"] = json!(endpoints.token);
    let mut store = read(home);
    store.insert(url.into(), with_tokens(entry, &answer)?);
    save(home, &store)
}

/// A PKCE authorization request; an empty value (no scopes) is left out.
fn authorize_url(endpoint: &str, fields: &[(&str, &str)]) -> String {
    let mut query = vec![("response_type", "code"), ("code_challenge_method", "S256")];
    query.extend(fields.iter().filter(|(_, value)| !value.is_empty()));
    format!("{endpoint}?{}", form(&query))
}

#[derive(Debug, PartialEq)]
struct Endpoints {
    authorization: String,
    token: String,
    registration: Option<String>,
}

impl Endpoints {
    /// From authorization server metadata, or the spec's default paths on the issuer's origin.
    fn new(metadata: Option<Value>, issuer: &str) -> Endpoints {
        let origin = origin(issuer);
        let field = |key: &str| metadata.as_ref().and_then(|meta| meta[key].as_str()).map(String::from);
        Endpoints {
            authorization: field("authorization_endpoint").unwrap_or_else(|| format!("{origin}/authorize")),
            token: field("token_endpoint").unwrap_or_else(|| format!("{origin}/token")),
            registration: if metadata.is_some() { field("registration_endpoint") } else { Some(format!("{origin}/register")) },
        }
    }
}

/// A client for this agent: `client_id` and maybe `client_secret`.
fn register(endpoints: &Endpoints, redirect: &str) -> Result<Value, String> {
    let Some(registration) = &endpoints.registration else {
        return Err("The server does not offer client registration, so the agent cannot log in to it.".into());
    };
    let request = json!({
        "client_name": "agent",
        "redirect_uris": [redirect],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    });
    let client = post(registration, "application/json", &request.to_string()).map_err(|e| format!("Client registration failed: {e}"))?;
    let mut kept = json!({"client_id": client["client_id"].as_str().ok_or("Client registration returned no client_id.")?});
    if let Some(secret) = client["client_secret"].as_str() {
        kept["client_secret"] = json!(secret);
    }
    Ok(kept)
}

fn with_tokens(mut entry: Value, answer: &Value) -> Result<Value, String> {
    entry["access"] = json!(answer["access_token"].as_str().ok_or("The token answer has no access_token.")?);
    if let Some(refresh) = answer["refresh_token"].as_str() {
        entry["refresh"] = json!(refresh);
    }
    entry["expires"] = answer["expires_in"].as_u64().map_or(Value::Null, |seconds| json!(now_ms() + seconds * 1000));
    Ok(entry)
}

fn scopes(resource: &Value) -> String {
    let scopes = resource["scopes_supported"].as_array().into_iter().flatten().filter_map(Value::as_str);
    scopes.collect::<Vec<_>>().join(" ")
}

/// RFC 8414 and 9728 put the well-known segment between the origin and the path; the root comes last.
fn metadata_urls(url: &str, kind: &str) -> Vec<String> {
    let origin = origin(url);
    let path = url[origin.len()..].split(['?', '#']).next().unwrap_or_default().trim_end_matches('/');
    let mut urls = vec![format!("{origin}/.well-known/{kind}{path}")];
    if !path.is_empty() {
        urls.push(format!("{origin}/.well-known/{kind}"));
    }
    urls
}

fn origin(url: &str) -> &str {
    let after_scheme = url.find("://").map_or(0, |at| at + 3);
    let end = url[after_scheme..].find(['/', '?', '#']).map_or(url.len(), |at| after_scheme + at);
    &url[..end]
}

/// The first of `urls` that answers 200 with JSON.
fn fetch_first(urls: &[String]) -> Option<Value> {
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(30))).build().into();
    urls.iter().find_map(|url| {
        let mut response = agent.get(url).header("accept", "application/json").call().ok()?;
        if response.status().as_u16() != 200 {
            return None;
        }
        serde_json::from_str(&response.body_mut().read_to_string().ok()?).ok()
    })
}

fn path(home: &Path) -> PathBuf {
    home.join(".agents/mcp-auth.json")
}

fn read(home: &Path) -> Map<String, Value> {
    let text = std::fs::read_to_string(path(home)).unwrap_or_default();
    serde_json::from_str(&text).unwrap_or_default()
}

/// Owner-only, written to a temp file and renamed so a crash never leaves half a file.
// shortcut: two servers refreshing at once may drop one token; take a file lock if that shows up.
fn save(home: &Path, store: &Map<String, Value>) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let path = path(home);
    let temp = path.with_extension("json.tmp");
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(path.parent().unwrap())?;
        let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&temp)?;
        file.write_all(serde_json::to_string_pretty(store).unwrap().as_bytes())?;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(&temp, &path)
    };
    write().map_err(|e| format!("Cannot save {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_looks_under_the_path_then_at_the_root() {
        assert_eq!(
            metadata_urls("https://mcp.example.com/v1/mcp/?x=1", "oauth-protected-resource"),
            ["https://mcp.example.com/.well-known/oauth-protected-resource/v1/mcp", "https://mcp.example.com/.well-known/oauth-protected-resource"]
        );
        assert_eq!(metadata_urls("https://auth.example.com", "oauth-authorization-server"), ["https://auth.example.com/.well-known/oauth-authorization-server"]);
    }

    #[test]
    fn endpoints_come_from_metadata_or_default_paths() {
        let metadata = json!({"authorization_endpoint": "https://a/auth", "token_endpoint": "https://a/tok"});
        let found = Endpoints::new(Some(metadata), "https://a/tenant");
        assert_eq!(found, Endpoints { authorization: "https://a/auth".into(), token: "https://a/tok".into(), registration: None });
        let defaults = Endpoints::new(None, "https://a/tenant");
        let expected = Endpoints { authorization: "https://a/authorize".into(), token: "https://a/token".into(), registration: Some("https://a/register".into()) };
        assert_eq!(defaults, expected);
        assert_eq!(scopes(&json!({"scopes_supported": ["read", "write"]})), "read write");
    }

    #[test]
    fn the_authorization_url_carries_pkce_and_the_resource() {
        let url = authorize_url("https://a/authorize", &[("client_id", "c1"), ("redirect_uri", "http://127.0.0.1:5/callback"), ("resource", "https://x/mcp"), ("scope", "")]);
        let expected = "https://a/authorize?response_type=code&code_challenge_method=S256&client_id=c1&redirect_uri=http%3A%2F%2F127.0.0.1%3A5%2Fcallback&resource=https%3A%2F%2Fx%2Fmcp";
        assert_eq!(url, expected);
    }

    #[test]
    fn a_fresh_token_is_used_without_refreshing() {
        let home = std::env::temp_dir().join(format!("agent-mcp-auth-{}", std::process::id()));
        let mut store = Map::new();
        store.insert("https://x/mcp".into(), json!({"access": "tok", "refresh": "r", "expires": now_ms() + 3_600_000, "token_endpoint": "http://127.0.0.1:9/"}));
        save(&home, &store).unwrap();
        let mode = std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(path(&home)).unwrap().permissions());
        let token = bearer(&home, "https://x/mcp");
        let other = bearer(&home, "https://y/mcp");
        std::fs::remove_dir_all(&home).unwrap();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(token.unwrap().as_deref(), Some("tok"));
        assert_eq!(other.unwrap(), None);
    }
}
