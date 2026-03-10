use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::config::VERSION;
use crate::handler::{self, App};
use crate::types::*;

const MCP_OUTPUT_CHANNEL_CAPACITY: usize = 1024;

// ---------------------------------------------------------------------------
// Parameter structs (independent from ConfigPatch which uses PatchField)
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
struct PsqlQueryParams {
    /// SQL statement to execute
    sql: String,
    /// Request identifier
    #[serde(default)]
    id: Option<String>,
    /// Session name to use
    #[serde(default)]
    session: Option<String>,
    /// Positional bind parameters
    #[serde(default)]
    params: Vec<Value>,
    /// Stream rows in batches instead of returning all at once
    #[serde(default)]
    stream_rows: Option<bool>,
    /// Maximum number of rows per batch when streaming
    #[serde(default)]
    batch_rows: Option<u64>,
    /// Maximum bytes per batch when streaming
    #[serde(default)]
    batch_bytes: Option<u64>,
    /// Per-statement timeout in milliseconds
    #[serde(default)]
    statement_timeout_ms: Option<u64>,
    /// Lock acquisition timeout in milliseconds
    #[serde(default)]
    lock_timeout_ms: Option<u64>,
    /// Execute in read-only mode
    #[serde(default)]
    read_only: Option<bool>,
    /// Maximum rows for inline (non-streaming) results
    #[serde(default)]
    inline_max_rows: Option<u64>,
    /// Maximum bytes for inline (non-streaming) results
    #[serde(default)]
    inline_max_bytes: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
struct PsqlConfigParams {
    /// Default session name
    #[serde(default)]
    default_session: Option<String>,
    /// Session connection configs (keyed by session name)
    #[serde(default)]
    sessions: Option<Value>,
    /// Default inline max rows
    #[serde(default)]
    inline_max_rows: Option<u64>,
    /// Default inline max bytes
    #[serde(default)]
    inline_max_bytes: Option<u64>,
    /// Default statement timeout in milliseconds
    #[serde(default)]
    statement_timeout_ms: Option<u64>,
    /// Default lock timeout in milliseconds
    #[serde(default)]
    lock_timeout_ms: Option<u64>,
    /// Log filter categories
    #[serde(default)]
    log: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// MCP server struct
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct AfpsqlMcp {
    app: Arc<App>,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl AfpsqlMcp {
    pub fn new(app: Arc<App>) -> Self {
        Self {
            app,
            tool_router: Self::tool_router(),
        }
    }

    /// Execute one SQL statement with positional bind parameters.
    #[tool(description = "Execute one SQL statement with positional bind parameters")]
    async fn psql_query(
        &self,
        params: Parameters<PsqlQueryParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;

        let query_id = p.id.unwrap_or_else(|| "mcp".to_string());
        let options = QueryOptions {
            stream_rows: p.stream_rows.unwrap_or(false),
            batch_rows: p.batch_rows.map(|v| v as usize),
            batch_bytes: p.batch_bytes.map(|v| v as usize),
            statement_timeout_ms: p.statement_timeout_ms,
            lock_timeout_ms: p.lock_timeout_ms,
            read_only: p.read_only,
            inline_max_rows: p.inline_max_rows.map(|v| v as usize),
            inline_max_bytes: p.inline_max_bytes.map(|v| v as usize),
        };

        let (tx, mut rx) = mpsc::channel::<Output>(MCP_OUTPUT_CHANNEL_CAPACITY);
        let call_app = Arc::new(App::new(self.app.config.read().await.clone(), tx));

        handler::execute_query(
            &call_app,
            Some(query_id),
            p.session,
            p.sql,
            p.params,
            options,
        )
        .await;

        // Drain all outputs into a single response
        drop(call_app);
        let mut outputs = vec![];
        while let Some(msg) = rx.recv().await {
            outputs.push(serde_json::to_value(&msg).unwrap_or(Value::Null));
        }

        let result = json!({"events": outputs});
        let json = agent_first_data::output_json(&result);
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Read or update runtime config. Call with no arguments to view current config.
    #[tool(description = "Read/update runtime config")]
    async fn psql_config(
        &self,
        params: Parameters<PsqlConfigParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;

        // Reconstruct a ConfigPatch-compatible JSON value and deserialize
        let mut patch_obj = serde_json::Map::new();
        if let Some(v) = p.default_session {
            patch_obj.insert("default_session".to_string(), Value::String(v));
        }
        if let Some(v) = p.sessions {
            patch_obj.insert("sessions".to_string(), v);
        }
        if let Some(v) = p.inline_max_rows {
            patch_obj.insert("inline_max_rows".to_string(), json!(v));
        }
        if let Some(v) = p.inline_max_bytes {
            patch_obj.insert("inline_max_bytes".to_string(), json!(v));
        }
        if let Some(v) = p.statement_timeout_ms {
            patch_obj.insert("statement_timeout_ms".to_string(), json!(v));
        }
        if let Some(v) = p.lock_timeout_ms {
            patch_obj.insert("lock_timeout_ms".to_string(), json!(v));
        }
        if let Some(v) = p.log {
            patch_obj.insert("log".to_string(), json!(v));
        }

        let apply_patch = !patch_obj.is_empty();

        if apply_patch {
            let patch: ConfigPatch =
                serde_json::from_value(Value::Object(patch_obj)).map_err(|e| {
                    McpError::internal_error(format!("invalid config patch: {e}"), None)
                })?;
            let sessions = crate::config::sessions_to_invalidate(&patch);
            let mut cfg = self.app.config.write().await;
            cfg.apply_update(patch);
            drop(cfg);
            self.app.executor.invalidate_sessions(&sessions).await;
        }

        let cfg_snapshot = self.app.config.read().await.clone();
        let json = serde_json::to_string(&cfg_snapshot)
            .map_err(|e| McpError::internal_error(format!("serialize config: {e}"), None))?;
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }
}

#[tool_handler]
impl ServerHandler for AfpsqlMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("afpsql", VERSION))
            .with_instructions(
                "afpsql PostgreSQL client — execute SQL queries and manage connection config. \
                 Use psql_query to execute SQL with bind parameters. \
                 Use psql_config to view or update runtime settings.",
            )
    }
}

#[cfg(test)]
#[path = "../tests/support/unit_mcp.rs"]
mod tests;
