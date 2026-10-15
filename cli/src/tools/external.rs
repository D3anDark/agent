use serde_json::Value;
use std::path::{Path, PathBuf};

use super::hashline::{resolve, str_arg};

const DEFAULT_MAX_RESULTS: u64 = 50;
const MAX_RESULTS: u64 = 1000;

/// The canonical directory `add_directory` names, or why it cannot be added.
pub fn directory(cwd: &Path, input: &Value) -> Result<PathBuf, String> {
    let path = str_arg(input, &["path"])?;
    let full = resolve(cwd, path).canonicalize().map_err(|e| format!("Cannot add {path}: {e}"))?;
    if !full.is_dir() {
        return Err(format!("{} is not a directory.", full.display()));
    }
    Ok(full)
}

/// `fd` over every added directory in order, grouped per directory, stopping at `maxResults` files in total.
pub fn search(dirs: &[PathBuf], input: &Value) -> Result<String, String> {
    if dirs.is_empty() {
        return Err("No external directories added. Use add_directory first.".into());
    }
    let pattern = str_arg(input, &["pattern"])?;
    if pattern.trim().is_empty() {
        return Err("File pattern must not be blank.".into());
    }
    let limit = input["maxResults"].as_u64().unwrap_or(DEFAULT_MAX_RESULTS).clamp(1, MAX_RESULTS) as usize;
    let mut remaining = limit;
    let mut groups = Vec::new();
    let mut truncated = false;
    for dir in dirs {
        if remaining == 0 {
            truncated = true;
            break;
        }
        // One extra result tells a cut search from one that found exactly `remaining` files.
        let found = super::search("fd", &fd_args(dir, pattern, remaining + 1), dir)?;
        let mut files: Vec<&str> = found.lines().collect();
        if files.len() > remaining {
            files.truncate(remaining);
            truncated = true;
        }
        if files.is_empty() {
            continue;
        }
        remaining -= files.len();
        groups.push(format!("{}:\n{}", dir.display(), files.join("\n")));
    }
    if groups.is_empty() {
        return Ok(format!("No files matching \"{pattern}\" found in {} added directories.", dirs.len()));
    }
    if truncated {
        groups.push(format!("[Truncated at {limit} files; later directories or files may hold more matches. Raise maxResults or narrow the pattern.]"));
    }
    Ok(groups.join("\n\n"))
}

/// A pattern with `/` is a glob on the path below `dir`, otherwise on the file name, as in pi-add-dir.
// shortcut: glob characters in `dir` itself break the full-path glob, escape them if a real directory needs it.
fn fd_args(dir: &Path, pattern: &str, max_results: usize) -> Vec<String> {
    let mut args: Vec<String> = ["--color=never", "--absolute-path", "--glob"].map(String::from).to_vec();
    let pattern = if pattern.contains('/') {
        args.push("--full-path".into());
        format!("{}/{}", dir.display(), pattern.trim_start_matches('/'))
    } else {
        pattern.to_string()
    };
    args.extend(["--max-results".into(), max_results.to_string(), "--".into(), pattern, ".".into()]);
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tree(name: &str, files: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agent-external-{name}-{}", std::process::id()));
        for file in files {
            let path = dir.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        dir.canonicalize().unwrap()
    }

    #[test]
    fn search_stops_at_max_results_across_directories() {
        let a = tree("a", &["one.md", "two.md"]);
        let b = tree("b", &["docs/three.md"]);
        let dirs = [a.clone(), b.clone()];
        let cut = search(&dirs, &json!({"pattern": "*.md", "maxResults": 2})).unwrap();
        let all = search(&dirs, &json!({"pattern": "*.md", "maxResults": 3})).unwrap();
        let nested = search(&dirs, &json!({"pattern": "docs/*.md"})).unwrap();
        let none = search(&dirs, &json!({"pattern": "*.rs"})).unwrap();
        std::fs::remove_dir_all(&a).unwrap();
        std::fs::remove_dir_all(&b).unwrap();
        assert_eq!(cut.lines().filter(|line| line.ends_with(".md")).count(), 2, "{cut}");
        assert!(cut.contains("[Truncated at 2 files") && !cut.contains("three.md"), "{cut}");
        assert!(all.contains(&format!("{}:\n", b.display())) && all.ends_with("docs/three.md") && !all.contains("[Truncated"), "{all}");
        assert_eq!(nested, format!("{}:\n{}", b.display(), b.join("docs/three.md").display()));
        assert!(none.starts_with("No files matching \"*.rs\""), "{none}");
    }

    #[test]
    fn a_missing_path_or_a_file_cannot_be_added() {
        let dir = tree("add", &["file.txt"]);
        let missing = directory(&dir, &json!({"path": "nope"}));
        let file = directory(&dir, &json!({"path": "file.txt"}));
        let added = directory(Path::new("/"), &json!({"path": dir.to_str().unwrap()}));
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(missing.unwrap_err().starts_with("Cannot add nope:"));
        assert!(file.unwrap_err().ends_with("file.txt is not a directory."));
        assert_eq!(added.unwrap(), dir);
    }
}
