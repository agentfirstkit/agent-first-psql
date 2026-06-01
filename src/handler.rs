use crate::conn::resolve_session_name;
use crate::db::{
    DbExecutor, ExecError, ExecOutcome, ExecRequest, PostgresExecutor, RowSink, StreamOutcome,
    TransportLogContext,
};
use crate::protocol::{command_tag, error_code, log_enabled, log_event};
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
                sqlstate: None,
                message: None,
                detail: None,
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
                    sqlstate: None,
                    message: None,
                    detail: None,
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
                    transport_log: Some(TransportLogContext {
                        session: resolved_session.clone(),
                        log: cfg.log.clone(),
                        writer: app.writer.clone(),
                    }),
                },
                &mut sink,
            )
            .await;
        if cancel_requested(&cancel_slot_for_suppression) {
            return;
        }
        if !try_claim_terminal_emit(&cancel_slot_for_suppression) {
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
            transport_log: Some(TransportLogContext {
                session: resolved_session.clone(),
                log: cfg.log.clone(),
                writer: app.writer.clone(),
            }),
        })
        .await;

    if cancel_requested(&cancel_slot) {
        return;
    }
    if !try_claim_terminal_emit(&cancel_slot) {
        return;
    }

    match result {
        Ok(ExecOutcome::Rows {
            columns,
            rows,
            truncated,
            truncated_at_rows,
            truncated_at_bytes,
        }) => {
            let status = emit_rows_result(
                app,
                id.clone(),
                Some(resolved_session.clone()),
                columns,
                rows,
                InlineTruncation {
                    truncated,
                    at_rows: truncated_at_rows,
                    at_bytes: truncated_at_bytes,
                },
                start,
                &resolved_opts,
            )
            .await;
            let RowEmitStatus::Sent { trace } = status;
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
        Ok(ExecOutcome::Command { affected }) => {
            emit_command_result(app, id, &resolved_session, affected, start).await;
        }
        Err(err) => emit_exec_error(app, id, &resolved_session, err, start).await,
    }
}

fn permission_error_hint(options: &QueryOptions, session: &SessionConfig) -> String {
    match (session.transport_kind(), options.permission) {
        (Ok(TransportKind::Ssh), Some(permission)) if !permission.allows_ssh() => format!(
            "this session uses afpsql SSH transport, so permission `{}` is invalid; use `ssh-read` for reads or `ssh-write` for writes",
            permission.as_str()
        ),
        (Ok(TransportKind::Container), Some(permission)) if !permission.allows_container() => format!(
            "this session uses afpsql container transport, so permission `{}` is invalid; use `container-read` for reads or `container-write` for writes",
            permission.as_str()
        ),
        (Ok(TransportKind::Direct), Some(permission)) if permission.allows_ssh() => format!(
            "this session does not use afpsql SSH transport, so permission `{}` is invalid; use `read` for reads or `write` for writes",
            permission.as_str()
        ),
        (Ok(TransportKind::Direct), Some(permission)) if permission.allows_container() => format!(
            "this session does not use afpsql container transport, so permission `{}` is invalid; use `read` for reads or `write` for writes",
            permission.as_str()
        ),
        (Err(message), _) => message,
        _ => {
            "use read/write for direct connections, ssh-read/ssh-write for afpsql SSH transport, and container-read/container-write for afpsql container transport"
                .to_string()
        }
    }
}

/// Returns true if the caller wins the right to emit the terminal event
/// (`result`/`sql_error`/`error`) for this query id. CLI mode (no
/// cancel_slot) always wins. Pipe mode wins when the cancel dispatcher
/// hasn't already claimed and emitted `cancelled`.
fn try_claim_terminal_emit(cancel_slot: &Option<crate::db::CancelSlot>) -> bool {
    cancel_slot
        .as_ref()
        .map(|slot| slot.claim_terminal_emit())
        .unwrap_or(true)
}

fn cancel_requested(cancel_slot: &Option<crate::db::CancelSlot>) -> bool {
    cancel_slot
        .as_ref()
        .map(|slot| slot.is_cancelled())
        .unwrap_or(false)
}

