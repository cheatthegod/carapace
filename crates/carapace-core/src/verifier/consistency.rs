use crate::types::*;

/// Checks current step for logical consistency with previous steps.
pub struct ConsistencyChecker;

impl ConsistencyChecker {
    pub fn new() -> Self {
        Self
    }

    pub fn check(&self, action: &StepAction, ctx: &ExecutionContext) -> Vec<CheckResult> {
        let mut results = Vec::new();

        results.push(self.check_contradictions(action, ctx));
        results.push(self.check_loop_trap(action, ctx));
        results.push(self.check_plan_deviation(action, ctx));

        results
    }

    /// Detect contradictory actions: e.g., editing a file that was just deleted.
    fn check_contradictions(&self, action: &StepAction, ctx: &ExecutionContext) -> CheckResult {
        for prev in ctx.previous_steps.iter().rev().take(5) {
            // Writing to a file that was just deleted
            if matches!(prev.action_type, ActionType::Delete)
                && matches!(action.action_type, ActionType::Write)
            {
                let overlap = has_file_overlap_from_desc(&prev.description, &action.description);
                if overlap {
                    return CheckResult {
                        checker_name: "contradictions".into(),
                        passed: false,
                        message: Some(format!(
                            "Contradictory: writing to file that was deleted in step {}",
                            prev.step_number
                        )),
                    };
                }
            }

            // Deleting a file that was just written
            if matches!(prev.action_type, ActionType::Write)
                && matches!(action.action_type, ActionType::Delete)
            {
                let overlap = has_file_overlap_from_desc(&prev.description, &action.description);
                if overlap {
                    return CheckResult {
                        checker_name: "contradictions".into(),
                        passed: false,
                        message: Some(format!(
                            "Suspicious: deleting file just written in step {}",
                            prev.step_number
                        )),
                    };
                }
            }
        }

        CheckResult {
            checker_name: "contradictions".into(),
            passed: true,
            message: None,
        }
    }

    /// Detect loop traps: same action description appearing repeatedly.
    fn check_loop_trap(&self, action: &StepAction, ctx: &ExecutionContext) -> CheckResult {
        let recent_same = ctx
            .previous_steps
            .iter()
            .rev()
            .take(5)
            .filter(|s| {
                s.action_type == action.action_type
                    && similar_descriptions(&s.description, &action.description)
            })
            .count();

        if recent_same >= 3 {
            return CheckResult {
                checker_name: "loop_trap".into(),
                passed: false,
                message: Some(format!(
                    "Loop detected: similar action repeated {} times in last 5 steps",
                    recent_same + 1
                )),
            };
        }

        CheckResult {
            checker_name: "loop_trap".into(),
            passed: true,
            message: None,
        }
    }

    /// Check if the current action deviates from the stated plan.
    fn check_plan_deviation(&self, action: &StepAction, ctx: &ExecutionContext) -> CheckResult {
        let Some(plan) = &ctx.plan else {
            return CheckResult {
                checker_name: "plan_deviation".into(),
                passed: true,
                message: None,
            };
        };

        // Simple heuristic: if the action description shares no significant words
        // with the plan, flag as potential deviation.
        let plan_words: std::collections::HashSet<&str> = plan
            .split_whitespace()
            .filter(|w| w.len() > 3)
            .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
            .collect();

        let action_words: std::collections::HashSet<&str> = action
            .description
            .split_whitespace()
            .filter(|w| w.len() > 3)
            .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
            .collect();

        let overlap = plan_words.intersection(&action_words).count();

        if !plan_words.is_empty() && !action_words.is_empty() && overlap == 0 {
            return CheckResult {
                checker_name: "plan_deviation".into(),
                passed: true, // Warn only, don't fail
                message: Some(
                    "Action description shares no keywords with the stated plan — possible drift"
                        .into(),
                ),
            };
        }

        CheckResult {
            checker_name: "plan_deviation".into(),
            passed: true,
            message: None,
        }
    }
}

/// Simple heuristic: check if two descriptions reference similar file names.
fn has_file_overlap_from_desc(desc_a: &str, desc_b: &str) -> bool {
    let files_a = extract_paths(desc_a);
    let files_b = extract_paths(desc_b);
    files_a.iter().any(|f| files_b.contains(f))
}

/// Extract path-like tokens from a description.
fn extract_paths(text: &str) -> Vec<&str> {
    text.split_whitespace()
        .filter(|w| w.contains('/') || w.contains('.'))
        .filter(|w| !w.starts_with("http"))
        .collect()
}

/// Check if two descriptions are similar (simple word overlap).
fn similar_descriptions(a: &str, b: &str) -> bool {
    let words_a: std::collections::HashSet<&str> = a.split_whitespace().collect();
    let words_b: std::collections::HashSet<&str> = b.split_whitespace().collect();
    let total = words_a.len().max(words_b.len());
    if total == 0 {
        return false;
    }
    let overlap = words_a.intersection(&words_b).count();
    (overlap as f64 / total as f64) > 0.6
}
