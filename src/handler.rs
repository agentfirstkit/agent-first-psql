use crate::conn::resolve_session_name;
use crate::db::{DbExecutor, ExecError, ExecOutcome, PostgresExecutor};
use crate::types::*;
use serde_json::Value;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, Mutex, RwLock};

pub struct App {
    pub config: RwLock<RuntimeConfig>,
    pub executor: Arc<dyn DbExecutor>,
    pub writer: mpsc::Sender<Output>,
    pub in_flight: Mutex<std::collections::HashMap<String, tokio::task::JoinHandle<()>>>,
    pub requests_total: std::sync::atomic::AtomicU64,
    pub start_time: Instant,
}

impl App {
    pub fn new(config: RuntimeConfig, writer: mpsc::Sender<Output>) -> Self {
        Self {
            config: RwLock::new(config),
            executor: Arc::new(PostgresExecutor::new()),
            writer,
            in_flight: Mutex::new(std::collections::HashMap::new()),
            requests_total: std::sync::atomic::AtomicU64::new(0),
            start_time: Instant::now(),
        }
    }
}

pub async fn execute_query(
    app: &Arc<App>,
    id: Option<String>,
    session: Option<String>,
    sql: String,
    params: Vec<Value>,
    options: QueryOptions,
) {
    let start = Instant::now();
    let cfg = app.config.read().await.clone();
    let resolved_session = resolve_session_name(&cfg, session.as_deref());
    let resolved_opts = cfg.resolve_options(&options);

    let Some(session_cfg) = cfg.sessions.get(&resolved_session).cloned() else {
        let trace = Trace::only_duration(start.elapsed().as_millis() as u64);
        let _ = app
            .writer
            .send(Output::Error {
                id: id.clone(),
                error_code: "connect_failed".to_string(),
                error: format!("unknown session: {resolved_session}"),
                retryable: true,
                trace: trace.clone(),
            })
            .await;
        emit_log(
            app,
            "query.error",
            id.as_deref(),
            Some(&resolved_session),
            Some("connect_failed"),
            None,
            &trace,
        )
        .await;
        return;
    };

    let result = app
        .executor
        .execute(
            &resolved_session,
            &session_cfg,
            &sql,
            &params,
            &resolved_opts,
        )
        .await;

    match result {
        Ok(ExecOutcome::Rows(rows)) => {
            let status = emit_rows_result(
                app,
                id.clone(),
                Some(resolved_session.clone()),
                rows,
                start,
                &resolved_opts,
            )
            .await;
            match status {
                RowEmitStatus::Sent { trace } => {
                    emit_log(
                        app,
                        "query.result",
                        id.as_deref(),
                        Some(&resolved_session),
                        None,
                        Some("SELECT"),
                        &trace,
                    )
                    .await;
                }
                RowEmitStatus::TooLarge { trace } => {
                    emit_log(
                        app,
                        "query.error",
                        id.as_deref(),
                        Some(&resolved_session),
                        Some("result_too_large"),
                        None,
                        &trace,
                    )
                    .await;
                }
            }
        }
        Ok(ExecOutcome::Command { affected }) => {
            let command_tag = format!("EXECUTE {affected}");
            let trace = Trace {
                duration_ms: start.elapsed().as_millis() as u64,
                row_count: Some(0),
                payload_bytes: Some(0),
            };
            let _ = app
                .writer
                .send(Output::Result {
                    id: id.clone(),
                    session: Some(resolved_session.clone()),
                    command_tag: command_tag.clone(),
                    columns: vec![],
                    rows: vec![],
                    row_count: 0,
                    trace: trace.clone(),
                })
                .await;
            emit_log(
                app,
                "query.result",
                id.as_deref(),
                Some(&resolved_session),
                None,
                Some("EXECUTE"),
                &trace,
            )
            .await;
        }
        Err(ExecError::Connect(message)) => {
            let trace = Trace::only_duration(start.elapsed().as_millis() as u64);
            let _ = app
                .writer
                .send(Output::Error {
                    id: id.clone(),
                    error_code: "connect_failed".to_string(),
                    error: message,
                    retryable: true,
                    trace: trace.clone(),
                })
                .await;
            emit_log(
                app,
                "query.error",
                id.as_deref(),
                Some(&resolved_session),
                Some("connect_failed"),
                None,
                &trace,
            )
            .await;
        }
        Err(ExecError::InvalidParams(message)) => {
            let trace = Trace::only_duration(start.elapsed().as_millis() as u64);
            let _ = app
                .writer
                .send(Output::Error {
                    id: id.clone(),
                    error_code: "invalid_params".to_string(),
                    error: message,
                    retryable: false,
                    trace: trace.clone(),
                })
                .await;
            emit_log(
                app,
                "query.error",
                id.as_deref(),
                Some(&resolved_session),
                Some("invalid_params"),
                None,
                &trace,
            )
            .await;
        }
        Err(ExecError::Sql {
            sqlstate,
            message,
            detail,
            hint,
            position,
        }) => {
            let trace = Trace::only_duration(start.elapsed().as_millis() as u64);
            let _ = app
                .writer
                .send(Output::SqlError {
                    id: id.clone(),
                    session: Some(resolved_session.clone()),
                    sqlstate: sqlstate.clone(),
                    message,
                    detail,
                    hint,
                    position,
                    trace: trace.clone(),
                })
                .await;
            emit_log(
                app,
                "query.sql_error",
                id.as_deref(),
                Some(&resolved_session),
                Some(&sqlstate),
                None,
                &trace,
            )
            .await;
        }
        Err(ExecError::Internal(message)) => {
            let trace = Trace::only_duration(start.elapsed().as_millis() as u64);
            let _ = app
                .writer
                .send(Output::Error {
                    id: id.clone(),
                    error_code: "invalid_request".to_string(),
                    error: message,
                    retryable: false,
                    trace: trace.clone(),
                })
                .await;
            emit_log(
                app,
                "query.error",
                id.as_deref(),
                Some(&resolved_session),
                Some("invalid_request"),
                None,
                &trace,
            )
            .await;
        }
    }
}

