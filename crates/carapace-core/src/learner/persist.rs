use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use super::rules::LearnedRule;

const RULES_FILENAME: &str = "learned_rules.json";

/// Resolve the path for persisted learned rules.
pub fn rules_path(data_dir: &Path) -> PathBuf {
    data_dir.join(RULES_FILENAME)
}

/// Save learned rules to disk.
pub fn save_rules(data_dir: &Path, rules: &[LearnedRule]) -> Result<()> {
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("Failed to create data dir: {}", data_dir.display()))?;

    let path = rules_path(data_dir);
    let json = serde_json::to_string_pretty(rules)
        .context("Failed to serialize learned rules")?;

    std::fs::write(&path, &json)
        .with_context(|| format!("Failed to write learned rules to {}", path.display()))?;

    tracing::info!("Saved {} learned rules to {}", rules.len(), path.display());
    Ok(())
}

/// Load learned rules from disk. Returns empty vec if file doesn't exist.
pub fn load_rules(data_dir: &Path) -> Result<Vec<LearnedRule>> {
    let path = rules_path(data_dir);

    if !path.exists() {
        return Ok(Vec::new());
    }

    let json = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read learned rules from {}", path.display()))?;

    let rules: Vec<LearnedRule> = serde_json::from_str(&json)
        .with_context(|| format!("Failed to parse learned rules from {}", path.display()))?;

    tracing::info!("Loaded {} learned rules from {}", rules.len(), path.display());
    Ok(rules)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learner::rules::LearnedCheck;
    use tempfile::TempDir;

    #[test]
    fn round_trip_persist() {
        let dir = TempDir::new().unwrap();
        let rules = vec![LearnedRule {
            name: "test_rule".into(),
            source_pattern: "test_pattern".into(),
            description: "A test rule".into(),
            confidence: 0.75,
            check: LearnedCheck::ConsecutiveWriteLimit { max: 3 },
        }];

        save_rules(dir.path(), &rules).unwrap();
        let loaded = load_rules(dir.path()).unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "test_rule");
        assert_eq!(loaded[0].confidence, 0.75);
    }

    #[test]
    fn load_from_empty_dir() {
        let dir = TempDir::new().unwrap();
        let loaded = load_rules(dir.path()).unwrap();
        assert!(loaded.is_empty());
    }
}
