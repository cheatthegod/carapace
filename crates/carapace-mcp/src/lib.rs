use carapace_core::{
    ActionType, BeginSessionRequest, CarapaceConfig, ExecutionEngine, RecordStepRequest,
    RollbackRequest, SaveCheckpointRequest, StepAction, StepOutcomeStatus, Storage,
    VerifyStepRequest,
};
use rmcp::{
    Error as McpError,
    ServerHandler,
    model::*,
    service::{Peer, RequestContext, RoleServer},
};
use serde::Serialize;
use serde_json::json;
use std::path::Path;
use std::str::FromStr;
use std::sync::Mutex;

#[derive(Clone)]
pub struct McpServer {
    engine: ExecutionEngine,
    peer: std::sync::Arc<Mutex<Option<Peer<RoleServer>>>>,
}

impl McpServer {
    pub async fn new<P: AsRef<Path>>(config: CarapaceConfig, db_path: P) -> anyhow::Result<Self> {
        let db_path_str = db_path.as_ref().to_string_lossy().into_owned();
        let storage = Storage::new(&db_path_str).await?;
        let engine = ExecutionEngine::new(config, storage);

        Ok(Self {
            engine,
            peer: std::sync::Arc::new(Mutex::new(None)),
        })
    }

    pub fn server_info_json(&self) -> anyhow::Result<String> {
        let info = <Self as ServerHandler>::get_info(self);
        Ok(serde_json::to_string_pretty(&info)?)
    }

    pub async fn serve_stdio(self) -> anyhow::Result<()> {
        let service = rmcp::serve_server(self, rmcp::transport::stdio()).await?;
        service.waiting().await?;
        Ok(())
    }

