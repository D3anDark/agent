//! A Model Context Protocol client: JSON-RPC 2.0 over a child's stdio or over streamable HTTP.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const PROTOCOL_VERSION: &str = "2025-06-18";
/// `npx -y <package>` downloads the server on its first start.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(120);
const TOOL_TIMEOUT: Duration = Duration::from_secs(600);

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
    child: Child,
    /// A writer thread owns stdin, so a server that stops reading never blocks the agent.
    stdin: Sender<String>,
    pending: Pending,
}

/// Reply senders by request id; `None` once the server's stdout closed.
type Pending = Arc<Mutex<Option<HashMap<u64, Sender<Value>>>>>;

struct Http {
    url: String,
    headers: Vec<(String, String)>,
    home: PathBuf,
    agent: ureq::Agent,
    /// `Mcp-Session-Id` from the initialize answer, sent back on every later request.
    session: Mutex<Option<String>>,
    /// The version the server chose in its initialize answer, sent on every later request.
    protocol: Mutex<String>,
}

impl Drop for Process {
    fn drop(&mut self) {
        crate::process::signal_group(self.child.id(), crate::process::SIGKILL);
        let _ = self.child.wait();
    }
}

impl Client {
    /// Starts or reaches the server, runs the handshake and lists its tools. A stdio server that
    /// exits later calls `exited` with its last stderr line; `home` holds the OAuth tokens.
    pub fn connect(launch: &Launch, home: &Path, exited: impl FnOnce(String) + Send + 'static) -> Result<(Client, Vec<Value>), String> {
        let transport = match launch {
            Launch::Stdio { command, args, env, cwd } => Transport::Stdio(spawn(command, args, env, cwd, exited)?),
            Launch::Http { url, headers } => {
                let agent = ureq::Agent::config_builder().http_status_as_error(false).timeout_connect(Some(Duration::from_secs(30))).timeout_global(Some(TOOL_TIMEOUT)).build().into();
                let (session, protocol) = (Mutex::new(None), Mutex::new(PROTOCOL_VERSION.into()));
                Transport::Http(Http { url: url.clone(), headers: headers.clone(), home: home.to_path_buf(), agent, session, protocol })
            }
        };
        let client = Client { transport, next_id: AtomicU64::new(0) };
        client.handshake()?;
        let never = AtomicBool::new(false);
        let mut tools = Vec::new();
        let mut cursor = Value::Null;
        loop {
            let params = if cursor.is_null() { json!({}) } else { json!({"cursor": cursor}) };
            let page = client.request("tools/list", params, CONNECT_TIMEOUT, &never)?;
            tools.extend(page["tools"].as_array().cloned().unwrap_or_default());
            if page["nextCursor"].is_null() {
                return Ok((client, tools));
            }
            if page["nextCursor"] == cursor {
                return Err("The server repeated its tools/list cursor.".into());
            }
            cursor = page["nextCursor"].clone();
        }
    }

    /// Runs a tool until it answers, `abort` is set or `TOOL_TIMEOUT` ends: the text and whether it is an error.
    pub fn call(&self, tool: &str, input: &Value, abort: &AtomicBool) -> (String, bool) {
        match self.request("tools/call", json!({"name": tool, "arguments": input}), TOOL_TIMEOUT, abort) {
            Ok(result) => (result_text(&result), result["isError"] == true),
            Err(error) => (error, true),
        }
    }

    fn handshake(&self) -> Result<(), String> {
        let params = json!({"protocolVersion": PROTOCOL_VERSION, "capabilities": {}, "clientInfo": {"name": "agent", "version": env!("CARGO_PKG_VERSION")}});
        let result = self.request("initialize", params, CONNECT_TIMEOUT, &AtomicBool::new(false))?;
        if let (Transport::Http(http), Some(version)) = (&self.transport, result["protocolVersion"].as_str()) {
            *http.protocol.lock().unwrap() = version.into();
        }
        self.notify("notifications/initialized")
    }

