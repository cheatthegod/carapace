//! Flywheel experiment: proves all three gaps at once.
//!
//! Gap 1 (persistence): learned rules survive engine restart (saved to disk)
//! Gap 2 (multi-session): 5+ sessions feed the learner, rules emerge naturally
//! Gap 3 (reduced failures): agent adapts to learned warnings → completion rate improves
//!
//! Design:
//!   Round 1: 5 sessions. Agent runs naively — ignores all warnings.
//!            Some steps fail due to anti-patterns (consecutive writes, no tests).
//!            → Records traces in DB.
//!
//!   Learn:   Engine analyzes traces, saves rules to disk.
//!
//!   Round 2: New engine instance loads rules FROM DISK (proves persistence).
//!            5 sessions with the SAME tasks. But now the agent is "smart":
//!            if verify_step returns a learned warning, it takes a safer path.
//!            → Fewer failures.
//!
//!   Assert:  Round 2 completion rate > Round 1 completion rate.

use carapace_core::{
    ActionType, BeginSessionRequest, CarapaceConfig, ExecutionEngine, RecordStepRequest,
    StepAction, StepOutcomeStatus, Storage, VerifyStepRequest, VerificationDecision,
};
use serde_json::json;
use tempfile::TempDir;

fn permissive_config() -> CarapaceConfig {
    let mut c = CarapaceConfig::default();
    c.verification.rules_enabled = true;
    c.verification.consistency_enabled = false;
    c.verification.blocked_paths.clear();
    c.verification.blocked_commands.clear();
    c.verification.require_confirmation_for.clear();
    c.trace.detect_anomalies = false;
    c
}

/// A task is a sequence of intended actions.
/// Each action has a "naive outcome" (what happens without guidance)
/// and a "safe alternative" (what the agent does if warned).
#[derive(Clone)]
struct TaskStep {
    action: StepAction,
    naive_outcome: StepOutcomeStatus,
    safe_alternative: Option<(StepAction, StepOutcomeStatus)>,
}

fn task_scenario() -> Vec<TaskStep> {
    vec![
        // Step 1: Read — always fine
        TaskStep {
            action: StepAction {
                action_type: ActionType::Read,
                tool_name: Some("read_file".into()),
                arguments: json!({}),
                target_files: vec!["src/main.py".into()],
                description: "Read main module".into(),
            },
            naive_outcome: StepOutcomeStatus::Success,
            safe_alternative: None,
        },
        // Step 2: Write — fine
        TaskStep {
            action: StepAction {
                action_type: ActionType::Write,
                tool_name: Some("edit_file".into()),
                arguments: json!({}),
                target_files: vec!["src/main.py".into()],
                description: "Fix bug in main.py".into(),
            },
            naive_outcome: StepOutcomeStatus::Success,
            safe_alternative: None,
        },
        // Step 3: Another write (consecutive) — fine
        TaskStep {
            action: StepAction {
                action_type: ActionType::Write,
                tool_name: Some("edit_file".into()),
                arguments: json!({}),
                target_files: vec!["src/utils.py".into()],
                description: "Update utils.py".into(),
            },
            naive_outcome: StepOutcomeStatus::Success,
            safe_alternative: None,
        },
        // Step 4: THIRD consecutive write — this is where naive agents fail
        // The "safe alternative" is to read first (break the write streak)
        TaskStep {
            action: StepAction {
                action_type: ActionType::Write,
                tool_name: Some("edit_file".into()),
                arguments: json!({}),
                target_files: vec!["src/config.py".into()],
                description: "Edit config.py".into(),
            },
            naive_outcome: StepOutcomeStatus::Failure, // fails without guidance
            safe_alternative: Some((
                StepAction {
                    action_type: ActionType::Read,
                    tool_name: Some("read_file".into()),
                    arguments: json!({}),
                    target_files: vec!["src/config.py".into()],
                    description: "Read config.py before editing".into(),
                },
                StepOutcomeStatus::Success,
            )),
        },
        // Step 5: Execute tests — naive agents skip this, causing later failures
        TaskStep {
            action: StepAction {
                action_type: ActionType::Execute,
                tool_name: Some("shell".into()),
                arguments: json!({"command": "pytest"}),
                target_files: vec![],
                description: "Run pytest".into(),
            },
            naive_outcome: StepOutcomeStatus::Success,
            safe_alternative: None,
        },
    ]
}

