use crate::learner::patterns::{FailurePattern, LearnedSuggestion};
use crate::types::*;
use serde::{Deserialize, Serialize};

/// A verification rule automatically generated from failure pattern analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearnedRule {
    pub name: String,
    pub source_pattern: String,
    pub description: String,
    pub confidence: f64,
    pub check: LearnedCheck,
}

/// The specific check a learned rule performs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LearnedCheck {
    /// Warn after N consecutive writes without a read.
    ConsecutiveWriteLimit { max: u32 },
    /// Warn when the same file is edited more than N times.
    RepeatedFileEditLimit { max: u32 },
    /// Suggest tests after source file edits.
    TestAfterSourceEdit,
    /// Suggest checkpoints at regular intervals.
    PeriodicCheckpoint { interval: u32 },
    /// Require checkpoint before specific action types.
    CheckpointBeforeAction { action_types: Vec<String> },
}

/// Convert discovered failure patterns into learned verification rules.
pub fn generate_rules(patterns: &[FailurePattern], min_confidence: f64) -> Vec<LearnedRule> {
    patterns
        .iter()
        .filter(|p| p.confidence >= min_confidence)
        .map(|p| LearnedRule {
            name: format!("learned_{}", p.name),
            source_pattern: p.name.clone(),
            description: p.description.clone(),
            confidence: p.confidence,
            check: suggestion_to_check(&p.suggestion),
        })
        .collect()
}

fn suggestion_to_check(suggestion: &LearnedSuggestion) -> LearnedCheck {
    match suggestion {
        LearnedSuggestion::WarnConsecutiveWrites { threshold } => {
            LearnedCheck::ConsecutiveWriteLimit { max: *threshold }
        }
        LearnedSuggestion::WarnRepeatedFileEdits { threshold } => {
            LearnedCheck::RepeatedFileEditLimit { max: *threshold }
        }
        LearnedSuggestion::SuggestTestAfterEdit => LearnedCheck::TestAfterSourceEdit,
        LearnedSuggestion::SuggestPeriodicCheckpoint { interval } => {
            LearnedCheck::PeriodicCheckpoint { interval: *interval }
        }
        LearnedSuggestion::CheckpointBefore { action_types } => {
            LearnedCheck::CheckpointBeforeAction {
                action_types: action_types.clone(),
            }
        }
    }
}

/// Apply a learned rule against a step action and execution context.
/// Returns a CheckResult (pass or warn).
pub fn evaluate_rule(
    rule: &LearnedRule,
    action: &StepAction,
    ctx: &ExecutionContext,
) -> CheckResult {
    match &rule.check {
        LearnedCheck::ConsecutiveWriteLimit { max } => {
            if !matches!(action.action_type, ActionType::Write) {
                return pass(&rule.name);
            }

            let consecutive = ctx
                .previous_steps
                .iter()
                .rev()
                .take_while(|s| matches!(s.action_type, ActionType::Write))
                .count() as u32;

            if consecutive >= *max {
                return warn(
                    &rule.name,
                    format!(
                        "{}+ consecutive writes without a read (learned: {})",
                        max, rule.description,
                    ),
                );
            }
            pass(&rule.name)
        }

        LearnedCheck::RepeatedFileEditLimit { max } => {
            for file in &action.target_files {
                let edit_count = ctx
                    .previous_steps
                    .iter()
                    .filter(|s| matches!(s.action_type, ActionType::Write) && s.description.contains(file))
                    .count() as u32;

                if edit_count >= *max {
                    return warn(
                        &rule.name,
                        format!(
                            "File '{}' edited {} times already (learned: {})",
                            file,
                            edit_count + 1,
                            rule.description,
                        ),
                    );
                }
            }
            pass(&rule.name)
        }

        LearnedCheck::TestAfterSourceEdit => {
            if !matches!(action.action_type, ActionType::Write) {
                return pass(&rule.name);
            }

            let is_source = action.target_files.iter().any(|f| {
                let f = f.to_lowercase();
                (f.ends_with(".py") || f.ends_with(".rs") || f.ends_with(".ts") || f.ends_with(".js"))
                    && !f.contains("test")
            });

            if !is_source {
                return pass(&rule.name);
            }

            // Check if any recent step was a test.
            let recent_test = ctx.previous_steps.iter().rev().take(3).any(|s| {
                matches!(s.action_type, ActionType::Execute)
                    && s.description.to_lowercase().contains("test")
            });

            if !recent_test && ctx.previous_steps.len() > 2 {
                return warn(
                    &rule.name,
                    format!(
                        "Source file edit without recent test run (learned: {})",
                        rule.description,
                    ),
                );
            }
            pass(&rule.name)
        }

        LearnedCheck::PeriodicCheckpoint { interval } => {
            let steps_since_checkpoint = ctx
                .previous_steps
                .iter()
                .rev()
                .take_while(|s| !s.description.contains("checkpoint"))
                .count() as u32;

            if steps_since_checkpoint >= *interval {
                return warn(
                    &rule.name,
                    format!(
                        "{} steps since last checkpoint (learned: {})",
                        steps_since_checkpoint, rule.description,
                    ),
                );
            }
            pass(&rule.name)
        }

        LearnedCheck::CheckpointBeforeAction { action_types } => {
            let action_str = action.action_type.as_str();
            if action_types.iter().any(|a| a == action_str) {
                return warn(
                    &rule.name,
                    format!(
                        "Action type '{}' has high failure rate — consider checkpoint (learned: {})",
                        action_str, rule.description,
                    ),
                );
            }
            pass(&rule.name)
        }
    }
}

