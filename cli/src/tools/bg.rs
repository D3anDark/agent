use serde_json::Value;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::agent::FollowUp;
use crate::tools::format_size;

/// pi's background-terminals limits.
const MAX_RUNNING: usize = 8;
const STATUS_TAIL: (u64, usize) = (16 * 1024, 400);
const STATUS_STDERR_TAIL: (u64, usize) = (8 * 1024, 200);
const RESULT_TAIL: (u64, usize) = (8 * 1024, 40);
const RESULT_STDERR_TAIL: (u64, usize) = (4 * 1024, 20);
// shortcut: the viewer rewraps the whole tail each time the log grows; past 1 MiB it points to the full log file.
const VIEW_TAIL: (u64, usize) = (1024 * 1024, 20_000);
const FORCE_KILL_AFTER: Duration = Duration::from_secs(2);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// The background terminals of one agent; shared with the UI for /ps and the widget.
#[derive(Default)]
pub struct Terminals {
    pub list: Vec<Arc<Terminal>>,
    counter: u64,
}

pub struct Terminal {
    pub id: String,
    pub title: String,
    pub cwd: PathBuf,
    pub pid: u32,
    started: Instant,
    stdout: PathBuf,
    stderr: PathBuf,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    exit: Option<(Status, String)>,
    settled: Option<Instant>,
    killed: bool,
    /// The model already saw the final state (bg_status or bg_kill), so no follow-up.
    reported: bool,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Status {
    Running,
    Done,
    Failed,
    Killed,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Running => "running",
            Status::Done => "done",
            Status::Failed => "failed",
            Status::Killed => "killed",
        }
    }
}

impl Terminals {
    pub fn run(&mut self, name: &str, input: &Value, cwd: &Path, follow_ups: &Arc<Mutex<Vec<FollowUp>>>) -> Result<String, String> {
        match name {
            "bg_start" => self.start(input, cwd, follow_ups),
            "bg_status" => {
                let terminal = self.find(input["id"].as_str().unwrap_or_default())?;
                let mut state = terminal.state.lock().unwrap();
                if state.exit.is_some() {
                    state.reported = true;
                    follow_ups.lock().unwrap().retain(|f| f.source != terminal.id);
                }
                drop(state);
                Ok(terminal.status_text())
            }
            "bg_list" if self.list.is_empty() => Ok("No background terminals.".into()),
            "bg_list" => Ok(self.list.iter().map(|t| t.describe()).collect::<Vec<_>>().join("\n")),
            _ => Err(format!("Tool {name} not found")),
        }
    }

