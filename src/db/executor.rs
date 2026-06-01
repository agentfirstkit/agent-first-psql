use super::errors::{map_pg_error, ConnectError, ExecError};
use super::params::{build_param_refs, build_params, validate_param_count, QueryParam};
use super::rows::{row_json_size, row_to_json_fallback};
use super::session::{
    connect_session, get_session, new_session_map, remove_sessions, shutdown_all_sessions,
    CancelSlot, SessionMap,
};
use crate::protocol::{log_enabled, log_event};
use crate::types::{ColumnInfo, Output, ResolvedOptions, SessionConfig, Trace};
use async_trait::async_trait;
use futures_util::TryStreamExt;
use serde_json::Value;
use std::collections::HashSet;
use std::pin::pin;
use tokio::sync::mpsc;
use tokio_postgres::types::ToSql;

#[derive(Debug)]
pub enum ExecOutcome {
    Rows {
        columns: Vec<ColumnInfo>,
        rows: Vec<Value>,
        /// When true, the inline row/byte limit was hit and `rows` only
        /// contains the prefix that fit. The underlying statement still
        /// executed in full — for `UPDATE ... RETURNING`, the writes
        /// happened even though their RETURNING projection was capped.
        truncated: bool,
        /// Inline-row limit if that's what fired (otherwise None).
        truncated_at_rows: Option<usize>,
        /// Inline-byte limit if that's what fired (otherwise None).
        truncated_at_bytes: Option<usize>,
    },
    Command {
        affected: usize,
    },
}

#[derive(Debug)]
pub struct DryRunOutcome {
    pub param_types: Vec<String>,
    pub columns: Vec<ColumnInfo>,
}

#[derive(Debug)]
pub enum StreamOutcome {
    Rows {
        row_count: usize,
        payload_bytes: usize,
    },
    Command {
        affected: usize,
    },
}

#[async_trait]
pub trait RowSink: Send {
    async fn start(&mut self, columns: Vec<ColumnInfo>) -> Result<(), ExecError>;
    async fn row(&mut self, row: Value, row_bytes: usize) -> Result<(), ExecError>;
}

pub struct ExecRequest<'a> {
    pub session_name: &'a str,
    pub session_cfg: &'a SessionConfig,
    pub sql: &'a str,
    pub params: &'a [Value],
    pub opts: &'a ResolvedOptions,
    pub cancel_slot: Option<CancelSlot>,
    pub transport_log: Option<TransportLogContext>,
}

#[derive(Clone)]
pub struct TransportLogContext {
    pub session: String,
    pub log: Vec<String>,
    pub writer: mpsc::Sender<Output>,
}

