use crate::config::VERSION;
use crate::handler::{self, App};
use crate::types::{
    CloseTrace, ConfigPatch, Output, PongTrace, QueryOptions, RuntimeConfig, SessionConfig,
};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::AsyncBufReadExt;
use tokio::sync::mpsc;

const OUTPUT_CHANNEL_CAPACITY: usize = 1024;

pub async fn run_mcp(session: SessionConfig, log: Vec<String>) {
    let mut config = RuntimeConfig::default();
    if has_session_override(&session) {
        config
            .sessions
            .insert(config.default_session.clone(), session);
    }
    if !log.is_empty() {
        config.log = log;
    }

    let (tx, mut rx) = mpsc::channel::<Output>(OUTPUT_CHANNEL_CAPACITY);
    let app = Arc::new(App::new(config, tx));

    let stdin = tokio::io::stdin();
    let reader = tokio::io::BufReader::new(stdin);
    let mut lines = reader.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let req: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                write_json(&jsonrpc_error(None, -32700, format!("parse error: {e}")));
                continue;
            }
        };

        let method = req
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let id = req.get("id").cloned();
        let params = req.get("params").cloned().unwrap_or(Value::Null);

        match method {
            "initialize" => {
                let result = json!({
                    "protocolVersion": "2024-11-05",
                    "serverInfo": {"name": "afpsql", "version": VERSION},
                    "capabilities": {"tools": {"listChanged": false}}
                });
                if let Some(id) = id {
                    write_json(&jsonrpc_result(id, result));
                }
            }
            "notifications/initialized" => {}
            "ping" => {
                if let Some(id) = id {
                    let result = json!({
                        "trace": PongTrace {
                            uptime_s: app.start_time.elapsed().as_secs(),
                            requests_total: app.requests_total.load(std::sync::atomic::Ordering::Relaxed),
                            in_flight: 0,
                        }
                    });
                    write_json(&jsonrpc_result(id, result));
                }
            }
            "tools/list" => {
                if let Some(id) = id {
                    write_json(&jsonrpc_result(id, tools_list()));
                }
            }
            "tools/call" => {
                if let Some(id) = id {
                    let result = handle_tool_call(&app, &mut rx, &params).await;
                    write_json(&jsonrpc_result(id, result));
                }
            }
            "shutdown" => {
                if let Some(id) = id {
                    write_json(&jsonrpc_result(id, json!({})));
                }
            }
            "exit" => break,
            _ => {
                if id.is_some() {
                    write_json(&jsonrpc_error(
                        id,
                        -32601,
                        format!("method not found: {method}"),
                    ));
                }
            }
        }
    }

    write_json(&json!({
        "jsonrpc":"2.0",
        "method":"afpsql/closed",
        "params": {
            "message":"shutdown",
            "trace": CloseTrace {
                uptime_s: app.start_time.elapsed().as_secs(),
                requests_total: app.requests_total.load(std::sync::atomic::Ordering::Relaxed),
            }
        }
    }));
}