#[derive(Clone)]
enum RowEmitStatus {
    Sent { trace: Trace },
    TooLarge { trace: Trace },
}

async fn emit_rows_result(
    app: &Arc<App>,
    id: Option<String>,
    session: Option<String>,
    rows: Vec<Value>,
    start: Instant,
    opts: &ResolvedOptions,
) -> RowEmitStatus {
    if opts.stream_rows {
        let req_id = id.clone().unwrap_or_else(|| "cli".to_string());
        let columns = infer_columns(&rows);
        let _ = app
            .writer
            .send(Output::ResultStart {
                id: req_id.clone(),
                session: session.clone(),
                columns,
            })
            .await;

        let mut batch: Vec<Value> = vec![];
        let mut batch_bytes = 0usize;
        let mut total_bytes = 0usize;
        let mut row_count = 0usize;

        for row in rows {
            let sz = serde_json::to_vec(&row).map(|b| b.len()).unwrap_or(0);
            batch_bytes += sz;
            total_bytes += sz;
            row_count += 1;
            batch.push(row);

            if batch.len() >= opts.batch_rows || batch_bytes >= opts.batch_bytes {
                let n = batch.len();
                let _ = app
                    .writer
                    .send(Output::ResultRows {
                        id: req_id.clone(),
                        rows: std::mem::take(&mut batch),
                        rows_batch_count: n,
                    })
                    .await;
                batch_bytes = 0;
            }
        }

        for tail in std::iter::once(batch).filter(|r| !r.is_empty()) {
            let n = tail.len();
            let _ = app
                .writer
                .send(Output::ResultRows {
                    id: req_id.clone(),
                    rows: tail,
                    rows_batch_count: n,
                })
                .await;
        }

        let trace = Trace {
            duration_ms: start.elapsed().as_millis() as u64,
            row_count: Some(row_count),
            payload_bytes: Some(total_bytes),
        };
        let _ = app
            .writer
            .send(Output::ResultEnd {
                id: req_id,
                session,
                command_tag: format!("ROWS {row_count}"),
                trace: trace.clone(),
            })
            .await;

        return RowEmitStatus::Sent { trace };
    }

    let columns = infer_columns(&rows);
    let mut payload_bytes = 0usize;
    for row in &rows {
        payload_bytes += serde_json::to_vec(row).map(|b| b.len()).unwrap_or(0);
    }

    if rows.len() > opts.inline_max_rows || payload_bytes > opts.inline_max_bytes {
        let trace = Trace {
            duration_ms: start.elapsed().as_millis() as u64,
            row_count: Some(rows.len()),
            payload_bytes: Some(payload_bytes),
        };
        let _ = app
            .writer
            .send(Output::Error {
                id,
                error_code: "result_too_large".to_string(),
                error: "result exceeds inline limits; retry with stream_rows=true".to_string(),
                retryable: false,
                trace: trace.clone(),
            })
            .await;
        return RowEmitStatus::TooLarge { trace };
    }

    let row_count = rows.len();
    let trace = Trace {
        duration_ms: start.elapsed().as_millis() as u64,
        row_count: Some(row_count),
        payload_bytes: Some(payload_bytes),
    };
    let _ = app
        .writer
        .send(Output::Result {
            id,
            session,
            command_tag: format!("ROWS {row_count}"),
            columns,
            rows,
            row_count,
            trace: trace.clone(),
        })
        .await;

    RowEmitStatus::Sent { trace }
}

fn infer_columns(rows: &[Value]) -> Vec<ColumnInfo> {
    let Some(Value::Object(first)) = rows.first() else {
        return vec![];
    };
    first
        .keys()
        .map(|k| ColumnInfo {
            name: k.clone(),
            type_name: "json".to_string(),
        })
        .collect()
}

async fn emit_log(
    app: &Arc<App>,
    event: &str,
    request_id: Option<&str>,
    session: Option<&str>,
    error_code: Option<&str>,
    command_tag: Option<&str>,
    trace: &Trace,
) {
    let enabled = {
        let cfg = app.config.read().await;
        log_enabled(&cfg.log, event)
    };
    if !enabled {
        return;
    }

    let _ = app
        .writer
        .send(Output::Log {
            event: event.to_string(),
            request_id: request_id.map(std::string::ToString::to_string),
            session: session.map(std::string::ToString::to_string),
            error_code: error_code.map(std::string::ToString::to_string),
            command_tag: command_tag.map(std::string::ToString::to_string),
            version: None,
            argv: None,
            config: None,
            args: None,
            env: None,
            trace: trace.clone(),
        })
        .await;
}

fn log_enabled(filters: &[String], event: &str) -> bool {
    if filters.is_empty() {
        return false;
    }
    if filters.iter().any(|f| f == "all" || f == "*") {
        return true;
    }
    if filters.iter().any(|f| f == event) {
        return true;
    }
    let prefix = event.split('.').next().unwrap_or(event);
    filters.iter().any(|f| f == prefix)
}

#[cfg(test)]
#[path = "../tests/support/unit_handler.rs"]
mod tests;
