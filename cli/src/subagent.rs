use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::agent::FollowUp;
use crate::permissions::Mode;
use crate::prompt;

const DEFAULT_SWARM_PROMPT: &str = include_str!("swarm-prompt.md");
const MAX_PER_SESSION: usize = 64;
/// Sent ahead of every task, since the child's system prompt is the root's.
const CHILD_BRIEF: &str = "You are a subagent. Another agent spawned you for the task below and reads only your final answer. \
Nobody answers questions here: finish the task on your own, making the reasonable call where it is unclear. \
End with a short report: what you found or changed with `path:line` references, how you checked it, and what you did not do and why.";

/// jcode's swarm prompt lookup: `.agents/swarm-prompt.md`, then `~/.agents/swarm-prompt.md`, then the built-in.
pub fn swarm_prompt(agent_dir: &Path, cwd: &Path, implementation_effort: &str) -> String {
    [cwd.join(".agents"), agent_dir.to_path_buf()]
        .iter()
        .filter_map(|dir| prompt::read_text(&dir.join("swarm-prompt.md")))
        .map(|text| text.trim().to_string())
        .find(|text| !text.is_empty())
        .unwrap_or_else(|| DEFAULT_SWARM_PROMPT.trim().to_string())
        .replace("{implementation_effort}", implementation_effort)
}

/// One child process, shared with /subagents.
pub struct Run {
    pub id: String,
    pub label: String,
    pub task: String,
    pub background: bool,
    started: Instant,
    child: Mutex<Option<Child>>,
    pub state: Mutex<RunState>,
}

#[derive(Default)]
pub struct RunState {
    pub tool_calls: u64,
    pub last_tool: String,
    /// Some once the child exited: its last answer, or the error.
    pub result: Option<Result<String, String>>,
    pub session: Option<String>,
    pub elapsed: Option<Duration>,
}

impl Run {
    pub fn status(&self) -> &'static str {
        match &self.state.lock().unwrap().result {
            None => "running",
            Some(Ok(_)) => "done",
            Some(Err(_)) => "failed",
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.state.lock().unwrap().elapsed.unwrap_or_else(|| self.started.elapsed())
    }

    pub fn stop(&self) {
        if let Some(child) = self.child.lock().unwrap().as_mut() {
            let _ = child.kill();
        }
    }

    /// The row /subagents and the transcript show: label, last tool, call count, time.
    pub fn summary(&self) -> String {
        let (status, elapsed) = (self.status(), self.elapsed().as_secs());
        let state = self.state.lock().unwrap();
        let tool = if state.last_tool.is_empty() { String::new() } else { format!(", last {}", state.last_tool) };
        format!("{} {} {status}: {} tool calls{tool}, {elapsed}s", self.id, self.label, state.tool_calls)
    }

    pub fn result_text(&self) -> String {
        let state = self.state.lock().unwrap();
        match &state.result {
            Some(Ok(answer)) => answer.clone(),
            Some(Err(error)) => format!("Error: {error}"),
            None => "Error: still running".into(),
        }
    }
}

/// What a child needs from its parent.
pub struct Launch {
    pub label: String,
    pub task: String,
    pub cwd: PathBuf,
    pub model: String,
    pub thinking: String,
    pub mode: Mode,
    pub parent_session: String,
}

#[derive(Default)]
pub struct Runs {
    pub list: Vec<Arc<Run>>,
}

