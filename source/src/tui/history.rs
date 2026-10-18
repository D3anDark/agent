use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// The file outgrows this many prompts, then drops to `KEEP`; trimming on every append would rewrite it each prompt.
const LIMIT: usize = 1000;
const KEEP: usize = 500;

/// The /history panel tabs, in the order `Entry::in_scope` numbers them.
pub const SCOPES: [&str; 3] = ["Global", "Project", "Session"];

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub prompt: String,
    pub cwd: PathBuf,
    pub session: String,
}

impl Entry {
    /// Whether the entry belongs in `SCOPES[scope]` for a session `session` running in `cwd`.
    pub fn in_scope(&self, scope: usize, cwd: &Path, session: &str) -> bool {
        match scope {
            1 => self.cwd == cwd,
            2 => self.session == session,
            _ => true,
        }
    }
}

/// Every prompt in `agent_dir/history.jsonl`, oldest first; a missing file is an empty history.
pub fn load(agent_dir: &Path) -> Vec<Entry> {
    let Ok(text) = std::fs::read_to_string(agent_dir.join("history.jsonl")) else { return Vec::new() };
    text.lines().filter_map(parse).collect()
}

fn parse(line: &str) -> Option<Entry> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    // Lines written before cwd and session were saved are bare strings; they show only under Global.
    if let Some(prompt) = value.as_str() {
        return Some(Entry { prompt: prompt.to_string(), cwd: PathBuf::new(), session: String::new() });
    }
    let field = |key: &str| value[key].as_str().map(str::to_string);
    Some(Entry { prompt: field("prompt")?, cwd: field("cwd")?.into(), session: field("session")? })
}

/// Appends `entry` unless it repeats the newest one or its prompt is blank, trimming the file past `LIMIT` prompts.
pub fn append(agent_dir: &Path, entry: Entry) -> Result<(), String> {
    let mut entries = load(agent_dir);
    if entry.prompt.trim().is_empty() || entries.last() == Some(&entry) {
        return Ok(());
    }
    entries.push(entry);
    let path = agent_dir.join("history.jsonl");
    let line = |entry: &Entry| serde_json::json!({ "prompt": entry.prompt, "cwd": entry.cwd.to_string_lossy(), "session": entry.session }).to_string() + "\n";
    if entries.len() > LIMIT {
        let kept: String = entries[entries.len() - KEEP..].iter().map(line).collect();
        return std::fs::write(&path, kept).map_err(|e| format!("Cannot write {}: {e}", path.display()));
    }
    let mut file = OpenOptions::new().create(true).append(true).mode(0o600).open(&path).map_err(|e| format!("Cannot open {}: {e}", path.display()))?;
    file.write_all(line(&entries[entries.len() - 1]).as_bytes()).map_err(|e| format!("Cannot write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(prompt: &str) -> Entry {
        Entry { prompt: prompt.to_string(), cwd: "/repo".into(), session: "s1".into() }
    }

    fn prompts(dir: &Path) -> Vec<String> {
        load(dir).into_iter().map(|entry| entry.prompt).collect()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agent-history-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn skips_a_repeat_of_the_newest_prompt_and_trims_past_the_limit() {
        let dir = temp_dir("trim");
        append(&dir, entry("fix the\nbug")).unwrap();
        append(&dir, entry("fix the\nbug")).unwrap();
        append(&dir, entry("  ")).unwrap();
        append(&dir, entry("ship it")).unwrap();
        append(&dir, entry("fix the\nbug")).unwrap();
        assert_eq!(prompts(&dir), ["fix the\nbug", "ship it", "fix the\nbug"]);

        for n in load(&dir).len()..LIMIT {
            append(&dir, entry(&n.to_string())).unwrap();
        }
        assert_eq!(load(&dir).len(), LIMIT);
        append(&dir, entry("one more")).unwrap();
        let prompts = prompts(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(prompts.len(), KEEP);
        assert_eq!(prompts.last().unwrap(), "one more");
    }

    #[test]
    fn scopes_entries_by_project_and_session_and_keeps_bare_string_lines_global() {
        let dir = temp_dir("scope");
        std::fs::write(dir.join("history.jsonl"), "\"old prompt\"\n").unwrap();
        append(&dir, entry("here")).unwrap();
        append(&dir, Entry { prompt: "elsewhere".into(), cwd: "/other".into(), session: "s1".into() }).unwrap();
        append(&dir, Entry { prompt: "other session".into(), cwd: "/repo".into(), session: "s2".into() }).unwrap();
        let entries = load(&dir);
        std::fs::remove_dir_all(&dir).unwrap();

        let in_scope = |scope| entries.iter().filter(|e| e.in_scope(scope, Path::new("/repo"), "s1")).map(|e| e.prompt.as_str()).collect::<Vec<_>>();
        assert_eq!(in_scope(0), ["old prompt", "here", "elsewhere", "other session"]);
        assert_eq!(in_scope(1), ["here", "other session"]);
        assert_eq!(in_scope(2), ["here", "elsewhere"]);
    }
}
