use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::checkpoint::CheckpointManager;
use crate::config::CarapaceConfig;
use crate::storage::Storage;
use crate::tracer::Tracer;
use crate::types::*;
use crate::verifier::{CompositeVerifier, Verifier};

pub type StepInput = StepAction;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeginSessionRequest {
    pub session_id: Option<SessionId>,
    pub agent_name: Option<String>,
    pub working_dir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeginSessionResponse {
    pub session: SessionRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyStepRequest {
    pub session_id: SessionId,
    pub step_number: Option<u32>,
    pub plan: Option<String>,
    pub action: StepInput,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyStepResponse {
    pub session_id: SessionId,
    pub step_number: u32,
    pub verification: VerificationOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum StepOutcomeStatus {
    Success,
    Failure,
    RolledBack,
    Skipped,
}

impl StepOutcomeStatus {
    pub fn into_step_result(self, message: Option<String>) -> StepResult {
        match self {
            StepOutcomeStatus::Success => StepResult::Success,
            StepOutcomeStatus::Failure => StepResult::Failure {
                error: message.unwrap_or_else(|| "step failed".to_string()),
            },
            StepOutcomeStatus::RolledBack => StepResult::RolledBack {
                reason: message.unwrap_or_else(|| "step rolled back".to_string()),
            },
            StepOutcomeStatus::Skipped => StepResult::Skipped {
                reason: message.unwrap_or_else(|| "step skipped".to_string()),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordStepRequest {
    pub session_id: SessionId,
    pub step_number: Option<u32>,
    pub plan: Option<String>,
    pub action: StepInput,
    pub reason: Option<String>,
    pub checkpoint_id: Option<CheckpointId>,
    pub result_status: StepOutcomeStatus,
    pub result_message: Option<String>,
    pub tokens_used: u64,
    pub cost_usd: f64,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordStepResponse {
    pub step_id: StepId,
    pub session_id: SessionId,
    pub step_number: u32,
    pub verification: VerificationOutcome,
    pub checkpoint_id: Option<CheckpointId>,
    pub result: StepResult,
    pub anomalies: Vec<Anomaly>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveCheckpointRequest {
    pub session_id: SessionId,
    pub action: StepInput,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveCheckpointResponse {
    pub session_id: SessionId,
    pub checkpoint_id: CheckpointId,
    pub checkpoint_type: CheckpointType,
    pub reference: String,
    pub files_affected: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollbackRequest {
    pub session_id: SessionId,
    pub checkpoint_id: Option<CheckpointId>,
    pub steps_back: Option<u32>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollbackResponse {
    pub step_id: StepId,
    pub session_id: SessionId,
    pub step_number: u32,
    pub checkpoint_id: Option<CheckpointId>,
    pub rollback: RollbackResult,
}

struct SessionRuntime {
    checkpoint_manager: CheckpointManager,
    pending_checkpoints: HashMap<CheckpointId, Checkpoint>,
}

impl SessionRuntime {
    fn new(working_dir: &str, config: crate::config::schema::CheckpointConfig) -> Self {
        Self {
            checkpoint_manager: CheckpointManager::new(std::path::Path::new(working_dir), config),
            pending_checkpoints: HashMap::new(),
        }
    }
}

#[derive(Clone)]
pub struct ExecutionEngine {
    config: CarapaceConfig,
    storage: Storage,
    runtimes: Arc<Mutex<HashMap<SessionId, SessionRuntime>>>,
    learned_rules: Arc<Mutex<Vec<crate::learner::rules::LearnedRule>>>,
}

impl ExecutionEngine {
    pub fn new(config: CarapaceConfig, storage: Storage) -> Self {
        Self {
            config,
            storage,
            runtimes: Arc::new(Mutex::new(HashMap::new())),
            learned_rules: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn storage(&self) -> &Storage {
        &self.storage
    }

    /// Analyze past sessions, generate rules, and load them into the verifier.
    pub async fn load_learned_rules(&self, min_confidence: f64) -> Result<usize> {
        let learner = crate::learner::Learner::new(self.storage.clone(), min_confidence);
        let rules = learner.learn_rules().await?;
        let count = rules.len();
        if let Ok(mut lr) = self.learned_rules.lock() {
            *lr = rules;
        }
        tracing::info!("Loaded {} learned rules from trace analysis", count);
        Ok(count)
    }

    /// Analyze past sessions, persist rules to disk, and load into verifier.
    pub async fn learn_and_save(&self, data_dir: &std::path::Path, min_confidence: f64) -> Result<crate::learner::LearningReport> {
        let learner = crate::learner::Learner::new(self.storage.clone(), min_confidence);
        let report = learner.learn_and_save(data_dir).await?;
        let count = report.rules_generated.len();
        if let Ok(mut lr) = self.learned_rules.lock() {
            *lr = report.rules_generated.clone();
        }
        tracing::info!("Learned and saved {} rules to {}", count, data_dir.display());
        Ok(report)
    }

    /// Load previously persisted rules from disk into the verifier.
    pub fn load_rules_from_disk(&self, data_dir: &std::path::Path) -> Result<usize> {
        let rules = crate::learner::persist::load_rules(data_dir)?;
        let count = rules.len();
        if let Ok(mut lr) = self.learned_rules.lock() {
            *lr = rules;
        }
        if count > 0 {
            tracing::info!("Loaded {} persisted learned rules from {}", count, data_dir.display());
        }
        Ok(count)
    }

    pub async fn begin_session(&self, request: BeginSessionRequest) -> Result<BeginSessionResponse> {
        let session_id = request.session_id.unwrap_or_else(new_id);

        if let Some(existing) = self.storage.get_session(&session_id).await? {
            self.ensure_runtime(&existing)?;
            self.storage.update_session_status(&session_id, "active").await?;
            return Ok(BeginSessionResponse { session: existing });
        }

        self.storage
            .create_session(
                &session_id,
                request.agent_name.as_deref(),
                &request.working_dir,
            )
            .await?;

        let session = self
            .storage
            .get_session(&session_id)
            .await?
            .with_context(|| format!("Session {session_id} was not readable after creation"))?;

        self.ensure_runtime(&session)?;
        self.storage
            .update_session_status(&session.session_id, "active")
            .await?;

        Ok(BeginSessionResponse { session })
    }

    pub async fn verify_step(&self, request: VerifyStepRequest) -> Result<VerifyStepResponse> {
        let (_, context) = self
            .build_context(
                &request.session_id,
                request.step_number,
                request.plan.clone(),
            )
            .await?;

        let verification = self.verify_action(&request.action, &context);

        Ok(VerifyStepResponse {
            session_id: context.session_id,
            step_number: context.step_number,
            verification,
        })
    }

    pub async fn save_checkpoint(
        &self,
        request: SaveCheckpointRequest,
    ) -> Result<SaveCheckpointResponse> {
        let session = self.load_session(&request.session_id).await?;
        self.ensure_runtime(&session)?;

        let checkpoint = {
            let mut runtimes = self.lock_runtimes()?;
            let runtime = runtimes
                .get_mut(&session.session_id)
                .with_context(|| format!("Missing runtime state for session {}", session.session_id))?;
            let step_id = new_id();
            let checkpoint =
                runtime
                    .checkpoint_manager
                    .save(&session.session_id, &step_id, &request.action)?;
            runtime
                .pending_checkpoints
                .insert(checkpoint.id.clone(), checkpoint.clone());
            checkpoint
        };

        Ok(SaveCheckpointResponse {
            session_id: session.session_id,
            checkpoint_id: checkpoint.id,
            checkpoint_type: checkpoint.checkpoint_type,
            reference: checkpoint.reference,
            files_affected: checkpoint.files_affected,
        })
    }

    pub async fn record_step(&self, request: RecordStepRequest) -> Result<RecordStepResponse> {
        let (session, context) = self
            .build_context(
                &request.session_id,
                request.step_number,
                request.plan.clone(),
            )
            .await?;

        let verification = self.verify_action(&request.action, &context);
        let result = request
            .result_status
            .clone()
            .into_step_result(request.result_message.clone());
        let step_id = new_id();
        let checkpoint = self
            .resolve_checkpoint(
                &session,
                request.checkpoint_id.as_deref(),
                &step_id,
                context.step_number,
                &request.action.description,
                &request.result_status,
            )
            .await?;
        let checkpoint_id = checkpoint.as_ref().map(|cp| cp.id.clone());

        let entry = TraceEntry {
            step_id: step_id.clone(),
            session_id: context.session_id.clone(),
            step_number: context.step_number,
            action: request.action,
            reason: request.reason,
            verification: verification.clone(),
            checkpoint_id: checkpoint_id.clone(),
            result: result.clone(),
            tokens_used: request.tokens_used,
            cost_usd: request.cost_usd,
            duration_ms: request.duration_ms,
            timestamp: Utc::now(),
        };

        let anomalies = self.record_entry(entry).await?;

        if let Some(checkpoint) = checkpoint {
            self.storage.insert_checkpoint(&checkpoint).await?;
        }

        Ok(RecordStepResponse {
            step_id,
            session_id: context.session_id,
            step_number: context.step_number,
            verification,
            checkpoint_id,
            result,
            anomalies,
        })
    }

    pub async fn rollback(&self, request: RollbackRequest) -> Result<RollbackResponse> {
        let (session, context) = self.build_context(&request.session_id, None, None).await?;
        self.ensure_runtime(&session)?;

        let rollback = if let Some(checkpoint_id) = request.checkpoint_id.as_deref() {
            if let Some(result) =
                self.rollback_pending_checkpoint(&session, checkpoint_id, context.step_number.saturating_sub(1))?
            {
                result
            } else {
                let maybe_result = {
                    let mut runtimes = self.lock_runtimes()?;
                    let runtime = runtimes.get_mut(&session.session_id).with_context(|| {
                        format!("Missing runtime state for session {}", session.session_id)
                    })?;
                    runtime.checkpoint_manager.rollback_to_checkpoint(checkpoint_id)
                };

                if let Some(result) = maybe_result {
                    result
                } else {
                    let checkpoint = self
                        .storage
                        .get_checkpoint(checkpoint_id)
                        .await?
                        .with_context(|| format!("Unknown checkpoint: {checkpoint_id}"))?;

                    let mut runtimes = self.lock_runtimes()?;
                    let runtime = runtimes.get_mut(&session.session_id).with_context(|| {
                        format!("Missing runtime state for session {}", session.session_id)
                    })?;
                    runtime.checkpoint_manager.restore_checkpoint(&checkpoint)?;

                    RollbackResult {
                        rolled_back_to_step: context.step_number.saturating_sub(1),
                        steps_undone: 1,
                        compensations_executed: 1,
                        compensations_failed: 0,
                    }
                }
            }
        } else {
            let mut runtimes = self.lock_runtimes()?;
            let runtime = runtimes
                .get_mut(&session.session_id)
                .with_context(|| format!("Missing runtime state for session {}", session.session_id))?;
            runtime.checkpoint_manager.rollback(request.steps_back)
        };

        self.storage
            .update_session_status(&session.session_id, "rolled_back")
            .await?;

        let reason = rollback_reason(&request);
        let step_id = new_id();
        let entry = TraceEntry {
            step_id: step_id.clone(),
            session_id: context.session_id.clone(),
            step_number: context.step_number,
            action: StepAction {
                action_type: ActionType::Other("rollback".to_string()),
                tool_name: Some("carapace_rollback".to_string()),
                arguments: json!({
                    "checkpoint_id": request.checkpoint_id.clone(),
                    "steps_back": request.steps_back,
                }),
                target_files: vec![],
                description: rollback_description(&request),
            },
            reason: request.reason,
            verification: VerificationOutcome {
                decision: VerificationDecision::Pass,
                checks_performed: vec![],
                duration_ms: 0,
            },
            checkpoint_id: request.checkpoint_id.clone(),
            result: StepResult::RolledBack { reason },
            tokens_used: 0,
            cost_usd: 0.0,
            duration_ms: 0,
            timestamp: Utc::now(),
        };
        self.record_entry(entry).await?;

        Ok(RollbackResponse {
            step_id,
            session_id: context.session_id,
            step_number: context.step_number,
            checkpoint_id: request.checkpoint_id,
            rollback,
        })
    }

    pub async fn session_summary(&self, session_id: &str) -> Result<SessionSummary> {
        self.load_session(session_id).await?;
        self.storage.get_session_summary(session_id).await
    }

    async fn build_context(
        &self,
        session_id: &str,
        step_number: Option<u32>,
        plan: Option<String>,
    ) -> Result<(SessionRecord, ExecutionContext)> {
        let session = self.load_session(session_id).await?;
        let previous_steps = self.storage.get_previous_summaries(session_id, 100).await?;
        let step_number = step_number.unwrap_or_else(|| next_step_number(&previous_steps));

        let context = ExecutionContext {
            session_id: session.session_id.clone(),
            step_number,
            working_dir: session.working_dir.clone(),
            agent_name: session.agent_name.clone(),
            plan,
            previous_steps,
        };

        Ok((session, context))
    }

    async fn load_session(&self, session_id: &str) -> Result<SessionRecord> {
        self.storage
            .get_session(session_id)
            .await?
            .with_context(|| format!("Unknown session: {session_id}"))
    }

    fn verify_action(&self, action: &StepAction, context: &ExecutionContext) -> VerificationOutcome {
        if !self.config.verification.enabled {
            return VerificationOutcome {
                decision: VerificationDecision::Pass,
                checks_performed: vec![],
                duration_ms: 0,
            };
        }

        let learned = self.learned_rules.lock().ok()
            .map(|lr| lr.clone())
            .unwrap_or_default();
        let verifier = CompositeVerifier::new(self.config.verification.clone())
            .with_learned_rules(learned);
        verifier.verify(action, context)
    }

    async fn resolve_checkpoint(
        &self,
        session: &SessionRecord,
        checkpoint_id: Option<&str>,
        step_id: &str,
        step_number: u32,
        description: &str,
        result_status: &StepOutcomeStatus,
    ) -> Result<Option<Checkpoint>> {
        let Some(checkpoint_id) = checkpoint_id else {
            return Ok(None);
        };

        self.ensure_runtime(session)?;

        let mut runtimes = self.lock_runtimes()?;
        let runtime = runtimes
            .get_mut(&session.session_id)
            .with_context(|| format!("Missing runtime state for session {}", session.session_id))?;
        let Some(mut checkpoint) = runtime.pending_checkpoints.remove(checkpoint_id) else {
            return Err(anyhow!("Unknown pending checkpoint: {checkpoint_id}"));
        };

        if !should_register_checkpoint(result_status) {
            return Ok(None);
        }

        checkpoint.step_id = step_id.to_string();
        runtime.checkpoint_manager.register_step(
            step_id.to_string(),
            step_number,
            Some(&checkpoint),
            description.to_string(),
        );
        Ok(Some(checkpoint))
    }

    fn rollback_pending_checkpoint(
        &self,
        session: &SessionRecord,
        checkpoint_id: &str,
        rolled_back_to_step: u32,
    ) -> Result<Option<RollbackResult>> {
        let mut runtimes = self.lock_runtimes()?;
        let runtime = runtimes
            .get_mut(&session.session_id)
            .with_context(|| format!("Missing runtime state for session {}", session.session_id))?;

        let Some(checkpoint) = runtime.pending_checkpoints.remove(checkpoint_id) else {
            return Ok(None);
        };

        runtime.checkpoint_manager.restore_checkpoint(&checkpoint)?;

        Ok(Some(RollbackResult {
            rolled_back_to_step,
            steps_undone: 1,
            compensations_executed: 1,
            compensations_failed: 0,
        }))
    }

    async fn record_entry(&self, entry: TraceEntry) -> Result<Vec<Anomaly>> {
        if self.config.trace.enabled {
            return Tracer::new(self.storage.clone(), self.config.trace.clone())
                .record_step(entry)
                .await;
        }

        self.storage.insert_step(&entry).await?;
        Ok(vec![])
    }

    fn ensure_runtime(&self, session: &SessionRecord) -> Result<()> {
        let mut runtimes = self.lock_runtimes()?;
        runtimes
            .entry(session.session_id.clone())
            .or_insert_with(|| SessionRuntime::new(&session.working_dir, self.config.checkpoint.clone()));
        Ok(())
    }

    fn lock_runtimes(&self) -> Result<MutexGuard<'_, HashMap<SessionId, SessionRuntime>>> {
        self.runtimes
            .lock()
            .map_err(|_| anyhow!("session runtime mutex poisoned"))
    }
}

fn should_register_checkpoint(result_status: &StepOutcomeStatus) -> bool {
    matches!(
        result_status,
        StepOutcomeStatus::Success | StepOutcomeStatus::Failure
    )
}

fn rollback_description(request: &RollbackRequest) -> String {
    if let Some(checkpoint_id) = &request.checkpoint_id {
        return format!("Rollback to checkpoint {checkpoint_id}");
    }

    match request.steps_back {
        Some(steps) => format!("Rollback last {steps} step(s)"),
        None => "Rollback recorded steps".to_string(),
    }
}

fn rollback_reason(request: &RollbackRequest) -> String {
    request
        .reason
        .clone()
        .unwrap_or_else(|| rollback_description(request))
}

fn next_step_number(previous_steps: &[StepSummary]) -> u32 {
    previous_steps
        .last()
        .map(|step| step.step_number.saturating_add(1))
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;
    use std::process::Command;
    use tempfile::TempDir;

    fn safe_action() -> StepAction {
        StepAction {
            action_type: ActionType::Read,
            tool_name: Some("read_file".to_string()),
            arguments: json!({"path": "src/lib.rs"}),
            target_files: vec!["src/lib.rs".to_string()],
            description: "Inspect lib.rs".to_string(),
        }
    }

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
        std::fs::write(path.join("notes.txt"), "baseline").unwrap();
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

    #[tokio::test]
    async fn begin_session_verify_and_record_flow() {
        let storage = Storage::in_memory().await.unwrap();
        let engine = ExecutionEngine::new(CarapaceConfig::default(), storage);

        let begin = engine
            .begin_session(BeginSessionRequest {
                session_id: Some("session-1".to_string()),
                agent_name: Some("test-agent".to_string()),
                working_dir: "/workspace".to_string(),
            })
            .await
            .unwrap();

        assert_eq!(begin.session.session_id, "session-1");
        assert_eq!(begin.session.working_dir, "/workspace");

        let verification = engine
            .verify_step(VerifyStepRequest {
                session_id: "session-1".to_string(),
                step_number: None,
                plan: Some("Inspect project".to_string()),
                action: safe_action(),
            })
            .await
            .unwrap();

        assert_eq!(verification.step_number, 1);
        assert!(matches!(
            verification.verification.decision,
            VerificationDecision::Pass
        ));

        let recorded = engine
            .record_step(RecordStepRequest {
                session_id: "session-1".to_string(),
                step_number: Some(verification.step_number),
                plan: Some("Inspect project".to_string()),
                action: safe_action(),
                reason: Some("agent requested read".to_string()),
                checkpoint_id: None,
                result_status: StepOutcomeStatus::Success,
                result_message: None,
                tokens_used: 42,
                cost_usd: 0.12,
                duration_ms: 15,
            })
            .await
            .unwrap();

        assert_eq!(recorded.step_number, 1);
        assert_eq!(recorded.result, StepResult::Success);

        let summary = engine.session_summary("session-1").await.unwrap();
        assert_eq!(summary.total_steps, 1);
        assert_eq!(summary.successful_steps, 1);
        assert_eq!(summary.failed_steps, 0);
        assert_eq!(summary.total_tokens, 42);
    }

    #[tokio::test]
    async fn verify_step_uses_rules_from_config() {
        let storage = Storage::in_memory().await.unwrap();
        let mut config = CarapaceConfig::default();
        config.verification.consistency_enabled = false;
        config.verification.blocked_paths = vec!["/workspace/secret.txt".to_string()];
        let engine = ExecutionEngine::new(config, storage);

        engine
            .begin_session(BeginSessionRequest {
                session_id: Some("session-2".to_string()),
                agent_name: None,
                working_dir: "/workspace".to_string(),
            })
            .await
            .unwrap();

        let verification = engine
            .verify_step(VerifyStepRequest {
                session_id: "session-2".to_string(),
                step_number: None,
                plan: None,
                action: StepAction {
                    action_type: ActionType::Write,
                    tool_name: Some("write_file".to_string()),
                    arguments: json!({"path": "/workspace/secret.txt"}),
                    target_files: vec!["/workspace/secret.txt".to_string()],
                    description: "Overwrite secret".to_string(),
                },
            })
            .await
            .unwrap();

        assert!(matches!(
            verification.verification.decision,
            VerificationDecision::Fail { .. }
        ));
    }

    #[tokio::test]
    async fn save_checkpoint_and_rollback_restore_file() {
        let (_dir, path) = init_repo();
        let storage = Storage::in_memory().await.unwrap();
        let engine = ExecutionEngine::new(CarapaceConfig::default(), storage);
        let working_dir = path.display().to_string();

        engine
            .begin_session(BeginSessionRequest {
                session_id: Some("session-3".to_string()),
                agent_name: Some("test-agent".to_string()),
                working_dir,
            })
            .await
            .unwrap();

        let action = StepAction {
            action_type: ActionType::Write,
            tool_name: Some("edit_file".to_string()),
            arguments: json!({"path": "notes.txt"}),
            target_files: vec!["notes.txt".to_string()],
            description: "Update notes".to_string(),
        };

        let checkpoint = engine
            .save_checkpoint(SaveCheckpointRequest {
                session_id: "session-3".to_string(),
                action: action.clone(),
            })
            .await
            .unwrap();

        std::fs::write(path.join("notes.txt"), "changed").unwrap();

        let recorded = engine
            .record_step(RecordStepRequest {
                session_id: "session-3".to_string(),
                step_number: None,
                plan: Some("Update notes safely".to_string()),
                action,
                reason: Some("simulate risky write".to_string()),
                checkpoint_id: Some(checkpoint.checkpoint_id.clone()),
                result_status: StepOutcomeStatus::Failure,
                result_message: Some("simulated execution failure".to_string()),
                tokens_used: 10,
                cost_usd: 0.01,
                duration_ms: 5,
            })
            .await
            .unwrap();

        assert_eq!(
            recorded.checkpoint_id.as_deref(),
            Some(checkpoint.checkpoint_id.as_str())
        );
        assert_eq!(std::fs::read_to_string(path.join("notes.txt")).unwrap(), "changed");

        let rollback = engine
            .rollback(RollbackRequest {
                session_id: "session-3".to_string(),
                checkpoint_id: Some(checkpoint.checkpoint_id.clone()),
                steps_back: None,
                reason: Some("restore baseline".to_string()),
            })
            .await
            .unwrap();

        assert_eq!(rollback.rollback.steps_undone, 1);
        assert_eq!(
            std::fs::read_to_string(path.join("notes.txt")).unwrap(),
            "baseline"
        );

        let summary = engine.session_summary("session-3").await.unwrap();
        assert_eq!(summary.failed_steps, 1);
        assert_eq!(summary.rollbacks, 1);
    }
}
