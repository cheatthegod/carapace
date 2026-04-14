pub mod anomaly;
pub mod export;
pub mod types;

use crate::config::schema::TraceConfig;
use crate::storage::Storage;
use crate::types::*;
use anyhow::Result;

/// Tracer records execution steps and detects anomalies.
pub struct Tracer {
    store: Storage,
    detector: anomaly::AnomalyDetector,
}

impl Tracer {
    pub fn new(store: Storage, config: TraceConfig) -> Self {
        Self {
            store,
            detector: anomaly::AnomalyDetector::new(config),
        }
    }

    /// Record a step and detect any anomalies.
    pub async fn record_step(&self, entry: TraceEntry) -> Result<Vec<Anomaly>> {
        // Get recent steps for anomaly detection context
        let recent = self.store.get_recent_steps(&entry.session_id, 10).await?;

        // Detect anomalies
        let anomalies = self.detector.detect(&entry, &recent);

        // Store the step
        self.store.insert_step(&entry).await?;

        // Store any detected anomalies
        for anomaly in &anomalies {
            self.store
                .insert_anomaly(&entry.session_id, anomaly)
                .await?;
        }

        Ok(anomalies)
    }

    /// Get the full trace for a session.
    pub async fn get_trace(&self, session_id: &str) -> Result<Vec<TraceEntry>> {
        self.store.get_session_steps(session_id).await
    }

    /// Get a session summary.
    pub async fn get_summary(&self, session_id: &str) -> Result<SessionSummary> {
        self.store.get_session_summary(session_id).await
    }
}
