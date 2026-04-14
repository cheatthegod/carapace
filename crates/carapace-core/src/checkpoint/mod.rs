pub mod git;
pub mod saga;
pub mod types;

use anyhow::Result;
use std::path::Path;
use std::sync::Arc;

use crate::config::schema::CheckpointConfig;
use crate::types::*;
use git::GitCheckpoint;
use saga::{CompensateFn, SagaCoordinator};

/// Manages checkpoints and saga-based rollback.
///
/// Combines git-based state snapshots with the saga pattern: each completed
/// step registers a compensating action, and on failure the coordinator
/// executes compensations in reverse order.
pub struct CheckpointManager {
    git: Option<Arc<GitCheckpoint>>,
    saga: SagaCoordinator,
    config: CheckpointConfig,
}

impl CheckpointManager {
    pub fn new(working_dir: &Path, config: CheckpointConfig) -> Self {
        let git = if config.enabled {
            GitCheckpoint::try_new(working_dir).map(Arc::new)
        } else {
            None
        };

        let saga = SagaCoordinator::new(config.max_rollback_depth);

        Self { git, saga, config }
    }

    /// Save a checkpoint if the action type warrants it.
    ///
    /// Returns the checkpoint metadata, or `None` if checkpointing is disabled
    /// or not applicable for this action type.
    pub fn save_if_needed(
        &self,
        session_id: &str,
        step_id: &str,
        action: &StepAction,
    ) -> Result<Option<Checkpoint>> {
        if !self.config.enabled || !self.config.auto_save {
            return Ok(None);
        }

        let action_str = action.action_type.as_str();
        if !self.config.auto_save_on.iter().any(|a| a == action_str) {
            return Ok(None);
        }

        Ok(Some(self.save(session_id, step_id, action)?))
    }

    /// Save a checkpoint for an explicit caller request.
    pub fn save(&self, session_id: &str, step_id: &str, action: &StepAction) -> Result<Checkpoint> {
        if !self.config.enabled {
            anyhow::bail!("checkpointing is disabled");
        }

        let Some(git) = &self.git else {
            anyhow::bail!("no git checkpoint backend is available for this working directory");
        };

        let checkpoint = git.save(session_id, step_id, &action.target_files)?;
        tracing::info!(
            checkpoint_id = %checkpoint.id,
            checkpoint_type = ?checkpoint.checkpoint_type,
            "Saved checkpoint"
        );

        Ok(checkpoint)
    }

    /// Register a completed step with the saga coordinator.
    ///
    /// Builds a compensating closure that restores the checkpoint when called.
    pub fn register_step(
        &mut self,
        step_id: StepId,
        step_number: u32,
        checkpoint: Option<&Checkpoint>,
        description: String,
    ) {
        let compensate: Option<CompensateFn> = match (&self.git, checkpoint) {
            (Some(git), Some(cp)) => {
                let git = Arc::clone(git);
                let cp = cp.clone();
                Some(Box::new(move || git.restore(&cp)))
            }
            _ => None,
        };

        let checkpoint_id = checkpoint.map(|cp| cp.id.clone());

        self.saga
            .register(step_id, step_number, checkpoint_id, compensate, description);
    }

    /// Roll back the last N steps.
    pub fn rollback(&mut self, steps_back: Option<u32>) -> RollbackResult {
        self.saga.rollback(steps_back)
    }

    /// Roll back to a specific checkpoint.
    pub fn rollback_to_checkpoint(&mut self, checkpoint_id: &str) -> Option<RollbackResult> {
        self.saga.rollback_to_checkpoint(checkpoint_id)
    }

    /// Restore a specific checkpoint directly.
    pub fn restore_checkpoint(&self, checkpoint: &Checkpoint) -> Result<()> {
        let Some(git) = &self.git else {
            anyhow::bail!("no git checkpoint backend is available for this working directory");
        };

        git.restore(checkpoint)
    }

    /// Current depth of the saga history.
    pub fn depth(&self) -> usize {
        self.saga.depth()
    }
}
