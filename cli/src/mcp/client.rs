//! A Model Context Protocol client: JSON-RPC 2.0 over a child's stdio or over streamable HTTP.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const PROTOCOL_VERSION: &str = "2025-06-18";
/// `npx -y <package>` downloads the server on its first start.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(120);

/// How to reach a server, from one `mcpServers` entry.
#[derive(Clone, Debug, PartialEq)]
pub enum Launch {
    Stdio { command: String, args: Vec<String>, env: Vec<(String, String)>, cwd: PathBuf },
    Http { url: String, headers: Vec<(String, String)> },
}

pub struct Client {
    transport: Transport,
    next_id: AtomicU64,
}

enum Transport {
    Stdio(Process),
    Http(Http),
}

struct Process {
    child: Mutex<Child>,
    /// A writer thread owns stdin, so a server that stops reading never blocks the agent.
    stdin: Sender<String>,
    pending: Arc<Mutex<HashMap<u64, Sender<Value>>>>,
}

struct Http {
    url: String,
    headers: Vec<(String, String)>,
    home: PathBuf,
    /// `Mcp-Session-Id` from the initialize answer, sent back on every later request.
    session: Mutex<Option<String>>,
}

impl Drop for Process {
    fn drop(&mut self) {
        let mut child = self.child.lock().unwrap();
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Client {
    /// Starts or reaches the server, runs the handshake and lists its tools. A stdio server that
    /// exits later calls `exited` with its last stderr line; `home` holds the OAuth tokens.
    pub fn connect(launch: &Launch, home: &Path, exited: impl FnOnce(String) + Send + 'static) -> Result<(Client, Vec<Value>), String> {
        let transport = match launch {
            Launch::Stdio { command, args, env, cwd } => Transport::Stdio(spawn(command, args, env, cwd, exited)?),
            Launch::Http { url, headers } => Transport::Http(Http { url: url.clone(), headers: headers.clone(), home: home.to_path_buf(), session: Mutex::new(None) }),
        };
        let client = Client { transport, next_id: AtomicU64::new(0) };
        let never = AtomicBool::new(false);
        let params = json!({"protocolVersion": PROTOCOL_VERSION, "capabilities": {}, "clientInfo": {"name": "agent", "version": env!("CARGO_PKG_VERSION")}});
        client.request("initialize", params, Some(CONNECT_TIMEOUT), &never)?;
        client.notify("notifications/initialized");
        let mut tools = Vec::new();
        let mut cursor = Value::Null;
        loop {
            let params = if cursor.is_null() { json!({}) } else { json!({"cursor": cursor}) };
            let page = client.request("tools/list", params, Some(CONNECT_TIMEOUT), &never)?;
            tools.extend(page["tools"].as_array().cloned().unwrap_or_default());
            cursor = page["nextCursor"].clone();
            if cursor.is_null() {
                return Ok((client, tools));
            }
        }
    }

    /// Runs a tool until it answers or `abort` is set: the text and whether it is an error.
    pub fn call(&self, tool: &str, input: &Value, abort: &AtomicBool) -> (String, bool) {
        match self.request("tools/call", json!({"name": tool, "arguments": input}), None, abort) {
            Ok(result) => (result_text(&result), result["isError"] == true),
            Err(error) => (error, true),
        }
    }

    /// The `result` of a request, or the error message.
    fn request(&self, method: &str, params: Value, timeout: Option<Duration>, abort: &AtomicBool) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let answer = match &self.transport {
            Transport::Stdio(process) => {
                let (reply, answer) = mpsc::channel();
                process.pending.lock().unwrap().insert(id, reply);
                let _ = process.stdin.send(message.to_string());
                let answer = wait(&answer, timeout, abort);
                process.pending.lock().unwrap().remove(&id);
                answer.ok_or_else(|| stopped(method, abort))?
            }
            Transport::Http(http) => {
                let (reply, answer) = mpsc::channel();
                let request = http.prepare(message)?;
                std::thread::spawn(move || {
                    let _ = reply.send(request.send(id));
                });
                let (answer, session) = wait(&answer, timeout, abort).ok_or_else(|| stopped(method, abort))??;
                if session.is_some() {
                    *http.session.lock().unwrap() = session;
                }
                answer
            }
        };
        match answer["error"]["message"].as_str() {
            Some(message) => Err(message.to_string()),
            None => Ok(answer["result"].clone()),
        }
    }

    fn notify(&self, method: &str) {
        let message = json!({"jsonrpc": "2.0", "method": method});
        match &self.transport {
            Transport::Stdio(process) => {
                let _ = process.stdin.send(message.to_string());
            }
            Transport::Http(http) => {
                if let Ok(request) = http.prepare(message) {
                    let _ = request.send(0);
                }
            }
        }
    }
}

fn stopped(method: &str, abort: &AtomicBool) -> String {
    if abort.load(Ordering::Relaxed) { "Operation aborted".into() } else { format!("The server did not answer {method}.") }
}

fn wait<T>(answer: &Receiver<T>, timeout: Option<Duration>, abort: &AtomicBool) -> Option<T> {
    let start = Instant::now();
    loop {
        match answer.recv_timeout(Duration::from_millis(100)) {
            Ok(answer) => return Some(answer),
            Err(mpsc::RecvTimeoutError::Disconnected) => return None,
            Err(mpsc::RecvTimeoutError::Timeout) if abort.load(Ordering::Relaxed) || timeout.is_some_and(|t| start.elapsed() >= t) => return None,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

fn spawn(command: &str, args: &[String], env: &[(String, String)], cwd: &Path, exited: impl FnOnce(String) + Send + 'static) -> Result<Process, String> {
    let mut child = crate::process::command(command)
        .args(args)
        .envs(env.iter().map(|(key, value)| (key, value)))
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Cannot start {command}: {e}"))?;
    let (mut stdin, stdout, stderr) = (child.stdin.take().unwrap(), child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let (lines, outbox) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in outbox {
            if writeln!(stdin, "{line}").is_err() {
                break;
            }
        }
    });
    let last_error = std::thread::spawn(move || BufReader::new(stderr).lines().map_while(Result::ok).filter(|line| !line.trim().is_empty()).last());
    let pending: Arc<Mutex<HashMap<u64, Sender<Value>>>> = Arc::default();
    let (reader_pending, replies) = (pending.clone(), lines.clone());
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let Ok(message) = serde_json::from_str::<Value>(&line) else { continue };
            match (message["id"].as_u64(), message.get("method")) {
                (Some(id), None) => {
                    if let Some(reply) = reader_pending.lock().unwrap().remove(&id) {
                        let _ = reply.send(message);
                    }
                }
                (_, Some(_)) if message.get("id").is_some() => {
                    let _ = replies.send(answer_server_request(&message).to_string());
                }
                _ => {}
            }
        }
        reader_pending.lock().unwrap().clear();
        exited(last_error.join().unwrap_or_default().unwrap_or_default());
    });
    Ok(Process { child: Mutex::new(child), stdin: lines, pending })
}

