//! Plugins are long-lived processes in ~/.agents/plugins/<name>/ that speak JSON-RPC 2.0 over stdio,
//! one message per line. The protocol is described in this repo's `plugins/README.md`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// How long a prompt waits for each plugin's `input` answer before it goes through unchanged.
const INPUT_TIMEOUT: Duration = Duration::from_secs(5);

/// The repo's `plugins/` directories, built in so `agent plugin install <name>` works offline.
const BUNDLED: &[(&str, &[(&str, &str)])] = &[(
    "usagefleet",
    &[("plugin.json", include_str!("../plugins/usagefleet/plugin.json")), ("index.ts", include_str!("../plugins/usagefleet/index.ts"))],
)];

/// The running plugins. Clones share them.
#[derive(Clone)]
pub struct Plugins {
    dir: PathBuf,
    state: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
    /// Installed plugins, sorted by name.
    plugins: Vec<Plugin>,
    /// Open `input` requests by id, with the plugin that owes the answer.
    pending: HashMap<u64, (String, Sender<Value>)>,
    next_id: u64,
    on_change: Option<Arc<dyn Fn() + Send + Sync>>,
    /// The last `ui.showPanel` not yet taken by the UI: plugin name and params (null closes).
    panel: Option<(String, Value)>,
}

struct Plugin {
    name: String,
    process: Option<Process>,
    /// Slash commands from the manifest the process was started with.
    commands: Vec<(String, String)>,
    status: Option<String>,
    /// Text for the `/account` panel by provider account id, set with `ui.setAccountLabels`.
    account_labels: HashMap<String, String>,
    /// Why the plugin is not running: a bad manifest, a failed start, or how it exited.
    error: Option<String>,
    /// Tools from the manifest, offered to the model as `<plugin>__<name>`.
    tools: Vec<Value>,
    /// Manifest defaults overlaid with the user's values, sent as `context.settings`.
    settings: Value,
}

struct Process {
    /// Tells this process's reader thread apart from one left over by an earlier start.
    run: u64,
    child: Child,
    /// A writer thread owns stdin, so a plugin that stops reading never blocks the agent.
    stdin: Sender<String>,
}

/// One row of `agent plugin list` and /plugins.
pub struct View {
    pub name: String,
    pub version: String,
    pub description: String,
    /// GitHub login of the plugin's author.
    pub author: Option<String>,
    /// The manifest's `settings` declarations: `key`, `description`, `default`.
    pub settings: Vec<Value>,
    pub installed: bool,
    pub running: bool,
    pub error: Option<String>,
}

impl Plugins {
    /// The plugins installed in `home`, none started yet; `sync` starts them.
    pub fn new(home: &Path) -> Plugins {
        Plugins { dir: home.join(".agents/plugins"), state: Arc::default() }
    }

    /// Called after a status changes or a plugin exits, from a plugin's reader thread.
    pub fn on_change(&self, callback: impl Fn() + Send + Sync + 'static) {
        self.state.lock().unwrap().on_change = Some(Arc::new(callback));
    }

    /// Matches the processes to the disk: starts installed plugins not in `disabled`, stops the rest.
    /// `values` holds the user's settings per plugin, `{ "<plugin>": { "<key>": value } }`.
    pub fn sync(&self, disabled: &[String], values: &Value) {
        let installed = installed(&self.dir);
        let mut state = self.state.lock().unwrap();
        for plugin in &mut state.plugins {
            if !installed.contains(&plugin.name) || disabled.contains(&plugin.name) {
                stop(plugin);
            }
        }
        state.plugins.retain(|plugin| installed.contains(&plugin.name));
        for name in installed {
            state.next_id += 1;
            let run = state.next_id;
            let index = match state.plugins.iter().position(|plugin| plugin.name == name) {
                Some(index) => index,
                None => {
                    state.plugins.push(Plugin { name: name.clone(), process: None, commands: Vec::new(), status: None, account_labels: HashMap::new(), error: None, tools: Vec::new(), settings: json!({}) });
                    state.plugins.len() - 1
                }
            };
            let plugin = &mut state.plugins[index];
            let dir = self.dir.join(&name);
            let manifest = read_manifest(&dir);
            if let Ok(manifest) = &manifest {
                plugin.settings = settings_for(&manifest.settings, &values[&name]);
            }
            if plugin.process.is_some() || disabled.contains(&name) {
                continue;
            }
            match manifest.and_then(|manifest| Ok((self.spawn(&name, &dir, &manifest.command, run)?, manifest))) {
                Ok((process, manifest)) => (plugin.process, plugin.commands, plugin.tools, plugin.error) = (Some(process), manifest.commands, manifest.tools, None),
                Err(error) => plugin.error = Some(error),
            }
        }
        state.plugins.sort_by(|a, b| a.name.cmp(&b.name));
    }