async fn handle_tool_call(
    app: &Arc<App>,
    rx: &mut mpsc::Receiver<Output>,
    params: &Value,
) -> Value {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return tool_error("missing tool name");
    };
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    match name {
        "psql_query" => {
            let Some(sql) = arguments.get("sql").and_then(Value::as_str) else {
                return tool_error("missing required argument: sql");
            };

            let query_id = arguments
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("mcp")
                .to_string();
            let session = arguments
                .get("session")
                .and_then(Value::as_str)
                .map(std::string::ToString::to_string);
            let params_vec = arguments
                .get("params")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let options = QueryOptions {
                stream_rows: arguments
                    .get("stream_rows")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                batch_rows: arguments
                    .get("batch_rows")
                    .and_then(Value::as_u64)
                    .map(|v| v as usize),
                batch_bytes: arguments
                    .get("batch_bytes")
                    .and_then(Value::as_u64)
                    .map(|v| v as usize),
                statement_timeout_ms: arguments
                    .get("statement_timeout_ms")
                    .and_then(Value::as_u64),
                lock_timeout_ms: arguments.get("lock_timeout_ms").and_then(Value::as_u64),
                read_only: arguments.get("read_only").and_then(Value::as_bool),
                inline_max_rows: arguments
                    .get("inline_max_rows")
                    .and_then(Value::as_u64)
                    .map(|v| v as usize),
                inline_max_bytes: arguments
                    .get("inline_max_bytes")
                    .and_then(Value::as_u64)
                    .map(|v| v as usize),
            };

            handler::execute_query(
                app,
                Some(query_id.clone()),
                session,
                sql.to_string(),
                params_vec,
                options,
            )
            .await;

            let outputs = drain_outputs(rx);
            tool_ok(json!({"events": outputs}))
        }
        "psql_config" => {
            if !arguments.is_object() {
                return tool_error("arguments must be an object");
            }
            let mut cfg = app.config.write().await;
            let patch: ConfigPatch = match serde_json::from_value(arguments.clone()) {
                Ok(v) => v,
                Err(e) => return tool_error(&format!("invalid config patch: {e}")),
            };
            if arguments
                .as_object()
                .map(|m| !m.is_empty())
                .unwrap_or(false)
            {
                cfg.apply_update(patch);
            }
            tool_ok(json!({"config": cfg.clone()}))
        }
        other => tool_error(&format!("unknown tool: {other}")),
    }
}

fn drain_outputs(rx: &mut mpsc::Receiver<Output>) -> Vec<Value> {
    let mut outputs = vec![];
    while let Ok(msg) = rx.try_recv() {
        outputs.push(serde_json::to_value(msg).unwrap_or(Value::Null));
    }
    outputs
}

fn tools_list() -> Value {
    json!({
        "tools": [
            {
                "name": "psql_query",
                "description": "Execute one SQL statement with positional bind parameters.",
                "inputSchema": {
                    "type": "object",
                    "required": ["sql"],
                    "properties": {
                        "id": {"type":"string"},
                        "session": {"type":"string"},
                        "sql": {"type":"string"},
                        "params": {"type":"array"},
                        "stream_rows": {"type":"boolean"},
                        "batch_rows": {"type":"integer"},
                        "batch_bytes": {"type":"integer"},
                        "statement_timeout_ms": {"type":"integer"},
                        "lock_timeout_ms": {"type":"integer"},
                        "read_only": {"type":"boolean"},
                        "inline_max_rows": {"type":"integer"},
                        "inline_max_bytes": {"type":"integer"}
                    }
                }
            },
            {
                "name": "psql_config",
                "description": "Read/update runtime config.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "default_session": {"type":"string"},
                        "sessions": {"type":"object"},
                        "inline_max_rows": {"type":"integer"},
                        "inline_max_bytes": {"type":"integer"},
                        "statement_timeout_ms": {"type":"integer"},
                        "lock_timeout_ms": {"type":"integer"},
                        "log": {"type":"array"}
                    }
                }
            }
        ]
    })
}

fn tool_ok(value: Value) -> Value {
    json!({
        "content": [{"type": "text", "text": value.to_string()}],
        "structuredContent": value,
        "isError": false
    })
}

fn tool_error(message: &str) -> Value {
    json!({
        "content": [{"type": "text", "text": message}],
        "isError": true
    })
}

fn jsonrpc_result(id: Value, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":result})
}

fn jsonrpc_error(id: Option<Value>, code: i64, message: String) -> Value {
    let mut obj = json!({
        "jsonrpc":"2.0",
        "error": {"code": code, "message": message}
    });
    if let Some(v) = id {
        obj["id"] = v;
    }
    obj
}

fn write_json(v: &Value) {
    let rendered = agent_first_data::output_json(v);
    println!("{rendered}");
}

fn has_session_override(session: &SessionConfig) -> bool {
    session.dsn_secret.is_some()
        || session.conninfo_secret.is_some()
        || session.host.is_some()
        || session.port.is_some()
        || session.user.is_some()
        || session.dbname.is_some()
        || session.password_secret.is_some()
}

#[cfg(test)]
#[path = "../tests/support/unit_mcp.rs"]
mod tests;
