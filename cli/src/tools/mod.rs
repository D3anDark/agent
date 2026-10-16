use serde_json::Value;
use std::io::Read;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub mod ask;
pub mod bg;
pub mod external;
pub mod goal;
pub mod guard;
pub mod hashline;
pub mod plan;
pub mod rtk;
pub mod todo;
pub mod web;

use hashline::{Anchors, resolve, str_arg};
use rtk::Rtk;

pub const MAX_LINES: usize = 2000;
pub const MAX_BYTES: usize = 50 * 1024;
const MAX_COLUMNS: usize = 500;

const SCHEMAS: &str = include_str!("tools.json");

/// pi's tool-loader groups: their tools stay out of requests until load_tools asks for the group.
pub const GROUPS: [(&str, &[&str]); 3] = [
    ("agents", &["subagent", "bg_wait"]),
    ("terminals", &["bg_start", "bg_status", "bg_list", "bg_kill"]),
    ("web", &["web_search", "source_check", "fetch_content", "get_search_content"]),
];

/// The base tools plus those of the `loaded` groups, in tools.json order; `subagent` carries the swarm prompt like jcode's `swarm`.
pub fn schemas(loaded: &[String], swarm_prompt: &str, context_tree_query: bool) -> Value {
    let all: Vec<Value> = serde_json::from_str(SCHEMAS).expect("tools.json is valid JSON");
    let hidden = |name: &str| {
        (name == "context_tree_query" && !context_tree_query) || GROUPS.iter().any(|(group, names)| names.contains(&name) && !loaded.iter().any(|l| l == group))
    };
    let mut tools: Vec<Value> = all.into_iter().filter(|tool| !hidden(tool["name"].as_str().unwrap_or_default())).collect();
    if let Some(tool) = tools.iter_mut().find(|tool| tool["name"] == "subagent") {
        let base = tool["description"].as_str().unwrap_or_default();
        tool["description"] = format!("{base}\n\nSwarm prompt (user-tunable via ~/.agents/swarm-prompt.md):\n{swarm_prompt}").into();
    }
    Value::Array(tools)
}

/// Runs one tool call. The bool is true when the result is an error. Setting `abort` kills a running bash command.
pub fn run(name: &str, input: &Value, cwd: &Path, anchors: &mut Anchors, rtk: &Rtk, abort: &Arc<AtomicBool>) -> (String, bool) {
    let result = match name {
        "read" => anchors.read(cwd, input),
        "bash" => bash(input, cwd, rtk, abort),
        "write" => write(input, cwd, anchors),
        "replace" => anchors.replace(input),
        "replace_match" => anchors.replace_match(input),
        "insert" => anchors.insert(input),
        "copy" => anchors.copy(input),
        "move" => anchors.move_lines(input),
        "undo_last_change" => anchors.undo(cwd, input),
        "rg" => rg(input, cwd, anchors),
        "fd" => fd(input, cwd),
        _ => Err(format!("Tool {name} not found")),
    };
    match result {
        Ok(text) => (text, false),
        Err(text) => (text, true),
    }
}

enum Killed {
    Timeout,
    Aborted,
}

fn bash(input: &Value, cwd: &Path, rtk: &Rtk, abort: &Arc<AtomicBool>) -> Result<String, String> {
    let command = rtk.rewrite(str_arg(input, &["command"])?);
    let timeout = input["timeout"].as_f64().filter(|s| *s > 0.0);
    let mut child = Command::new("bash")
        .arg("-c")
        .arg(format!("exec 2>&1\n{command}"))
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("Cannot start bash: {e}"))?;
    let pid = child.id();
    let (done, finished) = mpsc::channel::<()>();
    let (killed_tx, killed) = mpsc::channel::<Killed>();
    let watch_abort = abort.clone();
    std::thread::spawn(move || {
        let deadline = timeout.map(|secs| Instant::now() + Duration::from_secs_f64(secs));
        while finished.recv_timeout(Duration::from_millis(100)) == Err(mpsc::RecvTimeoutError::Timeout) {
            let reason = if watch_abort.load(Ordering::Relaxed) {
                Killed::Aborted
            } else if deadline.is_some_and(|d| Instant::now() >= d) {
                Killed::Timeout
            } else {
                continue;
            };
            let _ = killed_tx.send(reason);
            let _ = Command::new("kill").args(["-9", "--", &format!("-{pid}")]).stderr(Stdio::null()).status();
            return;
        }
    });
    let mut raw = Vec::new();
    child.stdout.take().unwrap().read_to_end(&mut raw).map_err(|e| e.to_string())?;
    let status = child.wait().map_err(|e| e.to_string())?;
    let _ = done.send(());
    let output = rtk.compact(truncate_tail(String::from_utf8_lossy(&raw).into_owned()));
    match killed.try_recv() {
        Ok(Killed::Timeout) => return Err(append_status(&output, &format!("Command timed out after {} seconds", timeout.unwrap()))),
        Ok(Killed::Aborted) => return Err(append_status(&output, "Command aborted")),
        Err(_) => {}
    }
    let code = status.code().unwrap_or(128 + status.signal().unwrap_or(0));
    if code != 0 {
        return Err(append_status(&output, &format!("Command exited with code {code}")));
    }
    Ok(if output.is_empty() { "(no output)".into() } else { output })
}

