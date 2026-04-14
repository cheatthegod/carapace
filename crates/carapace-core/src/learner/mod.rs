pub mod patterns;
pub mod persist;
pub mod rules;

use anyhow::Result;
use std::path::Path;

use crate::storage::Storage;
use crate::types::*;
use patterns::{SessionTrace, analyze_sessions};
use rules::{LearnedRule, generate_rules};

/// The Learner analyzes past session traces and produces adaptive
/// verification rules — rules that emerge from real failure data,
/// not from static configuration.
///
/// This is Carapace's core differentiator: it learns what to verify
/// from how agents actually fail.
pub struct Learner {
    storage: Storage,
    min_confidence: f64,
}

/// Report produced by the learning process.
#[derive(Debug, Clone)]
pub struct LearningReport {
    pub sessions_analyzed: u32,
    pub total_steps: u32,
    pub total_failures: u32,
    pub patterns_found: Vec<patterns::FailurePattern>,
    pub rules_generated: Vec<LearnedRule>,
}

impl Learner {
    pub fn new(storage: Storage, min_confidence: f64) -> Self {
        Self {
            storage,
            min_confidence,
        }
    }

    /// Analyze all sessions in the database and produce learned rules.
    pub async fn learn(&self) -> Result<LearningReport> {
        let sessions = self.load_all_sessions().await?;

        let total_steps: u32 = sessions.iter().map(|s| s.steps.len() as u32).sum();
        let total_failures: u32 = sessions
            .iter()
            .flat_map(|s| &s.steps)
            .filter(|s| !s.result.is_success())
            .count() as u32;

        let patterns = analyze_sessions(&sessions);
        let rules = generate_rules(&patterns, self.min_confidence);

        Ok(LearningReport {
            sessions_analyzed: sessions.len() as u32,
            total_steps,
            total_failures,
            patterns_found: patterns,
            rules_generated: rules,
        })
    }

    /// Analyze sessions and return only the rules (for integration with verifier).
    pub async fn learn_rules(&self) -> Result<Vec<LearnedRule>> {
        let report = self.learn().await?;
        Ok(report.rules_generated)
    }

    /// Analyze sessions, generate rules, persist to disk, and return them.
    pub async fn learn_and_save(&self, data_dir: &Path) -> Result<LearningReport> {
        let report = self.learn().await?;
        persist::save_rules(data_dir, &report.rules_generated)?;
        Ok(report)
    }

    async fn load_all_sessions(&self) -> Result<Vec<SessionTrace>> {
        let session_ids = self.storage.list_session_ids().await?;
        let mut traces = Vec::new();

        for session_id in &session_ids {
            let steps = self.storage.get_session_steps(session_id).await?;
            if !steps.is_empty() {
                traces.push(SessionTrace {
                    session_id: session_id.clone(),
                    steps,
                });
            }
        }

        Ok(traces)
    }
}
