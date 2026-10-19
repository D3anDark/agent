use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

/// Who used a skill: the agent read its SKILL.md, or the user typed /skill:.
#[derive(Clone, Copy)]
pub enum By {
    Agent,
    User,
}

/// Appends one use of `skill` to `<agent_dir>/skill-usage.jsonl`.
pub fn record(agent_dir: &Path, skill: &str, by: By, cwd: &Path) -> Result<(), String> {
    let by = match by {
        By::Agent => "agent",
        By::User => "user",
    };
    let line = json!({"at": crate::process::now_ms(), "skill": skill, "by": by, "cwd": cwd});
    let path = agent_dir.join("skill-usage.jsonl");
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&path).map_err(|e| format!("Cannot open {}: {e}", path.display()))?;
    // One write, so two agents recording at once cannot interleave mid-line.
    file.write_all(format!("{line}\n").as_bytes()).map_err(|e| format!("Cannot write {}: {e}", path.display()))
}

/// Uses per skill, fewest first so unused skills lead; `skills` start at 0, skills only in the log are kept.
pub fn counts(log: &str, skills: &[&str]) -> Vec<(String, usize)> {
    let mut by_skill: HashMap<String, usize> = skills.iter().map(|skill| (skill.to_string(), 0)).collect();
    for line in log.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else { continue };
        let Some(skill) = entry["skill"].as_str() else { continue };
        *by_skill.entry(skill.to_string()).or_default() += 1;
    }
    let mut counts: Vec<(String, usize)> = by_skill.into_iter().collect();
    counts.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_put_the_least_used_skills_first() {
        let dir = std::env::temp_dir().join(format!("agent-skill-usage-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (skill, by) in [("tdd", By::Agent), ("ui", By::User), ("tdd", By::User), ("gone", By::Agent)] {
            record(&dir, skill, by, Path::new("/w")).unwrap();
        }
        let log = std::fs::read_to_string(dir.join("skill-usage.jsonl")).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let counts = counts(&log, &["tdd", "ui", "unused"]);
        assert_eq!(counts, [("unused".into(), 0), ("gone".into(), 1), ("ui".into(), 1), ("tdd".into(), 2)]);
    }
}
