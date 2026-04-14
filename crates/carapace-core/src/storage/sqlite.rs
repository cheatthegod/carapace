use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;
use std::path::Path;

use crate::types::*;

const MIGRATION_SQL: &str = include_str!("migrations/001_initial.sql");

/// SQLite-backed storage for traces, checkpoints, and sessions.
#[derive(Clone)]
pub struct Storage {
    pool: SqlitePool,
}

impl Storage {
    /// Create a new storage instance. Creates the database file if needed.
    pub async fn new(db_path: &str) -> Result<Self> {
        if let Some(parent) = Path::new(db_path).parent() {
            std::fs::create_dir_all(parent)?;
        }

        let options = SqliteConnectOptions::new()
            .filename(db_path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(std::time::Duration::from_secs(5));

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .context("Failed to connect to database")?;

        let storage = Self { pool };
        storage.initialize().await?;
        Ok(storage)
    }

    /// Create an in-memory storage instance for testing.
    pub async fn in_memory() -> Result<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        let storage = Self { pool };
        storage.initialize().await?;
        Ok(storage)
    }

    async fn initialize(&self) -> Result<()> {
        sqlx::raw_sql(MIGRATION_SQL)
            .execute(&self.pool)
            .await
            .context("Failed to run migrations")?;
        Ok(())
    }

    // ── Sessions ─────────────────────────────────────────────

