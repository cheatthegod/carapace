use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use uuid::Uuid;

// ── Identifiers ──────────────────────────────────────────────

pub type SessionId = String;
pub type StepId = String;
pub type CheckpointId = String;

pub fn new_id() -> String {
    Uuid::new_v4().to_string()
}

// ── Step Action ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ActionType {
    Read,
    Write,
    Delete,
    Execute,
    ApiCall,
    Search,
    Other(String),
}

impl ActionType {
    pub fn as_str(&self) -> &str {
        match self {
            ActionType::Read => "read",
            ActionType::Write => "write",
            ActionType::Delete => "delete",
            ActionType::Execute => "execute",
            ActionType::ApiCall => "api_call",
            ActionType::Search => "search",
            ActionType::Other(value) => value.as_str(),
        }
    }

    pub fn risk_level(&self) -> RiskLevel {
        match self {
            ActionType::Read | ActionType::Search => RiskLevel::Low,
            ActionType::Write | ActionType::ApiCall => RiskLevel::Medium,
            ActionType::Execute => RiskLevel::High,
            ActionType::Delete => RiskLevel::Critical,
            ActionType::Other(_) => RiskLevel::Medium,
        }
    }
}

impl fmt::Display for ActionType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ActionType {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let trimmed = value.trim();
        let normalized = trimmed.to_ascii_lowercase();

        match normalized.as_str() {
            "read" => Ok(ActionType::Read),
            "write" => Ok(ActionType::Write),
            "delete" => Ok(ActionType::Delete),
            "execute" => Ok(ActionType::Execute),
            "api_call" => Ok(ActionType::ApiCall),
            "search" => Ok(ActionType::Search),
            _ if !trimmed.is_empty() => Ok(ActionType::Other(trimmed.to_string())),
            _ => Err("action type must not be empty".to_string()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepAction {
    pub action_type: ActionType,
    pub tool_name: Option<String>,
    pub arguments: serde_json::Value,
    pub target_files: Vec<String>,
    pub description: String,
}

// ── Execution Context ────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionContext {
    pub session_id: SessionId,
    pub step_number: u32,
    pub working_dir: String,
    pub agent_name: Option<String>,
    pub plan: Option<String>,
    pub previous_steps: Vec<StepSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub session_id: SessionId,
    pub agent_name: Option<String>,
    pub working_dir: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepSummary {
    pub step_number: u32,
    pub action_type: ActionType,
    pub description: String,
    pub result: StepResult,
}

impl From<&TraceEntry> for StepSummary {
    fn from(entry: &TraceEntry) -> Self {
        Self {
            step_number: entry.step_number,
            action_type: entry.action.action_type.clone(),
            description: entry.action.description.clone(),
            result: entry.result.clone(),
        }
    }
}

// ── Verification ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationOutcome {
    pub decision: VerificationDecision,
    pub checks_performed: Vec<CheckResult>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationDecision {
    Pass,
    Warn { reasons: Vec<String> },
    Fail { reasons: Vec<String>, suggestions: Vec<String> },
}

impl VerificationDecision {
    pub fn is_pass(&self) -> bool {
        matches!(self, VerificationDecision::Pass)
    }

    pub fn is_fail(&self) -> bool {
        matches!(self, VerificationDecision::Fail { .. })
    }

    pub fn reasons(&self) -> &[String] {
        match self {
            VerificationDecision::Pass => &[],
            VerificationDecision::Warn { reasons } => reasons,
            VerificationDecision::Fail { reasons, .. } => reasons,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
    pub checker_name: String,
    pub passed: bool,
    pub message: Option<String>,
}

// ── Step Result ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum StepResult {
    Success,
    Failure { error: String },
    RolledBack { reason: String },
    Skipped { reason: String },
}

impl StepResult {
    pub fn is_success(&self) -> bool {
        matches!(self, StepResult::Success)
    }
}

// ── Trace Entry ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEntry {
    pub step_id: StepId,
    pub session_id: SessionId,
    pub step_number: u32,
    pub action: StepAction,
    pub reason: Option<String>,
    pub verification: VerificationOutcome,
    pub checkpoint_id: Option<CheckpointId>,
    pub result: StepResult,
    pub tokens_used: u64,
    pub cost_usd: f64,
    pub duration_ms: u64,
    pub timestamp: DateTime<Utc>,
}

// ── Checkpoint ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointType {
    GitStash,
    GitCommit,
    FileCopy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub id: CheckpointId,
    pub session_id: SessionId,
    pub step_id: StepId,
    pub checkpoint_type: CheckpointType,
    pub reference: String,
    pub files_affected: Vec<String>,
    pub created_at: DateTime<Utc>,
}

// ── Saga ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SagaStepRecord {
    pub step_id: StepId,
    pub forward_description: String,
    pub compensate_description: Option<String>,
    pub reversible: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollbackResult {
    pub rolled_back_to_step: u32,
    pub steps_undone: u32,
    pub compensations_executed: u32,
    pub compensations_failed: u32,
}

// ── Anomaly ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AnomalyType {
    GoalDrift,
    LoopTrap,
    TokenSpike,
    PlanDeviation,
    ConsistencyViolation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Anomaly {
    pub anomaly_type: AnomalyType,
    pub severity: Severity,
    pub detail: String,
    pub step_id: Option<StepId>,
    pub detected_at: DateTime<Utc>,
}

// ── Session Summary ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub session_id: SessionId,
    pub total_steps: u32,
    pub successful_steps: u32,
    pub failed_steps: u32,
    pub rollbacks: u32,
    pub verifier_interceptions: u32,
    pub anomalies_detected: u32,
    pub total_tokens: u64,
    pub total_cost_usd: f64,
    pub total_duration_ms: u64,
}