#[async_trait]
pub trait DbExecutor: Send + Sync {
    async fn execute(&self, req: ExecRequest<'_>) -> Result<ExecOutcome, ExecError>;

    /// Validate `sql` and the param shape without running the statement. The
    /// server prepares the statement inside a transaction that is rolled back,
    /// returning the inferred parameter types and column metadata.
    async fn prepare_only(&self, req: ExecRequest<'_>) -> Result<DryRunOutcome, ExecError>;

    /// Open an explicit transaction on the named session. Subsequent
    /// `execute`/`execute_streaming` calls on that session bypass the
    /// implicit per-query `BEGIN..COMMIT` wrap until `tx_commit` or
    /// `tx_rollback` is called.
    async fn tx_begin(
        &self,
        _session_name: &str,
        _session_cfg: &SessionConfig,
        _read_only: bool,
    ) -> Result<(), ExecError> {
        Err(ExecError::Internal(
            "explicit transactions not implemented for this executor".to_string(),
        ))
    }

    async fn tx_commit(
        &self,
        _session_name: &str,
        _session_cfg: &SessionConfig,
    ) -> Result<(), ExecError> {
        Err(ExecError::Internal(
            "explicit transactions not implemented for this executor".to_string(),
        ))
    }

    async fn tx_rollback(
        &self,
        _session_name: &str,
        _session_cfg: &SessionConfig,
    ) -> Result<(), ExecError> {
        Err(ExecError::Internal(
            "explicit transactions not implemented for this executor".to_string(),
        ))
    }

    async fn execute_streaming(
        &self,
        req: ExecRequest<'_>,
        sink: &mut (dyn RowSink + Send),
    ) -> Result<StreamOutcome, ExecError> {
        match self.execute(req).await? {
            ExecOutcome::Rows { columns, rows, .. } => {
                sink.start(columns).await?;
                let mut row_count = 0usize;
                let mut payload_bytes = 0usize;
                for row in rows {
                    let row_bytes = row_json_size(&row);
                    payload_bytes += row_bytes;
                    row_count += 1;
                    sink.row(row, row_bytes).await?;
                }
                Ok(StreamOutcome::Rows {
                    row_count,
                    payload_bytes,
                })
            }
            ExecOutcome::Command { affected } => Ok(StreamOutcome::Command { affected }),
        }
    }

    async fn invalidate_sessions(&self, _session_names: &[String]) {}

    async fn shutdown(&self) {}
}

pub struct PostgresExecutor {
    sessions: SessionMap,
}

impl PostgresExecutor {
    pub fn new() -> Self {
        Self {
            sessions: new_session_map(),
        }
    }
}

#[async_trait]
impl DbExecutor for PostgresExecutor {
    async fn execute(&self, req: ExecRequest<'_>) -> Result<ExecOutcome, ExecError> {
        let session = get_session(&self.sessions, req.session_name).await;
        let in_explicit_tx = session.explicit_tx_active();
        let mut client_guard = session.client.lock().await;
        let transport = ensure_connected(&mut client_guard, req.session_cfg).await?;
        emit_transport_selected(&req, transport).await?;
        let Some(client) = client_guard.as_mut() else {
            return Err(ExecError::Connect(Box::new(ConnectError::new(
                "connection unavailable",
            ))));
        };
        let Some(pg_client) = client.client.as_mut() else {
            return Err(ExecError::Connect(Box::new(ConnectError::new(
                "connection unavailable",
            ))));
        };
        install_cancel_context(
            &req.cancel_slot,
            pg_client.cancel_token(),
            client.backend_pid,
            req.session_cfg,
        )
        .await;
        if cancel_requested(&req.cancel_slot) {
            return Err(ExecError::Cancelled);
        }
        let result = if in_explicit_tx {
            execute_in_open_tx(pg_client, &req).await
        } else {
            execute_with_client(pg_client, &req).await
        };
        if should_drop_connection(&result) {
            *client_guard = None;
            // Connection dropped means the in-PG explicit tx is also gone.
            session.set_explicit_tx(false);
        }
        result
    }

    async fn execute_streaming(
        &self,
        req: ExecRequest<'_>,
        sink: &mut (dyn RowSink + Send),
    ) -> Result<StreamOutcome, ExecError> {
        let session = get_session(&self.sessions, req.session_name).await;
        let in_explicit_tx = session.explicit_tx_active();
        let mut client_guard = session.client.lock().await;
        let transport = ensure_connected(&mut client_guard, req.session_cfg).await?;
        emit_transport_selected(&req, transport).await?;
        let Some(client) = client_guard.as_mut() else {
            return Err(ExecError::Connect(Box::new(ConnectError::new(
                "connection unavailable",
            ))));
        };
        let Some(pg_client) = client.client.as_mut() else {
            return Err(ExecError::Connect(Box::new(ConnectError::new(
                "connection unavailable",
            ))));
        };
        install_cancel_context(
            &req.cancel_slot,
            pg_client.cancel_token(),
            client.backend_pid,
            req.session_cfg,
        )
        .await;
        if cancel_requested(&req.cancel_slot) {
            return Err(ExecError::Cancelled);
        }
        let result = if in_explicit_tx {
            execute_streaming_in_open_tx(pg_client, &req, sink).await
        } else {
            execute_streaming_with_client(pg_client, &req, sink).await
        };
        if should_drop_connection(&result) {
            *client_guard = None;
            session.set_explicit_tx(false);
        }
        result
    }

    async fn tx_begin(
        &self,
        session_name: &str,
        session_cfg: &SessionConfig,
        read_only: bool,
    ) -> Result<(), ExecError> {
        let session = get_session(&self.sessions, session_name).await;
        if session.explicit_tx_active() {
            return Err(ExecError::InvalidParams(
                "session is already in an explicit transaction; commit or rollback first"
                    .to_string(),
            ));
        }
        let mut client_guard = session.client.lock().await;
        ensure_connected(&mut client_guard, session_cfg).await?;
        let Some(client) = client_guard.as_mut() else {
            return Err(ExecError::Connect(Box::new(ConnectError::new(
                "connection unavailable",
            ))));
        };
        let Some(pg_client) = client.client.as_mut() else {
            return Err(ExecError::Connect(Box::new(ConnectError::new(
                "connection unavailable",
            ))));
        };
        let sql = if read_only {
            "BEGIN READ ONLY"
        } else {
            "BEGIN"
        };
        pg_client.batch_execute(sql).await.map_err(map_pg_error)?;
        session.set_explicit_tx(true);
        Ok(())
    }

    async fn tx_commit(
        &self,
        session_name: &str,
        session_cfg: &SessionConfig,
    ) -> Result<(), ExecError> {
        let session = get_session(&self.sessions, session_name).await;
        if !session.explicit_tx_active() {
            return Err(ExecError::InvalidParams(
                "no explicit transaction is open on this session; send `begin` first".to_string(),
            ));
        }
        let mut client_guard = session.client.lock().await;
        ensure_connected(&mut client_guard, session_cfg).await?;
        let Some(client) = client_guard.as_mut() else {
            return Err(ExecError::Connect(Box::new(ConnectError::new(
                "connection unavailable",
            ))));
        };
        let Some(pg_client) = client.client.as_mut() else {
            return Err(ExecError::Connect(Box::new(ConnectError::new(
                "connection unavailable",
            ))));
        };
        let result = pg_client
            .batch_execute("COMMIT")
            .await
            .map_err(map_pg_error);
        session.set_explicit_tx(false);
        result
    }

    async fn tx_rollback(
        &self,
        session_name: &str,
        session_cfg: &SessionConfig,
    ) -> Result<(), ExecError> {
        let session = get_session(&self.sessions, session_name).await;
        if !session.explicit_tx_active() {
            return Err(ExecError::InvalidParams(
                "no explicit transaction is open on this session; send `begin` first".to_string(),
            ));
        }
        let mut client_guard = session.client.lock().await;
        ensure_connected(&mut client_guard, session_cfg).await?;
        let Some(client) = client_guard.as_mut() else {
            return Err(ExecError::Connect(Box::new(ConnectError::new(
                "connection unavailable",
            ))));
        };
        let Some(pg_client) = client.client.as_mut() else {
            return Err(ExecError::Connect(Box::new(ConnectError::new(
                "connection unavailable",
            ))));
        };
        let result = pg_client
            .batch_execute("ROLLBACK")
            .await
            .map_err(map_pg_error);
        session.set_explicit_tx(false);
        result
    }

    async fn prepare_only(&self, req: ExecRequest<'_>) -> Result<DryRunOutcome, ExecError> {
        let session = get_session(&self.sessions, req.session_name).await;
        let mut client_guard = session.client.lock().await;
        let transport = ensure_connected(&mut client_guard, req.session_cfg).await?;
        emit_transport_selected(&req, transport).await?;
        let Some(client) = client_guard.as_mut() else {
            return Err(ExecError::Connect(Box::new(ConnectError::new(
                "connection unavailable",
            ))));
        };
        let Some(pg_client) = client.client.as_mut() else {
            return Err(ExecError::Connect(Box::new(ConnectError::new(
                "connection unavailable",
            ))));
        };
        let result = prepare_only_with_client(pg_client, &req).await;
        if should_drop_connection_dry_run(&result) {
            *client_guard = None;
        }
        result
    }

    async fn invalidate_sessions(&self, session_names: &[String]) {
        remove_sessions(&self.sessions, session_names).await;
    }

    async fn shutdown(&self) {
        shutdown_all_sessions(&self.sessions).await;
    }
}

async fn ensure_connected(
    client: &mut Option<super::session::SessionClient>,
    session_cfg: &SessionConfig,
) -> Result<Option<super::session::TransportSelection>, ExecError> {
    if client.as_ref().map(|c| c.is_closed()).unwrap_or(false) {
        *client = None;
    }
    if client.is_none() {
        let (connected, transport) = connect_session(session_cfg).await?;
        *client = Some(connected);
        return Ok(Some(transport));
    }
    Ok(None)
}

async fn emit_transport_selected(
    req: &ExecRequest<'_>,
    selected: Option<super::session::TransportSelection>,
) -> Result<(), ExecError> {
    let (Some(selected), Some(ctx)) = (selected, req.transport_log.as_ref()) else {
        return Ok(());
    };
    emit_libpq_env_fallback(ctx, req.session_cfg).await?;
    if !log_enabled(&ctx.log, log_event::TRANSPORT_SELECTED) {
        return Ok(());
    }
    let chain = super::session::transport_chain_summary(req.session_cfg, !ctx.log.is_empty());
    ctx.writer
        .send(Output::Log {
            event: log_event::TRANSPORT_SELECTED.to_string(),
            request_id: None,
            session: Some(ctx.session.clone()),
            error_code: None,
            command_tag: None,
            version: None,
            config: None,
            args: None,
            env: None,
            chain: Some(chain),
            trace: Trace::only_duration(selected.duration_ms),
        })
        .await
        .map_err(|_| ExecError::Internal("output channel closed".to_string()))
}

async fn emit_libpq_env_fallback(
    ctx: &TransportLogContext,
    cfg: &SessionConfig,
) -> Result<(), ExecError> {
    if !log_enabled(&ctx.log, log_event::CONNECT_LIBPQ_ENV_FALLBACK) {
        return Ok(());
    }
    let used = crate::conn::libpq_env_fallbacks_in_use(cfg);
    if used.is_empty() {
        return Ok(());
    }
    let mut config = serde_json::Map::new();
    config.insert(
        "env_vars".to_string(),
        Value::Array(used.iter().map(|v| Value::from(*v)).collect()),
    );
    config.insert(
        "note".to_string(),
        Value::from(
            "libpq PG* environment variables filled connection fields not given via flags/secrets; prefer explicit --host/--user/--password-secret-env for agent runs",
        ),
    );
    ctx.writer
        .send(Output::Log {
            event: log_event::CONNECT_LIBPQ_ENV_FALLBACK.to_string(),
            request_id: None,
            session: Some(ctx.session.clone()),
            error_code: None,
            command_tag: None,
            version: None,
            config: Some(Value::Object(config)),
            args: None,
            env: None,
            chain: None,
            trace: Trace::only_duration(0),
        })
        .await
        .map_err(|_| ExecError::Internal("output channel closed".to_string()))
}

fn should_drop_connection<T>(result: &Result<T, ExecError>) -> bool {
    matches!(
        result,
        Err(ExecError::Connect(_)) | Err(ExecError::Internal(_))
    )
}

fn should_drop_connection_dry_run(result: &Result<DryRunOutcome, ExecError>) -> bool {
    matches!(
        result,
        Err(ExecError::Connect(_)) | Err(ExecError::Internal(_))
    )
}

async fn prepare_only_with_client(
    client: &mut tokio_postgres::Client,
    req: &ExecRequest<'_>,
) -> Result<DryRunOutcome, ExecError> {
    let mut tx = start_transaction(client, true).await?;
    let result = prepare_only_in_transaction(&mut tx, req).await;
    // Always rollback — dry-run never commits.
    let _ = tx.rollback().await;
    result
}

async fn prepare_only_in_transaction(
    tx: &mut tokio_postgres::Transaction<'_>,
    req: &ExecRequest<'_>,
) -> Result<DryRunOutcome, ExecError> {
    let stmt = tx.prepare(req.sql).await.map_err(map_pg_error)?;
    let columns = statement_columns(&stmt);
    validate_unique_column_names(&columns)?;
    validate_param_count(stmt.params().len(), req.params.len())?;
    let param_types = stmt.params().iter().map(|t| t.name().to_string()).collect();
    Ok(DryRunOutcome {
        param_types,
        columns,
    })
}

fn cancel_requested(cancel_slot: &Option<CancelSlot>) -> bool {
    cancel_slot
        .as_ref()
        .map(|slot| slot.is_cancelled())
        .unwrap_or(false)
}

async fn execute_with_client(
    client: &mut tokio_postgres::Client,
    req: &ExecRequest<'_>,
) -> Result<ExecOutcome, ExecError> {
    let mut tx = start_transaction(client, req.opts.read_only).await?;
    let result = execute_in_transaction(&mut tx, req).await;
    finish_transaction(tx, result).await
}

/// Run a query against a client that is already inside an explicit
/// transaction. The query is wrapped in a savepoint so a failure does not
/// abort the user's outer transaction — the agent can retry or recover
/// without losing prior progress.
async fn execute_in_open_tx(
    client: &mut tokio_postgres::Client,
    req: &ExecRequest<'_>,
) -> Result<ExecOutcome, ExecError> {
    client
        .batch_execute("SAVEPOINT afpsql_explicit")
        .await
        .map_err(map_pg_error)?;
    let result = execute_in_open_tx_inner(client, req).await;
    match &result {
        Ok(_) => {
            client
                .batch_execute("RELEASE SAVEPOINT afpsql_explicit")
                .await
                .map_err(map_pg_error)?;
        }
        Err(_) => {
            let _ = client
                .batch_execute("ROLLBACK TO SAVEPOINT afpsql_explicit")
                .await;
            let _ = client
                .batch_execute("RELEASE SAVEPOINT afpsql_explicit")
                .await;
        }
    }
    result
}

async fn execute_in_open_tx_inner(
    client: &mut tokio_postgres::Client,
    req: &ExecRequest<'_>,
) -> Result<ExecOutcome, ExecError> {
    apply_query_settings_client(client, req.opts).await?;
    let stmt = client.prepare(req.sql).await.map_err(map_pg_error)?;
    let columns = statement_columns(&stmt);
    validate_unique_column_names(&columns)?;
    validate_param_count(stmt.params().len(), req.params.len())?;
    let query_params = build_params(req.params, stmt.params())?;
    let bind_refs = build_param_refs(&query_params);

    if columns.is_empty() {
        let affected = client
            .execute(&stmt, &bind_refs)
            .await
            .map_err(map_pg_error)? as usize;
        return Ok(ExecOutcome::Command { affected });
    }

    let mut collector =
        InlineRowCollector::new(columns, req.opts.inline_max_rows, req.opts.inline_max_bytes);
    let stream = client
        .query_raw(&stmt, bind_refs)
        .await
        .map_err(map_pg_error)?;
    let mut rows = pin!(stream);
    while let Some(row) = rows.try_next().await.map_err(map_pg_error)? {
        let value = row_to_json_fallback(&row);
        let row_bytes = row_json_size(&value);
        let _ = collector.push(value, row_bytes)?;
        if collector.is_truncated() {
            break;
        }
    }
    Ok(ExecOutcome::Rows {
        truncated: collector.is_truncated(),
        truncated_at_rows: collector.truncated_at_rows,
        truncated_at_bytes: collector.truncated_at_bytes,
        columns: collector.columns,
        rows: collector.rows,
    })
}

async fn execute_streaming_in_open_tx(
    client: &mut tokio_postgres::Client,
    req: &ExecRequest<'_>,
    sink: &mut (dyn RowSink + Send),
) -> Result<StreamOutcome, ExecError> {
    client
        .batch_execute("SAVEPOINT afpsql_explicit")
        .await
        .map_err(map_pg_error)?;
    let result = execute_streaming_in_open_tx_inner(client, req, sink).await;
    match &result {
        Ok(_) => {
            client
                .batch_execute("RELEASE SAVEPOINT afpsql_explicit")
                .await
                .map_err(map_pg_error)?;
        }
        Err(_) => {
            let _ = client
                .batch_execute("ROLLBACK TO SAVEPOINT afpsql_explicit")
                .await;
            let _ = client
                .batch_execute("RELEASE SAVEPOINT afpsql_explicit")
                .await;
        }
    }
    result
}

async fn execute_streaming_in_open_tx_inner(
    client: &mut tokio_postgres::Client,
    req: &ExecRequest<'_>,
    sink: &mut (dyn RowSink + Send),
) -> Result<StreamOutcome, ExecError> {
    apply_query_settings_client(client, req.opts).await?;
    let stmt = client.prepare(req.sql).await.map_err(map_pg_error)?;
    let columns = statement_columns(&stmt);
    validate_unique_column_names(&columns)?;
    validate_param_count(stmt.params().len(), req.params.len())?;
    let query_params = build_params(req.params, stmt.params())?;
    let bind_refs = build_param_refs(&query_params);

    if columns.is_empty() {
        let affected = client
            .execute(&stmt, &bind_refs)
            .await
            .map_err(map_pg_error)? as usize;
        return Ok(StreamOutcome::Command { affected });
    }

    sink.start(columns).await?;
    let stream = client
        .query_raw(&stmt, bind_refs)
        .await
        .map_err(map_pg_error)?;
    let mut rows = pin!(stream);
    let mut row_count = 0usize;
    let mut payload_bytes = 0usize;
    while let Some(row) = rows.try_next().await.map_err(map_pg_error)? {
        let value = row_to_json_fallback(&row);
        let row_bytes = row_json_size(&value);
        payload_bytes += row_bytes;
        row_count += 1;
        sink.row(value, row_bytes).await?;
    }
    Ok(StreamOutcome::Rows {
        row_count,
        payload_bytes,
    })
}

async fn apply_query_settings_client(
    client: &mut tokio_postgres::Client,
    opts: &ResolvedOptions,
) -> Result<(), ExecError> {
    let statement_timeout = format!("{}ms", opts.statement_timeout_ms);
    client
        .execute(
            "select set_config('statement_timeout', $1, true)",
            &[&statement_timeout],
        )
        .await
        .map_err(map_pg_error)?;

    let lock_timeout = format!("{}ms", opts.lock_timeout_ms);
    client
        .execute(
            "select set_config('lock_timeout', $1, true)",
            &[&lock_timeout],
        )
        .await
        .map_err(map_pg_error)?;
    Ok(())
}

async fn execute_in_transaction(
    tx: &mut tokio_postgres::Transaction<'_>,
    req: &ExecRequest<'_>,
) -> Result<ExecOutcome, ExecError> {
    apply_query_settings(tx, req.opts).await?;
    let prepared = prepare_bound_statement(tx, req.sql, req.params).await?;
    let bind_refs = build_param_refs(&prepared.query_params);

    if !prepared.columns.is_empty() {
        let mut collector = InlineRowCollector::new(
            prepared.columns,
            req.opts.inline_max_rows,
            req.opts.inline_max_bytes,
        );
        collect_rows_wrapped_or_direct(
            tx,
            req.sql,
            req.params,
            &prepared.stmt,
            bind_refs,
            &mut collector,
            req.opts.batch_rows,
        )
        .await?;

        return Ok(ExecOutcome::Rows {
            truncated: collector.is_truncated(),
            truncated_at_rows: collector.truncated_at_rows,
            truncated_at_bytes: collector.truncated_at_bytes,
            columns: collector.columns,
            rows: collector.rows,
        });
    }

    let affected = tx
        .execute(&prepared.stmt, &bind_refs)
        .await
        .map_err(map_pg_error)? as usize;

    Ok(ExecOutcome::Command { affected })
}

async fn execute_streaming_with_client(
    client: &mut tokio_postgres::Client,
    req: &ExecRequest<'_>,
    sink: &mut (dyn RowSink + Send),
) -> Result<StreamOutcome, ExecError> {
    let mut tx = start_transaction(client, req.opts.read_only).await?;
    let result = execute_streaming_in_transaction(&mut tx, req, sink).await;
    finish_transaction(tx, result).await
}

async fn execute_streaming_in_transaction(
    tx: &mut tokio_postgres::Transaction<'_>,
    req: &ExecRequest<'_>,
    sink: &mut (dyn RowSink + Send),
) -> Result<StreamOutcome, ExecError> {
    apply_query_settings(tx, req.opts).await?;
    let prepared = prepare_bound_statement(tx, req.sql, req.params).await?;
    let bind_refs = build_param_refs(&prepared.query_params);

    if prepared.columns.is_empty() {
        let affected = tx
            .execute(&prepared.stmt, &bind_refs)
            .await
            .map_err(map_pg_error)? as usize;
        return Ok(StreamOutcome::Command { affected });
    }

    let stats = stream_rows_wrapped_or_direct(
        tx,
        req.sql,
        req.params,
        &prepared.stmt,
        bind_refs,
        prepared.columns,
        sink,
    )
    .await?;

    Ok(StreamOutcome::Rows {
        row_count: stats.row_count,
        payload_bytes: stats.payload_bytes,
    })
}

async fn finish_transaction<T>(
    tx: tokio_postgres::Transaction<'_>,
    result: Result<T, ExecError>,
) -> Result<T, ExecError> {
    match result {
        Ok(outcome) => {
            tx.commit().await.map_err(map_pg_error)?;
            Ok(outcome)
        }
        Err(err) => {
            tx.rollback().await.map_err(map_pg_error)?;
            Err(err)
        }
    }
}

async fn start_transaction(
    client: &mut tokio_postgres::Client,
    read_only: bool,
) -> Result<tokio_postgres::Transaction<'_>, ExecError> {
    client
        .build_transaction()
        .read_only(read_only)
        .start()
        .await
        .map_err(map_pg_error)
}

async fn install_cancel_context(
    slot: &Option<CancelSlot>,
    token: tokio_postgres::CancelToken,
    backend_pid: i32,
    session_cfg: &SessionConfig,
) {
    if let Some(slot) = slot {
        slot.set_context(token, backend_pid, session_cfg).await;
    }
}

struct PreparedStatement {
    stmt: tokio_postgres::Statement,
    columns: Vec<ColumnInfo>,
    query_params: Vec<QueryParam>,
}

async fn prepare_bound_statement(
    tx: &mut tokio_postgres::Transaction<'_>,
    sql: &str,
    params: &[Value],
) -> Result<PreparedStatement, ExecError> {
    let stmt = tx.prepare(sql).await.map_err(map_pg_error)?;
    let columns = statement_columns(&stmt);
    validate_unique_column_names(&columns)?;
    validate_param_count(stmt.params().len(), params.len())?;
    let query_params = build_params(params, stmt.params())?;
    Ok(PreparedStatement {
        stmt,
        columns,
        query_params,
    })
}

fn statement_columns(stmt: &tokio_postgres::Statement) -> Vec<ColumnInfo> {
    stmt.columns()
        .iter()
        .map(|col| ColumnInfo {
            name: col.name().to_string(),
            type_name: col.type_().name().to_string(),
        })
        .collect()
}

fn validate_unique_column_names(columns: &[ColumnInfo]) -> Result<(), ExecError> {
    let mut seen = HashSet::new();
    let mut duplicate_seen = HashSet::new();
    let mut duplicates = Vec::new();

    for column in columns {
        let name = column.name.as_str();
        if !seen.insert(name) && duplicate_seen.insert(name) {
            duplicates.push(column.name.clone());
        }
    }

    if duplicates.is_empty() {
        return Ok(());
    }

    Err(ExecError::InvalidParams(format!(
        "query result has duplicate column name(s): {}. JSON object rows cannot safely represent duplicate keys; use AS aliases such as `a.id AS a_id` and `b.id AS b_id` to make output column names unique",
        format_column_names(&duplicates)
    )))
}

fn format_column_names(names: &[String]) -> String {
    names
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ContainerConfig, SshConfig};
    use tokio::sync::mpsc;

