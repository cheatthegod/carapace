//! Closed-loop experiment: prove that learning from failures improves the next round.
//!
//! Round 1: Run tasks, some fail due to patterns the static verifier doesn't catch.
//! Learn:   Analyze traces, discover patterns, generate rules.
//! Round 2: Run the same tasks with learned rules active.
//! Assert:  Round 2 has more warnings (preventing the failures Round 1 hit).

use carapace_core::{
    ActionType, BeginSessionRequest, CarapaceConfig, ExecutionEngine, RecordStepRequest,
    StepAction, StepOutcomeStatus, Storage, VerifyStepRequest, VerificationDecision,
};
use serde_json::json;

/// Build a config with static rules only (no blocked paths, no confirmation).
/// This ensures Round 1 has zero warnings — the static verifier lets everything through.
fn permissive_config() -> CarapaceConfig {
    let mut config = CarapaceConfig::default();
    config.verification.rules_enabled = true;
    config.verification.consistency_enabled = false;
    config.verification.blocked_paths.clear();
    config.verification.blocked_commands.clear();
    config.verification.require_confirmation_for.clear();
    config.trace.detect_anomalies = false;
    config
}

fn write_action(file: &str, desc: &str) -> StepAction {
    StepAction {
        action_type: ActionType::Write,
        tool_name: Some("edit_file".into()),
        arguments: json!({"path": file}),
        target_files: vec![file.to_string()],
        description: desc.to_string(),
    }
}

fn read_action(file: &str, desc: &str) -> StepAction {
    StepAction {
        action_type: ActionType::Read,
        tool_name: Some("read_file".into()),
        arguments: json!({"path": file}),
        target_files: vec![file.to_string()],
        description: desc.to_string(),
    }
}

fn execute_action(desc: &str) -> StepAction {
    StepAction {
        action_type: ActionType::Execute,
        tool_name: Some("shell".into()),
        arguments: json!({"command": desc}),
        target_files: vec![],
        description: desc.to_string(),
    }
}

/// Seed a session that exhibits the "consecutive writes without read" anti-pattern,
/// ending in failure. This is a realistic scenario: the agent edits 4 files in a row
/// without checking its work, and the last edit breaks something.
async fn seed_failing_session(engine: &ExecutionEngine, session_id: &str) {
    engine
        .begin_session(BeginSessionRequest {
            session_id: Some(session_id.to_string()),
            agent_name: Some("test-agent".into()),
            working_dir: "/workspace".into(),
        })
        .await
        .unwrap();

    // 4 consecutive writes (no reads between them), last one fails
    let actions = vec![
        (write_action("src/a.py", "edit a.py"), StepOutcomeStatus::Success),
        (write_action("src/b.py", "edit b.py"), StepOutcomeStatus::Success),
        (write_action("src/c.py", "edit c.py"), StepOutcomeStatus::Success),
        (write_action("src/d.py", "edit d.py"), StepOutcomeStatus::Failure), // breaks!
    ];

    for (action, status) in actions {
        engine
            .record_step(RecordStepRequest {
                session_id: session_id.to_string(),
                step_number: None,
                plan: None,
                action,
                reason: Some("bulk edit".into()),
                checkpoint_id: None,
                result_status: status.clone(),
                result_message: if matches!(status, StepOutcomeStatus::Failure) {
                    Some("test failure after consecutive edits".into())
                } else {
                    None
                },
                tokens_used: 100,
                cost_usd: 0.01,
                duration_ms: 10,
            })
            .await
            .unwrap();
    }
}

