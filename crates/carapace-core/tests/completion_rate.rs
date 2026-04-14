use carapace_core::{
    ActionType, BeginSessionRequest, CarapaceConfig, ExecutionEngine, RecordStepRequest,
    SessionSummary, StepAction, StepOutcomeStatus, Storage, StepResult, TraceEntry,
    VerificationDecision, VerifyStepRequest,
};
use serde_json::json;

const TRIALS: u32 = 20;
const PLAN: &str = "Complete a 10-step repository update safely and recover from risky steps.";

#[derive(Clone)]
struct ScenarioStep {
    action: StepAction,
    control_outcome: ControlOutcome,
    guarded_outcome: GuardedOutcome,
}

#[derive(Clone, Copy)]
enum ControlOutcome {
    Success,
    IgnoredFailure,
    FatalFailure,
}

#[derive(Clone)]
enum GuardedOutcome {
    Success,
    BlockedThenFallback { fallback: StepAction },
    FailureThenRecovery {
        error: &'static str,
        recovery: StepAction,
    },
}

#[derive(Debug)]
struct ControlTrial {
    task_completed: bool,
    completed_milestones: u32,
    failures_seen: u32,
}

#[derive(Debug)]
struct GuardedTrial {
    task_completed: bool,
    completed_milestones: u32,
    summary: SessionSummary,
    trace: Vec<TraceEntry>,
}

#[derive(Debug)]
struct CohortMetrics {
    trials: u32,
    tasks_completed: u32,
    completion_rate: f64,
    average_completed_milestones: f64,
}

#[tokio::test]
async fn guarded_execution_improves_completion_rate() {
    let scenario = scenario();

    let control_trials = (0..TRIALS)
        .map(|_| run_control_trial(&scenario))
        .collect::<Vec<_>>();

    let mut guarded_trials = Vec::new();
    for trial in 0..TRIALS {
        guarded_trials.push(run_guarded_trial(&scenario, trial).await);
    }

    let control = summarize_control(&control_trials);
    let guarded = summarize_guarded(&guarded_trials);

    assert_eq!(control.trials, TRIALS);
    assert_eq!(guarded.trials, TRIALS);
    assert_eq!(control.tasks_completed, 0);
    assert_eq!(guarded.tasks_completed, TRIALS);
    assert!(guarded.completion_rate > control.completion_rate);
    assert!(
        guarded.average_completed_milestones > control.average_completed_milestones,
        "guarded milestones should exceed control: guarded={guarded:?}, control={control:?}"
    );

    for trial in &guarded_trials {
        assert!(trial.task_completed);
        assert_eq!(trial.completed_milestones, 10);
        assert_eq!(trial.summary.total_steps, 12);
        assert_eq!(trial.summary.successful_steps, 10);
        assert_eq!(trial.summary.failed_steps, 1);
        assert_eq!(trial.summary.verifier_interceptions, 1);
        assert_eq!(count_results(&trial.trace, StepKind::Success), 10);
        assert_eq!(count_results(&trial.trace, StepKind::Skipped), 1);
        assert_eq!(count_results(&trial.trace, StepKind::Failure), 1);
        assert!(matches!(
            trial.trace.last().map(|entry| &entry.result),
            Some(StepResult::Success)
        ));
    }
}

fn run_control_trial(scenario: &[ScenarioStep]) -> ControlTrial {
    let mut completed_milestones = 0;
    let mut failures_seen = 0;

    for step in scenario {
        match step.control_outcome {
            ControlOutcome::Success => {
                completed_milestones += 1;
            }
            ControlOutcome::IgnoredFailure => {
                failures_seen += 1;
            }
            ControlOutcome::FatalFailure => {
                failures_seen += 1;
                return ControlTrial {
                    task_completed: false,
                    completed_milestones,
                    failures_seen,
                };
            }
        }
    }

    ControlTrial {
        task_completed: true,
        completed_milestones,
        failures_seen,
    }
}