    fn column(name: &str) -> ColumnInfo {
        ColumnInfo {
            name: name.to_string(),
            type_name: "int4".to_string(),
        }
    }

    fn test_request<'a>(
        cfg: &'a SessionConfig,
        opts: &'a ResolvedOptions,
        log: Vec<String>,
        writer: mpsc::Sender<Output>,
    ) -> ExecRequest<'a> {
        ExecRequest {
            session_name: "default",
            session_cfg: cfg,
            sql: "select 1",
            params: &[],
            opts,
            cancel_slot: None,
            transport_log: Some(TransportLogContext {
                session: "default".to_string(),
                log,
                writer,
            }),
        }
    }

    fn default_opts() -> ResolvedOptions {
        ResolvedOptions {
            stream_rows: false,
            batch_rows: 1024,
            batch_bytes: 1 << 20,
            statement_timeout_ms: 0,
            lock_timeout_ms: 0,
            read_only: true,
            inline_max_rows: 100,
            inline_max_bytes: 1 << 20,
        }
    }

    #[test]
    fn validate_unique_column_names_accepts_aliases() {
        let columns = vec![column("a_id"), column("b_id")];
        assert!(validate_unique_column_names(&columns).is_ok());
    }

    #[test]
    fn validate_unique_column_names_rejects_duplicates_once() {
        let columns = vec![
            column("id"),
            column("name"),
            column("id"),
            column("name"),
            column("id"),
        ];

        assert!(matches!(
            validate_unique_column_names(&columns),
            Err(ExecError::InvalidParams(message))
                if message.contains("`id`, `name`")
                    && message.contains("JSON object rows")
                    && message.contains("AS aliases")
        ));
    }

    #[tokio::test]
    async fn emit_transport_selected_skips_when_log_filter_empty() {
        let cfg = SessionConfig {
            host: Some("127.0.0.1".to_string()),
            port: Some(5432),
            ..Default::default()
        };
        let opts = default_opts();
        let (tx, mut rx) = mpsc::channel::<Output>(4);
        let req = test_request(&cfg, &opts, vec![], tx);
        let selected = super::super::session::TransportSelection { duration_ms: 7 };
        assert!(emit_transport_selected(&req, Some(selected)).await.is_ok());
        assert!(
            rx.try_recv().is_err(),
            "log filter empty must suppress emission"
        );
    }

    #[tokio::test]
    async fn emit_transport_selected_skips_when_selection_none() {
        let cfg = SessionConfig::default();
        let opts = default_opts();
        let (tx, mut rx) = mpsc::channel::<Output>(4);
        let req = test_request(&cfg, &opts, vec!["transport".to_string()], tx);
        assert!(emit_transport_selected(&req, None).await.is_ok());
        assert!(
            rx.try_recv().is_err(),
            "no selection must suppress emission"
        );
    }

    async fn assert_transport_event(cfg: SessionConfig, chain_substring: &str, duration_ms: u64) {
        let opts = default_opts();
        let (tx, mut rx) = mpsc::channel::<Output>(4);
        let req = test_request(&cfg, &opts, vec!["transport".to_string()], tx);
        let selected = super::super::session::TransportSelection { duration_ms };
        assert!(emit_transport_selected(&req, Some(selected)).await.is_ok());
        let received = rx.try_recv().ok();
        assert!(
            matches!(received, Some(Output::Log { .. })),
            "expected Output::Log, got {received:?}"
        );
        let Some(Output::Log {
            event,
            session,
            chain,
            trace,
            ..
        }) = received
        else {
            return;
        };
        assert_eq!(event, "transport.selected");
        assert_eq!(session.as_deref(), Some("default"));
        let chain = chain.unwrap_or_default();
        assert!(
            chain.contains(chain_substring),
            "chain {chain:?} missing {chain_substring:?}"
        );
        assert_eq!(trace.duration_ms, duration_ms);
        assert!(trace.row_count.is_none());
        assert!(trace.payload_bytes.is_none());
    }

    #[tokio::test]
    async fn emit_transport_selected_direct_chain_includes_postgres_endpoint() {
        let cfg = SessionConfig {
            host: Some("127.0.0.1".to_string()),
            port: Some(5432),
            ..Default::default()
        };
        assert_transport_event(cfg, "127.0.0.1:5432", 11).await;
    }

    #[tokio::test]
    async fn emit_transport_selected_ssh_chain_includes_ssh_segment() {
        let cfg = SessionConfig {
            ssh: SshConfig {
                destination: Some("root@example.com".to_string()),
                ..Default::default()
            },
            host: Some("127.0.0.1".to_string()),
            port: Some(5432),
            ..Default::default()
        };
        assert_transport_event(cfg, "ssh:root@example.com ->", 22).await;
    }

    #[tokio::test]
    async fn emit_transport_selected_container_chain_includes_exec_segment() {
        let cfg = SessionConfig {
            container: ContainerConfig {
                target: Some("app-pod".to_string()),
                driver: Some("kubectl".to_string()),
                pod_container: Some("postgres".to_string()),
                ..Default::default()
            },
            host: Some("127.0.0.1".to_string()),
            port: Some(5432),
            ..Default::default()
        };
        assert_transport_event(cfg, "kubectl exec app-pod -c postgres", 33).await;
    }
}

