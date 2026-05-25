use crate::conn::resolve_session_name;
use crate::db::{
    DbExecutor, ExecError, ExecOutcome, ExecRequest, PostgresExecutor, RowSink, StreamOutcome,
};
use crate::protocol::{command_tag, error_code, log_event};
use crate::types::*;
use serde_json::Value;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, Mutex, RwLock};

const QUERY_QUEUED: u8 = 0;
const QUERY_RUNNING: u8 = 1;
const QUERY_FINISHED: u8 = 2;
const QUERY_CANCELLED: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryPhase {
    Queued,
    Running,
    Finished,
    Cancelled,
}

pub struct QueryState {
    phase: AtomicU8,
}

impl QueryState {
    pub fn queued() -> Self {
        Self {
            phase: AtomicU8::new(QUERY_QUEUED),
        }
    }

    pub fn phase(&self) -> QueryPhase {
        match self.phase.load(Ordering::SeqCst) {
            QUERY_RUNNING => QueryPhase::Running,
            QUERY_FINISHED => QueryPhase::Finished,
            QUERY_CANCELLED => QueryPhase::Cancelled,
            _ => QueryPhase::Queued,
        }
    }

    pub fn set_phase(&self, phase: QueryPhase) {
        let value = match phase {
            QueryPhase::Queued => QUERY_QUEUED,
            QueryPhase::Running => QUERY_RUNNING,
            QueryPhase::Finished => QUERY_FINISHED,
            QueryPhase::Cancelled => QUERY_CANCELLED,
        };
        self.phase.store(value, Ordering::SeqCst);
    }

