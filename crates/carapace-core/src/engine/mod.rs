use anyhow::Result;
use chrono::Utc;
use std::path::Path;
use std::time::Instant;

use crate::checkpoint::CheckpointManager;
use crate::config::CarapaceConfig;
use crate::storage::Storage;
use crate::tracer::Tracer;
use crate::types::*;
use crate::verifier::{CompositeVerifier, Verifier};

/// Result of executing a single step through Carapace.
pub struct StepExecution {
    pub step_id: StepId,
    pub verification: VerificationOutcome,
    pub checkpoint_id: Option<CheckpointId>,
    pub anomalies: Vec<Anomaly>,
    pub result: StepResult,
}

/// The main orchestrator that coordinates verification, checkpointing, and
/// tracing for every agent step.
///
/// ```text
/// Agent proposes action
///        ↓
///   ┌─────────┐
///   │ Verify  │ → fail? → record trace → return failure
///   └────┬────┘
///        ↓ pass
///   ┌─────────┐
///   │Checkpoint│ → save state before execution
///   └────┬────┘
///        ↓
///   ┌─────────┐
///   │ Execute │ → (caller performs the actual action)
///   └────┬────┘
///        ↓
///   ┌─────────┐
///   │  Trace  │ → record outcome + detect anomalies
///   └────┬────┘
///        ↓
///   ┌─────────┐
///   │  Saga   │ → register for potential rollback
///   └─────────┘
/// ```
pub struct ExecutionEngine {
    verifier: CompositeVerifier,
    checkpoint_mgr: CheckpointManager,
    tracer: Tracer,
    storage: Storage,
    config: CarapaceConfig,
}

impl ExecutionEngine {
    /// Create a new engine for the given working directory.
    pub async fn new(config: CarapaceConfig, working_dir: &Path, db_path: &str) -> Result<Self> {
        let storage = Storage::new(db_path).await?;
        let tracer = Tracer::new(storage.clone(), config.trace.clone());
        let verifier = CompositeVerifier::new(config.verification.clone());
        let checkpoint_mgr = CheckpointManager::new(working_dir, config.checkpoint.clone());

        Ok(Self {
            verifier,
            checkpoint_mgr,
            tracer,
            storage,
            config,
        })
    }

    /// Start a new session.
    pub async fn start_session(
        &self,
        session_id: &str,
        agent_name: Option<&str>,
        working_dir: &str,
    ) -> Result<()> {
        self.storage
            .create_session(session_id, agent_name, working_dir)
            .await
    }

    /// End a session with the given status.
    pub async fn end_session(&self, session_id: &str, status: &str) -> Result<SessionSummary> {
        self.storage.update_session_status(session_id, status).await?;
        self.tracer.get_summary(session_id).await
    }

    /// Pre-verify an action without executing or recording it.
    pub fn verify_action(
        &self,
        action: &StepAction,
        ctx: &ExecutionContext,
    ) -> VerificationOutcome {
        if !self.config.verification.enabled {
            return VerificationOutcome {
                decision: VerificationDecision::Pass,
                checks_performed: vec![],
                duration_ms: 0,
            };
        }
        self.verifier.verify(action, ctx)
    }

    /// Full step execution: verify → checkpoint → (caller executes) → trace.
    ///
    /// This is the core method. It does NOT execute the action itself — the
    /// caller is responsible for that. Instead it:
    ///
    /// 1. Verifies the proposed action
    /// 2. If verification passes, saves a checkpoint
    /// 3. Returns verification + checkpoint info so the caller can decide
    ///    whether to proceed
    ///
    /// After the caller executes (or skips), call `record_result` to complete
    /// the trace entry.
    pub async fn pre_execute(
        &mut self,
        action: &StepAction,
        ctx: &ExecutionContext,
        reason: Option<String>,
    ) -> Result<PreExecuteResult> {
        let start = Instant::now();
        let step_id = new_id();

        // 1. Verify
        let verification = self.verify_action(action, ctx);

        if verification.decision.is_fail() {
            // Record the failed verification as a trace entry.
            let entry = TraceEntry {
                step_id: step_id.clone(),
                session_id: ctx.session_id.clone(),
                step_number: ctx.step_number,
                action: action.clone(),
                reason,
                verification: verification.clone(),
                checkpoint_id: None,
                result: StepResult::Skipped {
                    reason: format!(
                        "Verification failed: {}",
                        verification.decision.reasons().join("; ")
                    ),
                },
                tokens_used: 0,
                cost_usd: 0.0,
                duration_ms: start.elapsed().as_millis() as u64,
                timestamp: Utc::now(),
            };

            let anomalies = self.tracer.record_step(entry).await?;

            return Ok(PreExecuteResult {
                step_id,
                verification,
                checkpoint_id: None,
                anomalies,
                proceed: false,
            });
        }

        // 2. Checkpoint (before execution)
        let checkpoint = self
            .checkpoint_mgr
            .save_if_needed(&ctx.session_id, &step_id, action)?;

        let checkpoint_id = checkpoint.as_ref().map(|cp| cp.id.clone());

        // Store checkpoint metadata.
        if let Some(cp) = &checkpoint {
            self.storage.insert_checkpoint(cp).await?;
        }

        Ok(PreExecuteResult {
            step_id,
            verification,
            checkpoint_id,
            anomalies: vec![],
            proceed: true,
        })
    }

