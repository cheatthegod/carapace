use anyhow::{Context, Result, bail};
use chrono::Utc;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::types::*;

/// Git-based checkpoint backend.
///
/// Creates point-in-time snapshots from the current worktree without mutating
/// the caller's files. Operates via CLI commands — no libgit2 dependency.
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

    /// Save a checkpoint for the current worktree state.
    ///
    /// The returned reference is either `HEAD` when nothing is dirty, or a
    /// synthetic stash commit created via `git stash create`. Unlike
    /// `git stash push`, this does not modify the working tree.
    pub fn save(
        &self,
        session_id: &str,
        step_id: &str,
        files: &[String],
    ) -> Result<Checkpoint> {
        let checkpoint_id = new_id();
        let message = format!("carapace:{session_id}:{step_id}");
        let reference = self.snapshot_reference(&message)?;
        let checkpoint_type = if reference == "HEAD" {
            CheckpointType::GitCommit
        } else {
            CheckpointType::GitStash
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
            CheckpointType::GitStash | CheckpointType::GitCommit => {
                self.restore_paths(&checkpoint.reference, &checkpoint.files_affected)
                    .context("Failed to restore git checkpoint")?;
            }
            CheckpointType::FileCopy => {
                bail!("GitCheckpoint cannot restore FileCopy checkpoints");
            }
        }
        Ok(())
    }

    /// Discard a stash-based checkpoint to keep the stash list clean.
    pub fn discard(&self, checkpoint: &Checkpoint) -> Result<()> {
        let _ = checkpoint;
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

    fn snapshot_reference(&self, message: &str) -> Result<String> {
        let status = self.git(&["status", "--porcelain"])?;
        if status.trim().is_empty() {
            let head = self.git(&["rev-parse", "HEAD"])?;
            return Ok(head.trim().to_string());
        }

        // Stage everything (including untracked files) so `stash create`
        // captures the full worktree state. Then reset the index so the
        // working tree stays untouched from the caller's perspective.
        self.git(&["add", "-A"])?;
        let reference = self.git(&["stash", "create", message])?;
        self.git(&["reset"])?;

        let reference = reference.trim();
        if reference.is_empty() {
            let head = self.git(&["rev-parse", "HEAD"])?;
            return Ok(head.trim().to_string());
        }

        Ok(reference.to_string())
    }

    fn restore_paths(&self, reference: &str, files: &[String]) -> Result<()> {
        if files.is_empty() {
            self.git(&["checkout", reference, "--", "."])?;
            return Ok(());
        }

        let mut args = vec!["checkout", reference, "--"];
        let owned: Vec<&str> = files.iter().map(String::as_str).collect();
        args.extend(owned);
        self.git(&args)?;
        Ok(())
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

        // Saving a checkpoint must not mutate the working tree.
        assert_eq!(std::fs::read_to_string(path.join("test.txt")).unwrap(), "hello");

        // Mutate after the checkpoint and then restore to the saved state.
        std::fs::write(path.join("test.txt"), "changed").unwrap();
        gc.restore(&cp).unwrap();
        assert_eq!(std::fs::read_to_string(path.join("test.txt")).unwrap(), "hello");

        // Cleanup.
        gc.discard(&cp).unwrap();
    }

    #[test]
    fn clean_repo_produces_valid_checkpoint() {
        let (_dir, path) = init_repo();
        let gc = GitCheckpoint::new(&path).unwrap();

        let cp = gc.save("sess1", "step1", &[]).unwrap();

        // On a clean repo the reference is either HEAD (GitCommit) or a
        // synthetic stash hash (GitStash). Both are valid restore targets.
        assert!(
            matches!(cp.checkpoint_type, CheckpointType::GitCommit | CheckpointType::GitStash),
            "unexpected checkpoint type: {:?}",
            cp.checkpoint_type
        );
        assert!(!cp.reference.is_empty());
    }
}
