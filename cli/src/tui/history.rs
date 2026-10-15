use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

/// The file outgrows this many prompts, then drops to `KEEP`; trimming on every append would rewrite it each prompt.
const LIMIT: usize = 1000;
const KEEP: usize = 500;

/// Every prompt in `agent_dir/history.jsonl`, oldest first; a missing file is an empty history.
pub fn load(agent_dir: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(agent_dir.join("history.jsonl")) else { return Vec::new() };
    text.lines().filter_map(|line| serde_json::from_str(line).ok()).collect()
}

/// Appends `prompt` unless it repeats the newest entry, trimming the file past `LIMIT` prompts.
pub fn append(agent_dir: &Path, prompt: &str) -> Result<(), String> {
    let mut prompts = load(agent_dir);
    if prompt.trim().is_empty() || prompts.last().is_some_and(|last| last == prompt) {
        return Ok(());
    }
    prompts.push(prompt.to_string());
    let path = agent_dir.join("history.jsonl");
    let line = |prompt: &String| serde_json::Value::from(prompt.as_str()).to_string() + "\n";
    if prompts.len() > LIMIT {
        let kept: String = prompts[prompts.len() - KEEP..].iter().map(line).collect();
        return std::fs::write(&path, kept).map_err(|e| format!("Cannot write {}: {e}", path.display()));
    }
    let mut file = OpenOptions::new().create(true).append(true).open(&path).map_err(|e| format!("Cannot open {}: {e}", path.display()))?;
    file.write_all(line(&prompts[prompts.len() - 1]).as_bytes()).map_err(|e| format!("Cannot write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_a_repeat_of_the_newest_prompt_and_trims_past_the_limit() {
        let dir = std::env::temp_dir().join(format!("agent-history-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        append(&dir, "fix the\nbug").unwrap();
        append(&dir, "fix the\nbug").unwrap();
        append(&dir, "  ").unwrap();
        append(&dir, "ship it").unwrap();
        append(&dir, "fix the\nbug").unwrap();
        assert_eq!(load(&dir), ["fix the\nbug", "ship it", "fix the\nbug"]);

        for n in load(&dir).len()..LIMIT {
            append(&dir, &n.to_string()).unwrap();
        }
        assert_eq!(load(&dir).len(), LIMIT);
        append(&dir, "one more").unwrap();
        let prompts = load(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(prompts.len(), KEEP);
        assert_eq!(prompts.last().unwrap(), "one more");
    }
}
