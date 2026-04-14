use crate::types::*;
use std::collections::HashMap;

/// A discovered pattern that correlates with step failure.
#[derive(Debug, Clone)]
pub struct FailurePattern {
    pub name: String,
    pub description: String,
    pub occurrences: u32,
    pub failure_rate: f64,
    pub confidence: f64,
    pub suggestion: LearnedSuggestion,
}

/// What the system recommends based on a discovered pattern.
#[derive(Debug, Clone)]
pub enum LearnedSuggestion {
    /// Add a checkpoint before this action type combination.
    CheckpointBefore { action_types: Vec<String> },
    /// Warn when N consecutive writes happen without a read.
    WarnConsecutiveWrites { threshold: u32 },
    /// Warn when the same file is edited more than N times.
    WarnRepeatedFileEdits { threshold: u32 },
    /// Suggest running tests after source edits.
    SuggestTestAfterEdit,
    /// Suggest a checkpoint every N steps.
    SuggestPeriodicCheckpoint { interval: u32 },
}

/// Extract failure patterns from a collection of session traces.
pub fn analyze_sessions(sessions: &[SessionTrace]) -> Vec<FailurePattern> {
    let mut patterns = Vec::new();

    if let Some(p) = detect_consecutive_writes_without_read(sessions) {
        patterns.push(p);
    }
    if let Some(p) = detect_repeated_file_edits(sessions) {
        patterns.push(p);
    }
    if let Some(p) = detect_missing_test_after_edit(sessions) {
        patterns.push(p);
    }
    if let Some(p) = detect_long_sessions_without_checkpoint(sessions) {
        patterns.push(p);
    }
    if let Some(p) = detect_failure_after_action_type(sessions) {
        patterns.push(p);
    }

    // Sort by confidence descending.
    patterns.sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap_or(std::cmp::Ordering::Equal));
    patterns
}

/// A complete session trace for analysis.
#[derive(Debug, Clone)]
pub struct SessionTrace {
    pub session_id: SessionId,
    pub steps: Vec<TraceEntry>,
}

// ── Pattern detectors ────────────────────────────────────

/// Consecutive writes without a read often indicate the agent is
/// blindly editing without verifying its work.
fn detect_consecutive_writes_without_read(sessions: &[SessionTrace]) -> Option<FailurePattern> {
    let mut total_runs = 0u32;
    let mut failure_runs = 0u32;

    for session in sessions {
        let mut consecutive_writes = 0u32;

        for step in &session.steps {
            match step.action.action_type {
                ActionType::Write => {
                    consecutive_writes += 1;
                    if consecutive_writes >= 3 {
                        total_runs += 1;
                        if !step.result.is_success() {
                            failure_runs += 1;
                        }
                    }
                }
                ActionType::Read | ActionType::Search => {
                    consecutive_writes = 0;
                }
                _ => {}
            }
        }
    }

    if total_runs < 2 {
        return None;
    }

    let failure_rate = failure_runs as f64 / total_runs as f64;
    let confidence = confidence_score(total_runs, failure_rate);

    Some(FailurePattern {
        name: "consecutive_writes_without_read".into(),
        description: format!(
            "3+ consecutive writes without a read: {failure_runs}/{total_runs} failed ({:.0}%)",
            failure_rate * 100.0
        ),
        occurrences: total_runs,
        failure_rate,
        confidence,
        suggestion: LearnedSuggestion::WarnConsecutiveWrites { threshold: 3 },
    })
}

/// Editing the same file multiple times in one session often means
/// the agent is struggling and not converging.
fn detect_repeated_file_edits(sessions: &[SessionTrace]) -> Option<FailurePattern> {
    let mut total_repeats = 0u32;
    let mut failed_repeats = 0u32;

    for session in sessions {
        let mut file_edit_counts: HashMap<&str, u32> = HashMap::new();

        for step in &session.steps {
            if !matches!(step.action.action_type, ActionType::Write) {
                continue;
            }

            for file in &step.action.target_files {
                let count = file_edit_counts.entry(file.as_str()).or_insert(0);
                *count += 1;

                if *count >= 3 {
                    total_repeats += 1;
                    if !step.result.is_success() {
                        failed_repeats += 1;
                    }
                }
            }
        }
    }

    if total_repeats < 2 {
        return None;
    }

    let failure_rate = failed_repeats as f64 / total_repeats as f64;
    let confidence = confidence_score(total_repeats, failure_rate);

    Some(FailurePattern {
        name: "repeated_file_edits".into(),
        description: format!(
            "Same file edited 3+ times: {failed_repeats}/{total_repeats} failed ({:.0}%)",
            failure_rate * 100.0
        ),
        occurrences: total_repeats,
        failure_rate,
        confidence,
        suggestion: LearnedSuggestion::WarnRepeatedFileEdits { threshold: 3 },
    })
}

