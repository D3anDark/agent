use std::path::{Path, PathBuf};

/// One checkout from `git worktree list --porcelain`.
#[derive(Clone, Debug, PartialEq)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: String,
}

/// Every checkout, main one first, and the one `cwd` is in.
pub fn list(cwd: &Path) -> Result<(Vec<Worktree>, PathBuf), String> {
    let porcelain = git(cwd, &["worktree", "list", "--porcelain"])?;
    // Both come from git as real paths, so they compare even when cwd went through a symlink.
    let current = PathBuf::from(git(cwd, &["rev-parse", "--show-toplevel"])?.trim());
    Ok((parse(&porcelain), current))
}

/// The worktree `name` names (branch or directory), created as `<main>-<name>` when none does.
pub fn open(cwd: &Path, worktrees: &[Worktree], name: &str) -> Result<PathBuf, String> {
    let existing = worktrees.iter().find(|w| w.branch == name || w.path.file_name().is_some_and(|n| n == name));
    if let Some(worktree) = existing {
        return Ok(worktree.path.clone());
    }
    let path = format!("{}-{}", worktrees[0].path.display(), name.replace('/', "-"));
    let branch_exists = git(cwd, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{name}")]).is_ok();
    let args: Vec<&str> = if branch_exists { vec!["worktree", "add", &path, name] } else { vec!["worktree", "add", "-b", name, &path] };
    git(cwd, &args)?;
    Ok(PathBuf::from(path))
}

pub fn remove(cwd: &Path, path: &Path) -> Result<(), String> {
    git(cwd, &["worktree", "remove", "--force", &path.display().to_string()]).map(|_| ())
}

fn parse(porcelain: &str) -> Vec<Worktree> {
    porcelain
        .trim()
        .split("\n\n")
        .map(|block| {
            let path = block.lines().next().unwrap_or_default().trim_start_matches("worktree ");
            let branch = block.lines().find_map(|l| l.strip_prefix("branch refs/heads/")).unwrap_or("detached");
            Worktree { path: PathBuf::from(path), branch: branch.to_string() }
        })
        .collect()
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = crate::process::command("git").args(args).current_dir(cwd).output().map_err(|e| format!("Cannot run git: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_branches_and_detached_checkouts() {
        let porcelain = "worktree /r/app\nHEAD abc\nbranch refs/heads/main\n\nworktree /r/app-feat\nHEAD def\nbranch refs/heads/feat/x\n\nworktree /r/app-old\nHEAD 123\ndetached\n";
        let worktree = |path: &str, branch: &str| Worktree { path: PathBuf::from(path), branch: branch.into() };
        assert_eq!(parse(porcelain), [worktree("/r/app", "main"), worktree("/r/app-feat", "feat/x"), worktree("/r/app-old", "detached")]);
    }
}