/// Servers may ping the client; it offers nothing else (no roots, sampling or elicitation).
fn answer_server_request(request: &Value) -> Value {
    match request["method"].as_str() {
        Some("ping") => json!({"jsonrpc": "2.0", "id": request["id"], "result": {}}),
        _ => json!({"jsonrpc": "2.0", "id": request["id"], "error": {"code": -32601, "message": "Method not found"}}),
    }
}

/// One HTTP POST, built under the client's locks and sent from a thread that may outlive an abort.
struct HttpRequest {
    url: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Http {
    fn prepare(&self, message: Value) -> Result<HttpRequest, String> {
        let mut headers = self.headers.clone();
        if let Some(token) = super::oauth::bearer(&self.home, &self.url)? {
            headers.push(("authorization".into(), format!("Bearer {token}")));
        }
        if let Some(session) = self.session.lock().unwrap().clone() {
            headers.push(("mcp-session-id".into(), session));
            headers.push(("mcp-protocol-version".into(), PROTOCOL_VERSION.into()));
        }
        Ok(HttpRequest { url: self.url.clone(), headers, body: message.to_string() })
    }
}

impl HttpRequest {
    /// The answer to request `id` and the session id the server handed out, if any.
    fn send(self, id: u64) -> Result<(Value, Option<String>), String> {
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).timeout_connect(Some(Duration::from_secs(30))).build().into();
        let mut request = agent.post(&self.url).header("content-type", "application/json").header("accept", "application/json, text/event-stream");
        for (name, value) in &self.headers {
            request = request.header(name, value);
        }
        let response = request.send(&self.body).map_err(|e| format!("Request to {} failed: {e}.", self.url))?;
        let status = response.status().as_u16();
        let header = |name: &str| response.headers().get(name).and_then(|value| value.to_str().ok()).map(String::from);
        let (session, content_type) = (header("mcp-session-id"), header("content-type").unwrap_or_default());
        if status == 401 {
            return Err("The server asks for a login: press l in /mcp.".into());
        }
        let mut reader = BufReader::new(response.into_body().into_reader());
        if !(200..300).contains(&status) {
            let mut text = String::new();
            let _ = reader.read_to_string(&mut text);
            return Err(format!("The server answered {status}: {}", text.trim()));
        }
        if status == 202 {
            return Ok((Value::Null, session));
        }
        Ok((read_reply(reader, content_type.starts_with("text/event-stream"), id)?, session))
    }
}

