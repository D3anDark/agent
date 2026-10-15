mod accounts;
mod agent;
mod api;
mod ask;
mod bg;
mod codex;
mod condense;
mod goal;
mod hashline;
mod models;
mod modes;
mod oauth;
mod permissions;
mod prompt;
mod rtk;
mod settings;
mod snapshots;
mod subagent;
mod todo;
mod tools;
mod tui;
mod update;
mod worktree;

use serde_json::Value;
use std::collections::HashSet;
use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use agent::{Agent, Event, History, Session, Spent};
use api::{Client, Usage};
use hashline::Anchors;
use permissions::{Decision, Grants, Mode};

const USAGE: &str = "Usage: agent [options] [message...]

Options:
  --model <id>          Model id, e.g. claude-opus-5-5
  --thinking <level>    off, minimal, low, medium, high, xhigh, max
  -p, --print           Run the message, print the answer, exit
  --json                Like --print, then print one JSON summary
  -c, --continue        Continue the latest session in this directory
  --resume <id>         Open a session by id or id prefix
  --no-session          Do not save the session
  --auto                Let a reviewer model approve tool calls
  --full-access, --yolo Run every tool call without asking
  --ask                 Ask before commands and file edits
  --headless            Run the message, print JSONL events (tool, session, done, error)
  --parent <id>         Run as a subagent of that session: linked to it, no load_tools or ask_user_question
  --version             Print the version";

struct Options {
    model: Option<String>,
    thinking: Option<String>,
    permission_mode: Option<Mode>,
    print: bool,
    json: bool,
    headless: bool,
    parent: Option<String>,
    save: bool,
    resume: Option<Resume>,
    prompt: Option<String>,
}

enum Resume {
    Latest,
    Id(String),
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("agent {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let options = match parse_args(std::env::args().skip(1).collect()) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}\n{USAGE}");
            std::process::exit(2);
        }
    };
    if let Err(message) = run(options) {
        eprintln!("error: {message}");
        std::process::exit(1);
    }
}

fn parse_args(args: Vec<String>) -> Result<Options, String> {
    let mut options = Options {
        model: None,
        thinking: None,
        permission_mode: None,
        print: false,
        json: false,
        headless: false,
        parent: None,
        save: true,
        resume: None,
        prompt: None,
    };
    let mut words = Vec::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or(format!("{name} needs a value."));
        match arg.as_str() {
            "--model" => options.model = Some(value("--model")?),
            "--thinking" | "--effort" => options.thinking = Some(value("--thinking")?),
            "--session" | "--resume" => options.resume = Some(Resume::Id(value("--session")?)),
            "-c" | "--continue" => options.resume = Some(Resume::Latest),
            "-p" | "--print" => options.print = true,
            "--json" => options.json = true,
            "--headless" => options.headless = true,
            "--parent" => options.parent = Some(value("--parent")?),
            "--no-session" => options.save = false,
            "--auto" => options.permission_mode = Some(Mode::Auto),
            "--full-access" | "--yolo" => options.permission_mode = Some(Mode::FullAccess),
            "--ask" => options.permission_mode = Some(Mode::Ask),
            "-h" | "--help" => return Err(String::new()),
            "--" => words.extend(args.by_ref()),
            flag if flag.starts_with('-') => return Err(format!("Unknown option {flag}.")),
            _ => words.push(arg),
        }
    }
    if !words.is_empty() {
        options.prompt = Some(words.join(" "));
    }
    Ok(options)
}

