use anyhow::Result;
use carapace_core::{CarapaceConfig, Storage};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerManifest {
    pub name: String,
    pub version: String,
    pub transport: String,
    pub tools: Vec<ToolDescriptor>,
}

pub struct McpServer {
    manifest: ServerManifest,
}

impl McpServer {
    pub async fn new<P: AsRef<Path>>(config: CarapaceConfig, db_path: P) -> Result<Self> {
        let db_path = db_path.as_ref().to_path_buf();
        let db_path_str = db_path.to_string_lossy().into_owned();
        let _ = Storage::new(&db_path_str).await?;

        Ok(Self {
            manifest: build_manifest(&config, db_path),
        })
    }

    pub fn manifest(&self) -> &ServerManifest {
        &self.manifest
    }

    pub fn manifest_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(&self.manifest)?)
    }

    pub async fn serve_stdio(&self) -> Result<()> {
        tracing::info!(
            "Starting placeholder Carapace MCP stdio server; protocol handling is not implemented yet"
        );
        tokio::signal::ctrl_c().await?;
        Ok(())
    }
}

fn build_manifest(config: &CarapaceConfig, db_path: PathBuf) -> ServerManifest {
    let tools = vec![
        ToolDescriptor {
            name: "verify_action".into(),
            description: "Run Carapace rule and consistency checks on a proposed action.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["action_type", "description"],
                "properties": {
                    "action_type": { "type": "string" },
                    "description": { "type": "string" },
                    "tool_name": { "type": ["string", "null"] },
                    "target_files": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "arguments": { "type": "object" }
                }
            }),
        },
        ToolDescriptor {
            name: "session_summary".into(),
            description: "Return audit summary for a recorded Carapace session.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["session_id"],
                "properties": {
                    "session_id": { "type": "string" }
                }
            }),
        },
        ToolDescriptor {
            name: "trace_export".into(),
            description: "Export a recorded session trace as JSON or CSV.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["session_id"],
                "properties": {
                    "session_id": { "type": "string" },
                    "format": {
                        "type": "string",
                        "enum": ["json", "csv"]
                    }
                }
            }),
        },
    ];

    let transport = format!(
        "stdio-placeholder (verification: {}, trace: {}, db: {})",
        config.verification.enabled,
        config.trace.enabled,
        db_path.display()
    );

    ServerManifest {
        name: "carapace".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        transport,
        tools,
    }
}