    /// The `result` of a request, or the error message.
    fn request(&self, method: &str, params: Value, timeout: Duration, abort: &AtomicBool) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let start = Instant::now();
        let answer = match &self.transport {
            Transport::Stdio(process) => process.request(id, &message, method, timeout, abort),
            Transport::Http(http) => match http.request(id, &message, method, timeout, abort) {
                Ok(Some(answer)) => Ok(answer),
                // Streamable HTTP: after a 404 to a session the client must initialize a new one.
                Ok(None) => self.handshake().and_then(|()| http.request(id, &message, method, timeout, abort)?.ok_or(format!("The server dropped its new session during {method}."))),
                Err(error) => Err(error),
            },
        };
        // The server may still be working on it; the spec forbids cancelling `initialize`.
        if answer.is_err() && method != "initialize" && (abort.load(Ordering::Relaxed) || start.elapsed() >= timeout) {
            self.cancel(id);
        }
        let answer = answer?;
        if answer["error"].is_null() {
            return Ok(answer["result"].clone());
        }
        Err(answer["error"]["message"].as_str().map_or_else(|| answer["error"].to_string(), String::from))
    }

    fn notify(&self, method: &str) -> Result<(), String> {
        let message = json!({"jsonrpc": "2.0", "method": method});
        match &self.transport {
            Transport::Stdio(process) => {
                let _ = process.stdin.send(message.to_string());
                Ok(())
            }
            // A notification gets 202 and no body, so the id is never read.
            Transport::Http(http) => http.prepare(&message).send(0).map(drop),
        }
    }

    /// Tells the server to stop request `id`; over HTTP on its own thread so an abort returns at once.
    fn cancel(&self, id: u64) {
        let message = json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": id}});
        match &self.transport {
            Transport::Stdio(process) => _ = process.stdin.send(message.to_string()),
            Transport::Http(http) => {
                let request = http.prepare(&message);
                std::thread::spawn(move || request.send(0));
            }
        }
    }
}

impl Process {
    fn request(&self, id: u64, message: &Value, method: &str, timeout: Duration, abort: &AtomicBool) -> Result<Value, String> {
        let (reply, answer) = mpsc::channel();
        match self.pending.lock().unwrap().as_mut() {
            Some(pending) => pending.insert(id, reply),
            None => return Err(format!("The server exited before {method}.")),
        };
        let _ = self.stdin.send(message.to_string());
        let answer = wait(&answer, method, timeout, abort);
        if let Some(pending) = self.pending.lock().unwrap().as_mut() {
            pending.remove(&id);
        }
        answer
    }
}

fn wait<T>(answer: &Receiver<T>, method: &str, timeout: Duration, abort: &AtomicBool) -> Result<T, String> {
    let start = Instant::now();
    loop {
        match answer.recv_timeout(Duration::from_millis(100)) {
            Ok(answer) => return Ok(answer),
            Err(RecvTimeoutError::Disconnected) => return Err(format!("The server exited during {method}.")),
            Err(RecvTimeoutError::Timeout) if abort.load(Ordering::Relaxed) => return Err("Operation aborted".into()),
            Err(RecvTimeoutError::Timeout) if start.elapsed() >= timeout => return Err(format!("The server did not answer {method}.")),
            Err(RecvTimeoutError::Timeout) => {}
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
    // Raw lines, so one non-UTF-8 line neither stops a reader nor leaves a pipe undrained.
    let last_error = std::thread::spawn(move || {
        let lines = BufReader::new(stderr).split(b'\n').map_while(Result::ok);
        lines.map(|line| String::from_utf8_lossy(&line).trim().to_string()).filter(|line| !line.is_empty()).last()
    });
    let pending: Pending = Arc::new(Mutex::new(Some(HashMap::new())));
    let (reader_pending, replies) = (pending.clone(), lines.clone());
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).split(b'\n').map_while(Result::ok) {
            let Ok(message) = serde_json::from_slice::<Value>(&line) else { continue };
            match (message["id"].as_u64(), message.get("method")) {
                (Some(id), None) => {
                    if let Some(reply) = reader_pending.lock().unwrap().as_mut().and_then(|pending| pending.remove(&id)) {
                        let _ = reply.send(message);
                    }
                }
                (_, Some(_)) if message.get("id").is_some() => {
                    let _ = replies.send(answer_server_request(&message).to_string());
                }
                _ => {}
            }
        }
        *reader_pending.lock().unwrap() = None;
        exited(last_error.join().unwrap_or_default().unwrap_or_default());
    });
    Ok(Process { child, stdin: lines, pending })
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
    agent: ureq::Agent,
    url: String,
    home: PathBuf,
    headers: Vec<(String, String)>,
    has_session: bool,
    body: String,
}

/// What one HTTP POST got back.
enum Reply {
    /// The JSON-RPC answer and the session id the server handed out, if any.
    Answer(Value, Option<String>),
    /// A 404 to a request that carried a session id.
    SessionGone,
}