fn run(options: Options) -> Result<(), String> {
    let home = PathBuf::from(std::env::var("HOME").map_err(|_| "HOME is not set.")?);
    let agent_dir = home.join(".agents");
    let mut settings = settings::load(&home);
    if let Some(model) = options.model {
        settings.model = model;
    }
    if let Some(thinking) = options.thinking {
        settings.thinking = thinking;
    }
    if let Some(mode) = options.permission_mode {
        settings.permission_mode = mode;
    }
    let catalog = models::all();
    let model = catalog.iter().find(|m| m.id == settings.model).ok_or(format!("Unknown model {}.", settings.model))?;
    let cwd = std::env::current_dir().map_err(|e| format!("Cannot read the working directory: {e}"))?;
    let sessions_dir = home.join(".agent/sessions");
    let (session, history) = match &options.resume {
        Some(Resume::Id(id)) => Session::resume(&sessions_dir, id)?,
        Some(Resume::Latest) => Session::resume(&sessions_dir, &Session::latest(&sessions_dir, &cwd)?)?,
        None => (Session::start(&sessions_dir, options.save, &cwd, &settings.model)?, History::default()),
    };
    if let Some(parent) = &options.parent {
        session.append(&serde_json::json!({"type": "parent", "session": parent}))?;
    }
    let cheap_model = models::cheapest(&catalog, &model.provider);
    let client = Client::new(&home, &catalog)?;
    client.configure(&settings);
    let modes_error = modes::fetch(&home, &settings.ponytail, &settings.caveman).err();
    let mut agent = Agent {
        client,
        system: prompt::system_prompt(&agent_dir, &cwd, &settings.disabled_skills),
        modes: modes::prompt(&home, &settings.ponytail, &settings.caveman),
        // agent.load below fills it, with the swarm prompt for the session's loaded groups.
        tools: Value::Null,
        anchors: Anchors::new(&session.id),
        rtk: if settings.rtk { rtk::Rtk::ON } else { rtk::Rtk::OFF },
        nested_instructions: HashSet::new(),
        shell_context: Vec::new(),
        images: Vec::new(),
        model: settings.model.clone(),
        effort: settings.thinking.clone(),
        context_window: settings.context_window(model),
        max_tokens: model.max_tokens,
        cheap_model: cheap_model.id.clone(),
        auto_compact: settings.auto_compact,
        reserve_tokens: settings.reserve_tokens,
        keep_recent_tokens: settings.keep_recent_tokens,
        last_context_tokens: 0,
        condense: condense::Config::new(&settings, &cheap_model.id),
        condensed: Vec::new(),
        next_ref: 1,
        pending_condense: Vec::new(),
        usage: Usage::default(),
        turns: 0,
        tool_calls: 0,
        abort: Arc::default(),
        steering: Arc::new(Mutex::new(Vec::new())),
        permission_mode: Arc::new(Mutex::new(settings.permission_mode)),
        grants: Grants::default(),
        todos: Arc::default(),
        goal: Arc::default(),
        follow_ups: Arc::default(),
        terminals: Arc::default(),
        subagents: Arc::default(),
        subagent_concurrency: settings.subagent_concurrency,
        subagent_effort: settings.subagent_effort.clone(),
        loaded_groups: Vec::new(),
        spent: Spent::default(),
        interactive: false,
        agent_dir: agent_dir.clone(),
        home: home.clone(),
        snapshots: None,
        checkpoints: Vec::new(),
        cwd,
        messages: Vec::new(),
        session,
        sessions_dir,
    };
    agent.load(history);
    // After load, which resets the tools to the session's loaded groups. Only the root spawns, and a child has no UI to ask in.
    if options.parent.is_some() {
        agent.tools = Value::Array(agent.tools.as_array().into_iter().flatten().filter(|tool| !matches!(tool["name"].as_str(), Some("load_tools" | "ask_user_question"))).cloned().collect());
    }
    let mut prompt = options.prompt;
    if !std::io::stdin().is_terminal() {
        let mut piped = String::new();
        std::io::stdin().read_to_string(&mut piped).map_err(|e| format!("Cannot read stdin: {e}"))?;
        if !piped.trim().is_empty() {
            prompt = Some(match prompt {
                Some(words) => format!("{words}\n\n{}", piped.trim_end()),
                None => piped.trim_end().to_string(),
            });
        }
    }
    if options.headless || options.print || options.json || !std::io::stdout().is_terminal() {
        if let Some(error) = &modes_error {
            eprintln!("{error}");
        }
        let result = if options.headless {
            headless_mode(&mut agent, prompt.as_deref().unwrap_or_default())
        } else {
            print_mode(&mut agent, &prompt.ok_or("Print mode needs a message.")?, options.json)
        };
        agent.terminals.lock().unwrap().kill_all();
        agent.subagents.lock().unwrap().stop_all();
        return result;
    }
    let skills = prompt::load_skills(&agent_dir, &agent.cwd, &settings.disabled_skills);
    let templates = prompt::load_templates(&agent_dir, &agent.cwd);
    agent.interactive = true;
    agent.snapshots = snapshots::Snapshots::open(&home, &agent.cwd);
    tui::run(agent, tui::Setup { home, settings, catalog, skills, templates, prompt, modes_error })
}

/// Non-interactive run: streams text to stdout (or nothing with --json) and progress to stderr.
fn print_mode(agent: &mut Agent, prompt: &str, json: bool) -> Result<(), String> {
    let answer = agent.prompt(prompt, &mut |event| match event {
        Event::Text(text) if !json => {
            print!("{text}");
            let _ = std::io::stdout().flush();
        }
        Event::ToolStart { name, input, .. } if !json => eprintln!("· {name} {}", preview(&input)),
        Event::Notice(text) => eprintln!("{text}"),
        Event::Permission { request, reply } => {
            eprintln!("Denied {}: {} (print mode cannot ask; use --auto or --full-access)", request.kind, request.detail);
            let _ = reply.send(Decision::Deny);
        }
        Event::Reviewed { allowed, detail, reason } => eprintln!("auto: {} {detail}: {reason}", if allowed { "allowed" } else { "denied" }),
        Event::TurnEnd { .. } if !json => println!(),
        _ => {}
    })?;
    if json {
        println!("{}", agent.summary(&answer));
    }
    Ok(())
}

/// Child-process mode: one JSON object per line on stdout, which the parent's subagent tool reads.
fn headless_mode(agent: &mut Agent, prompt: &str) -> Result<(), String> {
    let line = |event: Value| {
        println!("{event}");
        let _ = std::io::stdout().flush();
    };
    if prompt.trim().is_empty() {
        line(serde_json::json!({"type": "error", "message": "Headless mode needs a message."}));
        return Err("Headless mode needs a message.".into());
    }
    if let Some(path) = &agent.session.path {
        line(serde_json::json!({"type": "session", "path": path}));
    }
    let result = agent.prompt(prompt, &mut |event| match event {
        Event::ToolStart { name, .. } => line(serde_json::json!({"type": "tool", "name": name})),
        Event::Permission { reply, .. } => _ = reply.send(Decision::Deny),
        _ => {}
    });
    match result {
        Ok(answer) => line(serde_json::json!({"type": "done", "answer": answer})),
        Err(message) => line(serde_json::json!({"type": "error", "message": message})),
    }
    Ok(())
}

fn preview(input: &Value) -> String {
    let text = input.to_string();
    match text.char_indices().nth(160) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text,
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
}
