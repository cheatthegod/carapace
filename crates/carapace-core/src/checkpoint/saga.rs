use anyhow::Result;
use tracing;

use crate::types::*;

/// A compensation action that can undo a forward step.
pub type CompensateFn = Box<dyn Fn() -> Result<()> + Send + Sync>;

/// A completed saga step with its compensating action.
pub struct CompletedStep {
    pub step_id: StepId,
    pub step_number: u32,
    pub checkpoint_id: Option<CheckpointId>,
    pub compensate: Option<CompensateFn>,
    pub description: String,
}

/// Saga-pattern coordinator for multi-step transactions.
///
/// Maintains an ordered list of completed steps and their compensating actions.
/// On failure, executes compensations in reverse order to restore a consistent
/// state — like distributed saga transactions in microservice architectures.
///
/// ```text
/// Step 1 → ✓ → Step 2 → ✓ → Step 3 → FAIL
///                                        ↓
///                              compensate(Step 3)  // if registered
///                              compensate(Step 2)
///                              compensate(Step 1)
/// ```
pub struct SagaCoordinator {
    completed: Vec<CompletedStep>,
    max_depth: u32,
}

impl SagaCoordinator {
    pub fn new(max_rollback_depth: u32) -> Self {
        Self {
            completed: Vec::new(),
            max_depth: max_rollback_depth,
        }
    }

    /// Register a successfully completed step.
    pub fn register(
        &mut self,
        step_id: StepId,
        step_number: u32,
        checkpoint_id: Option<CheckpointId>,
        compensate: Option<CompensateFn>,
        description: String,
    ) {
        self.completed.push(CompletedStep {
            step_id,
            step_number,
            checkpoint_id,
            compensate,
            description,
        });

        // Prune if we exceed max depth: drop the oldest entries that are beyond
        // the rollback window. Their compensations are no longer reachable.
        if self.completed.len() > self.max_depth as usize {
            let excess = self.completed.len() - self.max_depth as usize;
            self.completed.drain(..excess);
        }
    }

    /// Roll back the last `steps_back` steps (or all if `None`), executing
    /// compensating actions in reverse order.
    pub fn rollback(&mut self, steps_back: Option<u32>) -> RollbackResult {
        let total = self.completed.len();
        let n = match steps_back {
            Some(n) => (n as usize).min(total),
            None => total,
        };

        if n == 0 {
            return RollbackResult {
                rolled_back_to_step: self
                    .completed
                    .last()
                    .map(|s| s.step_number)
                    .unwrap_or(0),
                steps_undone: 0,
                compensations_executed: 0,
                compensations_failed: 0,
            };
        }

        let mut executed = 0u32;
        let mut failed = 0u32;

        // Execute compensations in reverse order.
        for _ in 0..n {
            if let Some(step) = self.completed.pop() {
                if let Some(compensate) = &step.compensate {
                    match compensate() {
                        Ok(()) => {
                            tracing::info!(
                                step_id = %step.step_id,
                                step_number = step.step_number,
                                "Compensated step: {}",
                                step.description
                            );
                            executed += 1;
                        }
                        Err(err) => {
                            tracing::error!(
                                step_id = %step.step_id,
                                step_number = step.step_number,
                                error = %err,
                                "Compensation failed for step: {}",
                                step.description
                            );
                            failed += 1;
                        }
                    }
                } else {
                    // No compensation registered (e.g. read-only step).
                    tracing::debug!(
                        step_id = %step.step_id,
                        "No compensation needed for: {}",
                        step.description
                    );
                }
            }
        }

        let rolled_back_to = self
            .completed
            .last()
            .map(|s| s.step_number)
            .unwrap_or(0);

        RollbackResult {
            rolled_back_to_step: rolled_back_to,
            steps_undone: executed + failed,
            compensations_executed: executed,
            compensations_failed: failed,
        }
    }

    /// Roll back until reaching the step protected by the given checkpoint.
    ///
    /// Checkpoints are created before a step executes, so rolling back to a
    /// checkpoint must also undo the step that owns that checkpoint.
    pub fn rollback_to_checkpoint(&mut self, checkpoint_id: &str) -> Option<RollbackResult> {
        // Find how many steps to undo.
        let target_idx = self
            .completed
            .iter()
            .rposition(|s| s.checkpoint_id.as_deref() == Some(checkpoint_id));

        match target_idx {
            Some(idx) => {
                let steps_back = (self.completed.len() - idx) as u32;
                Some(self.rollback(Some(steps_back)))
            }
            None => None,
        }
    }

    /// Number of steps currently tracked.
    pub fn depth(&self) -> usize {
        self.completed.len()
    }

    /// Peek at the most recent step.
    pub fn last_step(&self) -> Option<&CompletedStep> {
        self.completed.last()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn rollback_executes_in_reverse() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut saga = SagaCoordinator::new(10);

        for i in 1..=3 {
            let order_clone = Arc::clone(&order);
            saga.register(
                format!("step-{i}"),
                i,
                None,
                Some(Box::new(move || {
                    order_clone.lock().unwrap().push(i);
                    Ok(())
                })),
                format!("Step {i}"),
            );
        }

        let result = saga.rollback(None);
        assert_eq!(result.steps_undone, 3);
        assert_eq!(result.compensations_executed, 3);
        assert_eq!(result.compensations_failed, 0);
        assert_eq!(*order.lock().unwrap(), vec![3, 2, 1]);
    }

    #[test]
    fn partial_rollback() {
        let mut saga = SagaCoordinator::new(10);

        for i in 1..=5 {
            saga.register(
                format!("step-{i}"),
                i,
                None,
                Some(Box::new(|| Ok(()))),
                format!("Step {i}"),
            );
        }

        let result = saga.rollback(Some(2));
        assert_eq!(result.steps_undone, 2);
        assert_eq!(result.rolled_back_to_step, 3);
        assert_eq!(saga.depth(), 3);
    }

    #[test]
    fn rollback_to_checkpoint() {
        let mut saga = SagaCoordinator::new(10);

        saga.register("s1".into(), 1, Some("cp-1".into()), Some(Box::new(|| Ok(()))), "S1".into());
        saga.register("s2".into(), 2, Some("cp-2".into()), Some(Box::new(|| Ok(()))), "S2".into());
        saga.register("s3".into(), 3, None, Some(Box::new(|| Ok(()))), "S3".into());
        saga.register("s4".into(), 4, None, Some(Box::new(|| Ok(()))), "S4".into());

        let result = saga.rollback_to_checkpoint("cp-2").unwrap();
        assert_eq!(result.steps_undone, 3); // Undo steps 4, 3, and 2
        assert_eq!(result.rolled_back_to_step, 1);
        assert_eq!(saga.depth(), 1);
    }

    #[test]
    fn rollback_to_unknown_checkpoint_returns_none() {
        let mut saga = SagaCoordinator::new(10);
        saga.register("s1".into(), 1, Some("cp-1".into()), Some(Box::new(|| Ok(()))), "S1".into());

        assert!(saga.rollback_to_checkpoint("missing").is_none());
        assert_eq!(saga.depth(), 1);
    }

    #[test]
    fn max_depth_pruning() {
        let mut saga = SagaCoordinator::new(3);

        for i in 1..=5 {
            saga.register(format!("s{i}"), i, None, None, format!("S{i}"));
        }

        assert_eq!(saga.depth(), 3);
        assert_eq!(saga.last_step().unwrap().step_number, 5);
    }
}