    /// Sends an `event` notification to every running plugin; `params` carries its `type`.
    pub fn event(&self, params: Value) {
        for plugin in &self.state.lock().unwrap().plugins {
            if let Some(process) = &plugin.process {
                let _ = process.stdin.send(message(plugin, None, "event", &params));
            }
        }
    }

    /// Sends an `event` notification to one running plugin, e.g. `panel_select` from its panel.
    pub fn event_to(&self, name: &str, params: Value) {
        for plugin in &self.state.lock().unwrap().plugins {
            if let (true, Some(process)) = (plugin.name == name, &plugin.process) {
                let _ = process.stdin.send(message(plugin, None, "event", &params));
            }
        }
    }

    /// Tool schemas of the running plugins, named `<plugin>__<tool>`.
    pub fn tools(&self) -> Vec<Value> {
        let state = self.state.lock().unwrap();
        let running = state.plugins.iter().filter(|plugin| plugin.process.is_some());
        running.flat_map(|plugin| plugin.tools.iter().map(|tool| {
            let mut tool = tool.clone();
            tool["name"] = json!(format!("{}__{}", plugin.name, tool["name"].as_str().unwrap_or_default()));
            tool
        })).collect()
    }

    /// Runs a plugin tool: None when `name` is no running plugin's tool, else the text and
    /// whether it is an error. Waits for the answer as long as it takes, until `abort` is set.
    pub fn tool(&self, name: &str, input: &Value, context: &Value, abort: &AtomicBool) -> Option<(String, bool)> {
        let (owner, tool) = name.split_once("__")?;
        let offered = self.state.lock().unwrap().plugins.iter().any(|plugin| plugin.name == owner && plugin.process.is_some() && plugin.tools.iter().any(|t| t["name"] == tool));
        if !offered {
            return None;
        }
        let params = json!({"name": tool, "input": input, "context": context});
        Some(match self.request_until(owner, "tool", params, None, abort) {
            None if abort.load(Ordering::Relaxed) => ("Operation aborted".into(), true),
            None => (format!("{owner} stopped before answering {name}."), true),
            Some(reply) => match reply["error"]["message"].as_str() {
                Some(message) => (message.to_string(), true),
                None => (reply["result"]["text"].as_str().unwrap_or_default().to_string(), reply["result"]["isError"] == true),
            },
        })
    }

    /// The panel a plugin last asked for with `ui.showPanel`, once: plugin name and params, null to close.
    pub fn take_panel(&self) -> Option<(String, Value)> {
        self.state.lock().unwrap().panel.take()
    }

    /// Asks each running plugin about a prompt, in name order: Ok with the text, rewritten by any
    /// `transform`, or Err with the reason of the first `block`. Silence, errors and crashes let it through.
    pub fn input(&self, text: &str, context: &Value) -> Result<String, String> {
        let mut text = text.to_string();
        for name in self.running() {
            let Some(answer) = self.request(&name, "input", json!({"text": text, "context": context})) else { continue };
            let result = &answer["result"];
            match result["action"].as_str() {
                Some("block") => return Err(result["reason"].as_str().map_or_else(|| format!("{name} blocked the prompt."), String::from)),
                Some("transform") => {
                    if let Some(rewritten) = result["text"].as_str() {
                        text = rewritten.to_string();
                    }
                }
                _ => {}
            }
        }
        Ok(text)
    }

    /// The footer lines set with `ui.setStatus`, in name order.
    pub fn statuses(&self) -> Vec<String> {
        self.state.lock().unwrap().plugins.iter().filter_map(|plugin| plugin.status.clone()).collect()
    }

