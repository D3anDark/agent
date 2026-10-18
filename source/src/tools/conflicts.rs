use std::path::Path;

/// Added after a git command while the repo in `cwd` has unmerged paths, so a merge, rebase, cherry-pick, pull
/// or stash pop that stopped on a conflict sends the agent to the resolving skill.
pub fn hint(command: &str, cwd: &Path) -> Option<String> {
    if !runs_git(command) {
        return None;
    }
    let unmerged = git(cwd, &["diff", "--name-only", "--diff-filter=U"]);
    let files: Vec<&str> = unmerged.lines().collect();
    if files.is_empty() {
        return None;
    }
    Some(format!("Unmerged paths: {}. Read the resolving-merge-conflicts skill and follow it to the end.", files.join(", ")))
}

/// Why a `git commit` must not run: changes since HEAD still add conflict markers.
pub fn commit_blocked(command: &str, cwd: &Path) -> Option<String> {
    if !runs_git(command) || !command.split_whitespace().any(|word| word == "commit") {
        return None;
    }
    let check = git(cwd, &["diff", "--check", "HEAD"]);
    let markers: Vec<&str> = check.lines().filter(|line| line.ends_with("leftover conflict marker")).collect();
    if markers.is_empty() {
        return None;
    }
    Some(format!("Commit blocked, conflict markers left:\n{}\nResolve them with the resolving-merge-conflicts skill first.", markers.join("\n")))
}

fn runs_git(command: &str) -> bool {
    command.split_whitespace().any(|word| word == "git")
}

/// Stdout of git in `dir`; empty when git fails, as it does outside a repo.
fn git(dir: &Path, args: &[&str]) -> String {
    match crate::process::command("git").args(args).current_dir(dir).output() {
        Ok(output) => String::from_utf8_lossy(&output.stdout).into_owned(),
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn git(dir: &Path, args: &[&str]) -> bool {
        crate::process::command("git").arg("-C").arg(dir).args(args).output().unwrap().status.success()
    }

    fn commit(dir: &Path, text: &str) {
        std::fs::write(dir.join("f"), text).unwrap();
        assert!(git(dir, &["add", "f"]) && git(dir, &["commit", "-qm", text]));
    }

    /// A repo stopped mid-merge with `f` conflicted between `main` and `side`.
    fn conflicted_repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agent-conflicts-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(git(&dir, &["init", "-q", "-b", "main"]) && git(&dir, &["config", "user.email", "a@b.c"]) && git(&dir, &["config", "user.name", "a"]));
        commit(&dir, "base");
        assert!(git(&dir, &["checkout", "-q", "-b", "side"]));
        commit(&dir, "side");
        assert!(git(&dir, &["checkout", "-q", "main"]));
        commit(&dir, "main");
        assert!(!git(&dir, &["merge", "side"]));
        dir
    }

    #[test]
    fn git_commands_point_at_the_skill_while_paths_are_unmerged() {
        let dir = conflicted_repo("hint");
        assert!(hint("git merge side", &dir).unwrap().contains(": f."));
        assert!(hint("git status", &dir).unwrap().contains("resolving-merge-conflicts"));
        assert_eq!(hint("cargo test", &dir), None);
        assert!(git(&dir, &["merge", "--abort"]));
        assert_eq!(hint("git status", &dir), None);
    }

    #[test]
    fn commits_with_conflict_markers_are_blocked() {
        let dir = conflicted_repo("commit");
        assert!(commit_blocked("git commit -am merge", &dir).unwrap().contains("f:1: leftover conflict marker"));
        assert_eq!(commit_blocked("git status", &dir), None);
        std::fs::write(dir.join("f"), "main and side").unwrap();
        assert_eq!(commit_blocked("git commit -am merge", &dir), None);
    }
}
