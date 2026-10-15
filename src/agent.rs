use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::api::{self, Client, Delta, Usage};
use crate::ask::{self, Answer, Question};
use crate::bg::{self, Terminals};
use crate::condense::{self, Condensed};
use crate::goal::{AfterTurn, Goal, Status as GoalStatus};
use crate::hashline::Anchors;
use crate::models::Model;
use crate::modes;
use crate::permissions::{self, Decision, Grants, Mode, Request};
use crate::rtk::Rtk;
use crate::settings::Settings;
use crate::snapshots::Snapshots;
use crate::subagent::{self, Launch, Runs};
use crate::todo::Todos;
use crate::{now_ms, prompt, tools};


const SUMMARY_SYSTEM: &str = "You are a context summarization assistant. Your task is to read a conversation between a user and an AI assistant, then produce a structured summary following the exact format specified.

Do NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.";

const SUMMARY_PROMPT: &str = "The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work.

Use this EXACT format:

## Goal
[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned by user]
- [Or \"(none)\" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Current work]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [Ordered list of what should happen next]

## Critical Context
- [Any data, examples, or references needed to continue]
- [Or \"(none)\" if not applicable]

Keep each section concise. Preserve exact file paths, function names, and error messages.";

const SUMMARY_PREFIX: &str = "The conversation history before this point was compacted into the following summary:\n\n<summary>\n";
const SUMMARY_SUFFIX: &str = "\n</summary>";
const TOOL_RESULT_SUMMARY_CHARS: usize = 2000;
const NAMING_SYSTEM: &str = "Name a coding session from the user's first message. Reply with the name only: 3 to 6 words naming the concrete task, in the language of the message, one line, no quotes, no markdown.";
/// What the agent reports while it works; the TUI renders these, print mode writes them to stdout.
pub enum Event {
    Text(String),
    Thinking(String),
    Notice(String),
    ToolStart { id: String, name: String, input: Value },
    ToolEnd { id: String, output: String, is_error: bool },
    Steered(String),
    TurnEnd { usage: Usage },
    Compacted { summary: String },
    SessionStarted { id: String, path: Option<String> },
    /// Ask mode: the agent waits for one Decision on `reply`.
    Permission { request: Request, reply: Sender<Decision> },
    /// Auto mode: what the reviewer decided.
    Reviewed { allowed: bool, detail: String, reason: String },
    /// ask_user_question: the agent waits for one answer slot per question; all None means declined.
    Ask { questions: Vec<Question>, reply: Sender<Vec<Option<Answer>>> },
}

/// Work that finished in the background; it reaches the model when its turn has no more tool calls, or starts a turn when idle.
pub struct FollowUp {
    /// What produced it, e.g. a terminal id; bg_status on that id withdraws it.
    pub source: String,
    pub text: String,
}
pub struct Agent {
    pub client: Client,
    pub model: String,
    pub effort: String,
    pub system: String,
    /// The ponytail and caveman section, sent after `system` and rebuilt when /settings changes them.
    pub modes: String,
    pub tools: Value,
    pub cwd: PathBuf,
    pub anchors: Anchors,
    pub rtk: Rtk,
    /// Output of `!cmd` shell escapes, sent ahead of the next prompt.
    pub shell_context: Vec<String>,
    /// Image blocks pasted into the prompt, sent with the next prompt.
    pub images: Vec<Value>,
    /// Subdirectory AGENTS.md files already attached to a read result in this session.
    pub nested_instructions: HashSet<PathBuf>,
    pub messages: Vec<Value>,
    pub session: Session,
    pub sessions_dir: PathBuf,
    pub context_window: u64,
    pub max_tokens: u64,
    /// The provider's cheapest model: auto-mode reviews and, by default, condense summaries.
    pub cheap_model: String,
    pub auto_compact: bool,
    pub reserve_tokens: u64,
    pub keep_recent_tokens: u64,
    pub last_context_tokens: u64,
    pub condense: condense::Config,
    /// Summarized batches; the provider sees their stubs, `messages` keeps the originals.
    pub condensed: Vec<Condensed>,
    /// Number of the next short ref (`t7`).
    pub next_ref: usize,
    pub pending_condense: Vec<JoinHandle<Result<(Condensed, Usage), String>>>,
    pub usage: Usage,
    pub turns: u64,
    pub tool_calls: u64,
    /// Set from the UI thread to stop the stream and kill a running bash command.
    pub abort: Arc<AtomicBool>,
    /// Messages typed while the agent works; delivered with the next tool results.
    pub steering: Arc<Mutex<Vec<String>>>,
    /// Shared with the UI so a mode change applies to the next tool call.
    pub permission_mode: Arc<Mutex<Mode>>,
    pub grants: Grants,
    /// Shared with the UI, which shows it and lets the user edit it in /todos.
    pub todos: Arc<Mutex<Todos>>,
    /// Shared with the UI for the footer and /goal pause, which must work mid-turn.
    pub goal: Arc<Mutex<Option<Goal>>>,
    pub follow_ups: Arc<Mutex<Vec<FollowUp>>>,
    /// Shared with the UI for /ps and the running count above the editor.
    pub terminals: Arc<Mutex<Terminals>>,
    /// Shared with the UI for /subagents.
    pub subagents: Arc<Mutex<Runs>>,
    pub subagent_concurrency: usize,
    pub subagent_effort: String,
    /// Tool groups load_tools added; the base set is always on.
    pub loaded_groups: Vec<String>,
    pub spent: Spent,
    /// False in print mode: tools that need the user fail instead of waiting.
    pub interactive: bool,
    /// pi's agent dir, for rebuilding the system prompt after /worktree.
    pub agent_dir: PathBuf,
    pub home: PathBuf,
    /// None outside a git repo and in print mode; then a rewind leaves files alone.
    pub snapshots: Option<Snapshots>,
    /// Work tree snapshots around each prompt, oldest first.
    pub checkpoints: Vec<Checkpoint>,
}

/// Snapshots of the work tree taken when the prompt at message `at` started and finished.
#[derive(Clone, Debug, PartialEq)]
pub struct Checkpoint {
    pub at: usize,
    pub before: String,
    /// None until the prompt finishes, and forever if the process died during it.
    pub after: Option<String>,
}

/// A user prompt the conversation can go back to.
#[derive(Debug, PartialEq)]
pub struct RewindPoint {
    /// Message index of the prompt.
    pub at: usize,
    pub text: String,
    pub images: Vec<Value>,
}

#[derive(Clone)]
pub struct Session {
    pub id: String,
    pub path: Option<PathBuf>,
}

/// What a saved session restores besides its id.
#[derive(Default)]
pub struct History {
    pub messages: Vec<Value>,
    pub condensed: Vec<Condensed>,
    pub todos: Todos,
    pub goal: Option<Goal>,
    pub loaded_groups: Vec<String>,
    pub spent: Spent,
    pub checkpoints: Vec<Checkpoint>,
}

/// Time and tokens of the user's commands, saved per command so the footer total survives a resume.
#[derive(Default, Clone, Copy, PartialEq, Debug)]
pub struct Spent {
    pub input: u64,
    pub output: u64,
    pub time: Duration,
}

impl Spent {
    pub fn add(&mut self, other: Spent) {
        self.input += other.input;
        self.output += other.output;
        self.time += other.time;
    }

    fn entry(&self) -> Value {
        json!({"type": "spent", "input": self.input, "output": self.output, "ms": self.time.as_millis() as u64})
    }

    fn from_entry(entry: &Value) -> Spent {
        let number = |key: &str| entry[key].as_u64().unwrap_or_default();
        Spent { input: number("input"), output: number("output"), time: Duration::from_millis(number("ms")) }
    }
}

impl Agent {
    /// Runs a `!cmd` the user typed, without asking; with `keep`, pi's "Ran `cmd`" block goes into the next prompt.
    /// shortcut: output kept before a /new, /resume or quit without a prompt is dropped; save it to the session if that matters.
    pub fn run_bash(&mut self, command: &str, keep: bool, emit: &mut dyn FnMut(Event)) {
        self.abort.store(false, Ordering::Relaxed);
        let id = format!("user-bash-{}", now_ms());
        let input = json!({"command": command});
        emit(Event::ToolStart { id: id.clone(), name: "bash".into(), input: input.clone() });
        let (output, is_error) = tools::run("bash", &input, &self.cwd, &mut self.anchors, &Rtk::OFF, &self.abort);
        if keep {
            self.shell_context.push(format!("Ran `{command}`\n```\n{}\n```", output.trim_end()));
        }
        emit(Event::ToolEnd { id, output, is_error });
    }

    /// Runs the model and its tool calls until it ends the turn; returns the last assistant text.
    pub fn prompt(&mut self, text: &str, emit: &mut dyn FnMut(Event)) -> Result<String, String> {
        self.abort.store(false, Ordering::Relaxed);
        if self.interactive && self.messages.is_empty() {
            self.start_naming(text);
        }
        let mut content: Vec<Value> = self.shell_context.drain(..).map(|text| json!({"type": "text", "text": text})).collect();
        let todo_notes = std::mem::take(&mut self.todos.lock().unwrap().user_notes);
        if !todo_notes.is_empty() {
            self.session.append(&self.todos.lock().unwrap().entry())?;
            content.push(json!({"type": "text", "text": todo_notes.join("\n")}));
        }
        content.append(&mut self.images);
        content.push(json!({"type": "text", "text": text}));
        let at = self.messages.len();
        self.push(json!({"role": "user", "content": content}))?;
        let checkpointed = self.start_checkpoint(at, emit)?;
        let turns_before = self.turns;
        let result = self.run_turns(text, emit);
        if checkpointed {
            self.finish_checkpoint(emit)?;
        }
        if result.is_err() && self.turns == turns_before {
            // No reply means the API refused this prompt (e.g. a 400 on its image); resending it would fail again.
            self.rewind(self.messages.len() - 1)?;
        }
        result
    }

    fn run_turns(&mut self, text: &str, emit: &mut dyn FnMut(Event)) -> Result<String, String> {
        loop {
            if self.auto_compact && self.last_context_tokens > self.context_window.saturating_sub(self.reserve_tokens) {
                emit(Event::Notice("Compacting context...".into()));
                if let Some(summary) = self.compact()? {
                    emit(Event::Compacted { summary });
                }
            }
            self.collect_condensed(emit)?;
            let messages = condense::view(&self.messages, &self.condensed);
            let system = if self.modes.is_empty() { self.system.clone() } else { format!("{}\n\n{}", self.system, self.modes) };
            let request = api::request(&self.model, self.max_tokens, &self.effort, &system, &self.tools, &messages);
            let mut reply = self.client.send(&request, &self.abort, &mut |delta| match delta {
                Delta::Text(text) => emit(Event::Text(text.to_string())),
                Delta::Thinking(text) => emit(Event::Thinking(text.to_string())),
                Delta::Notice(text) => emit(Event::Notice(text.to_string())),
            })?;
            self.turns += 1;
            self.usage.add(reply.usage);
            if let Some(goal) = self.goal.lock().unwrap().as_mut().filter(|g| g.status == GoalStatus::Active) {
                // Cache reads are left out: they re-read context an earlier request already paid for.
                goal.tokens_used += reply.usage.input + reply.usage.cache_write + reply.usage.output;
            }
            self.last_context_tokens = reply.usage.context_tokens();
            emit(Event::TurnEnd { usage: reply.usage });
            if reply.stop_reason == "aborted" {
                keep_finished_text(&mut reply.message);
            }
            let calls: Vec<Value> = reply.message["content"].as_array().into_iter().flatten().filter(|b| b["type"] == "tool_use").cloned().collect();
            let answer = text_of(&reply.message);
            if reply.message["content"].as_array().is_some_and(|c| !c.is_empty()) {
                self.push(reply.message)?;
            }
            if reply.stop_reason == "aborted" {
                emit(Event::Notice("Operation aborted".into()));
                return Ok(answer);
            }
            if calls.is_empty() {
                if reply.stop_reason == "max_tokens" || reply.stop_reason == "refusal" {
                    emit(Event::Notice(format!("Stopped: {}", reply.stop_reason)));
                    return Ok(answer);
                }
                let follow_ups = self.take_follow_ups(emit);
                if follow_ups.is_empty() {
                    return Ok(answer);
                }
                self.push(json!({"role": "user", "content": follow_ups}))?;
                continue;
            }
            let mut results = Vec::new();
            let mut finished = Vec::new();
            for call in calls {
                let id = call["id"].as_str().unwrap_or_default().to_string();
                let name = call["name"].as_str().unwrap_or_default().to_string();
                let input = call["input"].clone();
                let mut shown = input.clone();
                // Anchors mean nothing to the user, show the file they resolve to.
                if let Some(file) = self.edited_file(&input) {
                    shown["path"] = file.strip_prefix(&self.cwd).unwrap_or(&file).display().to_string().into();
                }
                emit(Event::ToolStart { id: id.clone(), name: name.clone(), input: shown });
                let (output, is_error) = self.run_tool(&name, &input, text, emit);
                self.tool_calls += 1;
                emit(Event::ToolEnd { id: id.clone(), output: output.clone(), is_error });
                results.push(json!({"type": "tool_result", "tool_use_id": id, "content": output, "is_error": is_error}));
                finished.push(condense::Call { id, name, input, output, is_error });
            }
            for text in std::mem::take(&mut *self.steering.lock().unwrap()) {
                results.push(json!({"type": "text", "text": text}));
                emit(Event::Steered(text));
            }
            self.push(json!({"role": "user", "content": results}))?;
            if self.abort.load(Ordering::Relaxed) {
                emit(Event::Notice("Operation aborted".into()));
                return Ok(answer);
            }
            self.start_condense(&answer, finished);
        }
    }

    /// Runs one tool call after the permission check; agent-level tools are handled here, file and shell tools in `tools`.
    fn run_tool(&mut self, name: &str, input: &Value, user_request: &str, emit: &mut dyn FnMut(Event)) -> (String, bool) {
        if self.abort.load(Ordering::Relaxed) {
            return ("Operation aborted".into(), true);
        }
        if let Some(reason) = self.check_permission(name, input, user_request, emit) {
            return (reason, true);
        }
        let result = match name {
            "context_tree_query" => condense::query(&self.messages, &self.condensed, input),
            "todo" => self.todo(input),
            "create_goal" | "get_goal" | "update_goal" => self.goal_tool(name, input),
            "load_tools" => self.load_tools(input),
            "bg_start" | "bg_status" | "bg_list" => self.terminals.lock().unwrap().run(name, input, &self.cwd, &self.follow_ups),
            "bg_kill" => {
                let targets = self.terminals.lock().unwrap().for_kill(input);
                targets.map(|targets| bg::kill(&targets, &self.follow_ups))
            }
            "subagent" => self.subagent(input, emit),
            "ask_user_question" => self.ask_user(input, emit),
            _ => return self.run_file_tool(name, input),
        };
        match result {
            Ok(text) => (text, false),
            Err(text) => (text, true),
        }
    }

    fn run_file_tool(&mut self, name: &str, input: &Value) -> (String, bool) {
        let (mut output, is_error) = tools::run(name, input, &self.cwd, &mut self.anchors, &self.rtk, &self.abort);
        if name == "read" && !is_error {
            let file = self.cwd.join(input["path"].as_str().unwrap_or_default());
            let nested = prompt::nested_context(&self.cwd, &file, &mut self.nested_instructions);
            if !nested.is_empty() {
                output = format!("{output}\n\n{nested}");
            }
        }
        (output, is_error)
    }

    fn todo(&mut self, input: &Value) -> Result<String, String> {
        let (text, changed) = self.todos.lock().unwrap().apply(input)?;
        if changed {
            self.session.append(&self.todos.lock().unwrap().entry())?;
        }
        Ok(text)
    }

    /// pi's tool-loader: adds a group's tools to every later request of this session.
    fn load_tools(&mut self, input: &Value) -> Result<String, String> {
        let before: Vec<String> = tool_names(&self.tools);
        for group in input["groups"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            if !tools::GROUPS.iter().any(|(name, _)| *name == group) {
                return Err(format!("Unknown tool group {group}"));
            }
            if !self.loaded_groups.iter().any(|g| g == group) {
                self.loaded_groups.push(group.to_string());
            }
        }
        self.tools = self.tool_schemas(&self.loaded_groups);
        self.session.append(&json!({"type": "tools_loaded", "groups": self.loaded_groups}))?;
        let added: Vec<String> = tool_names(&self.tools).into_iter().filter(|name| !before.contains(name)).collect();
        Ok(if added.is_empty() { "All requested tools were already active.".into() } else { format!("Enabled: {}", added.join(", ")) })
    }

    /// Drains the follow-up queue into text blocks, showing each in the transcript.
    fn take_follow_ups(&mut self, emit: &mut dyn FnMut(Event)) -> Vec<Value> {
        let follow_ups = std::mem::take(&mut *self.follow_ups.lock().unwrap());
        let mut blocks = Vec::new();
        for follow_up in follow_ups {
            emit(Event::Steered(follow_up.text.clone()));
            blocks.push(json!({"type": "text", "text": follow_up.text}));
        }
        blocks
    }

    /// Starts a turn from queued follow-ups while idle; Ok("") when another turn already took them.
    pub fn run_follow_ups(&mut self, emit: &mut dyn FnMut(Event)) -> Result<String, String> {
        let blocks = self.take_follow_ups(emit);
        let texts: Vec<&str> = blocks.iter().filter_map(|b| b["text"].as_str()).collect();
        if texts.is_empty() {
            return Ok(String::new());
        }
        self.pursue(&texts.join("\n\n"), emit)
    }

    /// Runs child agents: in the foreground `subagent_concurrency` at a time, or all in the background.
    fn subagent(&mut self, input: &Value, emit: &mut dyn FnMut(Event)) -> Result<String, String> {
        let tasks = subagent::tasks(input)?;
        let catalog = crate::models::all();
        // A child with an unknown model dies before its first JSONL line, so reject it here.
        if let Some(model) = tasks.iter().filter_map(|t| t.model.as_deref()).find(|m| !catalog.iter().any(|c| c.id == *m)) {
            return Err(format!("Unknown model {model}."));
        }
        let cwd = match input["cwd"].as_str() {
            Some(dir) => self.cwd.join(dir),
            None => self.cwd.clone(),
        };
        let background = input["background"].as_bool() == Some(true);
        let batch = if background { tasks.len() } else { self.subagent_concurrency.max(1) };
        let mode = *self.permission_mode.lock().unwrap();
        let mut runs = Vec::new();
        for chunk in tasks.chunks(batch) {
            let mut started = Vec::new();
            for task in chunk {
                let model = task.model.clone().unwrap_or_else(|| self.model.clone());
                let thinking = task.effort.clone().unwrap_or_else(|| self.effort.clone());
                let launch = Launch { label: task.label.clone(), task: task.prompt.clone(), cwd: cwd.clone(), model, thinking, mode, parent_session: self.session.id.clone() };
                started.push(self.subagents.lock().unwrap().spawn(launch, background, &self.follow_ups)?);
            }
            if !background {
                subagent::wait(&started, &self.abort);
                for run in &started {
                    emit(Event::Notice(run.summary()));
                }
            }
            runs.extend(started);
            if self.abort.load(Ordering::Relaxed) {
                return Err("Operation aborted".into());
            }
        }
        if background {
            let names: Vec<String> = runs.iter().map(|r| format!("{} ({})", r.id, r.label)).collect();
            return Ok(format!("Started {} in the background. Each answer arrives as a message when it finishes; /subagents shows progress.", names.join(", ")));
        }
        if let [run] = runs.as_slice() {
            return Ok(run.result_text());
        }
        let sections: Vec<String> = runs.iter().map(|r| format!("## {} ({})\n\n{}", r.label, r.id, r.result_text())).collect();
        Ok(sections.join("\n\n"))
    }

    fn goal_tool(&mut self, name: &str, input: &Value) -> Result<String, String> {
        let mut goal = self.goal.lock().unwrap();
        let text = match name {
            "create_goal" => {
                let objective = input["objective"].as_str().filter(|o| !o.trim().is_empty()).ok_or("Error: objective required")?;
                *goal = Some(Goal::new(objective, input["token_budget"].as_u64()));
                format!("Goal created: {objective}")
            }
            "update_goal" => {
                if input["status"] != "complete" {
                    return Err("Error: status must be \"complete\"".into());
                }
                let goal = goal.as_mut().ok_or("Error: no goal")?;
                goal.status = GoalStatus::Complete;
                goal.evidence = input["evidence"].as_str().map(String::from);
                "Goal marked complete.".into()
            }
            _ => return Ok(goal.as_ref().map_or("No goal.".into(), Goal::summary)),
        };
        self.session.append(&Goal::entry(goal.as_ref()))?;
        Ok(text)
    }

    /// Runs `text`, then keeps prompting while an active goal is not complete; Esc or a spent budget pause it.
    pub fn pursue(&mut self, text: &str, emit: &mut dyn FnMut(Event)) -> Result<String, String> {
        let mut answer = self.prompt(text, emit)?;
        loop {
            let next = match self.goal.lock().unwrap().as_mut() {
                Some(goal) => goal.after_turn(self.abort.load(Ordering::Relaxed)),
                None => AfterTurn::Idle,
            };
            match next {
                AfterTurn::Idle => return Ok(answer),
                AfterTurn::Paused(note) => {
                    self.save_goal()?;
                    emit(Event::Notice(note));
                    return Ok(answer);
                }
                AfterTurn::Continue(prompt) => {
                    self.save_goal()?;
                    emit(Event::Steered(prompt.clone()));
                    answer = self.prompt(&prompt, emit)?;
                }
            }
        }
    }

    pub fn save_spent(&mut self, spent: Spent) -> Result<(), String> {
        self.spent.add(spent);
        self.session.append(&spent.entry())
    }

    pub fn save_goal(&mut self) -> Result<(), String> {
        let entry = Goal::entry(self.goal.lock().unwrap().as_ref());
        self.session.append(&entry)
    }

    fn ask_user(&mut self, input: &Value, emit: &mut dyn FnMut(Event)) -> Result<String, String> {
        if !self.interactive {
            return Err(ask::NO_UI.into());
        }
        let questions = ask::parse(input)?;
        let (reply, answers) = mpsc::channel();
        emit(Event::Ask { questions: questions.clone(), reply });
        Ok(ask::response(&questions, &answers.recv().unwrap_or_default()))
    }

    /// Names a fresh session from its first prompt on a background thread, as /rename would.
    fn start_naming(&self, text: &str) {
        let prompt: String = text.chars().take(2000).collect();
        let request = api::plain_request(&self.cheap_model, NAMING_SYSTEM, &prompt, 1000);
        let (client, session) = (self.client.clone(), self.session.clone());
        std::thread::spawn(move || {
            // A name is cosmetic: on failure /resume falls back to the first prompt line.
            let Ok(reply) = client.send(&request, &AtomicBool::new(false), &mut |_| {}) else { return };
            let text = text_of(&reply.message);
            let name = text.lines().next().unwrap_or_default().trim().trim_matches('"');
            if !name.is_empty() {
                let _ = session.rename(name);
            }
        });
    }

    /// Summarizes the turn's tool results on a background thread; the next request picks the summary up.
    fn start_condense(&mut self, assistant_text: &str, calls: Vec<condense::Call>) {
        if !self.condense.enabled {
            return;
        }
        let calls = condense::select(calls, self.condense.min_batch_chars);
        if calls.is_empty() {
            return;
        }
        let (system, prompt) = condense::summarizer_prompt(assistant_text, &calls);
        let request = api::plain_request(&self.condense.model, system, &prompt, 4096);
        let client = self.client.clone();
        let first_ref = self.next_ref;
        self.next_ref += calls.len();
        self.pending_condense.push(std::thread::spawn(move || {
            let reply = client.send(&request, &AtomicBool::new(false), &mut |_| {})?;
            let summary = text_of(&reply.message);
            if summary.trim().is_empty() || reply.stop_reason == "max_tokens" {
                return Err("the summary came back empty or cut off".to_string());
            }
            Ok((condense::condensed(&summary, &calls, first_ref), reply.usage))
        }));
    }

    /// Applies finished summaries; a failed one leaves its originals in place.
    fn collect_condensed(&mut self, emit: &mut dyn FnMut(Event)) -> Result<(), String> {
        let (finished, running) = std::mem::take(&mut self.pending_condense).into_iter().partition(|handle| handle.is_finished());
        self.pending_condense = running;
        for handle in finished {
            match handle.join().unwrap_or_else(|_| Err("the summarizer thread panicked".into())) {
                Ok((batch, usage)) => {
                    self.usage.add(usage);
                    self.session.append(&condense::entry(&batch))?;
                    self.condensed.push(batch);
                }
                Err(error) => emit(Event::Notice(format!("Condense failed, kept the original tool outputs: {error}"))),
            }
        }
        Ok(())
    }

    fn edited_file(&self, input: &Value) -> Option<PathBuf> {
        let anchor = ["remove_from", "replace_from", "anchor"].iter().find_map(|key| input[*key].as_str())?;
        self.anchors.owner_of(anchor).ok()
    }

    /// None when the call may run; otherwise the tool result that explains the refusal.
    /// A user's "No" also stops the turn, like Esc.
    fn check_permission(&mut self, tool: &str, input: &Value, user_request: &str, emit: &mut dyn FnMut(Event)) -> Option<String> {
        let edited_file = self.edited_file(input);
        let request = permissions::request_for(tool, input, edited_file.as_deref())?;
        let mode = *self.permission_mode.lock().unwrap();
        if mode == Mode::FullAccess || self.grants.allows(&request) {
            return None;
        }
        if mode == Mode::Auto {
            return self.review(user_request, &request, emit);
        }
        let (reply, answer) = mpsc::channel();
        emit(Event::Permission { request: request.clone(), reply });
        match answer.recv().unwrap_or(Decision::Deny) {
            Decision::Allow => None,
            Decision::AllowAlways => {
                self.grants.add(&request);
                None
            }
            Decision::Deny => {
                self.abort.store(true, Ordering::Relaxed);
                Some("The user denied this tool call.".into())
            }
        }
    }

    /// Auto mode: a small model allows or denies the call; a failed review denies it.
    fn review(&mut self, user_request: &str, request: &Request, emit: &mut dyn FnMut(Event)) -> Option<String> {
        let prompt = permissions::review_prompt(user_request, &self.cwd, request);
        let verdict = self.client.send(&api::plain_request(&self.cheap_model, permissions::REVIEW_SYSTEM, &prompt, 200), &self.abort, &mut |_| {});
        let (allowed, reason) = match verdict {
            Ok(reply) => {
                self.usage.add(reply.usage);
                permissions::parse_review(&text_of(&reply.message))
            }
            Err(error) => (false, format!("review failed: {error}")),
        };
        emit(Event::Reviewed { allowed, detail: request.detail.clone(), reason: reason.clone() });
        if allowed { None } else { Some(format!("Denied by the auto-mode reviewer: {reason}. Try another way or ask the user.")) }
    }

    fn push(&mut self, message: Value) -> Result<(), String> {
        self.session.append(&json!({"type": "message", "message": message}))?;
        self.messages.push(message);
        Ok(())
    }


    /// Drops the message at `index` and everything after it, and puts back the files those prompts changed.
    /// None: the work tree is not snapshotted, so files stay as they are.
    pub fn rewind(&mut self, index: usize) -> Result<Option<RestoredFiles>, String> {
        let files = match &self.snapshots {
            Some(snapshots) => Some(restore_files(snapshots, &self.checkpoints, index)?),
            None => None,
        };
        self.checkpoints.retain(|checkpoint| checkpoint.at < index);
        self.messages.truncate(index);
        self.session.append(&json!({"type": "rewind", "to": index}))?;
        self.last_context_tokens = 0;
        Ok(files)
    }

    /// Returns whether the work tree was snapshotted; a failed snapshot only costs this prompt's file rewind.
    fn start_checkpoint(&mut self, at: usize, emit: &mut dyn FnMut(Event)) -> Result<bool, String> {
        let Some(snapshots) = &self.snapshots else { return Ok(false) };
        match snapshots.take() {
            Ok(before) => {
                self.session.append(&json!({"type": "checkpoint", "at": at, "before": before}))?;
                self.checkpoints.push(Checkpoint { at, before, after: None });
                Ok(true)
            }
            Err(error) => {
                emit(Event::Notice(format!("File snapshot failed, a rewind will not restore files from this prompt: {error}")));
                Ok(false)
            }
        }
    }

    fn finish_checkpoint(&mut self, emit: &mut dyn FnMut(Event)) -> Result<(), String> {
        let Some(snapshots) = &self.snapshots else { return Ok(()) };
        // Compaction during the prompt can drop its checkpoint along with the summarized messages.
        let Some(checkpoint) = self.checkpoints.last_mut().filter(|checkpoint| checkpoint.after.is_none()) else { return Ok(()) };
        match snapshots.take() {
            Ok(after) => {
                self.session.append(&json!({"type": "checkpoint_after", "after": after}))?;
                checkpoint.after = Some(after);
            }
            Err(error) => emit(Event::Notice(format!("File snapshot failed, a rewind will not restore files from this prompt: {error}"))),
        }
        Ok(())
    }

    /// Summarizes everything before a recent assistant message and keeps the rest verbatim, like pi.
    /// Returns None when there is nothing old enough to summarize.
    pub fn compact(&mut self) -> Result<Option<String>, String> {
        let Some(first_kept) = cut_point(&self.messages, self.keep_recent_tokens) else {
            return Ok(None);
        };
        let conversation = serialize(&self.messages[..first_kept]);
        let prompt = format!("<conversation>\n{conversation}\n</conversation>\n\n{SUMMARY_PROMPT}");
        let max_tokens = (self.reserve_tokens * 4 / 5).min(128_000);
        let request = api::plain_request(&self.model, SUMMARY_SYSTEM, &prompt, max_tokens);
        let reply = self.client.send(&request, &self.abort, &mut |_| {})?;
        if reply.stop_reason != "end_turn" {
            return Err(format!("Compaction summary stopped with {}.", reply.stop_reason));
        }
        self.usage.add(reply.usage);
        let summary = text_of(&reply.message);
        self.session.append(&json!({"type": "compaction", "summary": summary, "first_kept": first_kept}))?;
        self.messages = compacted(&self.messages, &summary, first_kept);
        shift_checkpoints(&mut self.checkpoints, first_kept);
        self.last_context_tokens = 0;
        Ok(Some(summary))
    }

    /// Applies settings changed in /settings; `model` is the catalog entry for `settings.model`, `cheap_model` its provider's cheapest.
    pub fn apply(&mut self, settings: &Settings, model: &Model, cheap_model: &str) {
        self.model = settings.model.clone();
        self.effort = settings.thinking.clone();
        self.context_window = settings.context_window(model);
        self.max_tokens = model.max_tokens;
        self.cheap_model = cheap_model.to_string();
        self.auto_compact = settings.auto_compact;
        self.reserve_tokens = settings.reserve_tokens;
        self.keep_recent_tokens = settings.keep_recent_tokens;
        self.condense = condense::Config::new(settings, cheap_model);
        self.subagent_concurrency = settings.subagent_concurrency;
        self.subagent_effort = settings.subagent_effort.clone();
        self.tools = self.tool_schemas(&self.loaded_groups);
        *self.permission_mode.lock().unwrap() = settings.permission_mode;
        self.rtk = if settings.rtk { Rtk::ON } else { Rtk::OFF };
        self.modes = modes::prompt(&self.home, &settings.ponytail, &settings.caveman);
    }

    /// Starts an empty session (unsaved if the current one is unsaved).
    pub fn new_session(&mut self) -> Result<(), String> {
        self.session = Session::start(&self.sessions_dir, self.session.path.is_some(), &self.cwd, &self.model)?;
        self.load(History::default());
        Ok(())
    }

    /// Switches to the saved session `id`; returns its messages for the transcript.
    pub fn open_session(&mut self, id: &str) -> Result<Vec<Value>, String> {
        let (session, history) = Session::resume(&self.sessions_dir, id)?;
        self.session = session;
        let messages = history.messages.clone();
        self.load(history);
        Ok(messages)
    }

    /// /worktree: pi pins a session to its cwd, so the history moves to a new session started in `cwd`.
    pub fn move_to(&mut self, cwd: PathBuf) -> Result<(), String> {
        let session = self.copy_session(&cwd)?;
        let from = self.cwd.display();
        self.shell_context.push(format!("Session moved from {from} to {}. Paths and git state from earlier messages refer to the old checkout.", cwd.display()));
        self.system = prompt::system_prompt(&self.agent_dir, &cwd, &crate::settings::load(&self.home).disabled_skills);
        self.anchors = Anchors::new(&session.id);
        self.nested_instructions.clear();
        // Earlier snapshots belong to the old checkout's snapshot repo.
        session.append(&json!({"type": "checkpoints_reset"}))?;
        self.checkpoints.clear();
        if self.snapshots.is_some() {
            self.snapshots = Snapshots::open(&self.home, &cwd);
        }
        self.cwd = cwd;
        self.session = session;
        Ok(())
    }

    /// /clone: continues in a copy of this session, so the original stays as it was.
    pub fn clone_session(&mut self) -> Result<(), String> {
        self.session = self.copy_session(&self.cwd)?;
        Ok(())
    }

    /// /import: copies an exported session into the sessions dir and resumes it.
    pub fn import_session(&mut self, file: &Path) -> Result<Vec<Value>, String> {
        let id = Session::import(&self.sessions_dir, file)?;
        let target = self.sessions_dir.join(format!("{id}.jsonl"));
        self.open_session(&id).inspect_err(|_| {
            let _ = std::fs::remove_file(&target);
        })
    }

    /// A new session in `cwd` holding this session's entries.
    fn copy_session(&self, cwd: &Path) -> Result<Session, String> {
        let session = Session::start(&self.sessions_dir, self.session.path.is_some(), cwd, &self.model)?;
        if let Some(path) = &self.session.path {
            let text = std::fs::read_to_string(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
            for line in text.lines().skip(1) {
                let entry: Value = serde_json::from_str(line).map_err(|e| format!("{}: {e}", path.display()))?;
                session.append(&entry)?;
            }
        }
        Ok(session)
    }

    /// /export: the session file as is, or for a `.md` path the conversation as plain text.
    pub fn export_session(&self, out: &Path) -> Result<(), String> {
        let text = if out.extension().is_some_and(|ext| ext == "md") {
            serialize(&self.messages)
        } else {
            let path = self.session.path.as_ref().ok_or("This session is not saved (--no-session); export to a .md path.")?;
            std::fs::read_to_string(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?
        };
        std::fs::write(out, text).map_err(|e| format!("Cannot write {}: {e}", out.display()))
    }

    fn tool_schemas(&self, loaded: &[String]) -> Value {
        tools::schemas(loaded, &subagent::swarm_prompt(&self.agent_dir, &self.cwd, &self.subagent_effort))
    }

    /// Replaces the conversation state with a saved (or empty) session's.
    pub fn load(&mut self, history: History) {
        self.next_ref = history.condensed.iter().map(|batch| batch.refs.len()).sum::<usize>() + 1;
        self.messages = history.messages;
        self.condensed = history.condensed;
        *self.todos.lock().unwrap() = history.todos;
        *self.goal.lock().unwrap() = history.goal;
        self.tools = self.tool_schemas(&history.loaded_groups);
        self.loaded_groups = history.loaded_groups;
        self.spent = history.spent;
        self.pending_condense.clear();
        self.last_context_tokens = 0;
        self.nested_instructions.clear();
        self.checkpoints = history.checkpoints;
    }

    pub fn summary(&self, answer: &str) -> Value {
        json!({
            "session_id": self.session.id,
            "model": self.model,
            "turns": self.turns,
            "tool_calls": self.tool_calls,
            "usage": {
                "input_tokens": self.usage.input,
                "output_tokens": self.usage.output,
                "cache_read_input_tokens": self.usage.cache_read,
                "cache_creation_input_tokens": self.usage.cache_write,
            },
            "final_output": answer,
        })
    }
}

/// An aborted stream can end inside a tool call or a thinking block; only finished text is valid history.
fn keep_finished_text(message: &mut Value) {
    if let Some(content) = message["content"].as_array_mut() {
        content.retain(|block| block["type"] == "text" && block["text"].as_str().is_some_and(|t| !t.is_empty()));
    }
}

pub struct SessionInfo {
    pub id: String,
    pub cwd: String,
    pub name: Option<String>,
    /// First line of the first prompt.
    pub title: String,
    pub turns: usize,
    pub modified: std::time::SystemTime,
}

impl Session {
    pub fn start(dir: &Path, save: bool, cwd: &Path, model: &str) -> Result<Session, String> {
        // 8 hex chars from the clock and pid: short enough to type after `--resume`.
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() ^ ((std::process::id() as u128) << 64);
        let id = format!("{:08x}", (seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 64) as u32);
        let session = Session { path: save.then(|| dir.join(format!("{id}.jsonl"))), id };
        if save {
            std::fs::create_dir_all(dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
        }
        session.append(&json!({"type": "session", "id": session.id, "cwd": cwd, "model": model, "timestamp": now_ms()}))?;
        Ok(session)
    }

    /// Opens session `id`, or the one saved session whose id starts with `id`.
    pub fn resume(dir: &Path, id: &str) -> Result<(Session, History), String> {
        let id = resolve_id(dir, id)?;
        let path = dir.join(format!("{id}.jsonl"));
        let text = std::fs::read_to_string(&path).map_err(|e| format!("Cannot read session {}: {e}", path.display()))?;
        let mut history = History::default();
        for (number, line) in text.lines().enumerate() {
            let entry: Value = serde_json::from_str(line).map_err(|e| format!("{}:{}: {e}", path.display(), number + 1))?;
            match entry["type"].as_str() {
                Some("message") => history.messages.push(entry["message"].clone()),
                Some("rewind") => {
                    let to = entry["to"].as_u64().ok_or(format!("{}:{}: rewind without to", path.display(), number + 1))?;
                    history.messages.truncate(to as usize);
                    history.checkpoints.retain(|checkpoint| checkpoint.at < to as usize);
                }
                Some("checkpoint") => {
                    let at = entry["at"].as_u64().ok_or(format!("{}:{}: checkpoint without at", path.display(), number + 1))?;
                    let before = entry["before"].as_str().ok_or(format!("{}:{}: checkpoint without before", path.display(), number + 1))?;
                    history.checkpoints.push(Checkpoint { at: at as usize, before: before.into(), after: None });
                }
                Some("checkpoint_after") => {
                    if let Some(checkpoint) = history.checkpoints.last_mut().filter(|checkpoint| checkpoint.after.is_none()) {
                        checkpoint.after = entry["after"].as_str().map(String::from);
                    }
                }
                Some("checkpoints_reset") => history.checkpoints.clear(),
                Some("compaction") => {
                    let first_kept = entry["first_kept"].as_u64().ok_or(format!("{}:{}: compaction without first_kept", path.display(), number + 1))?;
                    history.messages = compacted(&history.messages, entry["summary"].as_str().unwrap_or_default(), first_kept as usize);
                    shift_checkpoints(&mut history.checkpoints, first_kept as usize);
                }
                Some("condense") => history.condensed.push(condense::from_entry(&entry)),
                Some("goal") => history.goal = Goal::from_entry(&entry),
                Some("tools_loaded") => history.loaded_groups = entry["groups"].as_array().into_iter().flatten().filter_map(|g| g.as_str().map(String::from)).collect(),
                Some("todo") => history.todos = Todos::from_entry(&entry),
                Some("spent") => history.spent.add(Spent::from_entry(&entry)),
                _ => {}
            }
        }
        Ok((Session { id: id.to_string(), path: Some(path) }, history))
    }

    /// Saved sessions in `dir`, newest first. Unreadable files are skipped.
    pub fn list(dir: &Path) -> Vec<SessionInfo> {
        let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
        let mut sessions = Vec::new();
        for path in entries.filter_map(|e| e.ok()).map(|e| e.path()) {
            let Some(id) = path.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".jsonl")) else { continue };
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let Ok(modified) = path.metadata().and_then(|m| m.modified()) else { continue };
            let mut info = SessionInfo { id: id.to_string(), cwd: String::new(), name: None, title: String::new(), turns: 0, modified };
            let mut subagent = false;
            for entry in text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
                match entry["type"].as_str() {
                    Some("session") => info.cwd = entry["cwd"].as_str().unwrap_or_default().to_string(),
                    Some("session_info") => info.name = entry["name"].as_str().map(String::from),
                    // Subagent sessions stay out of /resume and --continue; /subagents opens them.
                    Some("parent") => subagent = true,
                    Some("message") if entry["message"]["role"] == "user" && entry["message"]["content"][0]["type"] == "text" => {
                        info.turns += 1;
                        if info.title.is_empty() {
                            info.title = entry["message"]["content"][0]["text"].as_str().unwrap_or_default().lines().next().unwrap_or_default().to_string();
                        }
                    }
                    _ => {}
                }
            }
            if info.turns > 0 && !subagent {
                sessions.push(info);
            }
        }
        sessions.sort_by_key(|s| std::cmp::Reverse(s.modified));
        sessions
    }

    /// Id of the newest saved session started in `cwd`.
    pub fn latest(dir: &Path, cwd: &Path) -> Result<String, String> {
        let cwd = cwd.display().to_string();
        let newest = Session::list(dir).into_iter().find(|s| s.cwd == cwd);
        newest.map(|s| s.id).ok_or_else(|| format!("No session for {cwd} yet."))
    }

    pub fn rename(&self, name: &str) -> Result<(), String> {
        self.append(&json!({"type": "session_info", "name": name}))
    }

    pub fn append(&self, entry: &Value) -> Result<(), String> {
        let Some(path) = &self.path else { return Ok(()) };
        let mut file = OpenOptions::new().create(true).append(true).open(path).map_err(|e| format!("Cannot open {}: {e}", path.display()))?;
        writeln!(file, "{entry}").map_err(|e| format!("Cannot write {}: {e}", path.display()))
    }

    /// /import: copies an exported session file into `dir` and returns its id for `resume`.
    pub fn import(dir: &Path, file: &Path) -> Result<String, String> {
        let text = std::fs::read_to_string(file).map_err(|e| format!("Cannot read {}: {e}", file.display()))?;
        let header: Value = text.lines().next().and_then(|line| serde_json::from_str(line).ok()).unwrap_or_default();
        let id = header["id"].as_str().unwrap_or_default();
        // The id becomes a file name, so it must not carry a path.
        if header["type"] != "session" || id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(format!("{} is not an exported session.", file.display()));
        }
        let target = dir.join(format!("{id}.jsonl"));
        if target.exists() {
            return Err(format!("Session {id} is already here; /resume {id}."));
        }
        std::fs::create_dir_all(dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
        std::fs::write(&target, text).map_err(|e| format!("Cannot write {}: {e}", target.display()))?;
        Ok(id.to_string())
    }
}

/// The full id for an exact id or a unique prefix of a saved session's id.
fn resolve_id(dir: &Path, prefix: &str) -> Result<String, String> {
    if dir.join(format!("{prefix}.jsonl")).is_file() {
        return Ok(prefix.to_string());
    }
    let entries = std::fs::read_dir(dir).map_err(|e| format!("Cannot read {}: {e}", dir.display()))?;
    let ids: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".jsonl")).map(String::from))
        .filter(|id| id.starts_with(prefix))
        .collect();
    match ids.as_slice() {
        [id] => Ok(id.clone()),
        [] => Err(format!("No saved session {prefix}.")),
        _ => Err(format!("{prefix} matches {} sessions; use more characters.", ids.len())),
    }
}