    /// `/account` panel labels as (plugin name, account id to text), one entry per plugin that set any.
    pub fn account_labels(&self) -> Vec<(String, HashMap<String, String>)> {
        let state = self.state.lock().unwrap();
        state.plugins.iter().filter(|plugin| !plugin.account_labels.is_empty()).map(|plugin| (plugin.name.clone(), plugin.account_labels.clone())).collect()
    }

    /// Slash commands of running plugins as (name, description), in plugin name order.
    pub fn commands(&self) -> Vec<(String, String)> {
        let state = self.state.lock().unwrap();
        state.plugins.iter().filter(|plugin| plugin.process.is_some()).flat_map(|plugin| plugin.commands.clone()).collect()
    }

    /// Runs `/name args` on the running plugin that offers it: None when none does,
    /// else the text to show or the plugin's error.
    pub fn command(&self, name: &str, args: &str, context: &Value) -> Option<Result<String, String>> {
        let owner = {
            let state = self.state.lock().unwrap();
            let plugin = state.plugins.iter().find(|plugin| plugin.process.is_some() && plugin.commands.iter().any(|(command, _)| command == name))?;
            plugin.name.clone()
        };
        let Some(answer) = self.request(&owner, "command", json!({"name": name, "args": args, "context": context})) else {
            return Some(Err(format!("{owner} did not answer /{name}.")));
        };
        if let Some(message) = answer["error"]["message"].as_str() {
            return Some(Err(message.to_string()));
        }
        Some(Ok(answer["result"]["text"].as_str().unwrap_or_default().to_string()))
    }

    /// Installed plugins as on disk, with how their processes run, then the bundled ones not installed yet.
    pub fn list(&self) -> Vec<View> {
        let state = self.state.lock().unwrap();
        let mut views = Vec::new();
        for name in installed(&self.dir) {
            let plugin = state.plugins.iter().find(|plugin| plugin.name == name);
            let (manifest, manifest_error) = match read_manifest(&self.dir.join(&name)) {
                Ok(manifest) => (Some(manifest), None),
                Err(error) => (None, Some(error)),
            };
            let running = plugin.is_some_and(|plugin| plugin.process.is_some());
            let error = plugin.and_then(|plugin| plugin.error.clone()).or(manifest_error);
            views.push(view(name, manifest, true, running, error));
        }
        for (name, files) in BUNDLED {
            if views.iter().any(|view| view.name == *name) {
                continue;
            }
            let manifest = files.iter().find(|(file, _)| *file == "plugin.json").and_then(|(_, text)| parse_manifest(text).ok());
            views.push(view(name.to_string(), manifest, false, false, None));
        }
        views
    }

    fn running(&self) -> Vec<String> {
        self.state.lock().unwrap().plugins.iter().filter(|plugin| plugin.process.is_some()).map(|plugin| plugin.name.clone()).collect()
    }

    /// The plugin's answer, or None when it is not running, exits, or misses INPUT_TIMEOUT.
    fn request(&self, name: &str, method: &str, params: Value) -> Option<Value> {
        self.request_until(name, method, params, Some(INPUT_TIMEOUT), &AtomicBool::new(false))
    }

