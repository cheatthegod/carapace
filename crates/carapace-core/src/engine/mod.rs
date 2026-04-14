use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};

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

#[derive(Clone)]
pub struct ExecutionEngine {
    config: CarapaceConfig,
    storage: Storage,
}

impl ExecutionEngine {
    pub fn new(config: CarapaceConfig, storage: Storage) -> Self {
        Self { config, storage }
    }

    pub fn storage(&self) -> &Storage {
        &self.storage
    }

    pub async fn begin_session(&self, request: BeginSessionRequest) -> Result<BeginSessionResponse> {
        let session_id = request.session_id.unwrap_or_else(new_id);

        if let Some(existing) = self.storage.get_session(&session_id).await? {
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

    pub async fn record_step(&self, request: RecordStepRequest) -> Result<RecordStepResponse> {
        let (_, context) = self
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

        let entry = TraceEntry {
            step_id: step_id.clone(),
            session_id: context.session_id.clone(),
            step_number: context.step_number,
            action: request.action,
            reason: request.reason,
            verification: verification.clone(),
            checkpoint_id: request.checkpoint_id.clone(),
            result: result.clone(),
            tokens_used: request.tokens_used,
            cost_usd: request.cost_usd,
            duration_ms: request.duration_ms,
            timestamp: Utc::now(),
        };

        let anomalies = self.record_entry(entry).await?;

        Ok(RecordStepResponse {
            step_id,
            session_id: context.session_id,
            step_number: context.step_number,
            verification,
            checkpoint_id: request.checkpoint_id,
            result,
            anomalies,
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

        let verifier = CompositeVerifier::new(self.config.verification.clone());
        verifier.verify(action, context)
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

    fn safe_action() -> StepAction {
        StepAction {
            action_type: ActionType::Read,
            tool_name: Some("read_file".to_string()),
            arguments: json!({"path": "src/lib.rs"}),
            target_files: vec!["src/lib.rs".to_string()],
            description: "Inspect lib.rs".to_string(),
        }
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
}
