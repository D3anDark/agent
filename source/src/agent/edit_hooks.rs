use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::tools::hashline::{self, Anchors};

const CONFIG: &str = ".agents/edit-hooks.json";
const MAX_FIX_ROUNDS: usize = 3;
const MAX_OUTPUT_CHARS: usize = 4000;

/// A command from `.agents/edit-hooks.json`, run in the directory that holds `.agents`.
#[derive(Debug)]
pub struct Hook {
    pub dir: PathBuf,
    pub command: String,
}

/// Files an edit tool call changes. Call it before the tool runs: a replace retires the anchors it names.
pub fn edited_files(tool: &str, input: &Value, cwd: &Path, anchors: &Anchors) -> Vec<PathBuf> {
    let anchor_keys: &[&str] = match tool {
        "write" | "undo_last_change" => return input["path"].as_str().map(|path| hashline::resolve(cwd, path)).into_iter().collect(),
        "replace" | "replace_match" => &["remove_from", "replace_from"],
        "insert" => &["anchor"],
        "move" => &["anchor", "source_from"],
        _ => return Vec::new(),
    };
    anchor_keys.iter().filter_map(|key| anchors.owner_of(input[*key].as_str()?).ok()).collect()
}

/// The onStop commands of the nearest config above each file; files without a config run nothing.
pub fn hooks(files: &[PathBuf]) -> Result<Vec<Hook>, String> {
    let mut files_by_dir: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for file in files {
        let Ok(file) = file.canonicalize() else { continue };
        let Some(dir) = config_dir(&file) else { continue };
        let relative = file.strip_prefix(&dir).expect("the config dir is an ancestor of the file").display().to_string();
        let group = files_by_dir.entry(dir).or_default();
        if !group.contains(&relative) {
            group.push(relative);
        }
    }
    let mut hooks = Vec::new();
    for (dir, files) in files_by_dir {
        let path = dir.join(CONFIG);
        let invalid = |e: String| format!("[edit-hooks] {} is invalid: {e}", path.display());
        let text = std::fs::read_to_string(&path).map_err(|e| invalid(e.to_string()))?;
        let config: Value = serde_json::from_str(&text).map_err(|e| invalid(e.to_string()))?;
        for command in commands(&config, &files).map_err(invalid)? {
            hooks.push(Hook { dir: dir.clone(), command });
        }
    }
    Ok(hooks)
}

/// Stops at the repository root, so a file never runs the hooks of a project around it.
/// Outside a repository there is no root, and the walk would reach configs in ~ or /tmp.
fn config_dir(file: &Path) -> Option<PathBuf> {
    let mut nearest = None;
    for dir in file.ancestors().skip(1) {
        if nearest.is_none() && dir.join(CONFIG).is_file() {
            nearest = Some(dir.to_path_buf());
        }
        if dir.join(".git").exists() {
            return nearest;
        }
    }
    None
}

/// Each file goes to the first glob it matches; a glob with a slash matches the path from the config dir.
// shortcut: serde_json sorts object keys, so "first" is key order, not file order; enable its preserve_order feature if a config needs file order.
fn commands(config: &Value, files: &[String]) -> Result<Vec<String>, String> {
    let on_stop = config["onStop"].as_object().ok_or("\"onStop\" must map globs to commands")?;
    let mut files_by_glob: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for file in files {
        let name = file.rsplit('/').next().unwrap_or(file);
        if let Some(glob) = on_stop.keys().find(|glob| matches(glob, if glob.contains('/') { file } else { name })) {
            files_by_glob.entry(glob).or_default().push(file);
        }
    }
    let mut commands = Vec::new();
    for (glob, value) in on_stop {
        let templates = match value {
            Value::String(command) => vec![command.as_str()],
            _ => value.as_array().and_then(|items| items.iter().map(Value::as_str).collect()).ok_or(format!("\"{glob}\" must be a command or a list of commands"))?,
        };
        let Some(matched) = files_by_glob.get(glob.as_str()) else { continue };
        for template in templates {
            if template.contains("{files}") {
                let quoted: Vec<String> = matched.iter().map(|file| shell_quote(file)).collect();
                commands.push(template.replace("{files}", &quoted.join(" ")));
            } else if template.contains("{file}") {
                commands.extend(matched.iter().map(|file| template.replace("{file}", &shell_quote(file))));
            } else {
                commands.push(template.to_string());
            }
        }
    }
    Ok(commands)
}

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// Glob over `/`-separated paths: `*` and `?` stay within a segment, `**` crosses them, `{a,b}` picks one.
fn matches(pattern: &str, path: &str) -> bool {
    if let Some(open) = pattern.find('{')
        && let Some(length) = pattern[open..].find('}')
    {
        let (head, tail) = (&pattern[..open], &pattern[open + length + 1..]);
        return pattern[open + 1..open + length].split(',').any(|choice| matches(&format!("{head}{choice}{tail}"), path));
    }
    let pattern: Vec<char> = pattern.chars().collect();
    let path: Vec<char> = path.chars().collect();
    matches_chars(&pattern, &path)
}