const INLINE_PORTAL_BATCH_ROWS: usize = 1024;

struct InlineRowCollector {
    columns: Vec<ColumnInfo>,
    rows: Vec<Value>,
    row_count: usize,
    payload_bytes: usize,
    max_rows: usize,
    max_bytes: usize,
    truncated_at_rows: Option<usize>,
    truncated_at_bytes: Option<usize>,
}

#[derive(Clone, Copy)]
struct InlineRowCollectorMark {
    rows_len: usize,
    row_count: usize,
    payload_bytes: usize,
    truncated_at_rows: Option<usize>,
    truncated_at_bytes: Option<usize>,
}

impl InlineRowCollector {
    fn new(columns: Vec<ColumnInfo>, max_rows: usize, max_bytes: usize) -> Self {
        Self {
            columns,
            rows: vec![],
            row_count: 0,
            payload_bytes: 0,
            max_rows,
            max_bytes,
            truncated_at_rows: None,
            truncated_at_bytes: None,
        }
    }

    fn is_truncated(&self) -> bool {
        self.truncated_at_rows.is_some() || self.truncated_at_bytes.is_some()
    }

    /// Try to append a row. Returns `Ok(true)` if the row was accepted;
    /// `Ok(false)` if the inline limit fired and the collector now refuses
    /// further rows. Never errors — callers should treat `Ok(false)` as a
    /// signal to stop fetching from the portal.
    fn push(&mut self, row: Value, row_bytes: usize) -> Result<bool, ExecError> {
        if self.is_truncated() {
            return Ok(false);
        }
        let next_row_count = self.row_count.saturating_add(1);
        let next_payload_bytes = self.payload_bytes.saturating_add(row_bytes);
        if next_row_count > self.max_rows {
            self.truncated_at_rows = Some(self.max_rows);
            return Ok(false);
        }
        if next_payload_bytes > self.max_bytes {
            self.truncated_at_bytes = Some(self.max_bytes);
            return Ok(false);
        }

        self.row_count = next_row_count;
        self.payload_bytes = next_payload_bytes;
        self.rows.push(row);
        Ok(true)
    }

