use super::errors::{map_pg_error, ExecError};
use super::params::{build_param_refs, build_params, validate_param_count, QueryParam};
use super::pool::{get_pool, new_pool_map, CancelSlot, PoolMap};
use super::rows::{row_json_size, row_to_json_fallback};
use crate::types::{ColumnInfo, ResolvedOptions, SessionConfig};
use async_trait::async_trait;
use futures_util::TryStreamExt;
use serde_json::Value;
use std::pin::pin;
use tokio_postgres::types::ToSql;

#[derive(Debug)]
pub enum ExecOutcome {
    Rows {
        columns: Vec<ColumnInfo>,
        rows: Vec<Value>,
    },
    Command {
        affected: usize,
    },
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
}

#[async_trait]
pub trait DbExecutor: Send + Sync {
    async fn execute(&self, req: ExecRequest<'_>) -> Result<ExecOutcome, ExecError>;

    async fn execute_streaming(
        &self,
        req: ExecRequest<'_>,
        sink: &mut (dyn RowSink + Send),
    ) -> Result<StreamOutcome, ExecError> {
        match self.execute(req).await? {
            ExecOutcome::Rows { columns, rows } => {
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
}

pub struct PostgresExecutor {
    pools: PoolMap,
}

impl PostgresExecutor {
    pub fn new() -> Self {
        Self {
            pools: new_pool_map(),
        }
    }
}

#[async_trait]
impl DbExecutor for PostgresExecutor {
    async fn execute(&self, req: ExecRequest<'_>) -> Result<ExecOutcome, ExecError> {
        let pool = get_pool(&self.pools, req.session_name, req.session_cfg).await?;
        let mut client = pool
            .get()
            .await
            .map_err(|e| ExecError::Connect(format!("get connection failed: {e}")))?;
        install_cancel_token(&req.cancel_slot, client.cancel_token()).await;

        let mut tx = client.transaction().await.map_err(map_pg_error)?;
        apply_query_settings(&mut tx, req.opts).await?;
        let prepared = prepare_bound_statement(&mut tx, req.sql, req.params).await?;
        let bind_refs = build_param_refs(&prepared.query_params);

        if !prepared.columns.is_empty() {
            let rows = query_rows_wrapped_or_direct(
                &mut tx,
                req.sql,
                req.params,
                &prepared.stmt,
                &bind_refs,
            )
            .await?;

            tx.commit().await.map_err(map_pg_error)?;

            let json_rows = rows
                .into_iter()
                .map(|row| {
                    if let Ok(value) = row.try_get::<_, Value>("row_json") {
                        return value;
                    }
                    row_to_json_fallback(&row)
                })
                .collect();

            return Ok(ExecOutcome::Rows {
                columns: prepared.columns,
                rows: json_rows,
            });
        }

        let affected = tx
            .execute(&prepared.stmt, &bind_refs)
            .await
            .map_err(map_pg_error)? as usize;
        tx.commit().await.map_err(map_pg_error)?;

        Ok(ExecOutcome::Command { affected })
    }

    async fn execute_streaming(
        &self,
        req: ExecRequest<'_>,
        sink: &mut (dyn RowSink + Send),
    ) -> Result<StreamOutcome, ExecError> {
        let pool = get_pool(&self.pools, req.session_name, req.session_cfg).await?;
        let mut client = pool
            .get()
            .await
            .map_err(|e| ExecError::Connect(format!("get connection failed: {e}")))?;
        install_cancel_token(&req.cancel_slot, client.cancel_token()).await;

        let mut tx = client.transaction().await.map_err(map_pg_error)?;
        apply_query_settings(&mut tx, req.opts).await?;
        let prepared = prepare_bound_statement(&mut tx, req.sql, req.params).await?;
        let bind_refs = build_param_refs(&prepared.query_params);

        if prepared.columns.is_empty() {
            let affected = tx
                .execute(&prepared.stmt, &bind_refs)
                .await
                .map_err(map_pg_error)? as usize;
            tx.commit().await.map_err(map_pg_error)?;
            return Ok(StreamOutcome::Command { affected });
        }

        let stats = stream_rows_wrapped_or_direct(
            &mut tx,
            req.sql,
            req.params,
            &prepared.stmt,
            bind_refs,
            prepared.columns,
            sink,
        )
        .await?;

        tx.commit().await.map_err(map_pg_error)?;
        Ok(StreamOutcome::Rows {
            row_count: stats.row_count,
            payload_bytes: stats.payload_bytes,
        })
    }

    async fn invalidate_sessions(&self, session_names: &[String]) {
        if session_names.is_empty() {
            return;
        }
        let mut pools = self.pools.write().await;
        for name in session_names {
            pools.remove(name);
        }
    }
}

async fn install_cancel_token(slot: &Option<CancelSlot>, token: tokio_postgres::CancelToken) {
    if let Some(slot) = slot {
        *slot.lock().await = Some(token);
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

async fn query_rows_wrapped_or_direct(
    tx: &mut tokio_postgres::Transaction<'_>,
    sql: &str,
    params: &[Value],
    stmt: &tokio_postgres::Statement,
    bind_refs: &[&(dyn ToSql + Sync)],
) -> Result<Vec<tokio_postgres::Row>, ExecError> {
    let wrapped = wrapped_rows_sql(sql);
    tx.execute("savepoint afpsql_wrap", &[])
        .await
        .map_err(map_pg_error)?;

    let wrapped_attempt = query_wrapped_rows(tx, &wrapped, params).await;
    match wrapped_attempt {
        Ok(rows) => {
            release_wrap_savepoint(tx).await?;
            Ok(rows)
        }
        Err(ExecError::InvalidParams(message)) => {
            rollback_wrap_savepoint(tx).await?;
            Err(ExecError::InvalidParams(message))
        }
        Err(_) => {
            rollback_wrap_savepoint(tx).await?;
            tx.query(stmt, bind_refs).await.map_err(map_pg_error)
        }
    }
}

async fn query_wrapped_rows(
    tx: &mut tokio_postgres::Transaction<'_>,
    wrapped_sql: &str,
    params: &[Value],
) -> Result<Vec<tokio_postgres::Row>, ExecError> {
    let wrapped_stmt = tx.prepare(wrapped_sql).await.map_err(map_pg_error)?;
    validate_param_count(wrapped_stmt.params().len(), params.len())?;
    let wrapped_params = build_params(params, wrapped_stmt.params())?;
    let wrapped_refs = build_param_refs(&wrapped_params);
    tx.query(&wrapped_stmt, &wrapped_refs)
        .await
        .map_err(map_pg_error)
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
            release_wrap_savepoint(tx).await?;
            sink.start(columns).await?;
            drain_row_stream(stream, sink, true).await
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
        let value = if wrapped_json {
            row.try_get::<_, Value>("row_json")
                .unwrap_or_else(|_| row_to_json_fallback(&row))
        } else {
            row_to_json_fallback(&row)
        };
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

    if opts.read_only {
        tx.execute("set local transaction read only", &[])
            .await
            .map_err(map_pg_error)?;
    }
    Ok(())
}