/// Index of the assistant message where the kept tail starts: the latest one with at least `keep_tokens` after it.
fn cut_point(messages: &[Value], keep_tokens: u64) -> Option<usize> {
    let mut kept_chars = 0;
    let mut latest_assistant = None;
    for index in (1..messages.len()).rev() {
        kept_chars += messages[index].to_string().len() as u64;
        if messages[index]["role"] != "assistant" {
            continue;
        }
        latest_assistant.get_or_insert(index);
        if kept_chars / 4 >= keep_tokens {
            return Some(index);
        }
    }
    latest_assistant
}

/// User prompts that can be rewound to, oldest first.
pub fn rewind_points(messages: &[Value]) -> Vec<RewindPoint> {
    let mut points = Vec::new();
    for (at, message) in messages.iter().enumerate() {
        let Some(blocks) = message["content"].as_array().filter(|_| message["role"] == "user") else { continue };
        if blocks.iter().any(|block| block["type"] == "tool_result") {
            continue;
        }
        // The typed text is the last block; shell output and todo notes come before it.
        let Some(text) = blocks.iter().rev().find_map(|block| block["text"].as_str()) else { continue };
        if !text.starts_with(SUMMARY_PREFIX) {
            let images = blocks.iter().filter(|block| block["type"] == "image").cloned().collect();
            points.push(RewindPoint { at, text: text.to_string(), images });
        }
    }
    points
}