fn append_status(output: &str, status: &str) -> String {
    if output.is_empty() { status.to_string() } else { format!("{output}\n\n{status}") }
}

fn write(input: &Value, cwd: &Path, anchors: &mut Anchors) -> Result<String, String> {
    let path = resolve(cwd, str_arg(input, &["path"])?);
    let content = str_arg(input, &["content"])?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Cannot create {}: {e}", parent.display()))?;
    }
    std::fs::write(&path, content).map_err(|e| format!("Cannot write {}: {e}", path.display()))?;
    let path = path.canonicalize().map_err(|e| e.to_string())?;
    let rows = anchors.written(&path)?;
    Ok(format!("Successfully wrote to {}\n\n\n--- Auto-read (hashline anchors) ---\n{rows}", path.display()))
}

/// pi file-search's rg argv as JSON events: the pattern after `--`, counts clamped.
fn rg_args(input: &Value) -> Result<Vec<String>, String> {
    let mut args = vec!["--json".to_string()];
    args.push(match input["case_sensitive"].as_bool() {
        Some(true) => "--case-sensitive".into(),
        Some(false) => "--ignore-case".into(),
        None => "--smart-case".into(),
    });
    if input["fixed_strings"].as_bool() == Some(true) {
        args.push("--fixed-strings".into());
    }
    if input["hidden"].as_bool() == Some(true) {
        args.push("--hidden".into());
    }
    if !input["context"].is_null() {
        args.extend(["--context".into(), clamped(&input["context"], 0, 0, 20)]);
    }
    if let Some(glob) = input["glob"].as_str().filter(|g| !g.is_empty()) {
        args.extend(["--glob".into(), glob.into()]);
    }
    if let Some(file_type) = input["file_type"].as_str().filter(|t| !t.is_empty()) {
        args.extend(["--type".into(), file_type.into()]);
    }
    args.extend(["--max-count".into(), clamped(&input["limit"], 100, 1, 1000)]);
    args.extend(["--".into(), str_arg(input, &["pattern"])?.into()]);
    args.extend(search_path(input));
    Ok(args)
}

fn fd_args(input: &Value) -> Result<Vec<String>, String> {
    let mut args: Vec<String> = vec!["--color=never".into()];
    if input["hidden"].as_bool() == Some(true) {
        args.push("--hidden".into());
    }
    if input["glob"].as_bool() == Some(true) {
        args.push("--glob".into());
    }
    if let Some(kind) = input["type"].as_str() {
        let short = match kind {
            "file" => "f",
            "directory" => "d",
            "symlink" => "l",
            other => return Err(format!("Unknown type \"{other}\". Use file, directory or symlink.")),
        };
        args.extend(["--type".into(), short.into()]);
    }
    if let Some(extension) = input["extension"].as_str().filter(|e| !e.is_empty()) {
        args.extend(["--extension".into(), extension.trim_start_matches('.').into()]);
    }
    if !input["max_depth"].is_null() {
        args.extend(["--max-depth".into(), clamped(&input["max_depth"], 1, 1, 64)]);
    }
    args.extend(["--max-results".into(), clamped(&input["limit"], 1000, 1, 10_000)]);
    args.extend(["--".into(), input["pattern"].as_str().unwrap_or("").into()]);
    args.extend(search_path(input));
    Ok(args)
}

fn clamped(value: &Value, default: u64, min: u64, max: u64) -> String {
    let number = value.as_f64().map_or(default, |n| n.floor().max(0.0) as u64);
    number.clamp(min, max).to_string()
}

/// The `path` argument with pi's normalization: a leading `@` dropped, `~` expanded.
fn search_path(input: &Value) -> Option<String> {
    let path = input["path"].as_str()?.trim();
    let path = path.strip_prefix('@').unwrap_or(path);
    let home = std::env::var("HOME").unwrap_or_default();
    let path = match path.strip_prefix('~') {
        Some("") => home,
        Some(rest) if rest.starts_with('/') => format!("{home}{rest}"),
        _ => path.to_string(),
    };
    (!path.is_empty()).then_some(path)
}

fn rg(input: &Value, cwd: &Path, anchors: &mut Anchors) -> Result<String, String> {
    let rows = rg_rows(&search("rg", &rg_args(input)?, cwd)?, cwd, anchors)?;
    Ok(if rows.is_empty() { "No matches found".into() } else { truncate_search(rows, "rg") })
}