    /// The plugin's answer, or None when it is not running, exits, misses `timeout`, or `abort` is set.
    fn request_until(&self, name: &str, method: &str, params: Value, timeout: Option<Duration>, abort: &AtomicBool) -> Option<Value> {
        let (reply, answer) = mpsc::channel();
        let id = {
            let mut state = self.state.lock().unwrap();
            state.next_id += 1;
            let id = state.next_id;
            let plugin = state.plugins.iter().find(|plugin| plugin.name == name)?;
            plugin.process.as_ref()?.stdin.send(message(plugin, Some(id), method, &params)).ok()?;
            state.pending.insert(id, (name.to_string(), reply));
            id
        };
        let start = Instant::now();
        let answer = loop {
            match answer.recv_timeout(Duration::from_millis(100)) {
                Ok(answer) => break Some(answer),
                Err(mpsc::RecvTimeoutError::Disconnected) => break None,
                Err(mpsc::RecvTimeoutError::Timeout) if abort.load(Ordering::Relaxed) || timeout.is_some_and(|t| start.elapsed() >= t) => break None,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        };
        self.state.lock().unwrap().pending.remove(&id);
        answer
    }

    fn spawn(&self, name: &str, dir: &Path, command: &[String], run: u64) -> Result<Process, String> {
        let mut child = Command::new(&command[0])
            .args(&command[1..])
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Cannot start {}: {e}", command[0]))?;
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
        let (plugins, name) = (self.clone(), name.to_string());
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                plugins.receive(&name, run, &line);
            }
            plugins.exited(&name, run, last_error.join().unwrap_or_default());
        });
        Ok(Process { run, child, stdin: lines })
    }

    fn receive(&self, name: &str, run: u64, line: &str) {
        let Ok(message) = serde_json::from_str::<Value>(line) else { return };
        let mut state = self.state.lock().unwrap();
        if let Some(id) = message["id"].as_u64()
            && (message.get("result").is_some() || message.get("error").is_some())
        {
            if let Some((_, reply)) = state.pending.remove(&id) {
                let _ = reply.send(message);
            }
            return;
        }
        let Some(plugin) = state.plugins.iter_mut().find(|plugin| plugin.name == name && plugin.process.as_ref().is_some_and(|p| p.run == run)) else { return };
        match message["method"].as_str() {
            Some(method @ ("ui.setStatus" | "ui.setAccountLabels" | "ui.showPanel")) => {
                if method == "ui.setStatus" {
                    plugin.status = message["params"]["text"].as_str().filter(|text| !text.is_empty()).map(String::from);
                } else if method == "ui.setAccountLabels" {
                    let labels = message["params"]["labels"].as_object().into_iter().flatten();
                    plugin.account_labels = labels.filter_map(|(id, text)| Some((id.clone(), text.as_str()?.to_string()))).collect();
                } else {
                    state.panel = Some((name.to_string(), message["params"].clone()));
                }
                let on_change = state.on_change.clone();
                drop(state);
                if let Some(on_change) = on_change {
                    on_change();
                }
            }
            _ if message.get("id").is_some() => {
                let error = json!({"jsonrpc": "2.0", "id": message["id"], "error": {"code": -32601, "message": "Method not found"}});
                let _ = plugin.process.as_ref().unwrap().stdin.send(error.to_string());
            }
            _ => {}
        }
    }

    fn exited(&self, name: &str, run: u64, last_error: Option<String>) {
        let mut state = self.state.lock().unwrap();
        let Some(plugin) = state.plugins.iter_mut().find(|plugin| plugin.name == name && plugin.process.as_ref().is_some_and(|p| p.run == run)) else { return };
        let mut process = plugin.process.take().unwrap();
        let code = process.child.wait().map(|status| status.to_string()).unwrap_or_else(|e| e.to_string());
        plugin.error = Some(match last_error {
            Some(line) => format!("Exited ({code}): {line}"),
            None => format!("Exited ({code})."),
        });
        plugin.status = None;
        plugin.account_labels.clear();
        state.pending.retain(|_, (owner, _)| owner != name);
        let on_change = state.on_change.clone();
        drop(state);
        if let Some(on_change) = on_change {
            on_change();
        }
    }
}

fn stop(plugin: &mut Plugin) {
    if let Some(mut process) = plugin.process.take() {
        let _ = process.child.kill();
        let _ = process.child.wait();
    }
    plugin.status = None;
    plugin.account_labels.clear();
}

/// Copies a bundled plugin by name, or a plugin directory by path, into ~/.agents/plugins; returns its name.
/// Files already there and not in the source stay, since a plugin may keep state next to its code.
pub fn install(home: &Path, source: &str) -> Result<String, String> {
    let plugins = home.join(".agents/plugins");
    if source.contains('/') {
        let from = Path::new(source).canonicalize().map_err(|e| format!("Cannot read {source}: {e}"))?;
        let name = from.file_name().unwrap_or_default().to_string_lossy().to_string();
        check_name(&name)?;
        read_manifest(&from)?;
        copy_dir(&from, &plugins.join(&name))?;
        return Ok(name);
    }
    check_name(source)?;
    let (_, files) = BUNDLED.iter().find(|(name, _)| *name == source).ok_or_else(|| format!("No bundled plugin named {source}. Pass a directory path to install your own."))?;
    let target = plugins.join(source);
    std::fs::create_dir_all(&target).map_err(|e| format!("Cannot create {}: {e}", target.display()))?;
    for (file, text) in *files {
        let path = target.join(file);
        std::fs::write(&path, text).map_err(|e| format!("Cannot write {}: {e}", path.display()))?;
    }
    Ok(source.to_string())
}

