pub mod consistency;
pub mod patterns;
pub mod rules;
pub mod types;

use crate::config::schema::VerificationConfig;
use crate::learner::rules::{LearnedRule, evaluate_rule};
use crate::types::*;
use std::time::Instant;

/// Trait for step verification.
pub trait Verifier: Send + Sync {
    fn verify(&self, action: &StepAction, ctx: &ExecutionContext) -> VerificationOutcome;
}

/// Chains multiple verifiers, returning the strictest result.
pub struct CompositeVerifier {
    rules_enabled: bool,
    consistency_enabled: bool,
    rule_verifier: rules::RuleVerifier,
    consistency_checker: consistency::ConsistencyChecker,
    learned_rules: Vec<LearnedRule>,
}

impl CompositeVerifier {
    pub fn new(config: VerificationConfig) -> Self {
        let rules_enabled = config.rules_enabled;
        let consistency_enabled = config.consistency_enabled;

        Self {
            rules_enabled,
            consistency_enabled,
            rule_verifier: rules::RuleVerifier::new(config),
            consistency_checker: consistency::ConsistencyChecker::new(),
            learned_rules: Vec::new(),
        }
    }

    /// Create a verifier with learned rules loaded.
    pub fn with_learned_rules(mut self, rules: Vec<LearnedRule>) -> Self {
        self.learned_rules = rules;
        self
    }
}

impl Verifier for CompositeVerifier {
    fn verify(&self, action: &StepAction, ctx: &ExecutionContext) -> VerificationOutcome {
        let start = Instant::now();
        let mut all_checks = Vec::new();

        // Run rule-based checks
        if self.rules_enabled {
            all_checks.extend(self.rule_verifier.check(action, ctx));
        }

        // Run consistency checks
        if self.consistency_enabled {
            all_checks.extend(self.consistency_checker.check(action, ctx));
        }

        // Run learned rules (from past failure analysis)
        for rule in &self.learned_rules {
            all_checks.push(evaluate_rule(rule, action, ctx));
        }

        let duration_ms = start.elapsed().as_millis() as u64;

        // Determine overall decision from individual checks
        let failed: Vec<&CheckResult> = all_checks.iter().filter(|c| !c.passed).collect();
        let warnings: Vec<&CheckResult> = all_checks
            .iter()
            .filter(|c| c.passed && c.message.is_some())
            .collect();

        let decision = if !failed.is_empty() {
            let reasons = failed
                .iter()
                .filter_map(|c| c.message.as_ref())
                .cloned()
                .collect();
            let suggestions = failed
                .iter()
                .map(|c| format!("Fix: address {} check", c.checker_name))
                .collect();
            VerificationDecision::Fail {
                reasons,
                suggestions,
            }
        } else if !warnings.is_empty() {
            let reasons = warnings
                .iter()
                .filter_map(|c| c.message.as_ref())
                .cloned()
                .collect();
            VerificationDecision::Warn { reasons }
        } else {
            VerificationDecision::Pass
        };

        VerificationOutcome {
            decision,
            checks_performed: all_checks,
            duration_ms,
        }
    }
}