    fn mark(&self) -> InlineRowCollectorMark {
        InlineRowCollectorMark {
            rows_len: self.rows.len(),
            row_count: self.row_count,
            payload_bytes: self.payload_bytes,
            truncated_at_rows: self.truncated_at_rows,
            truncated_at_bytes: self.truncated_at_bytes,
        }
    }

    fn reset(&mut self, mark: InlineRowCollectorMark) {
        self.rows.truncate(mark.rows_len);
        self.row_count = mark.row_count;
        self.payload_bytes = mark.payload_bytes;
        self.truncated_at_rows = mark.truncated_at_rows;
        self.truncated_at_bytes = mark.truncated_at_bytes;
    }
}

async fn collect_rows_wrapped_or_direct(
    tx: &mut tokio_postgres::Transaction<'_>,
    sql: &str,
    params: &[Value],
    stmt: &tokio_postgres::Statement,
    bind_refs: Vec<&(dyn ToSql + Sync)>,
    collector: &mut InlineRowCollector,
    batch_rows: usize,
) -> Result<(), ExecError> {
    let wrapped = wrapped_rows_sql(sql);
    tx.execute("savepoint afpsql_wrap", &[])
        .await
        .map_err(map_pg_error)?;

    let batch_rows = batch_rows.clamp(1, INLINE_PORTAL_BATCH_ROWS);
    let wrapped_mark = collector.mark();
    let wrapped_attempt = collect_wrapped_rows(tx, &wrapped, params, collector, batch_rows).await;
    match wrapped_attempt {
        Ok(()) => {
            release_wrap_savepoint(tx).await?;
            Ok(())
        }
        Err(ExecError::InvalidParams(message)) => {
            collector.reset(wrapped_mark);
            rollback_wrap_savepoint(tx).await?;
            Err(ExecError::InvalidParams(message))
        }
        Err(ExecError::ResultTooLarge {
            row_count,
            payload_bytes,
        }) => {
            collector.reset(wrapped_mark);
            rollback_wrap_savepoint(tx).await?;
            Err(ExecError::ResultTooLarge {
                row_count,
                payload_bytes,
            })
        }
        Err(_) => {
            collector.reset(wrapped_mark);
            rollback_wrap_savepoint(tx).await?;
            let portal = tx.bind(stmt, &bind_refs).await.map_err(map_pg_error)?;
            collect_portal_rows(tx, &portal, collector, false, batch_rows).await
        }
    }
}