/// Deletes ~/.agents/plugins/<name> with everything in it.
pub fn remove(home: &Path, name: &str) -> Result<(), String> {
    check_name(name)?;
    let dir = home.join(".agents/plugins").join(name);
    if !dir.is_dir() {
        return Err(format!("{name} is not installed."));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| format!("Cannot remove {}: {e}", dir.display()))
}

/// Names double as directory names, so nothing that could leave ~/.agents/plugins gets through.
fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err(format!("{name} is not a plugin name: use lowercase letters, digits and dashes."));
    }
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("Cannot create {}: {e}", to.display()))?;
    for entry in std::fs::read_dir(from).map_err(|e| format!("Cannot read {}: {e}", from.display()))? {
        let path = entry.map_err(|e| format!("Cannot read {}: {e}", from.display()))?.path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_dir(&path, &target)?;
        } else {
            std::fs::copy(&path, &target).map_err(|e| format!("Cannot copy {}: {e}", path.display()))?;
        }
    }
    Ok(())
}

fn installed(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| check_name(name).is_ok())
        .collect();
    names.sort();
    names
}

struct Manifest {
    version: String,
    description: String,
    author: Option<String>,
    command: Vec<String>,
    /// Slash commands as (name, description), offered while the plugin runs.
    commands: Vec<(String, String)>,
    /// Tool schemas (`name`, `description`, `input_schema`), offered to the model while the plugin runs.
    tools: Vec<Value>,
    /// Setting declarations: `key`, `description`, `default` (a boolean or a string).
    settings: Vec<Value>,
}

fn read_manifest(dir: &Path) -> Result<Manifest, String> {
    let path = dir.join("plugin.json");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    parse_manifest(&text).map_err(|e| format!("{}: {e}", path.display()))
}

