use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::now_ms;

// Client ids, endpoints and scopes are pi-ai's (auth/oauth/anthropic.js and openai-codex.js).
const CLAUDE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const CLAUDE_AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const CLAUDE_TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const CLAUDE_REDIRECT_URI: &str = "https://platform.claude.com/oauth/code/callback";
const CLAUDE_SCOPES: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CODEX_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CODEX_REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const CODEX_CALLBACK_ADDRESS: &str = "127.0.0.1:1455";
const CODEX_SCOPES: &str = "openid profile email offline_access";
const CODEX_JWT_CLAIM: &str = "https://api.openai.com/auth";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
/// Tokens are refreshed this long before they expire.
pub const REFRESH_MARGIN_MS: u64 = 5 * 60 * 1000;

#[derive(Clone, Debug, PartialEq)]
pub struct Tokens {
    pub access: String,
    pub refresh: String,
    /// Unix milliseconds.
    pub expires: u64,
    /// ChatGPT account read from the Codex access token, or the Claude account uuid from the token response.
    pub account_id: Option<String>,
}

/// A started login: the URL to open and what the code exchange needs.
pub struct Pending {
    pub url: String,
    verifier: String,
    state: String,
}

pub fn start_claude() -> Pending {
    let (verifier, challenge) = pkce();
    let query = [
        ("code", "true"),
        ("client_id", CLAUDE_CLIENT_ID),
        ("response_type", "code"),
        ("redirect_uri", CLAUDE_REDIRECT_URI),
        ("scope", CLAUDE_SCOPES),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
        ("state", &verifier),
    ];
    Pending { url: format!("{CLAUDE_AUTHORIZE_URL}?{}", form(&query)), state: verifier.clone(), verifier }
}

/// Exchanges the `code#state` Anthropic shows after sign-in.
pub fn finish_claude(pending: &Pending, pasted: &str) -> Result<Tokens, String> {
    let (code, state) = pasted.trim().split_once('#').unwrap_or((pasted.trim(), &pending.state));
    if state != pending.state {
        return Err("OAuth state mismatch. Start /login again.".into());
    }
    if code.is_empty() {
        return Err("Missing authorization code.".into());
    }
    let body = json!({"grant_type": "authorization_code", "client_id": CLAUDE_CLIENT_ID, "code": code, "state": state, "redirect_uri": CLAUDE_REDIRECT_URI, "code_verifier": pending.verifier});
    claude_tokens(post(CLAUDE_TOKEN_URL, "application/json", &body.to_string())?)
}

pub fn refresh_claude(refresh: &str) -> Result<Tokens, String> {
    let body = json!({"grant_type": "refresh_token", "client_id": CLAUDE_CLIENT_ID, "refresh_token": refresh});
    claude_tokens(post(CLAUDE_TOKEN_URL, "application/json", &body.to_string())?)
}

fn claude_tokens(data: Value) -> Result<Tokens, String> {
    let (Some(access), Some(refresh), Some(expires_in)) = (data["access_token"].as_str(), data["refresh_token"].as_str(), data["expires_in"].as_u64()) else {
        return Err("The token response is missing fields.".into());
    };
    // pi-ai stores Anthropic expiry five minutes early.
    let account_id = data["account"]["uuid"].as_str().map(String::from);
    Ok(Tokens { access: access.into(), refresh: refresh.into(), expires: now_ms() + expires_in * 1000 - REFRESH_MARGIN_MS, account_id })
}

pub fn start_codex() -> Pending {
    let (verifier, challenge) = pkce();
    let state = random_hex(16);
    let query = [
        ("response_type", "code"),
        ("client_id", CODEX_CLIENT_ID),
        ("redirect_uri", CODEX_REDIRECT_URI),
        ("scope", CODEX_SCOPES),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
        ("state", &state),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("originator", "pi"),
    ];
    Pending { url: format!("{CODEX_AUTHORIZE_URL}?{}", form(&query)), verifier, state }
}

/// Waits for the browser to reach the local callback, then exchanges its code. `abort` (Esc) gives up.
pub fn finish_codex(pending: &Pending, abort: &AtomicBool) -> Result<Tokens, String> {
    let listener = TcpListener::bind(CODEX_CALLBACK_ADDRESS).map_err(|e| format!("Cannot listen on {CODEX_CALLBACK_ADDRESS} ({e}). Close the Codex CLI login and try again."))?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let deadline = Instant::now() + LOGIN_TIMEOUT;
    let code = loop {
        if abort.load(Ordering::Relaxed) {
            return Err("Login cancelled.".into());
        }
        if Instant::now() > deadline {
            return Err("Login timed out.".into());
        }
        let Ok((stream, _)) = listener.accept() else {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        };
        stream.set_nonblocking(false).map_err(|e| e.to_string())?;
        let mut stream = stream;
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).map_err(|e| e.to_string())?;
        let target = line.split_whitespace().nth(1).unwrap_or_default();
        let Some(query) = target.strip_prefix("/auth/callback?") else {
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n");
            continue;
        };
        let params = parse_query(query);
        let param = |key: &str| params.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
        let page = "<!doctype html><p>Login finished. You can close this tab.</p>";
        let _ = write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\n\r\n{page}", page.len());
        if param("state").as_deref() != Some(pending.state.as_str()) {
            return Err("OAuth state mismatch. Start /login again.".into());
        }
        break param("code").ok_or("Missing authorization code.")?;
    };
    let body = form(&[("grant_type", "authorization_code"), ("client_id", CODEX_CLIENT_ID), ("code", &code), ("code_verifier", &pending.verifier), ("redirect_uri", CODEX_REDIRECT_URI)]);
    codex_tokens(post(CODEX_TOKEN_URL, "application/x-www-form-urlencoded", &body)?)
}

