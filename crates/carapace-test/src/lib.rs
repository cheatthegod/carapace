use anyhow::Result;
use carapace_core::{CarapaceConfig, ExecutionContext, Storage, default_db_path};
use std::path::PathBuf;
use tempfile::TempDir;

pub struct TestHarness {
    _tempdir: TempDir,
    pub root: PathBuf,
    pub db_path: PathBuf,
}

impl TestHarness {
    pub fn new() -> Result<Self> {
        let tempdir = tempfile::tempdir()?;
        let root = tempdir.path().to_path_buf();
        let db_path = root.join("carapace-test.db");

        Ok(Self {
            _tempdir: tempdir,
            root,
            db_path,
        })
    }

    pub async fn storage(&self) -> Result<Storage> {
        let db_path = self.db_path.to_string_lossy().into_owned();
        Storage::new(&db_path).await
    }

    pub fn config(&self) -> CarapaceConfig {
        CarapaceConfig::default()
    }

    pub fn context(&self, session_id: &str) -> ExecutionContext {
        ExecutionContext {
            session_id: session_id.to_string(),
            step_number: 1,
            working_dir: self.root.display().to_string(),
            agent_name: Some("carapace-test".into()),
            plan: None,
            previous_steps: vec![],
        }
    }
}

pub fn fallback_db_path() -> Result<PathBuf> {
    default_db_path()
}