/// Source file edits without subsequent test execution correlate
/// with undetected regressions.
fn detect_missing_test_after_edit(sessions: &[SessionTrace]) -> Option<FailurePattern> {
    let mut edits_without_test = 0u32;
    let mut edits_with_test = 0u32;
    let mut failures_without_test = 0u32;
    let mut failures_with_test = 0u32;

    for session in sessions {
        let steps = &session.steps;

        for (i, step) in steps.iter().enumerate() {
            if !matches!(step.action.action_type, ActionType::Write) {
                continue;
            }

            let is_source_edit = step.action.target_files.iter().any(|f| {
                let f = f.to_lowercase();
                (f.ends_with(".py") || f.ends_with(".rs") || f.ends_with(".ts") || f.ends_with(".js"))
                    && !f.contains("test")
            });

            if !is_source_edit {
                continue;
            }

            // Look ahead: is there a test execution within the next 3 steps?
            let has_test = steps[i + 1..].iter().take(3).any(|s| {
                matches!(s.action.action_type, ActionType::Execute)
                    && s.action.description.to_lowercase().contains("test")
            });

            if has_test {
                edits_with_test += 1;
                if !step.result.is_success() {
                    failures_with_test += 1;
                }
            } else {
                edits_without_test += 1;
                if !step.result.is_success() {
                    failures_without_test += 1;
                }
            }
        }
    }

    let total = edits_without_test + edits_with_test;
    if total < 3 || edits_without_test < 2 {
        return None;
    }

    let rate_without = if edits_without_test > 0 {
        failures_without_test as f64 / edits_without_test as f64
    } else {
        0.0
    };
    let rate_with = if edits_with_test > 0 {
        failures_with_test as f64 / edits_with_test as f64
    } else {
        0.0
    };

    // Only report if not testing is meaningfully worse.
    if rate_without <= rate_with + 0.1 {
        return None;
    }

    let confidence = confidence_score(edits_without_test, rate_without);

    Some(FailurePattern {
        name: "missing_test_after_edit".into(),
        description: format!(
            "Source edits without test: {:.0}% failure vs {:.0}% with test ({} edits observed)",
            rate_without * 100.0,
            rate_with * 100.0,
            total,
        ),
        occurrences: edits_without_test,
        failure_rate: rate_without,
        confidence,
        suggestion: LearnedSuggestion::SuggestTestAfterEdit,
    })
}

/// Long sessions without any checkpoint have higher failure rates
/// because there's no recovery point.
fn detect_long_sessions_without_checkpoint(sessions: &[SessionTrace]) -> Option<FailurePattern> {
    let mut long_no_cp = 0u32;
    let mut long_with_cp = 0u32;
    let mut failures_no_cp = 0u32;
    let mut failures_with_cp = 0u32;
    let threshold = 5;

    for session in sessions {
        if session.steps.len() < threshold {
            continue;
        }

        let has_checkpoint = session.steps.iter().any(|s| s.checkpoint_id.is_some());
        let has_failure = session.steps.iter().any(|s| !s.result.is_success());

        if has_checkpoint {
            long_with_cp += 1;
            if has_failure {
                failures_with_cp += 1;
            }
        } else {
            long_no_cp += 1;
            if has_failure {
                failures_no_cp += 1;
            }
        }
    }

    if long_no_cp < 2 {
        return None;
    }

    let rate_no_cp = failures_no_cp as f64 / long_no_cp as f64;
    let confidence = confidence_score(long_no_cp, rate_no_cp);

    Some(FailurePattern {
        name: "long_session_without_checkpoint".into(),
        description: format!(
            "Sessions with {}+ steps and no checkpoint: {}/{} had failures ({:.0}%)",
            threshold, failures_no_cp, long_no_cp, rate_no_cp * 100.0,
        ),
        occurrences: long_no_cp,
        failure_rate: rate_no_cp,
        confidence,
        suggestion: LearnedSuggestion::SuggestPeriodicCheckpoint { interval: 5 },
    })
}