    fn start(&mut self, input: &Value, cwd: &Path, follow_ups: &Arc<Mutex<Vec<FollowUp>>>) -> Result<String, String> {
        let command = input["command"].as_str().unwrap_or_default().trim();
        if command.is_empty() {
            return Err("command must not be empty.".into());
        }
        let cwd = match input["working_dir"].as_str() {
            Some(dir) => cwd.join(dir),
            None => cwd.to_path_buf(),
        };
        if !cwd.is_dir() {
            return Err(format!("working_dir is not a directory: {}", cwd.display()));
        }
        if self.list.iter().filter(|t| t.status() == Status::Running).count() >= MAX_RUNNING {
            return Err(format!("Max {MAX_RUNNING} background terminals can run concurrently. Stop one with bg_kill before starting another."));
        }
        let title: String = input["title"].as_str().unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" ").chars().take(80).collect();
        let title = if title.is_empty() { "terminal".to_string() } else { title };
        self.counter += 1;
        let id = format!("bt-{}", self.counter);
        let dir = std::env::temp_dir().join(format!("agent-bg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
        let (stdout, stderr) = (dir.join(format!("{id}.stdout")), dir.join(format!("{id}.stderr")));
        let file = |path: &Path| File::create(path).map_err(|e| format!("Cannot create {}: {e}", path.display()));
        let mut child = crate::process::command("sh")
            .args(["-c", command])
            .current_dir(&cwd)
            .stdout(file(&stdout)?)
            .stderr(file(&stderr)?)
            .spawn()
            .map_err(|e| format!("Cannot start sh: {e}"))?;
        let terminal = Arc::new(Terminal { id, title, cwd, pid: child.id(), started: Instant::now(), stdout, stderr, state: Mutex::default() });
        self.list.push(terminal.clone());
        let follow_ups = follow_ups.clone();
        let watched = terminal.clone();
        std::thread::spawn(move || {
            let status = child.wait();
            let mut state = watched.state.lock().unwrap();
            let exit = match status {
                Ok(status) if state.killed => (Status::Killed, exit_text(status.code(), status.signal())),
                Ok(status) if status.success() => (Status::Done, "exit 0".to_string()),
                Ok(status) => (Status::Failed, exit_text(status.code(), status.signal())),
                Err(e) => (Status::Failed, format!("wait failed: {e}")),
            };
            state.exit = Some(exit);
            state.settled = Some(Instant::now());
            let report = !state.reported;
            drop(state);
            if report {
                follow_ups.lock().unwrap().push(FollowUp { source: watched.id.clone(), text: watched.result_text() });
            }
        });
        Ok(format!(
            "Started background terminal {id} \"{title}\" (pid {pid}, {cwd}).\nIt runs in the background with no stdin. You'll get a message when it exits, or use bg_status(id: \"{id}\") to peek, bg_kill to stop it, bg_list to see all.",
            id = terminal.id,
            title = terminal.title,
            pid = terminal.pid,
            cwd = terminal.cwd.display()
        ))
    }

    fn find(&self, id: &str) -> Result<Arc<Terminal>, String> {
        let found = self.list.iter().find(|t| t.id == id).cloned();
        found.ok_or_else(|| format!("Unknown terminal id \"{id}\". Known: {}.", self.known()))
    }

    fn known(&self) -> String {
        let ids: Vec<&str> = self.list.iter().map(|t| t.id.as_str()).collect();
        if ids.is_empty() { "none".into() } else { ids.join(", ") }
    }

    /// The terminals `bg_kill` names; it runs without the lock, since stopping can take seconds.
    pub fn for_kill(&self, input: &Value) -> Result<Vec<Arc<Terminal>>, String> {
        let mut ids: Vec<&str> = Vec::new();
        for id in input["ids"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        if ids.is_empty() {
            return Err("Provide at least one terminal id.".into());
        }
        let unknown: Vec<&str> = ids.iter().copied().filter(|id| self.list.iter().all(|t| t.id != *id)).collect();
        if !unknown.is_empty() {
            return Err(format!("Unknown terminal id(s): {}. Known: {}.", unknown.join(", "), self.known()));
        }
        Ok(ids.iter().filter_map(|id| self.list.iter().find(|t| t.id == *id).cloned()).collect())
    }

    /// Process groups do not end with the agent, so quitting kills them.
    pub fn kill_all(&self) {
        for terminal in &self.list {
            if terminal.status() == Status::Running {
                let _ = crate::process::command("kill").args(["-KILL", "--", &format!("-{}", terminal.pid)]).stderr(Stdio::null()).status();
            }
        }
    }

    pub fn running(&self) -> usize {
        self.list.iter().filter(|t| t.status() == Status::Running).count()
    }
}

/// bg_kill: SIGTERM to each group, SIGKILL after two seconds, then the final states.
pub fn kill(terminals: &[Arc<Terminal>], follow_ups: &Arc<Mutex<Vec<FollowUp>>>) -> String {
    let was_running: Vec<bool> = terminals.iter().map(|t| t.stop()).collect();
    let deadline = Instant::now() + STOP_TIMEOUT;
    while terminals.iter().any(|t| t.status() == Status::Running) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut lines = Vec::new();
    for (terminal, was_running) in terminals.iter().zip(was_running) {
        terminal.state.lock().unwrap().reported = true;
        follow_ups.lock().unwrap().retain(|f| f.source != terminal.id);
        let (status, exit) = terminal.exit();
        let (id, title) = (&terminal.id, &terminal.title);
        lines.push(match status {
            Status::Running => format!("{id} \"{title}\" did not stop within {}s.", STOP_TIMEOUT.as_secs()),
            _ if !was_running => format!("{id} \"{title}\" was already {} ({exit}).", status.as_str()),
            Status::Killed => format!("Killed {id} \"{title}\" ({exit})."),
            _ => format!("{id} \"{title}\" exited on its own before the kill landed ({exit})."),
        });
    }
    lines.join("\n")
}

impl Terminal {
    pub fn status(&self) -> Status {
        self.exit().0
    }

    fn exit(&self) -> (Status, String) {
        let state = self.state.lock().unwrap();
        state.exit.clone().unwrap_or((Status::Running, "running".into()))
    }

    /// Sends SIGTERM and escalates to SIGKILL later; false when it had already exited.
    pub fn stop(&self) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.exit.is_some() {
            return false;
        }
        state.killed = true;
        drop(state);
        let group = format!("-{}", self.pid);
        let _ = crate::process::command("kill").args(["-TERM", "--", &group]).stderr(Stdio::null()).status();
        std::thread::spawn(move || {
            std::thread::sleep(FORCE_KILL_AFTER);
            let _ = crate::process::command("kill").args(["-KILL", "--", &group]).stderr(Stdio::null()).status();
        });
        true
    }

    fn describe(&self) -> String {
        let (status, exit) = self.exit();
        let exit = if status == Status::Running { "exit -".to_string() } else { exit };
        let size = |path: &Path| format_size(std::fs::metadata(path).map_or(0, |m| m.len() as usize));
        format!(
            "{} [{}] \"{}\" (pid {}, {}, {exit}, {}, stdout {}, stderr {})",
            self.id,
            status.as_str(),
            self.title,
            self.pid,
            self.elapsed(),
            self.cwd.display(),
            size(&self.stdout),
            size(&self.stderr)
        )
    }

    pub fn elapsed(&self) -> String {
        let end = self.state.lock().unwrap().settled.unwrap_or_else(Instant::now);
        let seconds = end.duration_since(self.started).as_secs_f64().round() as u64;
        if seconds >= 60 { format!("{}m{:02}s", seconds / 60, seconds % 60) } else { format!("{seconds}s") }
    }

    /// Combined size of both logs; the /ps viewer rereads `log` when it changes.
    pub fn log_size(&self) -> u64 {
        [&self.stdout, &self.stderr].iter().map(|path| std::fs::metadata(path).map_or(0, |m| m.len())).sum()
    }

    /// Both streams for the /ps viewer, stderr only when it has something.
    pub fn log(&self) -> String {
        let mut text = section("stdout", &self.stdout, VIEW_TAIL);
        if std::fs::metadata(&self.stderr).is_ok_and(|m| m.len() > 0) {
            text.push_str(&format!("\n\n{}", section("stderr", &self.stderr, VIEW_TAIL)));
        }
        text
    }

    fn status_text(&self) -> String {
        let stdout = section("stdout", &self.stdout, STATUS_TAIL);
        let stderr = section("stderr", &self.stderr, STATUS_STDERR_TAIL);
        format!("{}\n\n{stdout}\n\n{stderr}", self.describe())
    }

    fn result_text(&self) -> String {
        let (status, exit) = self.exit();
        let how = if status == Status::Killed { "was killed".to_string() } else { format!("exited ({exit})") };
        let mut text = format!("Background terminal {} \"{}\" {how} after {}.", self.id, self.title, self.elapsed());
        text.push_str(&format!("\n\n{}", section("stdout", &self.stdout, RESULT_TAIL)));
        if std::fs::metadata(&self.stderr).is_ok_and(|m| m.len() > 0) {
            text.push_str(&format!("\n\n{}", section("stderr", &self.stderr, RESULT_STDERR_TAIL)));
        }
        text
    }
}

fn exit_text(code: Option<i32>, signal: Option<i32>) -> String {
    match (code, signal) {
        (Some(code), _) => format!("exit {code}"),
        (None, Some(2)) => "SIGINT".into(),
        (None, Some(9)) => "SIGKILL".into(),
        (None, Some(15)) => "SIGTERM".into(),
        (None, Some(signal)) => format!("SIG{signal}"),
        (None, None) => "exit ?".into(),
    }
}

/// pi's output section: the stream's tail within `max_bytes` and `max_lines`, with a pointer to the full log.
fn section(label: &str, path: &Path, (max_bytes, max_lines): (u64, usize)) -> String {
    let total = std::fs::metadata(path).map_or(0, |m| m.len());
    if total == 0 {
        return format!("{label}: (empty)");
    }
    let mut raw = Vec::new();
    if let Ok(mut file) = File::open(path) {
        let _ = file.seek(SeekFrom::Start(total.saturating_sub(max_bytes)));
        let _ = file.take(max_bytes).read_to_end(&mut raw);
    }
    let text = String::from_utf8_lossy(&raw);
    let mut lines: Vec<&str> = text.trim_end_matches('\n').split('\n').collect();
    if total > max_bytes && lines.len() > 1 {
        lines.remove(0);
    }
    let kept = &lines[lines.len().saturating_sub(max_lines)..];
    let shown = kept.join("\n");
    let mut out = format!("{label}:\n{shown}");
    if shown.len() as u64 + 1 < total {
        out.push_str(&format!("\n[{label} truncated: showing last {} of {}. Full log: {}]", format_size(shown.len()), format_size(total as usize), path.display()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn exit_is_reported_once_unless_status_already_saw_it() {
        let dir = std::env::temp_dir();
        let follow_ups = Arc::default();
        let mut terminals = Terminals::default();
        let started = terminals.run("bg_start", &json!({"command": "echo out; echo err >&2; exit 3", "title": "  a\nb "}), &dir, &follow_ups).unwrap();
        assert!(started.starts_with("Started background terminal bt-1 \"a b\""));
        let deadline = Instant::now() + Duration::from_secs(5);
        while follow_ups.lock().unwrap().is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let text = follow_ups.lock().unwrap()[0].text.clone();
        assert!(text.starts_with("Background terminal bt-1 \"a b\" exited (exit 3) after 0s.\n\nstdout:\nout\n\nstderr:\nerr"), "{text}");
        let status = terminals.run("bg_status", &json!({"id": "bt-1"}), &dir, &follow_ups).unwrap();
        assert!(status.starts_with("bt-1 [failed] \"a b\""), "{status}");
        assert!(follow_ups.lock().unwrap().is_empty());
        assert_eq!(terminals.run("bg_status", &json!({"id": "bt-9"}), &dir, &follow_ups).unwrap_err(), "Unknown terminal id \"bt-9\". Known: bt-1.");
    }

    #[test]
    fn kill_stops_the_whole_group_without_a_follow_up() {
        let dir = std::env::temp_dir();
        let follow_ups = Arc::default();
        let mut terminals = Terminals::default();
        terminals.run("bg_start", &json!({"command": "sleep 30 & sleep 30", "title": "sleeper"}), &dir, &follow_ups).unwrap();
        let targets = terminals.for_kill(&json!({"ids": ["bt-1"]})).unwrap();
        assert_eq!(kill(&targets, &follow_ups), "Killed bt-1 \"sleeper\" (SIGTERM).");
        std::thread::sleep(Duration::from_millis(100));
        assert!(follow_ups.lock().unwrap().is_empty());
        assert_eq!(kill(&targets, &follow_ups), "bt-1 \"sleeper\" was already killed (SIGTERM).");
    }
}