fn pass(rule_name: &str) -> CheckResult {
    CheckResult {
        checker_name: rule_name.to_string(),
        passed: true,
        message: None,
    }
}

fn warn(rule_name: &str, message: String) -> CheckResult {
    CheckResult {
        checker_name: rule_name.to_string(),
        passed: true, // Warn, don't block.
        message: Some(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx_with_writes(count: u32) -> ExecutionContext {
        let previous = (1..=count)
            .map(|i| StepSummary {
                step_number: i,
                action_type: ActionType::Write,
                description: format!("edit file_{}.py", i),
                result: StepResult::Success,
            })
            .collect();

        ExecutionContext {
            session_id: "test".into(),
            step_number: count + 1,
            working_dir: "/workspace".into(),
            agent_name: None,
            plan: None,
            previous_steps: previous,
        }
    }

    #[test]
    fn consecutive_write_rule_fires() {
        let rule = LearnedRule {
            name: "learned_consecutive_writes".into(),
            source_pattern: "consecutive_writes_without_read".into(),
            description: "3+ writes without read".into(),
            confidence: 0.7,
            check: LearnedCheck::ConsecutiveWriteLimit { max: 3 },
        };

        let action = StepAction {
            action_type: ActionType::Write,
            tool_name: None,
            arguments: json!({}),
            target_files: vec!["new.py".into()],
            description: "another write".into(),
        };

        // 2 previous writes → rule should pass
        let result = evaluate_rule(&rule, &action, &ctx_with_writes(2));
        assert!(result.message.is_none());

        // 3 previous writes → rule should warn
        let result = evaluate_rule(&rule, &action, &ctx_with_writes(3));
        assert!(result.message.is_some());
        assert!(result.message.unwrap().contains("consecutive writes"));
    }

    #[test]
    fn generate_rules_filters_by_confidence() {
        let patterns = vec![
            FailurePattern {
                name: "high_conf".into(),
                description: "high".into(),
                occurrences: 10,
                failure_rate: 0.8,
                confidence: 0.7,
                suggestion: LearnedSuggestion::WarnConsecutiveWrites { threshold: 3 },
            },
            FailurePattern {
                name: "low_conf".into(),
                description: "low".into(),
                occurrences: 2,
                failure_rate: 0.3,
                confidence: 0.1,
                suggestion: LearnedSuggestion::WarnConsecutiveWrites { threshold: 3 },
            },
        ];

        let rules = generate_rules(&patterns, 0.5);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].name, "learned_high_conf");
    }
}