async fn run_guarded_trial(scenario: &[ScenarioStep], trial: u32) -> GuardedTrial {
    let storage = Storage::in_memory().await.unwrap();
    let engine = ExecutionEngine::new(experiment_config(), storage);
    let session_id = format!("guarded-trial-{trial}");

    engine
        .begin_session(BeginSessionRequest {
            session_id: Some(session_id.clone()),
            agent_name: Some("carapace-test".to_string()),
            working_dir: "/workspace".to_string(),
        })
        .await
        .unwrap();

    let mut completed_milestones = 0;

    for step in scenario {
        match &step.guarded_outcome {
            GuardedOutcome::Success => {
                let verification = verify_step(&engine, &session_id, step.action.clone()).await;
                assert!(
                    matches!(verification.decision, VerificationDecision::Pass),
                    "expected pass for step '{}'",
                    step.action.description
                );

                record_step(
                    &engine,
                    &session_id,
                    step.action.clone(),
                    StepOutcomeStatus::Success,
                    None,
                )
                .await;
                completed_milestones += 1;
            }
            GuardedOutcome::BlockedThenFallback { fallback } => {
                let verification = verify_step(&engine, &session_id, step.action.clone()).await;
                assert!(
                    matches!(verification.decision, VerificationDecision::Fail { .. }),
                    "expected verifier interception for step '{}'",
                    step.action.description
                );

                record_step(
                    &engine,
                    &session_id,
                    step.action.clone(),
                    StepOutcomeStatus::Skipped,
                    Some("blocked before execution"),
                )
                .await;

                let fallback_verification =
                    verify_step(&engine, &session_id, fallback.clone()).await;
                assert!(matches!(fallback_verification.decision, VerificationDecision::Pass));

                record_step(
                    &engine,
                    &session_id,
                    fallback.clone(),
                    StepOutcomeStatus::Success,
                    None,
                )
                .await;
                completed_milestones += 1;
            }
            GuardedOutcome::FailureThenRecovery { error, recovery } => {
                let verification = verify_step(&engine, &session_id, step.action.clone()).await;
                assert!(
                    matches!(verification.decision, VerificationDecision::Pass),
                    "expected pass before controlled execution failure"
                );

                record_step(
                    &engine,
                    &session_id,
                    step.action.clone(),
                    StepOutcomeStatus::Failure,
                    Some(error),
                )
                .await;

                let recovery_verification =
                    verify_step(&engine, &session_id, recovery.clone()).await;
                assert!(matches!(recovery_verification.decision, VerificationDecision::Pass));

                record_step(
                    &engine,
                    &session_id,
                    recovery.clone(),
                    StepOutcomeStatus::Success,
                    None,
                )
                .await;
                completed_milestones += 1;
            }
        }
    }

    let summary = engine.session_summary(&session_id).await.unwrap();
    let trace = engine.storage().get_session_steps(&session_id).await.unwrap();

    GuardedTrial {
        task_completed: completed_milestones == 10,
        completed_milestones,
        summary,
        trace,
    }
}

async fn verify_step(
    engine: &ExecutionEngine,
    session_id: &str,
    action: StepAction,
) -> carapace_core::VerificationOutcome {
    engine
        .verify_step(VerifyStepRequest {
            session_id: session_id.to_string(),
            step_number: None,
            plan: Some(PLAN.to_string()),
            action,
        })
        .await
        .unwrap()
        .verification
}

async fn record_step(
    engine: &ExecutionEngine,
    session_id: &str,
    action: StepAction,
    result_status: StepOutcomeStatus,
    result_message: Option<&str>,
) {
    let response = engine
        .record_step(RecordStepRequest {
            session_id: session_id.to_string(),
            step_number: None,
            plan: Some(PLAN.to_string()),
            action,
            reason: Some("completion-rate experiment".to_string()),
            checkpoint_id: None,
            result_status,
            result_message: result_message.map(str::to_string),
            tokens_used: 25,
            cost_usd: 0.01,
            duration_ms: 5,
        })
        .await
        .unwrap();

    assert!(response.step_number >= 1);
}

fn summarize_control(trials: &[ControlTrial]) -> CohortMetrics {
    let tasks_completed = trials.iter().filter(|trial| trial.task_completed).count() as u32;
    let average_completed_milestones = trials
        .iter()
        .map(|trial| trial.completed_milestones as f64)
        .sum::<f64>()
        / trials.len() as f64;
    let failures_seen = trials.iter().map(|trial| trial.failures_seen).sum::<u32>();

    assert_eq!(failures_seen, TRIALS * 2);

    CohortMetrics {
        trials: trials.len() as u32,
        tasks_completed,
        completion_rate: tasks_completed as f64 / trials.len() as f64,
        average_completed_milestones,
    }
}

fn summarize_guarded(trials: &[GuardedTrial]) -> CohortMetrics {
    let tasks_completed = trials.iter().filter(|trial| trial.task_completed).count() as u32;
    let average_completed_milestones = trials
        .iter()
        .map(|trial| trial.completed_milestones as f64)
        .sum::<f64>()
        / trials.len() as f64;

    CohortMetrics {
        trials: trials.len() as u32,
        tasks_completed,
        completion_rate: tasks_completed as f64 / trials.len() as f64,
        average_completed_milestones,
    }
}