/// Files a rewind put back, and files it left alone because they changed after the agent last wrote them.
#[derive(Debug, Default, PartialEq)]
pub struct RestoredFiles {
    pub restored: Vec<String>,
    pub kept: Vec<String>,
}

/// Puts each file the prompts from message `index` on changed back as it was before the first of them.
fn restore_files(snapshots: &Snapshots, checkpoints: &[Checkpoint], index: usize) -> Result<RestoredFiles, String> {
    let mut files = RestoredFiles::default();
    let rewound: Vec<&Checkpoint> = checkpoints.iter().filter(|checkpoint| checkpoint.at >= index).collect();
    let Some(first) = rewound.first() else { return Ok(files) };
    let mut last_written: BTreeMap<String, &str> = BTreeMap::new();
    for checkpoint in &rewound {
        let Some(after) = &checkpoint.after else { continue };
        for path in snapshots.changed(&checkpoint.before, after)? {
            last_written.insert(path, after);
        }
    }
    let now = snapshots.take()?;
    for (path, after) in last_written {
        let current = snapshots.blob(&now, &path);
        if current != snapshots.blob(after, &path) {
            files.kept.push(path);
        } else if current != snapshots.blob(&first.before, &path) {
            snapshots.restore(&first.before, &path)?;
            files.restored.push(path);
        }
    }
    Ok(files)
}

