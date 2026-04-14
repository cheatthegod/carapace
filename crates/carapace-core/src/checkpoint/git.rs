use anyhow::{Context, Result, bail};
use chrono::Utc;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::types::*;

/// Git-based checkpoint backend.
///
/// Creates checkpoints using `git stash` for targeted file sets, or lightweight
/// tags for broader snapshots. Operates via CLI commands — no libgit2 dependency.
pub struct GitCheckpoint {
    working_dir: PathBuf,
}

impl GitCheckpoint {
    pub fn new(working_dir: &Path) -> Result<Self> {
        if !is_git_repo(working_dir) {
            bail!(
                "Not a git repository: {}. Git checkpoints require a git repo.",
                working_dir.display()
            );
        }
        Ok(Self {
            working_dir: working_dir.to_path_buf(),
        })
    }

    /// Attempt to create; returns `None` instead of an error when the directory
    /// is not a git repository.
    pub fn try_new(working_dir: &Path) -> Option<Self> {
        if is_git_repo(working_dir) {
            Some(Self {
                working_dir: working_dir.to_path_buf(),
            })
        } else {
            None
        }
    }

    /// Save a checkpoint for the given files. If `files` is empty, stashes
    /// everything that is currently dirty.
    pub fn save(
        &self,
        session_id: &str,
        step_id: &str,
        files: &[String],
    ) -> Result<Checkpoint> {
        let checkpoint_id = new_id();
        let message = format!("carapace:{}:{}", session_id, step_id);

        // Stage the relevant files so that `git stash push` picks them up.
        if files.is_empty() {
            self.git(&["add", "-A"])?;
        } else {
            let mut args = vec!["add", "--"];
            let owned: Vec<&str> = files.iter().map(String::as_str).collect();
            args.extend(owned);
            self.git(&args)?;
        }

        // Create the stash. `--keep-index` is deliberately *not* used so that
        // restoring the stash later gives back a clean diff.
        let stash_args = vec!["stash", "push", "-m", &message];
        let output = self.git(&stash_args)?;

        // `git stash push` prints "No local changes to save" when there is
        // nothing to stash. Detect this and return a checkpoint that records
        // the current HEAD instead.
        let reference = if output.contains("No local changes") || output.contains("No stash") {
            let head = self.git(&["rev-parse", "HEAD"])?;
            head.trim().to_string()
        } else {
            // The most recent stash is always `stash@{0}`.
            "stash@{0}".to_string()
        };

        let checkpoint_type = if reference.starts_with("stash@") {
            CheckpointType::GitStash
        } else {
            CheckpointType::GitCommit
        };

        Ok(Checkpoint {
            id: checkpoint_id,
            session_id: session_id.to_string(),
            step_id: step_id.to_string(),
            checkpoint_type,
            reference,
            files_affected: files.to_vec(),
            created_at: Utc::now(),
        })
    }

    /// Restore a previously saved checkpoint.
    pub fn restore(&self, checkpoint: &Checkpoint) -> Result<()> {
        match checkpoint.checkpoint_type {
            CheckpointType::GitStash => {
                // Apply the stash without removing it (so we can retry).
                self.git(&["stash", "apply", &checkpoint.reference])
                    .context("Failed to apply git stash")?;
            }
            CheckpointType::GitCommit => {
                // Hard-reset to the recorded commit. This is destructive but
                // appropriate when rolling back to a known-good state.
                self.git(&["checkout", &checkpoint.reference, "--", "."])
                    .context("Failed to checkout commit")?;
            }
            CheckpointType::FileCopy => {
                bail!("GitCheckpoint cannot restore FileCopy checkpoints");
            }
        }
        Ok(())
    }

    /// Discard a stash-based checkpoint to keep the stash list clean.
    pub fn discard(&self, checkpoint: &Checkpoint) -> Result<()> {
        if checkpoint.checkpoint_type == CheckpointType::GitStash {
            // Best-effort: ignore errors if the stash was already dropped.
            let _ = self.git(&["stash", "drop", &checkpoint.reference]);
        }
        Ok(())
    }

    /// Run a git command in the working directory and return its stdout.
    fn git(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.working_dir)
            .output()
            .with_context(|| format!("Failed to run: git {}", args.join(" ")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            // Some git commands (like stash push with no changes) exit 1 but
            // are not actual errors.
            if stderr.contains("No local changes") || stdout.contains("No local changes") {
                return Ok(stdout.to_string());
            }
            bail!(
                "git {} failed (exit {}): {}",
                args.join(" "),
                output.status.code().unwrap_or(-1),
                stderr.trim()
            );
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

/// Check whether a directory is inside a git repository.
pub fn is_git_repo(dir: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(dir)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn init_repo() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_path_buf();
        Command::new("git")
            .args(["init"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "test"])
            .current_dir(&path)
            .output()
            .unwrap();
        // Initial commit so HEAD exists.
        std::fs::write(path.join("init.txt"), "init").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&path)
            .output()
            .unwrap();
        (dir, path)
    }

    #[test]
    fn detect_git_repo() {
        let (_dir, path) = init_repo();
        assert!(is_git_repo(&path));
        assert!(!is_git_repo(Path::new("/tmp")));
    }

    #[test]
    fn save_and_restore_checkpoint() {
        let (_dir, path) = init_repo();
        let gc = GitCheckpoint::new(&path).unwrap();

        // Create a dirty file.
        std::fs::write(path.join("test.txt"), "hello").unwrap();

        let cp = gc.save("sess1", "step1", &["test.txt".into()]).unwrap();
        assert_eq!(cp.checkpoint_type, CheckpointType::GitStash);

        // After stash the file should be gone (or reverted).
        assert!(!path.join("test.txt").exists() || std::fs::read_to_string(path.join("test.txt")).unwrap_or_default().is_empty());

        // Restore.
        gc.restore(&cp).unwrap();
        assert_eq!(std::fs::read_to_string(path.join("test.txt")).unwrap(), "hello");

        // Cleanup.
        gc.discard(&cp).unwrap();
    }
}