    fn tool_definitions() -> Vec<Tool> {
        vec![
            Tool {
                name: "carapace_begin_session".into(),
                description: "Start or resume a Carapace session before multi-step tool execution.".into(),
                input_schema: serde_json::from_value(json!({
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string", "description": "Optional session ID to resume; omit for a new session" },
                        "agent_name": { "type": "string", "description": "Name of the calling agent" },
                        "working_dir": { "type": "string", "description": "Absolute path to the project directory" }
                    }
                })).unwrap(),
            },
            Tool {
                name: "carapace_verify_step".into(),
                description: "Verify a proposed step before executing a tool action. Returns pass/warn/fail.".into(),
                input_schema: action_schema_with_required(&["session_id", "action_type", "description"]),
            },
            Tool {
                name: "carapace_save_checkpoint".into(),
                description: "Create a git-backed checkpoint before a risky step so the session can roll back to it.".into(),
                input_schema: action_schema_with_required(&["session_id", "action_type", "description"]),
            },
            Tool {
                name: "carapace_record_step".into(),
                description: "Record the outcome of a tool action after execution.".into(),
                input_schema: serde_json::from_value(json!({
                    "type": "object",
                    "required": ["session_id", "action_type", "description", "result_status"],
                    "properties": {
                        "session_id": { "type": "string" },
                        "step_number": { "type": "integer" },
                        "plan": { "type": "string" },
                        "action_type": { "type": "string" },
                        "tool_name": { "type": "string" },
                        "arguments": { "type": "object" },
                        "target_files": { "type": "array", "items": { "type": "string" } },
                        "description": { "type": "string" },
                        "reason": { "type": "string" },
                        "checkpoint_id": { "type": "string" },
                        "result_status": { "type": "string", "enum": ["success", "failure", "rolled_back", "skipped"] },
                        "result_message": { "type": "string" },
                        "tokens_used": { "type": "integer" },
                        "cost_usd": { "type": "number" },
                        "duration_ms": { "type": "integer" }
                    }
                })).unwrap(),
            },
            Tool {
                name: "carapace_rollback".into(),
                description: "Roll back to a saved checkpoint, or undo the last N checkpointed steps.".into(),
                input_schema: serde_json::from_value(json!({
                    "type": "object",
                    "required": ["session_id"],
                    "properties": {
                        "session_id": { "type": "string" },
                        "checkpoint_id": { "type": "string" },
                        "steps_back": { "type": "integer" },
                        "reason": { "type": "string" }
                    }
                })).unwrap(),
            },
            Tool {
                name: "carapace_session_summary".into(),
                description: "Return the summary for a recorded Carapace session.".into(),
                input_schema: serde_json::from_value(json!({
                    "type": "object",
                    "required": ["session_id"],
                    "properties": {
                        "session_id": { "type": "string" }
                    }
                })).unwrap(),
            },
            Tool {
                name: "carapace_learn".into(),
                description: "Analyze past sessions to discover failure patterns and generate adaptive verification rules.".into(),
                input_schema: serde_json::from_value(json!({
                    "type": "object",
                    "properties": {
                        "min_confidence": { "type": "number", "description": "Minimum confidence threshold (0.0-1.0, default 0.3)" }
                    }
                })).unwrap(),
            },
        ]
    }

    async fn dispatch(&self, name: &str, args: Option<JsonObject>) -> Result<CallToolResult, McpError> {
        let args = args.unwrap_or_default();
        let val = serde_json::Value::Object(args);

        match name {
            "carapace_begin_session" => self.handle_begin_session(val).await,
            "carapace_verify_step" => self.handle_verify_step(val).await,
            "carapace_save_checkpoint" => self.handle_save_checkpoint(val).await,
            "carapace_record_step" => self.handle_record_step(val).await,
            "carapace_rollback" => self.handle_rollback(val).await,
            "carapace_session_summary" => self.handle_session_summary(val).await,
            "carapace_learn" => self.handle_learn(val).await,
            _ => Err(McpError::method_not_found::<CallToolRequestMethod>()),
        }
    }

    async fn handle_begin_session(&self, val: serde_json::Value) -> Result<CallToolResult, McpError> {
        let session_id = val.get("session_id").and_then(|v| v.as_str()).map(String::from);
        let agent_name = val.get("agent_name").and_then(|v| v.as_str()).map(String::from);
        let working_dir = val.get("working_dir")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_else(|_| ".".into()));

        let response = self.engine
            .begin_session(BeginSessionRequest { session_id, agent_name, working_dir })
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        json_result(&response)
    }

    async fn handle_verify_step(&self, val: serde_json::Value) -> Result<CallToolResult, McpError> {
        let session_id = require_str(&val, "session_id")?;
        let action = build_action(&val)?;
        let step_number = val.get("step_number").and_then(|v| v.as_u64()).map(|v| v as u32);
        let plan = val.get("plan").and_then(|v| v.as_str()).map(String::from);

        let response = self.engine
            .verify_step(VerifyStepRequest { session_id, step_number, plan, action })
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        json_result(&response)
    }

    async fn handle_save_checkpoint(&self, val: serde_json::Value) -> Result<CallToolResult, McpError> {
        let session_id = require_str(&val, "session_id")?;
        let action = build_action(&val)?;

        let response = self.engine
            .save_checkpoint(SaveCheckpointRequest { session_id, action })
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        json_result(&response)
    }

    async fn handle_record_step(&self, val: serde_json::Value) -> Result<CallToolResult, McpError> {
        let session_id = require_str(&val, "session_id")?;
        let action = build_action(&val)?;
        let step_number = val.get("step_number").and_then(|v| v.as_u64()).map(|v| v as u32);
        let plan = val.get("plan").and_then(|v| v.as_str()).map(String::from);
        let reason = val.get("reason").and_then(|v| v.as_str()).map(String::from);
        let checkpoint_id = val.get("checkpoint_id").and_then(|v| v.as_str()).map(String::from);
        let result_status = parse_status(val.get("result_status").and_then(|v| v.as_str()).unwrap_or("success"))?;
        let result_message = val.get("result_message").and_then(|v| v.as_str()).map(String::from);
        let tokens_used = val.get("tokens_used").and_then(|v| v.as_u64()).unwrap_or(0);
        let cost_usd = val.get("cost_usd").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let duration_ms = val.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(0);

        let response = self.engine
            .record_step(RecordStepRequest {
                session_id, step_number, plan, action, reason, checkpoint_id,
                result_status, result_message, tokens_used, cost_usd, duration_ms,
            })
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        json_result(&response)
    }

    async fn handle_rollback(&self, val: serde_json::Value) -> Result<CallToolResult, McpError> {
        let session_id = require_str(&val, "session_id")?;
        let checkpoint_id = val.get("checkpoint_id").and_then(|v| v.as_str()).map(String::from);
        let steps_back = val.get("steps_back").and_then(|v| v.as_u64()).map(|v| v as u32);
        let reason = val.get("reason").and_then(|v| v.as_str()).map(String::from);

        let response = self.engine
            .rollback(RollbackRequest {
                session_id,
                checkpoint_id,
                steps_back,
                reason,
            })
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        json_result(&response)
    }

    async fn handle_session_summary(&self, val: serde_json::Value) -> Result<CallToolResult, McpError> {
        let session_id = require_str(&val, "session_id")?;
        let summary = self.engine
            .session_summary(&session_id)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        json_result(&summary)
    }

    async fn handle_learn(&self, val: serde_json::Value) -> Result<CallToolResult, McpError> {
        let min_confidence = val.get("min_confidence").and_then(|v| v.as_f64()).unwrap_or(0.3);

        let learner = carapace_core::learner::Learner::new(
            self.engine.storage().clone(),
            min_confidence,
        );

        let report = learner.learn().await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let output = serde_json::json!({
            "sessions_analyzed": report.sessions_analyzed,
            "total_steps": report.total_steps,
            "total_failures": report.total_failures,
            "patterns_found": report.patterns_found.len(),
            "rules_generated": report.rules_generated.len(),
            "rules": report.rules_generated,
        });

        json_result(&output)
    }
}