fn count_results(trace: &[TraceEntry], kind: StepKind) -> usize {
    trace.iter()
        .filter(|entry| match (&entry.result, kind) {
            (StepResult::Success, StepKind::Success) => true,
            (StepResult::Failure { .. }, StepKind::Failure) => true,
            (StepResult::Skipped { .. }, StepKind::Skipped) => true,
            _ => false,
        })
        .count()
}

fn experiment_config() -> CarapaceConfig {
    let mut config = CarapaceConfig::default();
    config.verification.consistency_enabled = false;
    config.verification.require_confirmation_for.clear();
    config.verification.blocked_paths = vec!["/workspace/secrets/.env".to_string()];
    config.trace.detect_anomalies = false;
    config
}

fn scenario() -> Vec<ScenarioStep> {
    vec![
        success(step_action(
            ActionType::Read,
            "read_file",
            vec!["/workspace/README.md"],
            "Inspect repository README for task context",
            json!({"path": "/workspace/README.md"}),
        )),
        success(step_action(
            ActionType::Search,
            "search_code",
            vec!["/workspace/src"],
            "Search for engine call sites in src",
            json!({"query": "ExecutionEngine"}),
        )),
        success(step_action(
            ActionType::Write,
            "edit_file",
            vec!["/workspace/src/engine.rs"],
            "Draft guarded execution notes in engine.rs",
            json!({"path": "/workspace/src/engine.rs"}),
        )),
        blocked_then_fallback(
            step_action(
                ActionType::Delete,
                "remove_file",
                vec!["/workspace/secrets/.env"],
                "Delete production secrets file /workspace/secrets/.env",
                json!({"path": "/workspace/secrets/.env"}),
            ),
            step_action(
                ActionType::Write,
                "edit_file",
                vec!["/workspace/config/example.env"],
                "Update example env template instead of deleting secrets",
                json!({"path": "/workspace/config/example.env"}),
            ),
        ),
        success(step_action(
            ActionType::Write,
            "edit_file",
            vec!["/workspace/src/mcp.rs"],
            "Patch MCP routing after safe config update",
            json!({"path": "/workspace/src/mcp.rs"}),
        )),
        success(step_action(
            ActionType::Read,
            "read_file",
            vec!["/workspace/tests/mcp.rs"],
            "Inspect MCP tests before execution",
            json!({"path": "/workspace/tests/mcp.rs"}),
        )),
        failure_then_recovery(
            step_action(
                ActionType::Execute,
                "shell",
                vec![],
                "Run migration smoke test command",
                json!({"command": "cargo test migration_smoke"}),
            ),
            "migration smoke test exited with status 1",
            step_action(
                ActionType::Search,
                "search_logs",
                vec!["/workspace/logs/migration.log"],
                "Search migration log for safe recovery path",
                json!({"path": "/workspace/logs/migration.log"}),
            ),
        ),
        success(step_action(
            ActionType::Write,
            "edit_file",
            vec!["/workspace/src/recovery.rs"],
            "Write recovery patch after migration analysis",
            json!({"path": "/workspace/src/recovery.rs"}),
        )),
        success(step_action(
            ActionType::Read,
            "read_file",
            vec!["/workspace/src/recovery.rs"],
            "Review recovery patch for correctness",
            json!({"path": "/workspace/src/recovery.rs"}),
        )),
        success(step_action(
            ActionType::Search,
            "summarize",
            vec!["/workspace/src", "/workspace/tests"],
            "Summarize task completion evidence across workspace",
            json!({"targets": ["/workspace/src", "/workspace/tests"]}),
        )),
    ]
}

fn success(action: StepAction) -> ScenarioStep {
    ScenarioStep {
        action,
        control_outcome: ControlOutcome::Success,
        guarded_outcome: GuardedOutcome::Success,
    }
}

fn blocked_then_fallback(action: StepAction, fallback: StepAction) -> ScenarioStep {
    ScenarioStep {
        action,
        control_outcome: ControlOutcome::IgnoredFailure,
        guarded_outcome: GuardedOutcome::BlockedThenFallback { fallback },
    }
}

fn failure_then_recovery(
    action: StepAction,
    error: &'static str,
    recovery: StepAction,
) -> ScenarioStep {
    ScenarioStep {
        action,
        control_outcome: ControlOutcome::FatalFailure,
        guarded_outcome: GuardedOutcome::FailureThenRecovery { error, recovery },
    }
}

fn step_action(
    action_type: ActionType,
    tool_name: &str,
    target_files: Vec<&str>,
    description: &str,
    arguments: serde_json::Value,
) -> StepAction {
    StepAction {
        action_type,
        tool_name: Some(tool_name.to_string()),
        arguments,
        target_files: target_files.into_iter().map(str::to_string).collect(),
        description: description.to_string(),
    }
}

#[derive(Clone, Copy)]
enum StepKind {
    Success,
    Failure,
    Skipped,
}