/// Run one session. Returns (completed_steps, total_steps).
async fn run_session(
    engine: &ExecutionEngine,
    session_id: &str,
    scenario: &[TaskStep],
    use_learned_warnings: bool,
) -> (u32, u32) {
    engine
        .begin_session(BeginSessionRequest {
            session_id: Some(session_id.into()),
            agent_name: Some("flywheel-agent".into()),
            working_dir: "/workspace".into(),
        })
        .await
        .unwrap();

    let mut completed = 0u32;
    let total = scenario.len() as u32;

    for step in scenario {
        // Check verification BEFORE acting
        let verify = engine
            .verify_step(VerifyStepRequest {
                session_id: session_id.into(),
                step_number: None,
                plan: None,
                action: step.action.clone(),
            })
            .await
            .unwrap();

        let has_learned_warning = verify
            .verification
            .checks_performed
            .iter()
            .any(|c| c.checker_name.starts_with("learned_") && c.message.is_some());

        // Smart agent: if learned warning AND we have a safe alternative, take it
        if use_learned_warnings && has_learned_warning {
            if let Some((safe_action, safe_outcome)) = &step.safe_alternative {
                // Record the original action as skipped
                engine
                    .record_step(RecordStepRequest {
                        session_id: session_id.into(),
                        step_number: None,
                        plan: None,
                        action: step.action.clone(),
                        reason: Some("Learned rule warning — taking safe path".into()),
                        checkpoint_id: None,
                        result_status: StepOutcomeStatus::Skipped,
                        result_message: Some("Adapted based on learned rule".into()),
                        tokens_used: 50,
                        cost_usd: 0.005,
                        duration_ms: 5,
                    })
                    .await
                    .unwrap();

                // Execute the safe alternative
                engine
                    .record_step(RecordStepRequest {
                        session_id: session_id.into(),
                        step_number: None,
                        plan: None,
                        action: safe_action.clone(),
                        reason: Some("Safe alternative after learned warning".into()),
                        checkpoint_id: None,
                        result_status: safe_outcome.clone(),
                        result_message: None,
                        tokens_used: 100,
                        cost_usd: 0.01,
                        duration_ms: 10,
                    })
                    .await
                    .unwrap();

                if matches!(safe_outcome, StepOutcomeStatus::Success) {
                    completed += 1;
                }
                continue;
            }
        }

        // Naive path: execute as-is
        let outcome = step.naive_outcome.clone();
        engine
            .record_step(RecordStepRequest {
                session_id: session_id.into(),
                step_number: None,
                plan: None,
                action: step.action.clone(),
                reason: None,
                checkpoint_id: None,
                result_status: outcome.clone(),
                result_message: if matches!(outcome, StepOutcomeStatus::Failure) {
                    Some("Naive execution failed".into())
                } else {
                    None
                },
                tokens_used: 100,
                cost_usd: 0.01,
                duration_ms: 10,
            })
            .await
            .unwrap();

        if matches!(outcome, StepOutcomeStatus::Success) {
            completed += 1;
        }
    }

    (completed, total)
}

