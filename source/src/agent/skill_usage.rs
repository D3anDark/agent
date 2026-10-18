use serde_json::{Value, json};
use std::io::Write;
use std::path::Path;

/// Appends one use of `skill` to `<agent_dir>/skill-usage.jsonl`; `by` is "agent" (read its SKILL.md) or "user" (typed /skill:).
pub fn record(agent_dir: &Path, skill: &str, by: &str, cwd: &Path) -> Result<(), String> {
    let line = json!({"at": crate::process::now_ms(), "skill": skill, "by": by, "cwd": cwd});
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(agent_dir.join("skill-usage.jsonl")).map_err(|e| e.to_string())?;
    writeln!(file, "{line}").map_err(|e| e.to_string())
}

/// Uses per skill, fewest first so unused skills lead; `skills` start at 0, skills only in the log are kept.
pub fn counts(log: &str, skills: &[&str]) -> Vec<(String, usize)> {
    let mut counts: Vec<(String, usize)> = skills.iter().map(|skill| (skill.to_string(), 0)).collect();
    for line in log.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else { continue };
        let Some(skill) = entry["skill"].as_str() else { continue };
        match counts.iter_mut().find(|(name, _)| name == skill) {
            Some((_, count)) => *count += 1,
            None => counts.push((skill.to_string(), 1)),
        }
    }
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
        for (skill, by) in [("tdd", "agent"), ("ui", "user"), ("tdd", "user"), ("gone", "agent")] {
            record(&dir, skill, by, Path::new("/w")).unwrap();
        }
        let log = std::fs::read_to_string(dir.join("skill-usage.jsonl")).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let counts = counts(&log, &["tdd", "ui", "unused"]);
        assert_eq!(counts, [("unused".into(), 0), ("gone".into(), 1), ("ui".into(), 1), ("tdd".into(), 2)]);
    }
}
