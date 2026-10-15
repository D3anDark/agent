use serde_json::{Value, json};
use std::path::Path;

use crate::api::Usage;

/// /usage tabs that sum session records, in this order.
pub const SCOPES: [&str; 4] = ["Session", "24 hours", "7 days", "All"];

const DAY_MS: u64 = 24 * 60 * 60 * 1000;

/// The session record for one model request; `totals` sums them.
pub fn entry(model: &str, usage: Usage, timestamp: u64) -> Value {
    json!({"type": "usage", "model": model, "timestamp": timestamp, "input": usage.input, "output": usage.output, "cache_read": usage.cache_read, "cache_write": usage.cache_write})
}

#[derive(Default, Clone, PartialEq, Debug)]
pub struct ModelTotal {
    pub model: String,
    pub requests: u64,
    pub usage: Usage,
}

/// Sums the `usage` records of every session in `dir` for each of `SCOPES`, sorted by model.
pub fn totals(dir: &Path, session_id: &str, now_ms: u64) -> [Vec<ModelTotal>; 4] {
    let mut totals: [Vec<ModelTotal>; 4] = Default::default();
    let Ok(files) = std::fs::read_dir(dir) else { return totals };
    for path in files.flatten().map(|file| file.path()) {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let current = path.file_stem().is_some_and(|stem| stem == session_id);
        // Skips the JSON parse of the far more common message lines; serde_json writes keys without spaces.
        for line in text.lines().filter(|line| line.contains(r#""type":"usage""#)) {
            let Ok(entry) = serde_json::from_str::<Value>(line) else { continue };
            if entry["type"] != "usage" {
                continue;
            }
            let number = |key: &str| entry[key].as_u64().unwrap_or(0);
            let usage = Usage { input: number("input"), output: number("output"), cache_read: number("cache_read"), cache_write: number("cache_write") };
            let age = now_ms.saturating_sub(number("timestamp"));
            let model = entry["model"].as_str().unwrap_or("unknown");
            for (scope, inside) in totals.iter_mut().zip([current, age < DAY_MS, age < 7 * DAY_MS, true]) {
                if inside {
                    add(scope, model, usage);
                }
            }
        }
    }
    for scope in &mut totals {
        scope.sort_by(|a, b| a.model.cmp(&b.model));
    }
    totals
}

fn add(scope: &mut Vec<ModelTotal>, model: &str, usage: Usage) {
    let index = match scope.iter().position(|total| total.model == model) {
        Some(index) => index,
        None => {
            scope.push(ModelTotal { model: model.to_string(), ..Default::default() });
            scope.len() - 1
        }
    };
    scope[index].requests += 1;
    scope[index].usage.add(usage);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_usage_records_per_model_for_each_scope() {
        let dir = std::env::temp_dir().join(format!("agent-usage-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let now = 100 * DAY_MS;
        let used = |input| Usage { input, output: 1, cache_read: 2, cache_write: 3 };
        let current = [
            json!({"type": "message", "message": {"role": "user", "content": "\"type\":\"usage\""}}),
            entry("opus", used(10), now - 1000),
            entry("opus", used(20), now - 2 * DAY_MS),
        ];
        let other = [entry("haiku", used(5), now - 1000), entry("opus", used(40), now - 8 * DAY_MS)];
        for (name, lines) in [("cur", &current[..]), ("old", &other[..])] {
            std::fs::write(dir.join(format!("{name}.jsonl")), lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
        }

        let [session, day, week, all] = totals(&dir, "cur", now);
        std::fs::remove_dir_all(&dir).unwrap();

        let total = |model: &str, requests, input| ModelTotal { model: model.into(), requests, usage: Usage { input, output: requests, cache_read: 2 * requests, cache_write: 3 * requests } };
        assert_eq!(session, [total("opus", 2, 30)]);
        assert_eq!(day, [total("haiku", 1, 5), total("opus", 1, 10)]);
        assert_eq!(week, [total("haiku", 1, 5), total("opus", 2, 30)]);
        assert_eq!(all, [total("haiku", 1, 5), total("opus", 3, 70)]);
    }
}