    pub async fn create_session(
        &self,
        id: &str,
        agent_name: Option<&str>,
        working_dir: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO sessions (id, agent_name, working_dir) VALUES (?, ?, ?)",
        )
        .bind(id)
        .bind(agent_name)
        .bind(working_dir)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_session_status(&self, id: &str, status: &str) -> Result<()> {
        sqlx::query("UPDATE sessions SET status = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(status)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get_session(&self, id: &str) -> Result<Option<SessionRecord>> {
        let row = sqlx::query_as::<_, SessionRow>(
            "SELECT id, agent_name, working_dir, status FROM sessions WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|row| row.into_session_record()))
    }

    // ── Steps ────────────────────────────────────────────────

    pub async fn insert_step(&self, entry: &TraceEntry) -> Result<()> {
        let action_detail = serde_json::to_string(&entry.action)?;
        let verification_result = verification_label(&entry.verification.decision);
        let verification_detail = serde_json::to_string(&entry.verification)?;
        let result = result_label(&entry.result);
        let result_detail = serde_json::to_string(&entry.result)?;

        sqlx::query(
            "INSERT INTO steps (id, session_id, step_number, action_type, action_detail, reason, \
             verification_result, verification_detail, checkpoint_id, result, result_detail, \
             tokens_used, cost_usd, duration_ms) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&entry.step_id)
        .bind(&entry.session_id)
        .bind(entry.step_number as i64)
        .bind(entry.action.action_type.as_str())
        .bind(&action_detail)
        .bind(&entry.reason)
        .bind(verification_result)
        .bind(&verification_detail)
        .bind(&entry.checkpoint_id)
        .bind(result)
        .bind(&result_detail)
        .bind(entry.tokens_used as i64)
        .bind(entry.cost_usd)
        .bind(entry.duration_ms as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_session_steps(&self, session_id: &str) -> Result<Vec<TraceEntry>> {
        let rows = sqlx::query_as::<_, StepRow>(
            "SELECT id, session_id, step_number, action_detail, reason, \
             verification_detail, checkpoint_id, result_detail, \
             tokens_used, cost_usd, duration_ms, created_at \
             FROM steps WHERE session_id = ? ORDER BY step_number",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(|r| r.into_trace_entry()).collect()
    }

    pub async fn get_recent_steps(&self, session_id: &str, count: u32) -> Result<Vec<TraceEntry>> {
        let rows = sqlx::query_as::<_, StepRow>(
            "SELECT id, session_id, step_number, action_detail, reason, \
             verification_detail, checkpoint_id, result_detail, \
             tokens_used, cost_usd, duration_ms, created_at \
             FROM steps WHERE session_id = ? ORDER BY step_number DESC LIMIT ?",
        )
        .bind(session_id)
        .bind(count as i64)
        .fetch_all(&self.pool)
        .await?;

        let mut entries: Vec<TraceEntry> = rows
            .into_iter()
            .map(|r| r.into_trace_entry())
            .collect::<Result<Vec<_>>>()?;
        entries.reverse();
        Ok(entries)
    }

    pub async fn get_previous_summaries(
        &self,
        session_id: &str,
        count: u32,
    ) -> Result<Vec<StepSummary>> {
        let entries = self.get_recent_steps(session_id, count).await?;
        Ok(entries.iter().map(StepSummary::from).collect())
    }

    pub async fn get_session_summary(&self, session_id: &str) -> Result<SessionSummary> {
        let row = sqlx::query_as::<_, SummaryRow>(
            "SELECT \
                COUNT(*) as total_steps, \
                COALESCE(SUM(CASE WHEN result = 'success' THEN 1 ELSE 0 END), 0) as successful_steps, \
                COALESCE(SUM(CASE WHEN result = 'failure' THEN 1 ELSE 0 END), 0) as failed_steps, \
                COALESCE(SUM(CASE WHEN result = 'rolled_back' THEN 1 ELSE 0 END), 0) as rollbacks, \
                COALESCE(SUM(CASE WHEN verification_result != 'pass' THEN 1 ELSE 0 END), 0) as interceptions, \
                COALESCE(SUM(tokens_used), 0) as total_tokens, \
                COALESCE(SUM(cost_usd), 0.0) as total_cost, \
                COALESCE(SUM(duration_ms), 0) as total_duration \
             FROM steps WHERE session_id = ?",
        )
        .bind(session_id)
        .fetch_one(&self.pool)
        .await?;

        let anomaly_count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM anomalies WHERE session_id = ?")
                .bind(session_id)
                .fetch_one(&self.pool)
                .await?;

        Ok(SessionSummary {
            session_id: session_id.to_string(),
            total_steps: row.total_steps as u32,
            successful_steps: row.successful_steps as u32,
            failed_steps: row.failed_steps as u32,
            rollbacks: row.rollbacks as u32,
            verifier_interceptions: row.interceptions as u32,
            anomalies_detected: anomaly_count.0 as u32,
            total_tokens: row.total_tokens as u64,
            total_cost_usd: row.total_cost,
            total_duration_ms: row.total_duration as u64,
        })
    }

    // ── Anomalies ────────────────────────────────────────────

    pub async fn insert_anomaly(&self, session_id: &str, anomaly: &Anomaly) -> Result<()> {
        let id = new_id();
        sqlx::query(
            "INSERT INTO anomalies (id, session_id, step_id, anomaly_type, severity, detail) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(session_id)
        .bind(&anomaly.step_id)
        .bind(anomaly_type_label(&anomaly.anomaly_type))
        .bind(severity_label(&anomaly.severity))
        .bind(&anomaly.detail)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ── Checkpoints ──────────────────────────────────────────

    pub async fn insert_checkpoint(&self, cp: &Checkpoint) -> Result<()> {
        sqlx::query(
            "INSERT INTO checkpoints (id, session_id, step_id, checkpoint_type, reference, files_affected) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&cp.id)
        .bind(&cp.session_id)
        .bind(&cp.step_id)
        .bind(checkpoint_type_label(&cp.checkpoint_type))
        .bind(&cp.reference)
        .bind(serde_json::to_string(&cp.files_affected)?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_checkpoint(&self, id: &str) -> Result<Option<Checkpoint>> {
        let row = sqlx::query_as::<_, CheckpointRow>(
            "SELECT id, session_id, step_id, checkpoint_type, reference, files_affected, created_at \
             FROM checkpoints WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        row.map(CheckpointRow::into_checkpoint).transpose()
    }
}

// ── Internal row types for sqlx ──────────────────────────────

#[derive(sqlx::FromRow)]
struct StepRow {
    id: String,
    session_id: String,
    step_number: i64,
    action_detail: String,
    reason: Option<String>,
    verification_detail: String,
    checkpoint_id: Option<String>,
    result_detail: String,
    tokens_used: i64,
    cost_usd: f64,
    duration_ms: i64,
    created_at: String,
}

#[derive(sqlx::FromRow)]
struct SessionRow {
    id: String,
    agent_name: Option<String>,
    working_dir: String,
    status: String,
}

impl SessionRow {
    fn into_session_record(self) -> SessionRecord {
        SessionRecord {
            session_id: self.id,
            agent_name: self.agent_name,
            working_dir: self.working_dir,
            status: self.status,
        }
    }
}

impl StepRow {
    fn into_trace_entry(self) -> Result<TraceEntry> {
        let action: StepAction = serde_json::from_str(&self.action_detail)?;
        let verification: VerificationOutcome = serde_json::from_str(&self.verification_detail)?;
        let result: StepResult = serde_json::from_str(&self.result_detail)?;
        let timestamp = chrono::NaiveDateTime::parse_from_str(&self.created_at, "%Y-%m-%d %H:%M:%S")
            .map(|dt| dt.and_utc())
            .unwrap_or_else(|_| chrono::Utc::now());

        Ok(TraceEntry {
            step_id: self.id,
            session_id: self.session_id,
            step_number: self.step_number as u32,
            action,
            reason: self.reason,
            verification,
            checkpoint_id: self.checkpoint_id,
            result,
            tokens_used: self.tokens_used as u64,
            cost_usd: self.cost_usd,
            duration_ms: self.duration_ms as u64,
            timestamp,
        })
    }
}

impl CheckpointRow {
    fn into_checkpoint(self) -> Result<Checkpoint> {
        let checkpoint_type = match self.checkpoint_type.as_str() {
            "git_stash" => CheckpointType::GitStash,
            "git_commit" => CheckpointType::GitCommit,
            "file_copy" => CheckpointType::FileCopy,
            other => anyhow::bail!("unknown checkpoint type: {other}"),
        };
        let files_affected = self
            .files_affected
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?
            .unwrap_or_default();
        let created_at = chrono::NaiveDateTime::parse_from_str(&self.created_at, "%Y-%m-%d %H:%M:%S")
            .map(|dt| dt.and_utc())
            .unwrap_or_else(|_| chrono::Utc::now());

        Ok(Checkpoint {
            id: self.id,
            session_id: self.session_id,
            step_id: self.step_id,
            checkpoint_type,
            reference: self.reference,
            files_affected,
            created_at,
        })
    }
}

#[derive(sqlx::FromRow)]
struct SummaryRow {
    total_steps: i64,
    successful_steps: i64,
    failed_steps: i64,
    rollbacks: i64,
    interceptions: i64,
    total_tokens: i64,
    total_cost: f64,
    total_duration: i64,
}

#[derive(sqlx::FromRow)]
struct CheckpointRow {
    id: String,
    session_id: String,
    step_id: String,
    checkpoint_type: String,
    reference: String,
    files_affected: Option<String>,
    created_at: String,
}

fn verification_label(decision: &VerificationDecision) -> &'static str {
    match decision {
        VerificationDecision::Pass => "pass",
        VerificationDecision::Warn { .. } => "warn",
        VerificationDecision::Fail { .. } => "fail",
    }
}

fn result_label(result: &StepResult) -> &'static str {
    match result {
        StepResult::Success => "success",
        StepResult::Failure { .. } => "failure",
        StepResult::RolledBack { .. } => "rolled_back",
        StepResult::Skipped { .. } => "skipped",
    }
}

fn anomaly_type_label(anomaly_type: &AnomalyType) -> &'static str {
    match anomaly_type {
        AnomalyType::GoalDrift => "goal_drift",
        AnomalyType::LoopTrap => "loop_trap",
        AnomalyType::TokenSpike => "token_spike",
        AnomalyType::PlanDeviation => "plan_deviation",
        AnomalyType::ConsistencyViolation => "consistency_violation",
    }
}

fn severity_label(severity: &Severity) -> &'static str {
    match severity {
        Severity::Info => "info",
        Severity::Warning => "warning",
        Severity::Critical => "critical",
    }
}

fn checkpoint_type_label(checkpoint_type: &CheckpointType) -> &'static str {
    match checkpoint_type {
        CheckpointType::GitStash => "git_stash",
        CheckpointType::GitCommit => "git_commit",
        CheckpointType::FileCopy => "file_copy",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;

    fn sample_entry(session_id: &str, step_number: u32, description: &str) -> TraceEntry {
        TraceEntry {
            step_id: new_id(),
            session_id: session_id.to_string(),
            step_number,
            action: StepAction {
                action_type: ActionType::Write,
                tool_name: Some("edit_file".into()),
                arguments: json!({ "line_count": 1 }),
                target_files: vec![format!("src/file_{step_number}.rs")],
                description: description.into(),
            },
            reason: None,
            verification: VerificationOutcome {
                decision: VerificationDecision::Pass,
                checks_performed: vec![],
                duration_ms: 1,
            },
            checkpoint_id: None,
            result: StepResult::Success,
            tokens_used: 10,
            cost_usd: 0.01,
            duration_ms: 5,
            timestamp: Utc::now(),
        }
    }

    #[tokio::test]
    async fn empty_summary_defaults_to_zero() {
        let storage = Storage::in_memory().await.unwrap();
        storage
            .create_session("session-1", Some("test-agent"), "/tmp/project")
            .await
            .unwrap();

        let summary = storage.get_session_summary("session-1").await.unwrap();

        assert_eq!(summary.total_steps, 0);
        assert_eq!(summary.successful_steps, 0);
        assert_eq!(summary.failed_steps, 0);
        assert_eq!(summary.rollbacks, 0);
        assert_eq!(summary.verifier_interceptions, 0);
        assert_eq!(summary.anomalies_detected, 0);
        assert_eq!(summary.total_tokens, 0);
        assert_eq!(summary.total_cost_usd, 0.0);
        assert_eq!(summary.total_duration_ms, 0);
    }

    #[tokio::test]
    async fn previous_summaries_preserve_step_order() {
        let storage = Storage::in_memory().await.unwrap();
        storage
            .create_session("session-2", Some("test-agent"), "/tmp/project")
            .await
            .unwrap();

        storage
            .insert_step(&sample_entry("session-2", 1, "edit auth config"))
            .await
            .unwrap();
        storage
            .insert_step(&sample_entry("session-2", 2, "edit auth handler"))
            .await
            .unwrap();

        let summaries = storage.get_previous_summaries("session-2", 10).await.unwrap();

        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].step_number, 1);
        assert_eq!(summaries[1].step_number, 2);
        assert_eq!(summaries[0].description, "edit auth config");
        assert_eq!(summaries[1].description, "edit auth handler");
    }

    #[tokio::test]
    async fn checkpoint_round_trip() {
        let storage = Storage::in_memory().await.unwrap();
        storage
            .create_session("session-3", Some("test-agent"), "/tmp/project")
            .await
            .unwrap();

        let entry = sample_entry("session-3", 1, "edit lib");
        storage.insert_step(&entry).await.unwrap();

        let checkpoint = Checkpoint {
            id: "cp-1".to_string(),
            session_id: "session-3".to_string(),
            step_id: entry.step_id.clone(),
            checkpoint_type: CheckpointType::GitCommit,
            reference: "abc123".to_string(),
            files_affected: vec!["src/lib.rs".to_string()],
            created_at: Utc::now(),
        };
        storage.insert_checkpoint(&checkpoint).await.unwrap();

        let loaded = storage.get_checkpoint("cp-1").await.unwrap().unwrap();
        assert_eq!(loaded.id, checkpoint.id);
        assert_eq!(loaded.step_id, checkpoint.step_id);
        assert_eq!(loaded.reference, checkpoint.reference);
        assert_eq!(loaded.files_affected, checkpoint.files_affected);
    }
}