async fn bind_wrapped_rows(
    tx: &mut tokio_postgres::Transaction<'_>,
    wrapped_sql: &str,
    params: &[Value],
) -> Result<tokio_postgres::Portal, ExecError> {
    let wrapped_stmt = tx.prepare(wrapped_sql).await.map_err(map_pg_error)?;
    validate_param_count(wrapped_stmt.params().len(), params.len())?;
    let wrapped_params = build_params(params, wrapped_stmt.params())?;
    let wrapped_refs = build_param_refs(&wrapped_params);
    tx.bind(&wrapped_stmt, &wrapped_refs)
        .await
        .map_err(map_pg_error)
}

async fn collect_wrapped_rows(
    tx: &mut tokio_postgres::Transaction<'_>,
    wrapped_sql: &str,
    params: &[Value],
    collector: &mut InlineRowCollector,
    batch_rows: usize,
) -> Result<(), ExecError> {
    let portal = bind_wrapped_rows(tx, wrapped_sql, params).await?;
    collect_portal_rows(tx, &portal, collector, true, batch_rows).await
}

async fn collect_portal_rows(
    tx: &mut tokio_postgres::Transaction<'_>,
    portal: &tokio_postgres::Portal,
    collector: &mut InlineRowCollector,
    wrapped_json: bool,
    batch_rows: usize,
) -> Result<(), ExecError> {
    loop {
        let fetch_rows = inline_fetch_rows(collector, batch_rows);
        if drain_portal_batch(tx, portal, collector, wrapped_json, fetch_rows).await? {
            return Ok(());
        }
    }
}

