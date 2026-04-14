pub mod schema;

use anyhow::{Context, Result};
use etcetera::{choose_app_strategy, AppStrategy, AppStrategyArgs};
use std::path::{Path, PathBuf};

pub use schema::CarapaceConfig;

const APP_NAME: &str = "carapace";

pub fn config_dir() -> Result<PathBuf> {
    let strategy = choose_app_strategy(AppStrategyArgs {
        top_level_domain: "dev".to_string(),
        author: "carapace".to_string(),
        app_name: APP_NAME.to_string(),
    })?;
    Ok(strategy.config_dir())
}

pub fn data_dir() -> Result<PathBuf> {
    let strategy = choose_app_strategy(AppStrategyArgs {
        top_level_domain: "dev".to_string(),
        author: "carapace".to_string(),
        app_name: APP_NAME.to_string(),
    })?;
    Ok(strategy.data_dir())
}

pub fn default_config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.yaml"))
}

pub fn default_db_path() -> Result<PathBuf> {
    let dir = data_dir()?;
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("carapace.db"))
}

pub fn load_config(path: Option<&Path>) -> Result<CarapaceConfig> {
    let config_path = match path {
        Some(p) => p.to_path_buf(),
        None => {
            let default = default_config_path()?;
            if default.exists() {
                default
            } else {
                tracing::info!("No config file found, using defaults");
                return Ok(CarapaceConfig::default());
            }
        }
    };

    let contents = std::fs::read_to_string(&config_path)
        .with_context(|| format!("Failed to read config: {}", config_path.display()))?;

    let config: CarapaceConfig = serde_yaml::from_str(&contents)
        .with_context(|| format!("Failed to parse config: {}", config_path.display()))?;

    tracing::info!("Loaded config from {}", config_path.display());
    Ok(config)
}

pub fn write_default_config(path: &Path) -> Result<()> {
    let config = CarapaceConfig::default();
    let yaml = serde_yaml::to_string(&config)?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, yaml)?;
    tracing::info!("Wrote default config to {}", path.display());
    Ok(())
}
