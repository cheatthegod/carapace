use crate::config::schema::TraceConfig;
use crate::types::*;
use chrono::Utc;

/// Detects anomalies by comparing current step against recent history.
pub struct AnomalyDetector {
    config: TraceConfig,
}

impl AnomalyDetector {
    pub fn new(config: TraceConfig) -> Self {
        Self { config }
    }

    /// Analyze the current step in context of recent steps.
    pub fn detect(&self, current: &TraceEntry, recent: &[TraceEntry]) -> Vec<Anomaly> {
        if !self.config.detect_anomalies {
            return vec![];
        }

        let mut anomalies = Vec::new();

        if let Some(a) = self.detect_token_spike(current, recent) {
            anomalies.push(a);
        }
        if let Some(a) = self.detect_loop_trap(current, recent) {
            anomalies.push(a);
        }
        if let Some(a) = self.detect_goal_drift(current, recent) {
            anomalies.push(a);
        }

        anomalies
    }

    /// Token spike: current step uses significantly more tokens than the rolling average.
    fn detect_token_spike(&self, current: &TraceEntry, recent: &[TraceEntry]) -> Option<Anomaly> {
        if recent.is_empty() || current.tokens_used == 0 {
            return None;
        }

        let avg_tokens: f64 =
            recent.iter().map(|s| s.tokens_used as f64).sum::<f64>() / recent.len() as f64;

        if avg_tokens > 0.0
            && (current.tokens_used as f64) > avg_tokens * self.config.token_spike_threshold
        {
            return Some(Anomaly {
                anomaly_type: AnomalyType::TokenSpike,
                severity: Severity::Warning,
                detail: format!(
                    "Token usage {} is {:.1}x the rolling average {:.0}",
                    current.tokens_used,
                    current.tokens_used as f64 / avg_tokens,
                    avg_tokens
                ),
                step_id: Some(current.step_id.clone()),
                detected_at: Utc::now(),
            });
        }

        None
    }

    /// Loop trap: same action description repeated in recent window.
    fn detect_loop_trap(&self, current: &TraceEntry, recent: &[TraceEntry]) -> Option<Anomaly> {
        let window = self.config.loop_detection_window as usize;
        let recent_window = if recent.len() > window {
            &recent[recent.len() - window..]
        } else {
            recent
        };

        let same_count = recent_window
            .iter()
            .filter(|s| {
                s.action.action_type == current.action.action_type
                    && s.action.description == current.action.description
            })
            .count();

        if same_count >= 3 {
            return Some(Anomaly {
                anomaly_type: AnomalyType::LoopTrap,
                severity: Severity::Critical,
                detail: format!(
                    "Action '{}' repeated {} times in last {} steps — possible infinite loop",
                    current.action.description,
                    same_count + 1,
                    window
                ),
                step_id: Some(current.step_id.clone()),
                detected_at: Utc::now(),
            });
        }

        None
    }

    /// Goal drift: action types shifting significantly from initial pattern.
    fn detect_goal_drift(&self, current: &TraceEntry, recent: &[TraceEntry]) -> Option<Anomaly> {
        if recent.len() < 5 {
            return None;
        }

        let mut combined = recent.to_vec();
        combined.push(current.clone());

        // Compare first half vs second half of actions, including the current step.
        let mid = combined.len() / 2;
        let first_half = &combined[..mid];
        let second_half = &combined[mid..];

        let first_reads = first_half
            .iter()
            .filter(|s| matches!(s.action.action_type, ActionType::Read))
            .count();
        let second_reads = second_half
            .iter()
            .filter(|s| matches!(s.action.action_type, ActionType::Read))
            .count();

        // If the agent was writing code and suddenly switched to all reads,
        // or vice versa, something may have changed.
        let first_ratio = first_reads as f64 / first_half.len().max(1) as f64;
        let second_ratio = second_reads as f64 / second_half.len().max(1) as f64;

        if (first_ratio - second_ratio).abs() > 0.6 {
            return Some(Anomaly {
                anomaly_type: AnomalyType::GoalDrift,
                severity: Severity::Info,
                detail: format!(
                    "Action pattern shifted: read ratio changed from {:.0}% to {:.0}%",
                    first_ratio * 100.0,
                    second_ratio * 100.0
                ),
                step_id: Some(current.step_id.clone()),
                detected_at: Utc::now(),
            });
        }

        None
    }
}