pub async fn handle_session_info(app: &Arc<App>, id: Option<String>, session: Option<String>) {
    let start = Instant::now();
    let cfg = app.config.read().await.clone();
    let resolved_session = resolve_session_name(&cfg, session.as_deref());

    let Some(session_cfg) = cfg.sessions.get(&resolved_session).cloned() else {
        let trace = Trace::only_duration(start.elapsed().as_millis() as u64);
        let _ = app
            .writer
            .send(Output::Error {
                id: id.clone(),
                error_code: error_code::INVALID_REQUEST.to_string(),
                error: format!("unknown session: {resolved_session}"),
                sqlstate: None,
                message: None,
                detail: None,
                hint: Some(
                    "list active sessions with a `config` request, or pick the default session by omitting `session`"
                        .to_string(),
                ),
                retryable: false,
                trace,
            })
            .await;
        return;
    };

    let transport_kind = match session_cfg.transport_kind() {
        Ok(kind) => kind,
        Err(message) => {
            let trace = Trace::only_duration(start.elapsed().as_millis() as u64);
            let _ = app
                .writer
                .send(Output::Error {
                    id: id.clone(),
                    error_code: error_code::INVALID_REQUEST.to_string(),
                    error: message,
                    sqlstate: None,
                    message: None,
                    detail: None,
                    hint: Some(
                        "this session's transport flags are inconsistent; update the session via a `config` request before requesting `session_info`"
                            .to_string(),
                    ),
                    retryable: false,
                    trace,
                })
                .await;
            return;
        }
    };

    let permission_default = match transport_kind {
        TransportKind::Direct => Permission::Read,
        TransportKind::Ssh => Permission::SshRead,
        TransportKind::Container => Permission::ContainerRead,
    };

    let resolved_opts = match cfg
        .resolve_options_for_session(&QueryOptions::default(), &session_cfg)
    {
        Ok(opts) => opts,
        Err(message) => {
            let trace = Trace::only_duration(start.elapsed().as_millis() as u64);
            let _ = app
                .writer
                .send(Output::Error {
                    id: id.clone(),
                    error_code: error_code::INVALID_REQUEST.to_string(),
                    error: message,
                    sqlstate: None,
                    message: None,
                    detail: None,
                    hint: Some(
                        "the runtime config could not resolve query defaults for this session; update inline_max_rows/inline_max_bytes via `config` and retry"
                            .to_string(),
                    ),
                    retryable: false,
                    trace,
                })
                .await;
            return;
        }
    };

    let (database, user, host, port, server_version) =
        probe_session_identity(app, &resolved_session, &session_cfg, &resolved_opts).await;

    let trace = Trace::only_duration(start.elapsed().as_millis() as u64);
    let _ = app
        .writer
        .send(Output::SessionInfo {
            id,
            session: resolved_session,
            transport_kind: transport_kind.as_str().to_string(),
            permission_default: permission_default.as_str().to_string(),
            stream_rows_default: resolved_opts.stream_rows,
            batch_rows: resolved_opts.batch_rows,
            batch_bytes: resolved_opts.batch_bytes,
            inline_max_rows: resolved_opts.inline_max_rows,
            inline_max_bytes: resolved_opts.inline_max_bytes,
            statement_timeout_ms: resolved_opts.statement_timeout_ms,
            lock_timeout_ms: resolved_opts.lock_timeout_ms,
            database,
            user,
            host,
            port,
            server_version,
            trace,
        })
        .await;
}

async fn probe_session_identity(
    app: &Arc<App>,
    session_name: &str,
    session_cfg: &SessionConfig,
    resolved_opts: &ResolvedOptions,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<u16>,
    Option<String>,
) {
    let probe = app
        .executor
        .execute(ExecRequest {
            session_name,
            session_cfg,
            sql: "select current_database()::text as database, \
                  current_user::text as user, \
                  inet_server_addr()::text as host, \
                  inet_server_port() as port, \
                  current_setting('server_version') as server_version",
            params: &[],
            opts: resolved_opts,
            cancel_slot: None,
            transport_log: None,
        })
        .await;

    if let Ok(ExecOutcome::Rows { rows, .. }) = probe {
        if let Some(row) = rows.first().and_then(|v| v.as_object()) {
            let s = |key: &str| -> Option<String> {
                row.get(key).and_then(|v| v.as_str().map(|s| s.to_string()))
            };
            let port = row
                .get("port")
                .and_then(|v| v.as_i64())
                .and_then(|n| u16::try_from(n).ok())
                .or_else(|| {
                    row.get("port")
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse().ok())
                });
            return (
                s("database").or_else(|| session_cfg.dbname.clone()),
                s("user").or_else(|| session_cfg.user.clone()),
                s("host").or_else(|| session_cfg.host.clone()),
                port.or(session_cfg.port),
                s("server_version"),
            );
        }
    }

    (
        session_cfg.dbname.clone(),
        session_cfg.user.clone(),
        session_cfg.host.clone(),
        session_cfg.port,
        None,
    )
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
            truncated: false,
            truncated_at_rows: None,
            truncated_at_bytes: None,
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

pub(crate) async fn emit_exec_error(
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
                    sqlstate: None,
                    message: None,
                    detail: None,
                    hint: Some(
                        "cancellation is final; submit a new query with a fresh id to retry"
                            .to_string(),
                    ),
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
        ExecError::Connect(connect) => {
            let connect = *connect;
            let trace = Trace::only_duration(start.elapsed().as_millis() as u64);
            let _ = app
                .writer
                .send(Output::Error {
                    id: id.clone(),
                    error_code: error_code::CONNECT_FAILED.to_string(),
                    error: connect.error,
                    sqlstate: connect.sqlstate,
                    message: connect.message,
                    detail: connect.detail,
                    hint: connect.hint,
                    retryable: connect.retryable,
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
                    sqlstate: None,
                    message: None,
                    detail: None,
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
                    sqlstate: None,
                    message: None,
                    detail: None,
                    hint: Some(
                        "check that `params` count and types match the $1, $2, ... placeholders in `sql`"
                            .to_string(),
                    ),
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
                    sqlstate: None,
                    message: None,
                    detail: None,
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
                    sqlstate: None,
                    message: None,
                    detail: None,
                    hint: Some(
                        "afpsql hit an internal error; retry the query, then restart the session if it persists"
                            .to_string(),
                    ),
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
}

/// Carry the inline-truncation flags from the executor's row collector into
/// `emit_rows_result` without ballooning the function's argument list.
#[derive(Clone, Copy, Default)]
pub(crate) struct InlineTruncation {
    pub truncated: bool,
    pub at_rows: Option<usize>,
    pub at_bytes: Option<usize>,
}

#[allow(clippy::too_many_arguments)]
async fn emit_rows_result(
    app: &Arc<App>,
    id: Option<String>,
    session: Option<String>,
    columns: Vec<ColumnInfo>,
    rows: Vec<Value>,
    truncation: InlineTruncation,
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
            truncated: truncation.truncated,
            truncated_at_rows: truncation.at_rows,
            truncated_at_bytes: truncation.at_bytes,
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
            chain: None,
            trace: trace.clone(),
        })
        .await;
}

#[cfg(test)]
#[path = "../tests/support/unit_handler.rs"]
mod tests;
