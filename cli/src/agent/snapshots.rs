use std::path::{Path, PathBuf};
use std::process::Command;

/// Trees of one git work tree, kept in a private repo under ~/.agents/snapshots so a rewind can put files back.
/// Ignored files are not stored, like in the project's own repo.
pub struct Snapshots {
    git_dir: PathBuf,
    work_tree: PathBuf,
    index: PathBuf,
}

impl Snapshots {
    /// None outside a git work tree: snapshotting any directory (say ~) could copy gigabytes.
    pub fn open(home: &Path, cwd: &Path) -> Option<Snapshots> {
        let top = run(Command::new("git").arg("-C").arg(cwd).args(["rev-parse", "--show-toplevel"])).ok()?;
        let work_tree = PathBuf::from(top);
        let git_dir = home.join(".agents/snapshots").join(work_tree.to_string_lossy().replace('/', "-"));
        // One index per process, so two agents in the same repo do not fight over index.lock.
        let index = std::env::temp_dir().join(format!("agent-snapshot-{}.index", std::process::id()));
        Some(Snapshots { git_dir, work_tree, index })
    }

    /// Stores the work tree as it is now and returns the tree id.
    pub fn take(&self) -> Result<String, String> {
        if !self.git_dir.exists() {
            run(Command::new("git").args(["init", "-q", "--bare"]).arg(&self.git_dir))?;
            // Snapshots are referenced only from session files, so gc must never prune them.
            self.git(&["config", "gc.auto", "0"])?;
        }
        self.git(&["add", "-A"])?;
        self.git(&["write-tree"])
    }

    /// Paths that differ between two trees.
    pub fn changed(&self, from: &str, to: &str) -> Result<Vec<String>, String> {
        let names = self.git(&["diff", "--name-only", "--no-renames", "-z", from, to])?;
        Ok(names.split('\0').filter(|name| !name.is_empty()).map(String::from).collect())
    }

    /// The blob id of `path` in `tree`, None when the tree lacks it.
    pub fn blob(&self, tree: &str, path: &str) -> Option<String> {
        self.git(&["rev-parse", "--verify", "-q", &format!("{tree}:{path}")]).ok()
    }

    /// Writes `path` as it is in `tree`, or deletes it when the tree lacks it.
    pub fn restore(&self, tree: &str, path: &str) -> Result<(), String> {
        if self.blob(tree, path).is_some() {
            return self.git(&["checkout", tree, "--", path]).map(|_| ());
        }
        let file = self.work_tree.join(path);
        if file.exists() {
            std::fs::remove_file(&file).map_err(|e| format!("Cannot delete {}: {e}", file.display()))?;
        }
        Ok(())
    }

    fn git(&self, args: &[&str]) -> Result<String, String> {
        let mut command = Command::new("git");
        command.arg("--git-dir").arg(&self.git_dir).arg("--work-tree").arg(&self.work_tree);
        command.args(["-c", "core.autocrlf=false"]).args(args).env("GIT_INDEX_FILE", &self.index);
        run(command.current_dir(&self.work_tree))
    }
}

fn run(command: &mut Command) -> Result<String, String> {
    let output = command.output().map_err(|e| format!("Cannot run git: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim_end_matches('\n').to_string())
}