/// Seed a session that exhibits the "execute actions fail often" pattern.
async fn seed_execute_failure_session(engine: &ExecutionEngine, session_id: &str) {
    engine
        .begin_session(BeginSessionRequest {
            session_id: Some(session_id.to_string()),
            agent_name: Some("test-agent".into()),
            working_dir: "/workspace".into(),
        })
        .await
        .unwrap();

    let actions = vec![
        (read_action("src/main.py", "read main"), StepOutcomeStatus::Success),
        (write_action("src/main.py", "edit main"), StepOutcomeStatus::Success),
        (execute_action("cargo test"), StepOutcomeStatus::Failure),
        (write_action("src/main.py", "fix main"), StepOutcomeStatus::Success),
        (execute_action("cargo test"), StepOutcomeStatus::Failure),
        (execute_action("cargo test"), StepOutcomeStatus::Success),
    ];

    for (action, status) in actions {
        engine
            .record_step(RecordStepRequest {
                session_id: session_id.to_string(),
                step_number: None,
                plan: None,
                action,
                reason: None,
                checkpoint_id: None,
                result_status: status.clone(),
                result_message: if matches!(status, StepOutcomeStatus::Failure) {
                    Some("test failed".into())
                } else {
                    None
                },
                tokens_used: 100,
                cost_usd: 0.01,
                duration_ms: 10,
            })
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn learning_loop_improves_verification() {
    let storage = Storage::in_memory().await.unwrap();
    let config = permissive_config();
    let engine = ExecutionEngine::new(config, storage);

    // ── Round 1: Seed sessions with failure patterns ─────────
    // We need at least 2 sessions for the pattern detector's minimum thresholds.
    seed_failing_session(&engine, "round1-session-a").await;
    seed_failing_session(&engine, "round1-session-b").await;
    seed_execute_failure_session(&engine, "round1-session-c").await;
    seed_execute_failure_session(&engine, "round1-session-d").await;

    // Verify: in Round 1, the static verifier would not have warned about
    // consecutive writes or high-failure execute actions.
    let round1_verify = engine
        .verify_step(VerifyStepRequest {
            session_id: "round1-session-a".to_string(),
            step_number: Some(99),
            plan: None,
            action: write_action("src/e.py", "yet another consecutive write"),
        })
        .await
        .unwrap();

    let round1_warnings: Vec<_> = round1_verify
        .verification
        .checks_performed
        .iter()
        .filter(|c| c.message.is_some())
        .collect();

    // Static verifier gives zero warnings for a plain write (permissive config).
    assert!(
        round1_warnings.is_empty(),
        "Round 1 should have no warnings from static verifier, got: {:?}",
        round1_warnings,
    );

    // ── Learn: analyze traces, generate rules ────────────────
    let rules_loaded = engine.load_learned_rules(0.1).await.unwrap();
    assert!(
        rules_loaded > 0,
        "Learner should discover at least 1 rule from the seeded failure data",
    );

    // ── Round 2: verify the same action with learned rules ───
    // Re-create the context that had consecutive writes.
    let round2_verify = engine
        .verify_step(VerifyStepRequest {
            session_id: "round1-session-a".to_string(),
            step_number: Some(99),
            plan: None,
            action: write_action("src/e.py", "yet another consecutive write"),
        })
        .await
        .unwrap();

    let round2_warnings: Vec<_> = round2_verify
        .verification
        .checks_performed
        .iter()
        .filter(|c| c.message.is_some())
        .collect();

    let learned_warnings: Vec<_> = round2_warnings
        .iter()
        .filter(|c| c.checker_name.starts_with("learned_"))
        .collect();

    // After learning, there should be at least one learned warning.
    assert!(
        !learned_warnings.is_empty(),
        "Round 2 should have learned warnings, got checks: {:?}",
        round2_verify.verification.checks_performed,
    );

    // Print what was learned (for human review).
    eprintln!("\n=== Learning Loop Results ===");
    eprintln!("Rules loaded: {}", rules_loaded);
    eprintln!("Round 1 warnings: {}", round1_warnings.len());
    eprintln!("Round 2 warnings: {}", round2_warnings.len());
    for w in &learned_warnings {
        eprintln!("  Learned: [{}] {}", w.checker_name, w.message.as_deref().unwrap_or(""));
    }
    eprintln!("============================\n");

    // ── The core assertion: Round 2 is strictly better ───────
    assert!(
        round2_warnings.len() > round1_warnings.len(),
        "Round 2 must have more warnings than Round 1: {} > {}",
        round2_warnings.len(),
        round1_warnings.len(),
    );
}