impl Http {
    /// The answer to `message`, or `None` when the server no longer knows the session.
    fn request(&self, id: u64, message: &Value, method: &str, timeout: Duration, abort: &AtomicBool) -> Result<Option<Value>, String> {
        let request = self.prepare(message);
        let (reply, answer) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = reply.send(request.send(id));
        });
        match wait(&answer, method, timeout, abort)?? {
            Reply::Answer(answer, session) => {
                if session.is_some() {
                    *self.session.lock().unwrap() = session;
                }
                Ok(Some(answer))
            }
            Reply::SessionGone => {
                *self.session.lock().unwrap() = None;
                Ok(None)
            }
        }
    }

    fn prepare(&self, message: &Value) -> HttpRequest {
        let mut headers = self.headers.clone();
        if message["method"] != "initialize" {
            headers.push(("mcp-protocol-version".into(), self.protocol.lock().unwrap().clone()));
        }
        let session = self.session.lock().unwrap().clone();
        let has_session = session.is_some();
        if let Some(session) = session {
            headers.push(("mcp-session-id".into(), session));
        }
        HttpRequest { agent: self.agent.clone(), url: self.url.clone(), home: self.home.clone(), headers, has_session, body: message.to_string() }
    }
}

impl HttpRequest {
    /// Runs on its own thread, so the token refresh in `bearer` also gives way to an abort.
    fn send(self, id: u64) -> Result<Reply, String> {
        let has_authorization = self.headers.iter().any(|(name, _)| name.eq_ignore_ascii_case("authorization"));
        let token = if has_authorization { None } else { super::oauth::bearer(&self.home, &self.url, None)? };
        let mut response = self.post(token.as_deref())?;
        // A token revoked before its expiry gets one refresh and one retry.
        if response.status().as_u16() == 401
            && let Some(rejected) = &token
            && let Some(refreshed) = super::oauth::bearer(&self.home, &self.url, Some(rejected))?
            && refreshed != *rejected
        {
            response = self.post(Some(&refreshed))?;
        }
        let status = response.status().as_u16();
        let header = |name: &str| response.headers().get(name).and_then(|value| value.to_str().ok()).map(String::from);
        let (session, content_type) = (header("mcp-session-id"), header("content-type").unwrap_or_default());
        if status == 401 {
            return Err("The server asks for a login: open /mcp in the interactive agent and press l on this server.".into());
        }
        if status == 404 && self.has_session {
            return Ok(Reply::SessionGone);
        }
        let mut reader = BufReader::new(response.into_body().into_reader());
        if !(200..300).contains(&status) {
            let mut text = String::new();
            let _ = reader.read_to_string(&mut text);
            return Err(format!("The server answered {status}: {}", text.trim().chars().take(500).collect::<String>()));
        }
        if status == 202 {
            return Ok(Reply::Answer(Value::Null, session));
        }
        let answer = if content_type.to_ascii_lowercase().starts_with("text/event-stream") {
            read_reply(reader, id)?
        } else {
            serde_json::from_reader(reader).map_err(|e| format!("The response is not JSON: {e}."))?
        };
        Ok(Reply::Answer(answer, session))
    }

    fn post(&self, token: Option<&str>) -> Result<ureq::http::Response<ureq::Body>, String> {
        let mut request = self.agent.post(&self.url).header("content-type", "application/json").header("accept", "application/json, text/event-stream");
        for (name, value) in &self.headers {
            request = request.header(name, value);
        }
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        request.send(&self.body).map_err(|e| format!("Request to {} failed: {e}.", super::oauth::origin(&self.url)))
    }
}

/// The JSON-RPC answer to `id` in a server-sent event stream: the first matching event, which
/// may come after the server's own notifications.
fn read_reply(reader: impl BufRead, id: u64) -> Result<Value, String> {
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
        assert_eq!(read_reply(Cursor::new(stream), 7).unwrap()["result"]["ok"], true);
        assert!(read_reply(Cursor::new("data: {\"id\":1}\n\n"), 7).is_err());
    }

    /// Writes a non-UTF-8 line to stdout and stderr, answers like a server, and exits on `tools/call`.
    const EXITING_SERVER: &str = r#"
import json, sys
sys.stdout.buffer.write(b"\xff\n"); sys.stdout.flush()
sys.stderr.buffer.write(b"\xff\nlast words\n"); sys.stderr.flush()
for line in sys.stdin:
    m = json.loads(line)
    if "id" not in m:
        continue
    if m["method"] == "tools/call":
        sys.exit(1)
    r = {"tools": [{"name": "t"}]} if m["method"] == "tools/list" else {}
    print(json.dumps({"jsonrpc": "2.0", "id": m["id"], "result": r}), flush=True)