    pub fn try_start(&self) -> bool {
        self.phase
            .compare_exchange(
                QUERY_QUEUED,
                QUERY_RUNNING,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
    }

    pub fn is_finished(&self) -> bool {
        matches!(self.phase(), QueryPhase::Finished | QueryPhase::Cancelled)
    }
}

#[derive(Clone)]
pub struct InFlightQuery {
    pub cancel_slot: crate::db::CancelSlot,
    pub state: Arc<QueryState>,
}

impl InFlightQuery {
    pub async fn cancel_server_query(&self) -> Result<bool, String> {
        crate::db::cancel_query(&self.cancel_slot).await
    }
}

pub struct App {
    pub config: RwLock<RuntimeConfig>,
    pub executor: Arc<dyn DbExecutor>,
    pub writer: mpsc::Sender<Output>,
    pub in_flight: Mutex<std::collections::HashMap<String, InFlightQuery>>,
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
    cancel_slot: Option<crate::db::CancelSlot>,
) {
    let start = Instant::now();
    let cfg = app.config.read().await.clone();
    let resolved_session = resolve_session_name(&cfg, session.as_deref());

    let Some(session_cfg) = cfg.sessions.get(&resolved_session).cloned() else {
        let trace = Trace::only_duration(start.elapsed().as_millis() as u64);
        let _ = app
            .writer
            .send(Output::Error {
                id: id.clone(),
                error_code: error_code::CONNECT_FAILED.to_string(),
                error: format!("unknown session: {resolved_session}"),
                hint: Some(
                    "check --host/--port or PGHOST/PGPORT environment variables".to_string(),
                ),
                retryable: true,
                trace: trace.clone(),
            })
            .await;
        emit_log(
            app,
            log_event::QUERY_ERROR,
            id.as_deref(),
            Some(&resolved_session),
            Some(error_code::CONNECT_FAILED),
            None,
            &trace,
        )
        .await;
        return;
    };

    let resolved_opts = match cfg.resolve_options_for_session(&options, &session_cfg) {
        Ok(opts) => opts,
        Err(message) => {
            let trace = Trace::only_duration(start.elapsed().as_millis() as u64);
            let hint = permission_error_hint(&options, &session_cfg);
            let _ = app
                .writer
                .send(Output::Error {
                    id: id.clone(),
                    error_code: error_code::INVALID_REQUEST.to_string(),
                    error: message,
                    hint: Some(hint),
                    retryable: false,
                    trace: trace.clone(),
                })
                .await;
            emit_log(
                app,
                log_event::QUERY_ERROR,
                id.as_deref(),
                Some(&resolved_session),
                Some(error_code::INVALID_REQUEST),
                None,
                &trace,
            )
            .await;
            return;
        }
    };

    let cancel_slot_for_suppression = cancel_slot.clone();

    if resolved_opts.stream_rows {
        let mut sink = OutputRowSink::new(
            app.clone(),
            id.clone().unwrap_or_else(|| "cli".to_string()),
            Some(resolved_session.clone()),
            resolved_opts.batch_rows,
            resolved_opts.batch_bytes,
        );
        let result = app
            .executor
            .execute_streaming(
                ExecRequest {
                    session_name: &resolved_session,
                    session_cfg: &session_cfg,
                    sql: &sql,
                    params: &params,
                    opts: &resolved_opts,
                    cancel_slot: cancel_slot.clone(),
                },
                &mut sink,
            )
            .await;
        if cancel_requested(&cancel_slot_for_suppression) {
            return;
        }
        handle_streaming_result(app, id, resolved_session, result, sink, start).await;
        return;
    }

    let result = app
        .executor
        .execute(ExecRequest {
            session_name: &resolved_session,
            session_cfg: &session_cfg,
            sql: &sql,
            params: &params,
            opts: &resolved_opts,
            cancel_slot: cancel_slot.clone(),
        })
        .await;

    if cancel_requested(&cancel_slot) {
        return;
    }

    match result {
        Ok(ExecOutcome::Rows { columns, rows }) => {
            let status = emit_rows_result(
                app,
                id.clone(),
                Some(resolved_session.clone()),
                columns,
                rows,
                start,
                &resolved_opts,
            )
            .await;
            match status {
                RowEmitStatus::Sent { trace } => {
                    emit_log(
                        app,
                        log_event::QUERY_RESULT,
                        id.as_deref(),
                        Some(&resolved_session),
                        None,
                        Some(command_tag::SELECT),
                        &trace,
                    )
                    .await;
                }
                RowEmitStatus::TooLarge { trace } => {
                    emit_log(
                        app,
                        log_event::QUERY_ERROR,
                        id.as_deref(),
                        Some(&resolved_session),
                        Some(error_code::RESULT_TOO_LARGE),
                        None,
                        &trace,
                    )
                    .await;
                }
            }
        }
        Ok(ExecOutcome::Command { affected }) => {
            emit_command_result(app, id, &resolved_session, affected, start).await;
        }
        Err(err) => emit_exec_error(app, id, &resolved_session, err, start).await,
    }
}

fn permission_error_hint(options: &QueryOptions, session: &SessionConfig) -> String {
    let uses_ssh = session.uses_ssh_transport();
    match (uses_ssh, options.permission) {
        (true, Some(permission)) if !permission.allows_ssh() => {
            format!(
                "this session uses afpsql SSH transport, so permission `{}` is invalid; use `ssh-read` for reads or `ssh-write` for writes",
                permission.as_str()
            )
        }
        (false, Some(permission)) if permission.allows_ssh() => {
            format!(
                "this session does not use afpsql SSH transport, so permission `{}` is invalid; use `read` for reads or `write` for writes",
                permission.as_str()
            )
        }
        _ => {
            "use permission read/write for direct connections and ssh-read/ssh-write for afpsql SSH transport"
                .to_string()
        }
    }
}

fn cancel_requested(cancel_slot: &Option<crate::db::CancelSlot>) -> bool {
    cancel_slot
        .as_ref()
        .map(|slot| slot.is_cancelled())
        .unwrap_or(false)
}

async fn handle_streaming_result(
    app: &Arc<App>,
    id: Option<String>,
    resolved_session: String,
    result: Result<StreamOutcome, ExecError>,
    mut sink: OutputRowSink,
    start: Instant,
) {
    match result {
        Ok(StreamOutcome::Rows {
            row_count,
            payload_bytes,
        }) => {
            let _ = sink.flush_batch().await;
            let trace = Trace {
                duration_ms: start.elapsed().as_millis() as u64,
                row_count: Some(row_count),
                payload_bytes: Some(payload_bytes),
            };
            let _ = app
                .writer
                .send(Output::ResultEnd {
                    id: sink.id.clone(),
                    session: Some(resolved_session.clone()),
                    command_tag: command_tag::rows(row_count),
                    trace: trace.clone(),
                })
                .await;
            emit_log(
                app,
                log_event::QUERY_RESULT,
                id.as_deref(),
                Some(&resolved_session),
                None,
                Some(command_tag::SELECT),
                &trace,
            )
            .await;
        }
        Ok(StreamOutcome::Command { affected }) => {
            emit_command_result(app, id, &resolved_session, affected, start).await;
        }
        Err(err) => {
            emit_exec_error(app, id, &resolved_session, err, start).await;
        }
    }
}

async fn emit_command_result(
    app: &Arc<App>,
    id: Option<String>,
    resolved_session: &str,
    affected: usize,
    start: Instant,
) {
    let command_tag = command_tag::execute(affected);
    let trace = Trace {
        duration_ms: start.elapsed().as_millis() as u64,
        row_count: Some(0),
        payload_bytes: Some(0),
    };
    let _ = app
        .writer
        .send(Output::Result {
            id: id.clone(),
            session: Some(resolved_session.to_string()),
            command_tag: command_tag.clone(),
            columns: vec![],
            rows: vec![],
            row_count: 0,
            trace: trace.clone(),
        })
        .await;
    emit_log(
        app,
        log_event::QUERY_RESULT,
        id.as_deref(),
        Some(resolved_session),
        None,
        Some(command_tag::EXECUTE),
        &trace,
    )
    .await;
}

async fn emit_exec_error(
    app: &Arc<App>,
    id: Option<String>,
    resolved_session: &str,
    err: ExecError,
    start: Instant,
) {
    match err {
        ExecError::Cancelled => {
            let trace = Trace::only_duration(start.elapsed().as_millis() as u64);
            let _ = app
                .writer
                .send(Output::Error {
                    id: id.clone(),
                    error_code: error_code::CANCELLED.to_string(),
                    error: "query cancelled".to_string(),
                    hint: None,
                    retryable: false,
                    trace: trace.clone(),
                })
                .await;
            emit_log(
                app,
                log_event::QUERY_ERROR,
                id.as_deref(),
                Some(resolved_session),
                Some(error_code::CANCELLED),
                None,
                &trace,
            )
            .await;
        }
        ExecError::Connect(message) => {
            let trace = Trace::only_duration(start.elapsed().as_millis() as u64);
            let _ = app
                .writer
                .send(Output::Error {
                    id: id.clone(),
                    error_code: error_code::CONNECT_FAILED.to_string(),
                    error: message,
                    hint: Some("check --host/--port or PGHOST/PGPORT; for remote local-only PostgreSQL use --ssh user@server; for sudo-only Unix-socket access use --ssh-sudo-user with an explicit --ssh-remote-socket, or set --host/PGHOST to the remote socket directory".to_string()),
                    retryable: true,
                    trace: trace.clone(),
                })
                .await;
            emit_log(
                app,
                log_event::QUERY_ERROR,
                id.as_deref(),
                Some(resolved_session),
                Some(error_code::CONNECT_FAILED),
                None,
                &trace,
            )
            .await;
        }
        ExecError::Config { message, hint } => {
            let trace = Trace::only_duration(start.elapsed().as_millis() as u64);
            let _ = app
                .writer
                .send(Output::Error {
                    id: id.clone(),
                    error_code: error_code::INVALID_REQUEST.to_string(),
                    error: message,
                    hint,
                    retryable: false,
                    trace: trace.clone(),
                })
                .await;
            emit_log(
                app,
                log_event::QUERY_ERROR,
                id.as_deref(),
                Some(resolved_session),
                Some(error_code::INVALID_REQUEST),
                None,
                &trace,
            )
            .await;
        }
        ExecError::InvalidParams(message) => {
            let trace = Trace::only_duration(start.elapsed().as_millis() as u64);
            let _ = app
                .writer
                .send(Output::Error {
                    id: id.clone(),
                    error_code: error_code::INVALID_PARAMS.to_string(),
                    error: message,
                    hint: None,
                    retryable: false,
                    trace: trace.clone(),
                })
                .await;
            emit_log(
                app,
                log_event::QUERY_ERROR,
                id.as_deref(),
                Some(resolved_session),
                Some(error_code::INVALID_PARAMS),
                None,
                &trace,
            )
            .await;
        }
        ExecError::ResultTooLarge {
            row_count,
            payload_bytes,
        } => {
            let trace = Trace {
                duration_ms: start.elapsed().as_millis() as u64,
                row_count: Some(row_count),
                payload_bytes: Some(payload_bytes),
            };
            let _ = app
                .writer
                .send(Output::Error {
                    id: id.clone(),
                    error_code: error_code::RESULT_TOO_LARGE.to_string(),
                    error: "result exceeds inline limits".to_string(),
                    hint: Some(result_too_large_hint()),
                    retryable: false,
                    trace: trace.clone(),
                })
                .await;
            emit_log(
                app,
                log_event::QUERY_ERROR,
                id.as_deref(),
                Some(resolved_session),
                Some(error_code::RESULT_TOO_LARGE),
                None,
                &trace,
            )
            .await;
        }
        ExecError::Sql {
            sqlstate,
            message,
            detail,
            hint,
            position,
        } => {
            let trace = Trace::only_duration(start.elapsed().as_millis() as u64);
            let _ = app
                .writer
                .send(Output::SqlError {
                    id: id.clone(),
                    session: Some(resolved_session.to_string()),
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
                log_event::QUERY_SQL_ERROR,
                id.as_deref(),
                Some(resolved_session),
                Some(&sqlstate),
                None,
                &trace,
            )
            .await;
        }
        ExecError::Internal(message) => {
            let trace = Trace::only_duration(start.elapsed().as_millis() as u64);
            let _ = app
                .writer
                .send(Output::Error {
                    id: id.clone(),
                    error_code: error_code::INVALID_REQUEST.to_string(),
                    error: message,
                    hint: None,
                    retryable: false,
                    trace: trace.clone(),
                })
                .await;
            emit_log(
                app,
                log_event::QUERY_ERROR,
                id.as_deref(),
                Some(resolved_session),
                Some(error_code::INVALID_REQUEST),
                None,
                &trace,
            )
            .await;
        }
    }
}

struct OutputRowSink {
    app: Arc<App>,
    id: String,
    session: Option<String>,
    batch: Vec<Value>,
    batch_bytes: usize,
    batch_rows_limit: usize,
    batch_bytes_limit: usize,
}

impl OutputRowSink {
    fn new(
        app: Arc<App>,
        id: String,
        session: Option<String>,
        batch_rows_limit: usize,
        batch_bytes_limit: usize,
    ) -> Self {
        Self {
            app,
            id,
            session,
            batch: vec![],
            batch_bytes: 0,
            batch_rows_limit,
            batch_bytes_limit,
        }
    }

    async fn flush_batch(&mut self) -> Result<(), ExecError> {
        if self.batch.is_empty() {
            return Ok(());
        }
        let n = self.batch.len();
        let rows = std::mem::take(&mut self.batch);
        self.batch_bytes = 0;
        self.app
            .writer
            .send(Output::ResultRows {
                id: self.id.clone(),
                rows,
                rows_batch_count: n,
            })
            .await
            .map_err(|_| ExecError::Internal("output channel closed".to_string()))
    }
}

#[async_trait::async_trait]
impl RowSink for OutputRowSink {
    async fn start(&mut self, columns: Vec<ColumnInfo>) -> Result<(), ExecError> {
        self.app
            .writer
            .send(Output::ResultStart {
                id: self.id.clone(),
                session: self.session.clone(),
                columns,
            })
            .await
            .map_err(|_| ExecError::Internal("output channel closed".to_string()))
    }

    async fn row(&mut self, row: Value, row_bytes: usize) -> Result<(), ExecError> {
        self.batch_bytes += row_bytes;
        self.batch.push(row);
        if self.batch.len() >= self.batch_rows_limit || self.batch_bytes >= self.batch_bytes_limit {
            self.flush_batch().await?;
        }
        Ok(())
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
    columns: Vec<ColumnInfo>,
    rows: Vec<Value>,
    start: Instant,
    opts: &ResolvedOptions,
) -> RowEmitStatus {
    if opts.stream_rows {
        let req_id = id.clone().unwrap_or_else(|| "cli".to_string());
        let _ = app
            .writer
            .send(Output::ResultStart {
                id: req_id.clone(),
                session: session.clone(),
                columns: columns.clone(),
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
                command_tag: command_tag::rows(row_count),
                trace: trace.clone(),
            })
            .await;

        return RowEmitStatus::Sent { trace };
    }

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
                error_code: error_code::RESULT_TOO_LARGE.to_string(),
                error: "result exceeds inline limits".to_string(),
                hint: Some(result_too_large_hint()),
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
            command_tag: command_tag::rows(row_count),
            columns,
            rows,
            row_count,
            trace: trace.clone(),
        })
        .await;

    RowEmitStatus::Sent { trace }
}

fn result_too_large_hint() -> String {
    "retry with stream_rows=true, or increase --inline-max-rows/--inline-max-bytes".to_string()
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