impl Runs {
    /// Starts the child and a thread reading its JSONL; a background run queues its answer as a follow-up.
    pub fn spawn(&mut self, launch: Launch, background: bool, follow_ups: &Arc<Mutex<Vec<FollowUp>>>) -> Result<Arc<Run>, String> {
        if self.list.len() >= MAX_PER_SESSION {
            return Err(format!("This session already started {MAX_PER_SESSION} subagents, the limit."));
        }
        let exe = std::env::current_exe().map_err(|e| format!("Cannot find the agent binary: {e}"))?;
        // A child has no UI to ask in, so ask becomes auto.
        let mode = match launch.mode {
            Mode::FullAccess => "--full-access",
            Mode::Ask | Mode::Auto => "--auto",
        };
        let args = ["--headless", "--model", &launch.model, "--thinking", &launch.thinking, mode, "--parent", &launch.parent_session];
        let mut child = Command::new(exe)
            .args(args)
            .current_dir(&launch.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Cannot start subagent: {e}"))?;
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let message = format!("{CHILD_BRIEF}\n\n<task>\n{}\n</task>", launch.task);
        stdin.write_all(message.as_bytes()).map_err(|e| format!("Cannot send the task: {e}"))?;
        drop(stdin);
        let stdout = child.stdout.take().expect("stdout is piped");
        let run = Arc::new(Run {
            id: format!("sa-{}", self.list.len() + 1),
            label: launch.label,
            task: launch.task,
            background,
            started: Instant::now(),
            child: Mutex::new(Some(child)),
            state: Mutex::default(),
        });
        self.list.push(run.clone());
        let (watched, follow_ups) = (run.clone(), follow_ups.clone());
        std::thread::spawn(move || {
            let mut result = Err("The subagent exited without an answer.".to_string());
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let Ok(event) = serde_json::from_str::<Value>(&line) else { continue };
                let mut state = watched.state.lock().unwrap();
                match event["type"].as_str() {
                    Some("tool") => {
                        state.tool_calls += 1;
                        state.last_tool = event["name"].as_str().unwrap_or_default().to_string();
                    }
                    Some("session") => state.session = event["path"].as_str().map(String::from),
                    Some("done") => result = Ok(event["answer"].as_str().unwrap_or_default().to_string()),
                    Some("error") => result = Err(event["message"].as_str().unwrap_or_default().to_string()),
                    _ => {}
                }
            }
            if let Some(mut child) = watched.child.lock().unwrap().take() {
                let _ = child.wait();
            }
            let mut state = watched.state.lock().unwrap();
            state.elapsed = Some(watched.started.elapsed());
            state.result = Some(result);
            drop(state);
            if watched.background {
                let text = format!("Subagent {} ({}) finished.\n\n{}", watched.id, watched.label, watched.result_text());
                follow_ups.lock().unwrap().push(FollowUp { source: watched.id.clone(), text });
            }
        });
        Ok(run)
    }

    /// Children belong to the session, so quitting stops them.
    pub fn stop_all(&self) {
        self.list.iter().for_each(|run| run.stop());
    }

    pub fn running(&self) -> usize {
        self.list.iter().filter(|run| run.status() == "running").count()
    }
}

/// Waits for foreground runs; setting `abort` (Esc) kills them.
pub fn wait(runs: &[Arc<Run>], abort: &AtomicBool) {
    while runs.iter().any(|r| r.status() == "running") {
        if abort.load(Ordering::Relaxed) {
            runs.iter().for_each(|r| r.stop());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// One child to spawn; `model` and `effort` are None when the parent's apply.
#[derive(Debug, PartialEq)]
pub struct Task {
    pub label: String,
    pub prompt: String,
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// The `subagent` input: `{label, prompt, model?, effort?}` or `{tasks: [...]}`.
pub fn tasks(input: &Value) -> Result<Vec<Task>, String> {
    let one = |item: &Value| -> Result<Task, String> {
        let label = item["label"].as_str().filter(|l| !l.trim().is_empty()).ok_or("Each task needs a label.")?;
        let prompt = item["prompt"].as_str().filter(|p| !p.trim().is_empty()).ok_or("Each task needs a prompt.")?;
        let model = item["model"].as_str().filter(|m| *m != "inherit").map(String::from);
        Ok(Task { label: label.to_string(), prompt: prompt.to_string(), model, effort: item["effort"].as_str().map(String::from) })
    };
    match input["tasks"].as_array() {
        Some(list) if !list.is_empty() => list.iter().map(one).collect(),
        Some(_) => Err("tasks must not be empty.".into()),
        None => Ok(vec![one(input)?]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn project_swarm_prompt_wins_and_blank_falls_through() {
        let root = std::env::temp_dir().join(format!("agent-swarm-prompt-{}", std::process::id()));
        let (home, project) = (root.join("home"), root.join("project"));
        std::fs::create_dir_all(project.join(".agents")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        assert!(swarm_prompt(&home, &project, "medium").contains("Implementation tasks: `effort: \"medium\"`."));
        std::fs::write(home.join("swarm-prompt.md"), "global {implementation_effort}\n").unwrap();
        std::fs::write(project.join(".agents/swarm-prompt.md"), "  \n").unwrap();
        assert_eq!(swarm_prompt(&home, &project, "high"), "global high");
        std::fs::write(project.join(".agents/swarm-prompt.md"), "project").unwrap();
        assert_eq!(swarm_prompt(&home, &project, "high"), "project");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn single_and_parallel_task_shapes() {
        let one = tasks(&json!({"label": "scout", "prompt": "look", "model": "inherit", "effort": "low"})).unwrap();
        assert_eq!(one, [Task { label: "scout".into(), prompt: "look".into(), model: None, effort: Some("low".into()) }]);
        assert_eq!(tasks(&json!({"tasks": [{"label": "a", "prompt": "x"}, {"label": "b", "prompt": "y"}]})).unwrap().len(), 2);
        assert_eq!(tasks(&json!({"tasks": [{"label": " ", "prompt": "x"}]})).unwrap_err(), "Each task needs a label.");
    }
}