fn inline_fetch_rows(collector: &InlineRowCollector, batch_rows: usize) -> i32 {
    let remaining = collector.max_rows.saturating_sub(collector.row_count);
    let fetch_rows = remaining.saturating_add(1).min(batch_rows).max(1);
    fetch_rows.min(i32::MAX as usize) as i32
}

async fn drain_portal_batch(
    tx: &mut tokio_postgres::Transaction<'_>,
    portal: &tokio_postgres::Portal,
    collector: &mut InlineRowCollector,
    wrapped_json: bool,
    fetch_rows: i32,
) -> Result<bool, ExecError> {
    let stream = tx
        .query_portal_raw(portal, fetch_rows)
        .await
        .map_err(map_pg_error)?;
    let mut rows = pin!(stream);

    while let Some(row) = rows.try_next().await.map_err(map_pg_error)? {
        let value = row_to_json_value(&row, wrapped_json);
        let row_bytes = row_json_size(&value);
        // collector.push returns Ok(false) once the inline cap is hit; we
        // keep draining the current portal batch so PG's protocol stays in
        // a clean state, but stop accepting new rows.
        let _ = collector.push(value, row_bytes)?;
    }

    let portal_exhausted = rows.rows_affected().is_some();
    Ok(portal_exhausted || collector.is_truncated())
}