    /// Record the outcome after the caller has executed (or skipped) the action.
    pub async fn record_result(
        &mut self,
        step_id: StepId,
        action: StepAction,
        ctx: &ExecutionContext,
        reason: Option<String>,
        verification: VerificationOutcome,
        checkpoint_id: Option<CheckpointId>,
        result: StepResult,
        tokens_used: u64,
        cost_usd: f64,
        duration_ms: u64,
    ) -> Result<Vec<Anomaly>> {
        let entry = TraceEntry {
            step_id: step_id.clone(),
            session_id: ctx.session_id.clone(),
            step_number: ctx.step_number,
            action: action.clone(),
            reason,
            verification,
            checkpoint_id: checkpoint_id.clone(),
            result: result.clone(),
            tokens_used,
            cost_usd,
            duration_ms,
            timestamp: Utc::now(),
        };

        let anomalies = self.tracer.record_step(entry).await?;

        // Register with saga for potential rollback — only if the step
        // succeeded (failed steps don't need compensating).
        if result.is_success() {
            self.checkpoint_mgr.register_step(
                step_id,
                ctx.step_number,
                None, // TODO: look up checkpoint by id
                action.description.clone(),
            );
        }

        Ok(anomalies)
    }

    /// Convenience: verify + record in one call (for steps that don't need
    /// separate pre/post handling).
    pub async fn execute_step(
        &mut self,
        action: StepAction,
        ctx: &ExecutionContext,
        reason: Option<String>,
        result: StepResult,
        tokens_used: u64,
        cost_usd: f64,
    ) -> Result<StepExecution> {
        let start = Instant::now();

        let pre = self.pre_execute(&action, ctx, reason.clone()).await?;

        if !pre.proceed {
            return Ok(StepExecution {
                step_id: pre.step_id,
                verification: pre.verification,
                checkpoint_id: pre.checkpoint_id,
                anomalies: pre.anomalies,
                result: StepResult::Skipped {
                    reason: "Verification failed".into(),
                },
            });
        }

        let duration_ms = start.elapsed().as_millis() as u64;

        let anomalies = self
            .record_result(
                pre.step_id.clone(),
                action,
                ctx,
                reason,
                pre.verification.clone(),
                pre.checkpoint_id.clone(),
                result.clone(),
                tokens_used,
                cost_usd,
                duration_ms,
            )
            .await?;

        Ok(StepExecution {
            step_id: pre.step_id,
            verification: pre.verification,
            checkpoint_id: pre.checkpoint_id,
            anomalies,
            result,
        })
    }

    /// Roll back the last N steps.
    pub fn rollback(&mut self, steps_back: Option<u32>) -> RollbackResult {
        self.checkpoint_mgr.rollback(steps_back)
    }

    /// Roll back to a specific checkpoint.
    pub fn rollback_to_checkpoint(&mut self, checkpoint_id: &str) -> RollbackResult {
        self.checkpoint_mgr.rollback_to_checkpoint(checkpoint_id)
    }

    /// Get the full trace for a session.
    pub async fn get_trace(&self, session_id: &str) -> Result<Vec<TraceEntry>> {
        self.tracer.get_trace(session_id).await
    }

    /// Get a session summary.
    pub async fn get_summary(&self, session_id: &str) -> Result<SessionSummary> {
        self.tracer.get_summary(session_id).await
    }
}

/// Intermediate result from `pre_execute`.
pub struct PreExecuteResult {
    pub step_id: StepId,
    pub verification: VerificationOutcome,
    pub checkpoint_id: Option<CheckpointId>,
    pub anomalies: Vec<Anomaly>,
    pub proceed: bool,
}