fn parse_manifest(text: &str) -> Result<Manifest, String> {
    let json: Value = serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
    let command: Vec<String> = json["command"].as_array().into_iter().flatten().filter_map(|part| Some(part.as_str()?.to_string())).collect();
    if command.is_empty() {
        return Err("`command` needs at least the program to run.".into());
    }
    let mut commands = Vec::new();
    for entry in json["commands"].as_array().into_iter().flatten() {
        let name = entry["name"].as_str().ok_or("every entry in `commands` needs a `name`.")?;
        commands.push((name.to_string(), entry["description"].as_str().unwrap_or_default().to_string()));
    }
    let mut tools = Vec::new();
    for entry in json["tools"].as_array().into_iter().flatten() {
        // The API takes 64 characters of [a-zA-Z0-9_-] per name, and `__` separates the plugin.
        let name = entry["name"].as_str().filter(|name| !name.is_empty() && !name.contains("__") && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'));
        let name = name.ok_or("every entry in `tools` needs a `name` of letters, digits, `_` and `-`, without `__`.")?;
        let schema = if entry["input_schema"].is_object() { entry["input_schema"].clone() } else { json!({"type": "object", "properties": {}}) };
        tools.push(json!({"name": name, "description": entry["description"].as_str().unwrap_or_default(), "input_schema": schema}));
    }
    let settings: Vec<Value> = json["settings"].as_array().cloned().unwrap_or_default();
    if settings.iter().any(|entry| !entry["key"].is_string() || !(entry["default"].is_boolean() || entry["default"].is_string())) {
        return Err("every entry in `settings` needs a `key` and a boolean or string `default`.".into());
    }
    let text = |key: &str| json[key].as_str().unwrap_or_default().to_string();
    let author = json["author"].as_str().filter(|author| !author.is_empty()).map(String::from);
    Ok(Manifest { version: text("version"), description: text("description"), author, command, commands, tools, settings })
}

/// The values a plugin sees: each declared default, replaced by the user's value of the same type.
fn settings_for(declared: &[Value], values: &Value) -> Value {
    Value::Object(declared.iter().map(|entry| (entry["key"].as_str().unwrap_or_default().to_string(), setting(entry, values))).collect())
}

/// One declared setting's value: the user's value from `values` when it has the default's type, else the default.
pub fn setting(entry: &Value, values: &Value) -> Value {
    let value = &values[entry["key"].as_str().unwrap_or_default()];
    let default = &entry["default"];
    if value.is_boolean() == default.is_boolean() && value.is_string() == default.is_string() && !value.is_null() { value.clone() } else { default.clone() }
}

/// A JSON-RPC message to `plugin`, with its settings added to `params.context` when there is one.
fn message(plugin: &Plugin, id: Option<u64>, method: &str, params: &Value) -> String {
    let mut params = params.clone();
    if params["context"].is_object() {
        params["context"]["settings"] = plugin.settings.clone();
    }
    let mut message = json!({"jsonrpc": "2.0", "method": method, "params": params});
    if let Some(id) = id {
        message["id"] = json!(id);
    }
    message.to_string()
}

fn view(name: String, manifest: Option<Manifest>, installed: bool, running: bool, error: Option<String>) -> View {
    let manifest = manifest.unwrap_or(Manifest { version: String::new(), description: String::new(), author: None, command: Vec::new(), commands: Vec::new(), tools: Vec::new(), settings: Vec::new() });
    View { name, version: manifest.version, description: manifest.description, author: manifest.author, settings: manifest.settings, installed, running, error }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// A home with one plugin per (name, script); each script runs as `sh plugin.sh`.
    fn home_with(test: &str, plugins: &[(&str, &str)]) -> PathBuf {
        let home = std::env::temp_dir().join(format!("agent-plugins-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        for (name, script) in plugins {
            let dir = home.join(".agents/plugins").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("plugin.json"), r#"{"version": "1.0.0", "command": ["sh", "plugin.sh"]}"#).unwrap();
            std::fs::write(dir.join("plugin.sh"), script).unwrap();
        }
        home
    }

    /// Answers every `input` request with `result`.
    fn answering(result: &str) -> String {
        format!(
            r#"while read -r line; do
  case "$line" in *'"method":"input"'*)
    id=$(echo "$line" | sed 's/.*"id":\([0-9]*\).*/\1/')
    echo '{{"jsonrpc":"2.0","id":'"$id"',"result":{result}}}';;
  esac
done"#
        )
    }

    fn eventually(check: impl Fn() -> bool) {
        let started = Instant::now();
        while !check() {
            assert!(started.elapsed() < Duration::from_secs(5), "timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn first_block_refuses_the_prompt_after_earlier_rewrites() {
        let home = home_with(
            "block",
            &[
                ("a-rewrite", &answering(r#"{"action":"transform","text":"rewritten"}"#)),
                ("b-block", &answering(r#"{"action":"block","reason":"over budget"}"#)),
            ],
        );
        let plugins = Plugins::new(&home);
        plugins.sync(&[], &json!({}));
        assert_eq!(plugins.input("hi", &json!({})), Err("over budget".into()));
        plugins.sync(&["b-block".into()], &json!({}));
        assert_eq!(plugins.input("hi", &json!({})), Ok("rewritten".into()));
    }

    #[test]
    fn a_crashed_plugin_lets_prompts_through_and_reports_why() {
        let home = home_with("crash", &[("broken", "echo 'no bun here' >&2; exit 3")]);
        let plugins = Plugins::new(&home);
        plugins.sync(&[], &json!({}));
        eventually(|| !plugins.list()[0].running);
        assert_eq!(plugins.list()[0].error.as_deref(), Some("Exited (exit status: 3): no bun here"));
        assert_eq!(plugins.input("hi", &json!({})), Ok("hi".into()));
    }

    #[test]
    fn status_and_account_labels_reach_the_ui() {
        let script = r#"echo '{"jsonrpc":"2.0","method":"ui.setStatus","params":{"text":"5h 9%"}}'; echo '{"jsonrpc":"2.0","method":"ui.setAccountLabels","params":{"labels":{"uuid-1":"uf_tVdC…"}}}'; cat >/dev/null"#;
        let home = home_with("status", &[("fleet", script)]);
        let plugins = Plugins::new(&home);
        let woken = Arc::new(Mutex::new(false));
        let flag = woken.clone();
        plugins.on_change(move || *flag.lock().unwrap() = true);
        plugins.sync(&[], &json!({}));
        eventually(|| *woken.lock().unwrap() && !plugins.account_labels().is_empty());
        assert_eq!(plugins.statuses(), ["5h 9%"]);
        assert_eq!(plugins.account_labels(), [("fleet".to_string(), HashMap::from([("uuid-1".to_string(), "uf_tVdC…".to_string())]))]);
        plugins.sync(&["fleet".into()], &json!({}));
        assert!(plugins.statuses().is_empty());
        assert!(plugins.account_labels().is_empty());
    }

    #[test]
    fn a_running_plugin_offers_and_answers_its_slash_commands() {
        let script = r#"while read -r line; do
  case "$line" in *'"method":"command"'*)
    id=$(echo "$line" | sed 's/.*"id":\([0-9]*\).*/\1/')
    echo '{"jsonrpc":"2.0","id":'"$id"',"result":{"text":"Saved."}}';;
  esac
done"#;
        let home = home_with("command", &[("fleet", script)]);
        let manifest = r#"{"command": ["sh", "plugin.sh"], "commands": [{"name": "fleet", "description": "Set the token"}]}"#;
        std::fs::write(home.join(".agents/plugins/fleet/plugin.json"), manifest).unwrap();
        let plugins = Plugins::new(&home);
        plugins.sync(&[], &json!({}));
        assert_eq!(plugins.commands(), [("fleet".to_string(), "Set the token".to_string())]);
        assert_eq!(plugins.command("fleet", "uf_x", &json!({})), Some(Ok("Saved.".into())));
        assert_eq!(plugins.command("other", "", &json!({})), None);
        plugins.sync(&["fleet".into()], &json!({}));
        assert!(plugins.commands().is_empty());
    }

    #[test]
    fn tools_settings_and_panels_reach_the_agent() {
        // Echoes the setting it was sent back as the tool's answer, and opens a panel on start.
        let script = r#"echo '{"jsonrpc":"2.0","method":"ui.showPanel","params":{"title":"Fleet","rows":[{"id":"a","left":"one"}]}}'
while read -r line; do
  case "$line" in *'"method":"tool"'*)
    id=$(echo "$line" | sed 's/.*"id":\([0-9]*\).*/\1/')
    greeting=$(echo "$line" | sed 's/.*"greeting":"\([^"]*\)".*/\1/')
    echo '{"jsonrpc":"2.0","id":'"$id"',"result":{"text":"'"$greeting"'"}}';;
  esac
done"#;
        let home = home_with("tools", &[("fleet", script)]);
        let manifest = r#"{"command": ["sh", "plugin.sh"], "author": "octocat",
            "tools": [{"name": "hello", "description": "Greets"}],
            "settings": [{"key": "greeting", "default": "hi"}, {"key": "loud", "default": false}]}"#;
        std::fs::write(home.join(".agents/plugins/fleet/plugin.json"), manifest).unwrap();
        let plugins = Plugins::new(&home);
        plugins.sync(&[], &json!({"fleet": {"greeting": "hey", "loud": "yes"}}));
        assert_eq!(plugins.tools()[0]["name"], "fleet__hello");
        assert_eq!(plugins.list()[0].author.as_deref(), Some("octocat"));
        let abort = AtomicBool::new(false);
        assert_eq!(plugins.tool("fleet__hello", &json!({}), &json!({}), &abort), Some(("hey".into(), false)));
        assert_eq!(plugins.tool("fleet__nope", &json!({}), &json!({}), &abort), None);
        // A value of the wrong type falls back to the default.
        assert_eq!(plugins.state.lock().unwrap().plugins[0].settings, json!({"greeting": "hey", "loud": false}));
        eventually(|| plugins.state.lock().unwrap().panel.is_some());
        assert_eq!(plugins.take_panel().unwrap().1["rows"][0]["id"], "a");
        assert!(plugins.take_panel().is_none());
        assert!(parse_manifest(r#"{"command": ["x"], "tools": [{"name": "a__b"}]}"#).is_err());
    }

    #[test]
    fn install_and_remove_stay_inside_the_plugins_directory() {
        let home = home_with("install", &[]);
        assert_eq!(install(&home, "usagefleet"), Ok("usagefleet".into()));
        assert!(home.join(".agents/plugins/usagefleet/plugin.json").is_file());
        assert!(install(&home, "nope").unwrap_err().starts_with("No bundled plugin named nope."));
        assert!(remove(&home, "..").is_err());
        assert_eq!(remove(&home, "usagefleet"), Ok(()));
        assert!(!home.join(".agents/plugins/usagefleet").exists());
    }
}
