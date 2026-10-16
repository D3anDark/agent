mod agent;
mod api;
mod mcp;
mod plugins;
mod settings;
mod tools;
mod tui;
mod update;
mod worktree;

use serde_json::Value;
use std::collections::HashSet;
use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use agent::permissions::{Decision, Grants, Mode};
use agent::{Agent, Event, History, Session, Spent, condense, modes, prompt, snapshots, usage};
use api::{Client, Usage, models};
use tools::hashline::Anchors;
use tools::rtk;

const USAGE: &str = "Usage: agent [options] [message...]
       agent plugin install|update <name|path>   Copy a bundled plugin or a plugin directory into ~/.agents/plugins
       agent plugin remove <name>
       agent plugin list

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

fn plugin_command(args: &[String]) -> Result<(), String> {
    let home = PathBuf::from(std::env::var("HOME").map_err(|_| "HOME is not set.")?);
    match args {
        [action, source] if action == "install" || action == "update" => println!("Installed {}.", plugins::install(&home, source)?),
        [action, name] if action == "remove" => {
            plugins::remove(&home, name)?;
            println!("Removed {name}.");
        }
        [action] if action == "list" => {
            let disabled = settings::load(&home).disabled_plugins;
            for view in plugins::Plugins::new(&home).list() {
                let state = match (view.installed, disabled.contains(&view.name)) {
                    (false, _) => "available",
                    (true, true) => "disabled",
                    (true, false) => "enabled",
                };
                println!("{:<16} {:<10} {:<10} {}", view.name, view.version, state, view.description);
            }
        }
        _ => return Err(format!("Unknown plugin command.\n{USAGE}")),
    }
    Ok(())
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("agent {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|command| command == "plugin") {
        if let Err(message) = plugin_command(&args[1..]) {
            eprintln!("error: {message}");
            std::process::exit(1);
        }
        return;
    }
    let options = match parse_args(args) {
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
    let (client, mut model_notices) = Client::new(&home)?;
    let catalog = client.catalog().to_vec();
    // A model no logged-in account lists gives way to one an account lists, then to OpenRouter's public list.
    let model = catalog.iter().find(|m| m.id == settings.model).or_else(|| catalog.iter().find(|m| !m.accounts.is_empty())).or_else(|| catalog.first());
    let Some(model) = model else {
        return Err(format!("No provider listed any model. {}", model_notices.join(" ")));
    };
    if model.id != settings.model {
        model_notices.push(format!("{} is not on any account's model list; using {}.", settings.model, model.id));
        settings.model = model.id.clone();
    }
    client.configure(&settings);
    let cwd = std::env::current_dir().map_err(|e| format!("Cannot read the working directory: {e}"))?;
    let sessions_dir = home.join(".agents/sessions");
    let (session, history) = match &options.resume {
        Some(Resume::Id(id)) => Session::resume(&sessions_dir, id)?,
        Some(Resume::Latest) => Session::resume(&sessions_dir, &Session::latest(&sessions_dir, &cwd)?)?,
        None => (Session::start(&sessions_dir, options.save, &cwd, &settings.model)?, History::default()),
    };
    if let Some(max_age) = settings.prune_sessions_after() {
        Session::prune(&sessions_dir, max_age);
    }
    if let Some(parent) = &options.parent {
        session.append(&serde_json::json!({"type": "parent", "session": parent}))?;
    }
    let cheap_model = models::small_model(&catalog, model);
    let modes_error = modes::fetch(&home, &settings.ponytail, &settings.caveman, true).err();
    // Before agent.plugins.sync starts the processes, so they run the updated code.
    let plugin_updates = plugins::update_bundled(&home);
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
        pending_title: None,
        usage: Usage::default(),
        turns: 0,
        tool_calls: 0,
        abort: Arc::default(),
        steering: Arc::new(Mutex::new(Vec::new())),
        permission_mode: Arc::new(Mutex::new(settings.permission_mode)),
        grants: Grants::default(),
        todos: Arc::default(),
        goal: Arc::default(),
        plan: Arc::default(),
        follow_ups: Arc::default(),
        terminals: Arc::default(),
        web: tools::web::Web::new(&home, std::env::temp_dir().join(format!("agent-web-{}", std::process::id()))),
        subagents: Arc::default(),
        subagent_concurrency: settings.subagent_concurrency,
        subagent_effort: settings.subagent_effort.clone(),
        loaded_groups: Vec::new(),
        extra_dirs: Vec::new(),
        spent: Spent::default(),
        interactive: false,
        agent_dir: agent_dir.clone(),
        home: home.clone(),
        plugins: plugins::Plugins::new(&home),
        mcp: mcp::Mcp::new(&home, &cwd),
        snapshots: None,
        lsp: agent::lsp::Lsp::new(settings.lsp.clone()),
        checkpoints: Vec::new(),
        cwd,
        messages: Vec::new(),
        session,
        sessions_dir,
    };
    agent.plugins.sync(&settings.disabled_plugins, &settings.plugin_settings);
    agent.mcp.sync(&settings.disabled_mcp);
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
        if let Some(notice) = agent.session.fork_notice() {
            eprintln!("{notice}");
        }
        for notice in &plugin_updates {
            eprintln!("{notice}");
        }
        for notice in &model_notices {
            eprintln!("{notice}");
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
    // All skills: /skills lists the disabled ones too, the TUI filters by settings.disabled_skills.
    let skills = prompt::load_skills(&agent_dir, &agent.cwd, &[]);
    let templates = prompt::load_templates(&agent_dir, &agent.cwd);
    agent.interactive = true;
    agent.snapshots = snapshots::Snapshots::open(&home, &agent.cwd);
    tui::run(agent, tui::Setup { home, settings, catalog, skills, templates, prompt, modes_error, plugin_updates, model_notices })
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
    line(usage::entry(&agent.model, agent.usage, now_ms()));
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

/// Whether `binary` is an executable file in some PATH directory; rtk and headroom are optional installs.
pub fn installed(binary: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Some(path) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&path).any(|dir| std::fs::metadata(dir.join(binary)).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
}

#[cfg(test)]
mod tests {
    #[test]
    fn installed_finds_only_executables_on_path() {
        assert!(super::installed("sh"));
        assert!(!super::installed("agent-no-such-binary"));
    }
}