/// The summary takes index 0, so a kept message moves from first_kept to 1.
fn shift_checkpoints(checkpoints: &mut Vec<Checkpoint>, first_kept: usize) {
    checkpoints.retain(|checkpoint| checkpoint.at >= first_kept);
    for checkpoint in checkpoints {
        checkpoint.at = checkpoint.at - first_kept + 1;
    }
}

fn compacted(messages: &[Value], summary: &str, first_kept: usize) -> Vec<Value> {
    let mut kept = vec![json!({"role": "user", "content": [{"type": "text", "text": format!("{SUMMARY_PREFIX}{summary}{SUMMARY_SUFFIX}")}]})];
    kept.extend_from_slice(&messages[first_kept..]);
    kept
}

/// pi's serializeConversation: plain text, so the summarizer does not continue the conversation.
fn serialize(messages: &[Value]) -> String {
    let mut parts = Vec::new();
    for message in messages {
        let blocks = message["content"].as_array().into_iter().flatten();
        if message["role"] == "assistant" {
            let text = text_of(message);
            if !text.is_empty() {
                parts.push(format!("[Assistant]: {text}"));
            }
            let calls: Vec<String> = blocks.filter(|b| b["type"] == "tool_use").map(|b| format!("{}({})", b["name"].as_str().unwrap_or_default(), call_args(&b["input"]))).collect();
            if !calls.is_empty() {
                parts.push(format!("[Assistant tool calls]: {}", calls.join("; ")));
            }
            continue;
        }
        for block in blocks {
            match block["type"].as_str() {
                Some("text") => parts.push(format!("[User]: {}", block["text"].as_str().unwrap_or_default())),
                Some("tool_result") => parts.push(format!("[Tool result]: {}", truncate_chars(block["content"].as_str().unwrap_or_default(), TOOL_RESULT_SUMMARY_CHARS))),
                _ => {}
            }
        }
    }
    parts.join("\n\n")
}