/// The JSON-RPC answer to `id` in a response body: plain JSON, or the first matching event of a
/// server-sent event stream, which may carry the server's own notifications before it.
fn read_reply(reader: impl BufRead, events: bool, id: u64) -> Result<Value, String> {
    if !events {
        let mut text = String::new();
        let mut reader = reader;
        reader.read_to_string(&mut text).map_err(|e| format!("The response is unreadable: {e}."))?;
        return serde_json::from_str(&text).map_err(|e| format!("The response is not JSON: {e}."));
    }
    let mut data = String::new();
    for line in reader.lines() {
        let line = line.map_err(|e| format!("The event stream broke: {e}."))?;
        if let Some(chunk) = line.strip_prefix("data:") {
            data.push_str(chunk.strip_prefix(' ').unwrap_or(chunk));
            data.push('\n');
            continue;
        }
        if !line.is_empty() || data.is_empty() {
            continue;
        }
        let message: Value = serde_json::from_str(&data).unwrap_or_default();
        data.clear();
        if message["id"] == id && message.get("method").is_none() {
            return Ok(message);
        }
    }
    Err("The event stream ended without an answer.".into())
}

/// A tool result as text: text blocks as they are, other blocks by what they are.
fn result_text(result: &Value) -> String {
    let mut parts = Vec::new();
    for block in result["content"].as_array().into_iter().flatten() {
        parts.push(match block["type"].as_str() {
            Some("text") => block["text"].as_str().unwrap_or_default().to_string(),
            Some("resource") => block["resource"]["text"].as_str().map_or_else(|| format!("[resource {}]", block["resource"]["uri"].as_str().unwrap_or_default()), String::from),
            Some("resource_link") => format!("[resource {}]", block["uri"].as_str().unwrap_or_default()),
            Some(kind) => format!("[{kind} {}]", block["mimeType"].as_str().unwrap_or_default()),
            None => block.to_string(),
        });
    }
    if parts.is_empty() && !result["structuredContent"].is_null() {
        return result["structuredContent"].to_string();
    }
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn an_event_stream_answers_with_the_event_for_the_request() {
        let stream = concat!(
            "event: message\n",
            "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\n",
            "data: {\"jsonrpc\":\"2.0\",\n",
            "data: \"id\":7,\"result\":{\"ok\":true}}\n\n",
        );
        assert_eq!(read_reply(Cursor::new(stream), true, 7).unwrap()["result"]["ok"], true);
        assert!(read_reply(Cursor::new("data: {\"id\":1}\n\n"), true, 7).is_err());
        assert_eq!(read_reply(Cursor::new("{\"id\":7,\"result\":{}}"), false, 7).unwrap()["id"], 7);
    }

    #[test]
    fn tool_results_become_text() {
        let result = json!({"content": [{"type": "text", "text": "hi"}, {"type": "image", "mimeType": "image/png", "data": "…"}]});
        assert_eq!(result_text(&result), "hi\n[image image/png]");
        assert_eq!(result_text(&json!({"content": [], "structuredContent": {"n": 1}})), "{\"n\":1}");
    }
}