fn matches_chars(pattern: &[char], path: &[char]) -> bool {
    match pattern {
        [] => path.is_empty(),
        ['*', '*', '/', rest @ ..] => matches_chars(rest, path) || path.iter().enumerate().any(|(i, c)| *c == '/' && matches_chars(rest, &path[i + 1..])),
        ['*', '*', rest @ ..] => (0..=path.len()).any(|i| matches_chars(rest, &path[i..])),
        ['*', rest @ ..] => {
            let segment_end = path.iter().position(|c| *c == '/').unwrap_or(path.len());
            (0..=segment_end).any(|i| matches_chars(rest, &path[i..]))
        }
        ['?', rest @ ..] => path.first().is_some_and(|c| *c != '/') && matches_chars(rest, &path[1..]),
        [c, rest @ ..] => path.first() == Some(c) && matches_chars(rest, &path[1..]),
    }
}

pub fn failure(command: &str, output: &str) -> String {
    let kept: String = output.chars().take(MAX_OUTPUT_CHARS).collect();
    let cut = if kept.len() < output.len() { "\n[output cut]" } else { "" };
    format!("[edit-hooks] {command} failed:\n{kept}{cut}")
}

/// The message for the model after `round` failed hook runs, and whether the turn goes on so it can fix them.
pub fn report(failures: &[String], round: usize) -> (String, bool) {
    let failures = failures.join("\n\n");
    if round > MAX_FIX_ROUNDS {
        return (format!("Edit hooks still fail after {MAX_FIX_ROUNDS} fix rounds, stopped retrying.\n\n{failures}"), false);
    }
    (failures, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_match_names_and_paths() {
        let cases = [
            ("*.ts", "a.ts", true),
            ("*.ts", "a.tsx", false),
            ("*.ts", "src/a.ts", false),
            ("*.{ts,tsx}", "a.tsx", true),
            ("*.{ts,tsx}", "a.js", false),
            ("?.rs", "a.rs", true),
            ("?.rs", "ab.rs", false),
            ("src/**/*.rs", "src/a.rs", true),
            ("src/**/*.rs", "src/x/y/a.rs", true),
            ("src/**/*.rs", "lib/a.rs", false),
            ("src/*", "src/x/a.rs", false),
            ("**", "a/b/c", true),
        ];
        for (pattern, path, expected) in cases {
            assert_eq!(matches(pattern, path), expected, "{pattern} vs {path}");
        }
    }

    #[test]
    fn files_run_the_nearest_config_once_per_first_matching_glob() {
        let root = std::env::temp_dir().join(format!("agent-edit-hooks-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let write = |path: &str, text: &str| {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        std::fs::create_dir_all(root.join(".git")).unwrap();
        write(".agents/edit-hooks.json", r#"{"onStop": {"*.ts": ["fmt {files}", "check {file}", "tsc"], "*.md": "fmt {files}", "src/**": "never"}}"#);
        write("pkg/.agents/edit-hooks.json", r#"{"onStop": {"*": "lint {files}"}}"#);
        for file in ["a.ts", "it's.ts", "README.md", "pkg/b.rs", "notes.txt"] {
            write(file, "");
        }
        let files: Vec<PathBuf> = ["a.ts", "it's.ts", "README.md", "pkg/b.rs", "notes.txt", "a.ts", "deleted.ts"].iter().map(|f| root.join(f)).collect();
        let hooks = hooks(&files).unwrap();
        let commands: Vec<(&Path, &str)> = hooks.iter().map(|hook| (hook.dir.as_path(), hook.command.as_str())).collect();
        let pkg = root.join("pkg");
        assert_eq!(
            commands,
            [
                (root.as_path(), "fmt 'README.md'"),
                (root.as_path(), r"fmt 'a.ts' 'it'\''s.ts'"),
                (root.as_path(), "check 'a.ts'"),
                (root.as_path(), r"check 'it'\''s.ts'"),
                (root.as_path(), "tsc"),
                (pkg.as_path(), "lint 'b.rs'"),
            ]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn files_outside_a_repository_run_no_hooks() {
        let root = std::env::temp_dir().join(format!("agent-edit-hooks-norepo-{}", std::process::id()));
        std::fs::create_dir_all(root.join(".agents")).unwrap();
        std::fs::write(root.join(CONFIG), r#"{"onStop": {"*": "touch pwned"}}"#).unwrap();
        std::fs::write(root.join("a.txt"), "").unwrap();
        let hooks = hooks(&[root.join("a.txt")]).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        assert!(hooks.is_empty());
    }

    #[test]
    fn failures_go_back_to_the_model_for_three_rounds() {
        let failures = [failure("tsc", "error TS2322")];
        assert_eq!(report(&failures, 1), ("[edit-hooks] tsc failed:\nerror TS2322".to_string(), true));
        assert!(report(&failures, 3).1);
        let (message, retry) = report(&failures, 4);
        assert!(!retry);
        assert!(message.starts_with("Edit hooks still fail after 3 fix rounds"), "{message}");
    }
}