fn call_args(input: &Value) -> String {
    let fields = input.as_object().into_iter().flatten();
    fields.map(|(key, value)| format!("{key}={value}")).collect::<Vec<_>>().join(", ")
}

fn truncate_chars(text: &str, max: usize) -> String {
    let total = text.chars().count();
    if total <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max).collect();
    format!("{kept}\n\n[... {} more characters truncated]", total - max)
}

fn tool_names(tools: &Value) -> Vec<String> {
    tools.as_array().into_iter().flatten().filter_map(|t| t["name"].as_str().map(String::from)).collect()
}

fn text_of(message: &Value) -> String {
    let blocks = message["content"].as_array().into_iter().flatten();
    blocks.filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n")
}


#[cfg(test)]
mod tests {
    use super::*;

    fn assistant(text: &str) -> Value {
        json!({"role": "assistant", "content": [{"type": "text", "text": text}]})
    }

    fn user(text: &str) -> Value {
        json!({"role": "user", "content": [{"type": "text", "text": text}]})
    }

    #[test]
    fn cut_point_keeps_recent_tokens_and_starts_at_an_assistant_message() {
        let big = "x".repeat(400);
        let messages = [user("a"), assistant(&big), user(&big), assistant(&big), user("b")];
        assert_eq!(cut_point(&messages, 100), Some(3));
        assert_eq!(cut_point(&messages, 300), Some(1));
        assert_eq!(cut_point(&messages, 10_000), Some(3), "a tail shorter than the budget still compacts up to the latest reply");
        assert_eq!(cut_point(&[user("only")], 1), None);
    }