/// rg's JSON events as `path:anchor│line` rows, `path-anchor│line` for context, so an edit can follow without a read.
/// A file the anchors cannot track (binary, not UTF-8) shows line numbers instead.
fn rg_rows(json: &str, cwd: &Path, anchors: &mut Anchors) -> Result<String, String> {
    let mut rows = Vec::new();
    let mut tracked: Option<PathBuf> = None;
    for line in json.lines() {
        let event: Value = serde_json::from_str(line).map_err(|e| format!("Cannot parse rg output: {e}"))?;
        let data = &event["data"];
        let path = data["path"]["text"].as_str().unwrap_or_default();
        let separator = match event["type"].as_str() {
            Some("begin") => {
                tracked = cwd.join(path).canonicalize().ok().filter(|file| anchors.track(file).is_ok());
                continue;
            }
            Some("match") => ':',
            Some("context") => '-',
            _ => continue,
        };
        let number = data["line_number"].as_u64().unwrap_or_default() as usize;
        let mark = match tracked.as_deref().and_then(|file| anchors.line_anchor(file, number)) {
            Some(anchor) => format!("{anchor}│"),
            None => format!("{number}{separator}"),
        };
        let text = data["lines"]["text"].as_str().map_or("[line is not UTF-8]", |text| text.trim_end_matches(['\n', '\r']));
        rows.push(format!("{path}{separator}{mark}{}", cut_columns(text)));
    }
    Ok(rows.join("\n"))
}

/// rg ignores --max-columns in JSON mode, so long lines are cut here, the way its preview cuts them.
fn cut_columns(text: &str) -> String {
    match text.char_indices().nth(MAX_COLUMNS) {
        Some((end, _)) => format!("{} [... omitted end of long line]", &text[..end]),
        None => text.to_string(),
    }
}

fn fd(input: &Value, cwd: &Path) -> Result<String, String> {
    let files = search("fd", &fd_args(input)?, cwd)?;
    Ok(if files.is_empty() { "No files found".into() } else { truncate_search(files, "fd") })
}

fn search(program: &str, args: &[String], cwd: &Path) -> Result<String, String> {
    let output = Command::new(program).args(args).current_dir(cwd).stdin(Stdio::null()).output().map_err(|e| format!("Cannot run {program}: {e}"))?;
    let code = output.status.code().unwrap_or(-1);
    // rg exits 1 for "no matches"; fd exits 0 even with no results.
    if code != 0 && !(program == "rg" && code == 1) {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("{program} failed: {}", if stderr.is_empty() { format!("exit code {code}") } else { stderr }));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim_end_matches('\n').to_string())
}

/// pi file-search's page: the first MAX_LINES / MAX_BYTES, with the full output in a temp file.
fn truncate_search(text: String, label: &str) -> String {
    let Some(head) = cut_head(&text, label) else { return text };
    let content = &text[..head.bytes];
    format!(
        "{content}\n\n[Output truncated: {} of {} lines ({} of {}). Full output saved to: {}. Read it with offset={} to continue.]",
        head.lines,
        head.total,
        format_size(content.len()),
        format_size(text.len()),
        head.path,
        head.lines + 1
    )
}

/// Keeps the first MAX_LINES / MAX_BYTES of `text`; the full text goes to a temp file named after `label`.
pub fn truncate_head(text: String, label: &str) -> String {
    let Some(head) = cut_head(&text, label) else { return text };
    format!("{}\n\n[Showing lines 1-{} of {}. Full output: {}]", &text[..head.bytes], head.lines, head.total, head.path)
}

struct Head {
    bytes: usize,
    lines: usize,
    total: usize,
    path: String,
}

/// The longest head of `text` within MAX_LINES / MAX_BYTES, or None when it all fits; spills the full text.
fn cut_head(text: &str, label: &str) -> Option<Head> {
    let total = text.lines().count();
    if total <= MAX_LINES && text.len() <= MAX_BYTES {
        return None;
    }
    let mut lines = 0;
    let mut bytes = 0;
    for line in text.lines().take(MAX_LINES) {
        if bytes + line.len() + 1 > MAX_BYTES {
            break;
        }
        bytes += line.len() + 1;
        lines += 1;
    }
    Some(Head { bytes: bytes.saturating_sub(1), lines, total, path: spill(text, label) })
}

