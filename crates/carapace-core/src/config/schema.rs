use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CarapaceConfig {
    pub verification: VerificationConfig,
    pub checkpoint: CheckpointConfig,
    pub trace: TraceConfig,
    pub cost: CostConfig,
}

impl Default for CarapaceConfig {
    fn default() -> Self {
        Self {
            verification: VerificationConfig::default(),
            checkpoint: CheckpointConfig::default(),
            trace: TraceConfig::default(),
            cost: CostConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VerificationConfig {
    pub enabled: bool,
    pub rules_enabled: bool,
    pub consistency_enabled: bool,
    pub blocked_commands: Vec<String>,
    pub blocked_paths: Vec<String>,
    pub allowed_paths: Vec<String>,
    pub max_files_per_step: usize,
    pub require_confirmation_for: Vec<String>,
}

impl Default for VerificationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            rules_enabled: true,
            consistency_enabled: true,
            blocked_commands: vec![
                "rm -rf /".into(),
                "rm -rf /*".into(),
                "mkfs".into(),
                "dd if=/dev/zero".into(),
                ":(){:|:&};:".into(),
                "chmod -R 777 /".into(),
                r"curl\s.*\|\s*(ba)?sh".into(),   // curl URL | sh  or  curl URL | bash
                r"wget\s.*\|\s*(ba)?sh".into(),   // wget URL | sh  or  wget URL | bash
            ],
            blocked_paths: vec![
                "/etc/passwd".into(),
                "/etc/shadow".into(),
                "~/.ssh".into(),
                "~/.aws/credentials".into(),
                "~/.aws/config".into(),
                ".env".into(),          // blocks files named exactly ".env"
                ".env.local".into(),    // blocks .env.local
                ".env.production".into(),
                "secrets".into(),       // blocks any path containing a "secrets" directory
            ],
            allowed_paths: vec![],
            max_files_per_step: 20,
            require_confirmation_for: vec![
                "delete".into(),
                "execute".into(),
            ],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CheckpointConfig {
    pub enabled: bool,
    pub strategy: CheckpointStrategy,
    pub auto_save: bool,
    pub max_rollback_depth: u32,
    pub auto_save_on: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointStrategy {
    Git,
    FileCopy,
    None,
}

impl Default for CheckpointConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            strategy: CheckpointStrategy::Git,
            auto_save: true,
            max_rollback_depth: 10,
            auto_save_on: vec![
                "write".into(),
                "delete".into(),
                "execute".into(),
            ],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TraceConfig {
    pub enabled: bool,
    pub retention_days: u32,
    pub detect_anomalies: bool,
    pub token_spike_threshold: f64,
    pub loop_detection_window: u32,
}

impl Default for TraceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            retention_days: 30,
            detect_anomalies: true,
            token_spike_threshold: 3.0,
            loop_detection_window: 5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CostConfig {
    pub track_tokens: bool,
    pub daily_limit_usd: Option<f64>,
    pub monthly_limit_usd: Option<f64>,
    pub alert_threshold: f64,
}

impl Default for CostConfig {
    fn default() -> Self {
        Self {
            track_tokens: true,
            daily_limit_usd: None,
            monthly_limit_usd: None,
            alert_threshold: 0.8,
        }
    }
}

impl CarapaceConfig {
    pub fn should_checkpoint_action(&self, action_type: &str) -> bool {
        self.checkpoint.enabled
            && self.checkpoint.auto_save
            && self.checkpoint.auto_save_on.contains(&action_type.to_string())
    }

    pub fn blocked_commands_set(&self) -> HashSet<&str> {
        self.verification
            .blocked_commands
            .iter()
            .map(|s| s.as_str())
            .collect()
    }
}