    #[test]
    fn compacted_history_starts_with_the_summary() {
        let messages = [user("a"), assistant("b"), user("c")];
        let kept = compacted(&messages, "S", 1);
        assert_eq!(text_of(&json!({"content": kept[0]["content"]})), format!("{SUMMARY_PREFIX}S{SUMMARY_SUFFIX}"));
        assert_eq!(kept[1..], messages[1..]);
    }

    #[test]
    fn serialize_matches_pi_format() {
        let messages = [
            user("fix it"),
            json!({"role": "assistant", "content": [{"type": "thinking", "thinking": "t"}, {"type": "text", "text": "ok"}, {"type": "tool_use", "id": "1", "name": "bash", "input": {"command": "ls"}}]}),
            json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "1", "content": "a.rs"}]}),
        ];
        assert_eq!(serialize(&messages), "[User]: fix it\n\n[Assistant]: ok\n\n[Assistant tool calls]: bash(command=\"ls\")\n\n[Tool result]: a.rs");
    }

    #[test]
    fn resumed_session_gives_the_provider_the_same_condensed_view() {
        let dir = std::env::temp_dir().join(format!("agent-condense-{}", std::process::id()));
        let session = Session::start(&dir, true, Path::new("/w"), "m").unwrap();
        let messages = [
            json!({"role": "assistant", "content": [{"type": "tool_use", "id": "a", "name": "bash", "input": {}}]}),
            json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "a", "content": "raw", "is_error": false}]}),
        ];
        for message in &messages {
            session.append(&json!({"type": "message", "message": message})).unwrap();
        }
        let batch = Condensed { refs: vec![("t1".into(), "a".into())], summary: "`t1` ran".into() };
        session.append(&condense::entry(&batch)).unwrap();
        let (_, history) = Session::resume(&dir, &session.id).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(history.messages, messages);
        assert_eq!(condense::view(&history.messages, &history.condensed), condense::view(&messages, &[batch]));
    }

    #[test]
    fn resumed_session_sums_the_time_and_tokens_spent() {
        let dir = std::env::temp_dir().join(format!("agent-spent-{}", std::process::id()));
        let session = Session::start(&dir, true, Path::new("/w"), "m").unwrap();
        session.append(&Spent { input: 100, output: 20, time: Duration::from_millis(1500) }.entry()).unwrap();
        session.append(&Spent { input: 50, output: 5, time: Duration::from_secs(61) }.entry()).unwrap();
        let (_, history) = Session::resume(&dir, &session.id).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(history.spent, Spent { input: 150, output: 25, time: Duration::from_millis(62_500) });
    }

    #[test]
    fn resumed_session_drops_rewound_messages_and_their_checkpoints() {
        let dir = std::env::temp_dir().join(format!("agent-rewind-{}", std::process::id()));
        let session = Session::start(&dir, true, Path::new("/w"), "m").unwrap();
        session.append(&json!({"type": "checkpoint", "at": 0, "before": "t0"})).unwrap();
        session.append(&json!({"type": "checkpoint_after", "after": "t1"})).unwrap();
        for message in [user("hi"), assistant("hello")] {
            session.append(&json!({"type": "message", "message": message})).unwrap();
        }
        session.append(&json!({"type": "checkpoint", "at": 2, "before": "t1"})).unwrap();
        for message in [user("look at this image"), assistant("nice")] {
            session.append(&json!({"type": "message", "message": message})).unwrap();
        }
        session.append(&json!({"type": "rewind", "to": 2})).unwrap();
        let (_, history) = Session::resume(&dir, &session.id).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(history.messages, [user("hi"), assistant("hello")]);
        assert_eq!(history.checkpoints, [Checkpoint { at: 0, before: "t0".into(), after: Some("t1".into()) }]);
    }

    #[test]
    fn rewind_points_are_typed_prompts_not_tool_results_or_the_summary() {
        let summary = user(&format!("{SUMMARY_PREFIX}S{SUMMARY_SUFFIX}"));
        let tool_result = json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "1", "content": "x"}, {"type": "text", "text": "steer"}]});
        let image = json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AA"}});
        let with_context = json!({"role": "user", "content": [{"type": "text", "text": "$ ls"}, image, {"type": "text", "text": "fix [Image #1]"}]});
        let messages = [summary, assistant("a"), user("hi"), assistant("b"), tool_result, with_context];
        let hi = RewindPoint { at: 2, text: "hi".into(), images: Vec::new() };
        let fix = RewindPoint { at: 5, text: "fix [Image #1]".into(), images: vec![image] };
        assert_eq!(rewind_points(&messages), [hi, fix]);
    }

    #[test]
    fn rewind_restores_what_the_agent_changed_and_keeps_later_user_edits() {
        let root = std::env::temp_dir().join(format!("agent-restore-{}", std::process::id()));
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let git = |args: &[&str]| assert!(std::process::Command::new("git").arg("-C").arg(&work).args(args).output().unwrap().status.success());
        git(&["init", "-q"]);
        let write = |name: &str, text: &str| std::fs::write(work.join(name), text).unwrap();
        let read = |name: &str| std::fs::read_to_string(work.join(name)).ok();
        write("a.txt", "v1");
        let snapshots = Snapshots::open(&root, &work).unwrap();
        let first = Checkpoint { at: 0, before: snapshots.take().unwrap(), after: None };
        write("a.txt", "v2");
        let first = Checkpoint { after: Some(snapshots.take().unwrap()), ..first };
        write("between.txt", "user");
        let second = Checkpoint { at: 2, before: snapshots.take().unwrap(), after: None };
        write("a.txt", "v3");
        write("new.txt", "agent");
        write("shared.txt", "agent");
        let second = Checkpoint { after: Some(snapshots.take().unwrap()), ..second };
        write("shared.txt", "mine");

        let files = restore_files(&snapshots, &[first, second], 2).unwrap();
        let state = [read("a.txt"), read("new.txt"), read("shared.txt"), read("between.txt")];
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(files, RestoredFiles { restored: vec!["a.txt".into(), "new.txt".into()], kept: vec!["shared.txt".into()] });
        assert_eq!(state, [Some("v2".into()), None, Some("mine".into()), Some("user".into())]);
    }

    #[test]
    fn imported_session_resumes_and_a_non_session_file_is_refused() {
        let root = std::env::temp_dir().join(format!("agent-import-{}", std::process::id()));
        let session = Session::start(&root.join("a"), true, Path::new("/repo"), "m").unwrap();
        session.append(&json!({"type": "message", "message": {"role": "user", "content": "hi"}})).unwrap();

        let id = Session::import(&root.join("b"), session.path.as_ref().unwrap()).unwrap();
        let (_, history) = Session::resume(&root.join("b"), &id).unwrap();
        assert_eq!(history.messages, vec![json!({"role": "user", "content": "hi"})]);
        assert!(Session::import(&root.join("b"), session.path.as_ref().unwrap()).is_err());

        let forged = root.join("forged.jsonl");
        std::fs::write(&forged, "{\"type\": \"session\", \"id\": \"../x\"}\n").unwrap();
        assert!(Session::import(&root.join("b"), &forged).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