struct StreamStats {
    row_count: usize,
    payload_bytes: usize,
}

async fn stream_rows_wrapped_or_direct(
    tx: &mut tokio_postgres::Transaction<'_>,
    sql: &str,
    params: &[Value],
    stmt: &tokio_postgres::Statement,
    bind_refs: Vec<&(dyn ToSql + Sync)>,
    columns: Vec<ColumnInfo>,
    sink: &mut (dyn RowSink + Send),
) -> Result<StreamStats, ExecError> {
    let wrapped = wrapped_rows_sql(sql);
    tx.execute("savepoint afpsql_wrap", &[])
        .await
        .map_err(map_pg_error)?;

    let wrapped_setup = stream_wrapped_rows(tx, &wrapped, params).await;
    match wrapped_setup {
        Ok(stream) => {
            sink.start(columns).await?;
            let stats = drain_row_stream(stream, sink, true).await?;
            release_wrap_savepoint(tx).await?;
            Ok(stats)
        }
        Err(ExecError::InvalidParams(message)) => {
            rollback_wrap_savepoint(tx).await?;
            Err(ExecError::InvalidParams(message))
        }
        Err(_) => {
            rollback_wrap_savepoint(tx).await?;
            let stream = tx.query_raw(stmt, bind_refs).await.map_err(map_pg_error)?;
            sink.start(columns).await?;
            drain_row_stream(stream, sink, false).await
        }
    }
}

async fn stream_wrapped_rows(
    tx: &mut tokio_postgres::Transaction<'_>,
    wrapped_sql: &str,
    params: &[Value],
) -> Result<tokio_postgres::RowStream, ExecError> {
    let wrapped_stmt = tx.prepare(wrapped_sql).await.map_err(map_pg_error)?;
    validate_param_count(wrapped_stmt.params().len(), params.len())?;
    let wrapped_params = build_params(params, wrapped_stmt.params())?;
    let wrapped_refs = build_param_refs(&wrapped_params);
    tx.query_raw(&wrapped_stmt, wrapped_refs)
        .await
        .map_err(map_pg_error)
}

async fn drain_row_stream(
    stream: tokio_postgres::RowStream,
    sink: &mut (dyn RowSink + Send),
    wrapped_json: bool,
) -> Result<StreamStats, ExecError> {
    let mut rows = pin!(stream);
    let mut row_count = 0usize;
    let mut payload_bytes = 0usize;
    while let Some(row) = rows.try_next().await.map_err(map_pg_error)? {
        let value = row_to_json_value(&row, wrapped_json);
        let row_bytes = row_json_size(&value);
        payload_bytes += row_bytes;
        row_count += 1;
        sink.row(value, row_bytes).await?;
    }
    Ok(StreamStats {
        row_count,
        payload_bytes,
    })
}

fn row_to_json_value(row: &tokio_postgres::Row, wrapped_json: bool) -> Value {
    if wrapped_json {
        row.try_get::<_, Value>("row_json")
            .unwrap_or_else(|_| row_to_json_fallback(row))
    } else {
        row_to_json_fallback(row)
    }
}

fn wrapped_rows_sql(sql: &str) -> String {
    // Preserve PostgreSQL's own type serialization for SELECT and RETURNING-style rows.
    format!("with __afpsql_rows as ({sql}) select to_jsonb(__afpsql_rows) as row_json from __afpsql_rows")
}

async fn rollback_wrap_savepoint(
    tx: &mut tokio_postgres::Transaction<'_>,
) -> Result<(), ExecError> {
    tx.execute("rollback to savepoint afpsql_wrap", &[])
        .await
        .map_err(map_pg_error)?;
    release_wrap_savepoint(tx).await
}

async fn release_wrap_savepoint(tx: &mut tokio_postgres::Transaction<'_>) -> Result<(), ExecError> {
    tx.execute("release savepoint afpsql_wrap", &[])
        .await
        .map_err(map_pg_error)?;
    Ok(())
}

async fn apply_query_settings(
    tx: &mut tokio_postgres::Transaction<'_>,
    opts: &ResolvedOptions,
) -> Result<(), ExecError> {
    let statement_timeout = format!("{}ms", opts.statement_timeout_ms);
    tx.execute(
        "select set_config('statement_timeout', $1, true)",
        &[&statement_timeout],
    )
    .await
    .map_err(map_pg_error)?;

    let lock_timeout = format!("{}ms", opts.lock_timeout_ms);
    tx.execute(
        "select set_config('lock_timeout', $1, true)",
        &[&lock_timeout],
    )
    .await
    .map_err(map_pg_error)?;

    Ok(())
}