impl ServerHandler for McpServer {
    fn list_tools(
        &self,
        _request: PaginatedRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListToolsResult, McpError>> + Send + '_ {
        std::future::ready(Ok(ListToolsResult {
            tools: Self::tool_definitions(),
            next_cursor: None,
        }))
    }

    fn call_tool(
        &self,
        request: CallToolRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<CallToolResult, McpError>> + Send + '_ {
        let name = request.name.to_string();
        let arguments = request.arguments;
        async move { self.dispatch(&name, arguments).await }
    }

    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation {
                name: "carapace".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
            instructions: Some("Start with carapace_begin_session before a multi-step task. Call carapace_verify_step before each tool action. Call carapace_save_checkpoint before risky writes or deletes. Call carapace_record_step after each tool action finishes. Use carapace_rollback to restore a saved checkpoint, and carapace_session_summary to inspect progress.".into()),
        }
    }

    fn get_peer(&self) -> Option<Peer<RoleServer>> {
        self.peer.lock().ok().and_then(|guard| guard.clone())
    }

    fn set_peer(&mut self, peer: Peer<RoleServer>) {
        if let Ok(mut guard) = self.peer.lock() {
            *guard = Some(peer);
        }
    }
}

fn action_schema_with_required(required: &[&str]) -> std::sync::Arc<JsonObject> {
    serde_json::from_value(json!({
        "type": "object",
        "required": required,
        "properties": {
            "session_id": { "type": "string" },
            "step_number": { "type": "integer" },
            "plan": { "type": "string" },
            "action_type": { "type": "string", "enum": ["read", "write", "delete", "execute", "api_call", "search"] },
            "tool_name": { "type": "string" },
            "arguments": { "type": "object" },
            "target_files": { "type": "array", "items": { "type": "string" } },
            "description": { "type": "string" }
        }
    }))
    .unwrap()
}

fn require_str(val: &serde_json::Value, field: &str) -> Result<String, McpError> {
    val.get(field)
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| McpError::invalid_params(format!("missing required field: {field}"), None))
}

fn build_action(val: &serde_json::Value) -> Result<StepAction, McpError> {
    let action_type_str = val.get("action_type").and_then(|v| v.as_str()).unwrap_or("read");
    let action_type = ActionType::from_str(action_type_str)
        .map_err(|e| McpError::invalid_params(format!("invalid action_type: {e}"), None))?;

    let description = val.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if description.is_empty() {
        return Err(McpError::invalid_params("description must not be empty", None));
    }

    let tool_name = val.get("tool_name").and_then(|v| v.as_str()).map(String::from);
    let arguments = val.get("arguments").cloned().unwrap_or(json!({}));
    let target_files: Vec<String> = val.get("target_files")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    Ok(StepAction { action_type, tool_name, arguments, target_files, description })
}

fn parse_status(value: &str) -> Result<StepOutcomeStatus, McpError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "success" => Ok(StepOutcomeStatus::Success),
        "failure" => Ok(StepOutcomeStatus::Failure),
        "rolled_back" => Ok(StepOutcomeStatus::RolledBack),
        "skipped" => Ok(StepOutcomeStatus::Skipped),
        _ => Err(McpError::invalid_params(format!("invalid result_status: {value}"), None)),
    }
}

fn json_result<T: Serialize>(value: &T) -> Result<CallToolResult, McpError> {
    let body = serde_json::to_string_pretty(value)
        .map_err(|e| McpError::internal_error(format!("serialization failed: {e}"), None))?;
    Ok(CallToolResult::success(vec![Content::text(body)]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_definitions_include_checkpoint_tools() {
        let tool_names: Vec<String> = McpServer::tool_definitions()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();

        assert!(tool_names.contains(&"carapace_begin_session".to_string()));
        assert!(tool_names.contains(&"carapace_verify_step".to_string()));
        assert!(tool_names.contains(&"carapace_save_checkpoint".to_string()));
        assert!(tool_names.contains(&"carapace_record_step".to_string()));
        assert!(tool_names.contains(&"carapace_rollback".to_string()));
        assert!(tool_names.contains(&"carapace_session_summary".to_string()));
    }

    #[test]
    fn parse_statuses() {
        assert!(matches!(parse_status("success").unwrap(), StepOutcomeStatus::Success));
        assert!(matches!(parse_status("failure").unwrap(), StepOutcomeStatus::Failure));
        assert!(matches!(parse_status("rolled_back").unwrap(), StepOutcomeStatus::RolledBack));
        assert!(matches!(parse_status("skipped").unwrap(), StepOutcomeStatus::Skipped));
        assert!(parse_status("oops").is_err());
    }

    #[test]
    fn build_action_requires_description() {
        let val = json!({
            "action_type": "write",
            "tool_name": "edit_file",
            "target_files": ["src/lib.rs"]
        });

        assert!(build_action(&val).is_err());
    }
}