"#;

    #[test]
    fn a_server_with_non_utf8_output_works_and_calls_after_its_exit_fail() {
        let launch = Launch::Stdio { command: "python3".into(), args: vec!["-c".into(), EXITING_SERVER.into()], env: Vec::new(), cwd: std::env::temp_dir() };
        let (exited, exit) = mpsc::channel();
        let (client, tools) = Client::connect(&launch, &std::env::temp_dir(), move |line| exited.send(line).unwrap()).unwrap();
        assert_eq!(tools[0]["name"], "t");
        let abort = Arc::new(AtomicBool::new(false));
        let stopper = abort.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(5));
            stopper.store(true, Ordering::Relaxed);
        });

        assert_eq!(client.call("t", &json!({}), &abort), ("The server exited during tools/call.".into(), true));
        assert_eq!(exit.recv_timeout(Duration::from_secs(5)).unwrap(), "last words");
        assert_eq!(client.call("t", &json!({}), &abort), ("The server exited before tools/call.".into(), true));
    }

    #[test]
    fn an_http_session_carries_its_headers_and_a_lost_session_starts_again() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            let sessions = Arc::new(Mutex::new(0));
            for stream in listener.incoming() {
                let (log, sessions) = (log.clone(), sessions.clone());
                std::thread::spawn(move || serve_mcp(stream.unwrap(), &log, &sessions));
            }
        });
        let (_client, tools) = Client::connect(&Launch::Http { url, headers: Vec::new() }, &std::env::temp_dir(), |_| {}).unwrap();

        assert_eq!(tools[0]["name"], "t");
        let seen = seen.lock().unwrap().clone();
        let lists: Vec<_> = seen.iter().filter(|request| request.contains("tools/list")).collect();
        assert_eq!(lists.len(), 2);
        assert!(lists.iter().all(|request| request.contains("mcp-protocol-version: 2025-03-26")));
        assert!(lists[0].contains("mcp-session-id: s1") && lists[1].contains("mcp-session-id: s2"));
    }

    /// Answers MCP requests on one connection; the first session's `tools/list` gets 404.
    fn serve_mcp(stream: std::net::TcpStream, log: &Mutex<Vec<String>>, sessions: &Mutex<u32>) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        loop {
            let mut head = String::new();
            let mut line = String::new();
            while line != "\r\n" {
                line.clear();
                if reader.read_line(&mut line).unwrap() == 0 {
                    return;
                }
                head.push_str(&line.to_ascii_lowercase());
            }
            let length = head.split("content-length: ").nth(1).and_then(|rest| rest.split("\r\n").next()?.parse().ok()).unwrap_or(0);
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let message: Value = serde_json::from_slice(&body).unwrap();
            log.lock().unwrap().push(format!("{head}{message}"));
            let (status, session, result) = match message["method"].as_str().unwrap() {
                "initialize" => {
                    let mut count = sessions.lock().unwrap();
                    *count += 1;
                    ("200 OK", format!("mcp-session-id: s{count}\r\n"), json!({"protocolVersion": "2025-03-26"}))
                }
                "tools/list" if head.contains("mcp-session-id: s1") => ("404 Not Found", String::new(), Value::Null),
                "tools/list" => ("200 OK", String::new(), json!({"tools": [{"name": "t"}]})),
                _ => ("202 Accepted", String::new(), Value::Null),
            };
            let body = if result.is_null() { String::new() } else { json!({"jsonrpc": "2.0", "id": message["id"], "result": result}).to_string() };
            let answer = format!("HTTP/1.1 {status}\r\ncontent-type: application/json\r\n{session}content-length: {}\r\n\r\n{body}", body.len());
            writer.write_all(answer.as_bytes()).unwrap();
        }
    }

    #[test]
    fn an_aborted_call_is_cancelled_on_the_server() {
        let server = r#"
import json, sys
for line in sys.stdin:
    m = json.loads(line)
    if m["method"] == "notifications/cancelled":
        print("cancelled", m["params"]["requestId"], file=sys.stderr, flush=True)
        sys.exit(0)
    if "id" in m and m["method"] != "tools/call":
        r = {"tools": [{"name": "t"}]} if m["method"] == "tools/list" else {}
        print(json.dumps({"jsonrpc": "2.0", "id": m["id"], "result": r}), flush=True)
"#;
        let launch = Launch::Stdio { command: "python3".into(), args: vec!["-c".into(), server.into()], env: Vec::new(), cwd: std::env::temp_dir() };
        let (exited, exit) = mpsc::channel();
        let (client, _) = Client::connect(&launch, &std::env::temp_dir(), move |line| _ = exited.send(line)).unwrap();

        assert_eq!(client.call("t", &json!({}), &AtomicBool::new(true)), ("Operation aborted".into(), true));
        assert_eq!(exit.recv_timeout(Duration::from_secs(5)).unwrap(), "cancelled 3");
    }

    #[test]
    fn tool_results_become_text() {
        let result = json!({"content": [{"type": "text", "text": "hi"}, {"type": "image", "mimeType": "image/png", "data": "…"}]});
        assert_eq!(result_text(&result), "hi\n[image image/png]");
        assert_eq!(result_text(&json!({"content": [], "structuredContent": {"n": 1}})), "{\"n\":1}");
    }
}