/// Which action types have the highest failure rates?
fn detect_failure_after_action_type(sessions: &[SessionTrace]) -> Option<FailurePattern> {
    let mut action_counts: HashMap<String, (u32, u32)> = HashMap::new(); // (total, failures)

    for session in sessions {
        for step in &session.steps {
            let key = step.action.action_type.as_str().to_string();
            let entry = action_counts.entry(key).or_insert((0, 0));
            entry.0 += 1;
            if !step.result.is_success() {
                entry.1 += 1;
            }
        }
    }

    // Find the action type with the highest failure rate (min 3 occurrences).
    let worst = action_counts
        .iter()
        .filter(|(_, (total, _))| *total >= 3)
        .max_by(|(_, (t1, f1)), (_, (t2, f2))| {
            let r1 = *f1 as f64 / *t1 as f64;
            let r2 = *f2 as f64 / *t2 as f64;
            r1.partial_cmp(&r2).unwrap_or(std::cmp::Ordering::Equal)
        });

    let (action_type, (total, failures)) = worst?;
    let failure_rate = *failures as f64 / *total as f64;

    if failure_rate < 0.15 {
        return None;
    }

    let confidence = confidence_score(*total, failure_rate);

    Some(FailurePattern {
        name: "high_failure_action_type".into(),
        description: format!(
            "Action type '{}': {}/{} failed ({:.0}%)",
            action_type, failures, total, failure_rate * 100.0,
        ),
        occurrences: *total,
        failure_rate,
        confidence,
        suggestion: LearnedSuggestion::CheckpointBefore {
            action_types: vec![action_type.clone()],
        },
    })
}

/// Simple confidence score: higher with more data and higher failure rate.
fn confidence_score(occurrences: u32, failure_rate: f64) -> f64 {
    let sample_factor = 1.0 - (1.0 / (1.0 + occurrences as f64));
    let rate_factor = failure_rate;
    (sample_factor * rate_factor).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;

    fn make_step(
        num: u32,
        action_type: ActionType,
        desc: &str,
        files: Vec<&str>,
        success: bool,
        checkpoint: bool,
    ) -> TraceEntry {
        TraceEntry {
            step_id: format!("step-{num}"),
            session_id: "test-session".into(),
            step_number: num,
            action: StepAction {
                action_type,
                tool_name: None,
                arguments: json!({}),
                target_files: files.into_iter().map(String::from).collect(),
                description: desc.into(),
            },
            reason: None,
            verification: VerificationOutcome {
                decision: VerificationDecision::Pass,
                checks_performed: vec![],
                duration_ms: 0,
            },
            checkpoint_id: if checkpoint { Some(format!("cp-{num}")) } else { None },
            result: if success {
                StepResult::Success
            } else {
                StepResult::Failure { error: "test failure".into() }
            },
            tokens_used: 100,
            cost_usd: 0.01,
            duration_ms: 10,
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn detects_consecutive_writes() {
        let sessions = vec![
            SessionTrace {
                session_id: "s1".into(),
                steps: vec![
                    make_step(1, ActionType::Write, "edit a", vec!["a.py"], true, false),
                    make_step(2, ActionType::Write, "edit b", vec!["b.py"], true, false),
                    make_step(3, ActionType::Write, "edit c", vec!["c.py"], false, false),
                    make_step(4, ActionType::Write, "edit d", vec!["d.py"], false, false),
                ],
            },
            SessionTrace {
                session_id: "s2".into(),
                steps: vec![
                    make_step(1, ActionType::Write, "edit a", vec!["a.py"], true, false),
                    make_step(2, ActionType::Write, "edit b", vec!["b.py"], true, false),
                    make_step(3, ActionType::Write, "edit c", vec!["c.py"], false, false),
                ],
            },
        ];

        let patterns = analyze_sessions(&sessions);
        let found = patterns.iter().find(|p| p.name == "consecutive_writes_without_read");
        assert!(found.is_some(), "should detect consecutive writes pattern");
        assert!(found.unwrap().failure_rate > 0.0);
    }

    #[test]
    fn detects_high_failure_action_type() {
        let sessions = vec![SessionTrace {
            session_id: "s1".into(),
            steps: vec![
                make_step(1, ActionType::Read, "read", vec!["a.py"], true, false),
                make_step(2, ActionType::Read, "read", vec!["b.py"], true, false),
                make_step(3, ActionType::Read, "read", vec!["c.py"], true, false),
                make_step(4, ActionType::Execute, "run", vec![], false, false),
                make_step(5, ActionType::Execute, "run", vec![], false, false),
                make_step(6, ActionType::Execute, "run", vec![], true, false),
            ],
        }];

        let patterns = analyze_sessions(&sessions);
        let found = patterns.iter().find(|p| p.name == "high_failure_action_type");
        assert!(found.is_some());
        assert!(found.unwrap().description.contains("execute"));
    }

    #[test]
    fn no_patterns_from_clean_sessions() {
        let sessions = vec![SessionTrace {
            session_id: "s1".into(),
            steps: vec![
                make_step(1, ActionType::Read, "read", vec!["a.py"], true, false),
                make_step(2, ActionType::Write, "write", vec!["a.py"], true, true),
            ],
        }];

        let patterns = analyze_sessions(&sessions);
        assert!(patterns.is_empty(), "clean session should produce no warnings");
    }
}