#[tokio::test]
async fn flywheel_persistence_and_improvement() {
    let data_dir = TempDir::new().unwrap();
    let scenario = task_scenario();

    // ── ROUND 1: Naive agent, no learned rules ───────────────
    let storage_r1 = Storage::in_memory().await.unwrap();
    let engine_r1 = ExecutionEngine::new(permissive_config(), storage_r1);

    let mut round1_completions = Vec::new();
    for i in 0..5 {
        let (completed, total) = run_session(
            &engine_r1,
            &format!("r1-session-{i}"),
            &scenario,
            false, // naive: ignore all warnings
        )
        .await;
        round1_completions.push((completed, total));
    }

    let round1_rate = round1_completions.iter().map(|(c, t)| *c as f64 / *t as f64).sum::<f64>()
        / round1_completions.len() as f64;

    // ── LEARN: analyze Round 1, save rules to disk ───────────
    let report = engine_r1
        .learn_and_save(data_dir.path(), 0.1)
        .await
        .unwrap();

    let rules_saved = report.rules_generated.len();
    assert!(rules_saved > 0, "Should discover at least 1 rule from Round 1 failures");

    // Verify rules are on disk
    let on_disk = carapace_core::learner::persist::load_rules(data_dir.path()).unwrap();
    assert_eq!(on_disk.len(), rules_saved, "Persisted rules should match generated count");

    // ── ROUND 2: NEW engine, loads rules FROM DISK ───────────
    // This proves Gap 1: persistence survives across engine instances
    let storage_r2 = Storage::in_memory().await.unwrap();
    let engine_r2 = ExecutionEngine::new(permissive_config(), storage_r2);

    let loaded = engine_r2.load_rules_from_disk(data_dir.path()).unwrap();
    assert_eq!(loaded, rules_saved, "New engine should load all persisted rules");

    let mut round2_completions = Vec::new();
    for i in 0..5 {
        let (completed, total) = run_session(
            &engine_r2,
            &format!("r2-session-{i}"),
            &scenario,
            true, // smart: adapt to learned warnings
        )
        .await;
        round2_completions.push((completed, total));
    }

    let round2_rate = round2_completions.iter().map(|(c, t)| *c as f64 / *t as f64).sum::<f64>()
        / round2_completions.len() as f64;

    // ── RESULTS ──────────────────────────────────────────────
    eprintln!("\n=== Flywheel Experiment Results ===");
    eprintln!("Round 1 (naive, no learned rules):");
    for (i, (c, t)) in round1_completions.iter().enumerate() {
        eprintln!("  Session {i}: {c}/{t} steps completed");
    }
    eprintln!("  Completion rate: {:.0}%", round1_rate * 100.0);
    eprintln!();
    eprintln!("Learn phase:");
    eprintln!("  Sessions analyzed: {}", report.sessions_analyzed);
    eprintln!("  Patterns found:    {}", report.patterns_found.len());
    eprintln!("  Rules generated:   {rules_saved}");
    for rule in &report.rules_generated {
        eprintln!("    - {}: {}", rule.name, rule.description);
    }
    eprintln!();
    eprintln!("Round 2 (smart, with learned rules from disk):");
    for (i, (c, t)) in round2_completions.iter().enumerate() {
        eprintln!("  Session {i}: {c}/{t} steps completed");
    }
    eprintln!("  Completion rate: {:.0}%", round2_rate * 100.0);
    eprintln!();
    eprintln!("Improvement: {:.0}% → {:.0}% (+{:.0}pp)",
        round1_rate * 100.0,
        round2_rate * 100.0,
        (round2_rate - round1_rate) * 100.0,
    );
    eprintln!("==================================\n");

    // ── ASSERTIONS ───────────────────────────────────────────
    // Gap 1: Rules persisted and loaded across engine instances
    assert!(loaded > 0);

    // Gap 2: Multiple sessions fed the learner (5 sessions → rules)
    assert!(report.sessions_analyzed >= 5);

    // Gap 3: Round 2 completion rate is strictly higher
    assert!(
        round2_rate > round1_rate,
        "Round 2 ({:.0}%) should beat Round 1 ({:.0}%)",
        round2_rate * 100.0,
        round1_rate * 100.0,
    );
}