pub fn refresh_codex(refresh: &str) -> Result<Tokens, String> {
    let body = form(&[("grant_type", "refresh_token"), ("refresh_token", refresh), ("client_id", CODEX_CLIENT_ID)]);
    codex_tokens(post(CODEX_TOKEN_URL, "application/x-www-form-urlencoded", &body)?)
}

fn codex_tokens(data: Value) -> Result<Tokens, String> {
    let (Some(access), Some(refresh), Some(expires_in)) = (data["access_token"].as_str(), data["refresh_token"].as_str(), data["expires_in"].as_u64()) else {
        return Err("The token response is missing fields.".into());
    };
    let account_id = codex_account_id(access).ok_or("The token has no ChatGPT account id.")?;
    Ok(Tokens { access: access.into(), refresh: refresh.into(), expires: now_ms() + expires_in * 1000, account_id: Some(account_id) })
}

/// The ChatGPT account id from the access token's JWT payload.
fn codex_account_id(access: &str) -> Option<String> {
    let payload = access.split('.').nth(1)?;
    let claims: Value = serde_json::from_slice(&base64url_decode(payload)?).ok()?;
    claims[CODEX_JWT_CLAIM]["chatgpt_account_id"].as_str().filter(|id| !id.is_empty()).map(String::from)
}

pub fn open_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    // spawn() and not status(): xdg-open runs a browser without a desktop handler in the foreground.
    let _ = std::process::Command::new(opener).arg(url).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
}

/// POSTs to a token endpoint. The error carries the status and body, never the request, which holds the secrets.
fn post(url: &str, content_type: &str, body: &str) -> Result<Value, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(30))).build().into();
    let response = agent.post(url).header("content-type", content_type).header("accept", "application/json").send(body).map_err(|e| format!("Token request failed: {e}."))?;
    let status = response.status().as_u16();
    let mut text = String::new();
    response.into_body().into_reader().read_to_string(&mut text).map_err(|e| format!("Token response unreadable: {e}."))?;
    if status != 200 {
        return Err(format!("Token request failed ({status}): {}", text.trim()));
    }
    serde_json::from_str(&text).map_err(|e| format!("Token response is not JSON: {e}."))
}

/// RFC 7636 S256: a random 32-byte verifier and the base64url SHA-256 of it.
fn pkce() -> (String, String) {
    let verifier = base64url_encode(&random_bytes::<32>());
    let challenge = challenge(&verifier);
    (verifier, challenge)
}

fn challenge(verifier: &str) -> String {
    base64url_encode(ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()).as_ref())
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes).expect("the system random source works");
    bytes
}

fn random_hex(len: usize) -> String {
    random_bytes::<32>()[..len].iter().map(|b| format!("{b:02x}")).collect()
}

const BASE64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn base64url_encode(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(BASE64URL[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

fn base64url_decode(text: &str) -> Option<Vec<u8>> {
    let mut bits = 0u32;
    let mut count = 0;
    let mut out = Vec::new();
    for ch in text.trim_end_matches('=').bytes() {
        let ch = match ch {
            b'+' => b'-',
            b'/' => b'_',
            other => other,
        };
        bits = bits << 6 | BASE64URL.iter().position(|c| *c == ch)? as u32;
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
        }
    }
    Some(out)
}

/// `application/x-www-form-urlencoded`, also used for query strings.
fn form(pairs: &[(&str, &str)]) -> String {
    let encode = |text: &str| -> String {
        let mut out = String::new();
        for byte in text.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
                _ => out.push_str(&format!("%{byte:02X}")),
            }
        }
        out
    };
    pairs.iter().map(|(key, value)| format!("{}={}", encode(key), encode(value))).collect::<Vec<_>>().join("&")
}

fn parse_query(query: &str) -> Vec<(String, String)> {
    let decode = |text: &str| -> String {
        let bytes = text.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            let hex = (bytes[i] == b'%').then(|| text.get(i + 1..i + 3)).flatten().and_then(|h| u8::from_str_radix(h, 16).ok());
            match (bytes[i], hex) {
                (_, Some(byte)) => {
                    out.push(byte);
                    i += 3;
                }
                (b'+', None) => {
                    out.push(b' ');
                    i += 1;
                }
                (byte, None) => {
                    out.push(byte);
                    i += 1;
                }
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    };
    query.split('&').filter_map(|pair| pair.split_once('=')).map(|(k, v)| (decode(k), decode(v))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_the_rfc_7636_vector() {
        assert_eq!(challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        assert_eq!(base64url_encode(b"f"), "Zg");
        assert_eq!(base64url_encode(b"foob"), "Zm9vYg");
        assert_eq!(base64url_decode("Zm9vYmE").unwrap(), b"fooba");
        let (verifier, challenge_text) = pkce();
        assert_eq!((verifier.len(), challenge_text), (43, challenge(&verifier)));
    }

    #[test]
    fn codex_account_id_comes_from_the_jwt_claim() {
        let claims = json!({"https://api.openai.com/auth": {"chatgpt_account_id": "acc-1"}});
        let token = format!("e30.{}.sig", base64url_encode(claims.to_string().as_bytes()));
        assert_eq!(codex_account_id(&token).as_deref(), Some("acc-1"));
        assert_eq!(codex_account_id("not-a-jwt"), None);
        assert_eq!(parse_query("code=a%2Fb+c&state=x"), [("code".to_string(), "a/b c".to_string()), ("state".to_string(), "x".to_string())]);
        assert_eq!(form(&[("scope", "a b"), ("u", "http://x/y")]), "scope=a%20b&u=http%3A%2F%2Fx%2Fy");
    }
}