/// Keeps the last MAX_LINES / MAX_BYTES of `text`, like pi's bash tool.
fn truncate_tail(text: String) -> String {
    let total = text.lines().count();
    if total <= MAX_LINES && text.len() <= MAX_BYTES {
        return text;
    }
    let mut kept = 0;
    let mut bytes = 0;
    for line in text.lines().rev().take(MAX_LINES) {
        if bytes + line.len() + 1 > MAX_BYTES {
            break;
        }
        bytes += line.len() + 1;
        kept += 1;
    }
    let path = spill(&text, "bash");
    let tail = text.trim_end_matches('\n');
    let tail = &tail[tail.len() - bytes.saturating_sub(1).min(tail.len())..];
    format!("{tail}\n\n[Showing lines {}-{total} of {total}. Full output: {path}]", total - kept + 1)
}

fn spill(text: &str, label: &str) -> String {
    let path = std::env::temp_dir().join(format!("agent-{label}-{}-{}.log", std::process::id(), crate::now_ms()));
    match std::fs::write(&path, text) {
        Ok(()) => path.display().to_string(),
        Err(e) => format!("(not saved: {e})"),
    }
}

pub fn format_size(bytes: usize) -> String {
    match bytes {
        b if b < 1024 => format!("{b}B"),
        b if b < 1024 * 1024 => format!("{:.1}KB", b as f64 / 1024.0),
        b => format!("{:.1}MB", b as f64 / (1024.0 * 1024.0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run_bash(input: Value, abort: &Arc<AtomicBool>) -> (String, bool) {
        run("bash", &input, Path::new("/tmp"), &mut Anchors::new("t"), &Rtk::OFF, abort)
    }

    #[test]
    fn context_tree_query_is_offered_only_when_asked() {
        let offers = |query: bool| schemas(&[], "", query).as_array().unwrap().iter().any(|tool| tool["name"] == "context_tree_query");
        assert!(offers(true));
        assert!(!offers(false));
    }

    #[test]
    fn bash_reports_exit_code_and_merged_output() {
        let (out, err) = run_bash(json!({"command": "echo out; echo err >&2; exit 3", "timeout": null}), &Arc::default());
        assert!(err);
        assert_eq!(out, "out\nerr\n\n\nCommand exited with code 3");
    }

    #[test]
    fn bash_timeout_kills_the_command() {
        let (out, err) = run_bash(json!({"command": "echo start; sleep 30", "timeout": 0.5}), &Arc::default());
        assert!(err);
        assert_eq!(out, "start\n\n\nCommand timed out after 0.5 seconds");
    }

    #[test]
    fn abort_kills_a_running_command() {
        let abort = Arc::new(AtomicBool::new(false));
        let setter = abort.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            setter.store(true, Ordering::Relaxed);
        });
        let started = Instant::now();
        let (out, err) = run_bash(json!({"command": "echo start; sleep 30"}), &abort);
        assert!(err && started.elapsed() < Duration::from_secs(5));
        assert_eq!(out, "start\n\n\nCommand aborted");
    }

    #[test]
    fn tail_truncation_keeps_the_end() {
        let text: String = (1..=2500).map(|i| format!("{i}\n")).collect();
        let out = truncate_tail(text);
        assert!(out.starts_with("501\n"));
        assert!(out.contains("[Showing lines 501-2500 of 2500."));
    }

    #[test]
    fn rg_rows_carry_anchors_that_replace_accepts_without_a_read() {
        let dir = std::env::temp_dir().join(format!("agent-rg-anchors-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "alpha\nbeta\ngamma\n").unwrap();
        let mut anchors = Anchors::new("");
        let abort = Arc::new(AtomicBool::new(false));
        let (found, is_error) = run("rg", &json!({"pattern": "beta", "context": 1}), &dir, &mut anchors, &Rtk::OFF, &abort);
        assert!(!is_error, "{found}");
        let rows: Vec<&str> = found.lines().collect();
        assert_eq!(rows.len(), 3, "{found}");
        assert!(rows[0].starts_with("a.txt-") && rows[0].ends_with("│alpha"), "{found}");
        let anchor = rows[1].strip_prefix("a.txt:").and_then(|row| row.strip_suffix("│beta")).expect(&found);
        let (replaced, is_error) = run("replace", &json!({"remove_from": anchor, "remove_to": anchor, "text": "BETA"}), &dir, &mut anchors, &Rtk::OFF, &abort);
        assert!(!is_error, "{replaced}");
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "alpha\nBETA\ngamma\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn search_arguments_match_pi_file_search() {
        let rg = rg_args(&json!({"pattern": "-x", "path": "@src", "context": 50, "limit": 5000, "case_sensitive": false, "glob": "*.rs"})).unwrap();
        assert_eq!(
            rg.join(" "),
            "--json --ignore-case --context 20 --glob *.rs --max-count 1000 -- -x src"
        );
        let fd = fd_args(&json!({"type": "file", "extension": ".rs", "max_depth": 0, "limit": 0})).unwrap();
        assert_eq!(fd.join(" "), "--color=never --type f --extension rs --max-depth 1 --max-results 1 -- ");
    }
}
