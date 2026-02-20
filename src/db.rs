use crate::conn::resolve_conn_string;
use crate::types::{ResolvedOptions, SessionConfig};
use async_trait::async_trait;
use deadpool_postgres::{Manager, ManagerConfig, Pool, RecyclingMethod};
use serde_json::{json, Value};
use std::collections::HashMap;
use tokio::sync::RwLock;
use tokio_postgres::types::{Json, ToSql, Type};

#[derive(Debug)]
pub enum ExecOutcome {
    Rows(Vec<Value>),
    Command { affected: usize },
}

#[derive(Debug)]
pub enum ExecError {
    Connect(String),
    InvalidParams(String),
    Sql {
        sqlstate: String,
        message: String,
        detail: Option<String>,
        hint: Option<String>,
        position: Option<String>,
    },
    Internal(String),
}

#[async_trait]
pub trait DbExecutor: Send + Sync {
    async fn execute(
        &self,
        session_name: &str,
        session_cfg: &SessionConfig,
        sql: &str,
        params: &[Value],
        opts: &ResolvedOptions,
    ) -> Result<ExecOutcome, ExecError>;
}

pub struct PostgresExecutor {
    pools: RwLock<HashMap<String, Pool>>,
}

impl PostgresExecutor {
    pub fn new() -> Self {
        Self {
            pools: RwLock::new(HashMap::new()),
        }
    }

    async fn get_pool(&self, session_name: &str, cfg: &SessionConfig) -> Result<Pool, ExecError> {
        if let Some(pool) = self.pools.read().await.get(session_name) {
            return Ok(pool.clone());
        }

        let conn_str = resolve_conn_string(cfg).map_err(ExecError::Connect)?;
        let pg_cfg: tokio_postgres::Config = conn_str
            .parse()
            .map_err(|e| ExecError::Connect(format!("invalid postgres conn string: {e}")))?;
        let mgr = Manager::from_config(
            pg_cfg,
            tokio_postgres::NoTls,
            ManagerConfig {
                recycling_method: RecyclingMethod::Fast,
            },
        );
        let pool = Pool::builder(mgr)
            .max_size(5)
            .build()
            .map_err(|e| ExecError::Connect(format!("create pool failed: {e}")))?;

        self.pools
            .write()
            .await
            .insert(session_name.to_string(), pool.clone());

        Ok(pool)
    }
}

#[async_trait]
impl DbExecutor for PostgresExecutor {
    async fn execute(
        &self,
        session_name: &str,
        session_cfg: &SessionConfig,
        sql: &str,
        params: &[Value],
        opts: &ResolvedOptions,
    ) -> Result<ExecOutcome, ExecError> {
        let pool = self.get_pool(session_name, session_cfg).await?;
        let mut client = pool
            .get()
            .await
            .map_err(|e| ExecError::Connect(format!("get connection failed: {e}")))?;

        let mut tx = client.transaction().await.map_err(map_pg_error)?;
        apply_query_settings(&mut tx, opts).await?;
        let stmt = tx.prepare(sql).await.map_err(map_pg_error)?;
        validate_param_count(stmt.params().len(), params.len())?;
        let query_params = build_params(params, stmt.params())?;
        let bind_refs = build_param_refs(&query_params);

        if !stmt.columns().is_empty() {
            // Primary row path: CTE + to_jsonb to preserve PostgreSQL's own type
            // serialization. This supports SELECT and RETURNING-style statements.
            let wrapped = format!(
                "with __afpsql_rows as ({sql}) select to_jsonb(__afpsql_rows) as row_json from __afpsql_rows"
            );
            tx.execute("savepoint afpsql_wrap", &[])
                .await
                .map_err(map_pg_error)?;

            let wrapped_attempt: Result<Vec<tokio_postgres::Row>, ExecError> = async {
                let wrapped_stmt = tx.prepare(&wrapped).await.map_err(map_pg_error)?;
                validate_param_count(wrapped_stmt.params().len(), params.len())?;
                let wrapped_params = build_params(params, wrapped_stmt.params())?;
                let wrapped_refs = build_param_refs(&wrapped_params);
                tx.query(&wrapped_stmt, &wrapped_refs)
                    .await
                    .map_err(map_pg_error)
            }
            .await;

            let rows = match wrapped_attempt {
                Ok(rows) => {
                    tx.execute("release savepoint afpsql_wrap", &[])
                        .await
                        .map_err(map_pg_error)?;
                    rows
                }
                Err(ExecError::InvalidParams(message)) => {
                    tx.execute("rollback to savepoint afpsql_wrap", &[])
                        .await
                        .map_err(map_pg_error)?;
                    tx.execute("release savepoint afpsql_wrap", &[])
                        .await
                        .map_err(map_pg_error)?;
                    return Err(ExecError::InvalidParams(message));
                }
                Err(_) => {
                    // Some utility statements (e.g. SHOW) cannot be wrapped in CTE.
                    // Roll back wrapper failure and fall back to direct row decode.
                    tx.execute("rollback to savepoint afpsql_wrap", &[])
                        .await
                        .map_err(map_pg_error)?;
                    tx.execute("release savepoint afpsql_wrap", &[])
                        .await
                        .map_err(map_pg_error)?;
                    tx.query(&stmt, &bind_refs).await.map_err(map_pg_error)?
                }
            };

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

            return Ok(ExecOutcome::Rows(json_rows));
        }

        let affected = tx.execute(&stmt, &bind_refs).await.map_err(map_pg_error)? as usize;
        tx.commit().await.map_err(map_pg_error)?;

        Ok(ExecOutcome::Command { affected })
    }
}

fn map_pg_error(err: tokio_postgres::Error) -> ExecError {
    if let Some(db) = err.as_db_error() {
        return ExecError::Sql {
            sqlstate: db.code().code().to_string(),
            message: db.message().to_string(),
            detail: db.detail().map(std::string::ToString::to_string),
            hint: db.hint().map(std::string::ToString::to_string),
            position: db.position().map(|p| match p {
                tokio_postgres::error::ErrorPosition::Original(pos) => pos.to_string(),
                tokio_postgres::error::ErrorPosition::Internal { position, .. } => {
                    position.to_string()
                }
            }),
        };
    }
    ExecError::Internal(err.to_string())
}

enum QueryParam {
    Null(AnyNull),
    Bool(bool),
    Int16(i16),
    Int32(i32),
    Int64(i64),
    Float32(f32),
    Float(f64),
    Text(String),
    Json(Json<Value>),
}

#[derive(Debug)]
struct AnyNull;

impl ToSql for AnyNull {
    fn to_sql(
        &self,
        _ty: &Type,
        _out: &mut bytes::BytesMut,
    ) -> Result<tokio_postgres::types::IsNull, Box<dyn std::error::Error + Sync + Send>> {
        Ok(tokio_postgres::types::IsNull::Yes)
    }

    fn accepts(_ty: &Type) -> bool {
        true
    }

    tokio_postgres::types::to_sql_checked!();
}

fn build_params(values: &[Value], expected_types: &[Type]) -> Result<Vec<QueryParam>, ExecError> {
    let mut params = Vec::with_capacity(values.len());
    for (idx, v) in values.iter().enumerate() {
        let ty = expected_types.get(idx).unwrap_or(&Type::TEXT);
        let p = match v {
            Value::Null => QueryParam::Null(AnyNull),
            Value::Array(_) | Value::Object(_) if *ty == Type::JSON || *ty == Type::JSONB => {
                QueryParam::Json(Json(v.clone()))
            }
            _ if *ty == Type::BOOL => QueryParam::Bool(parse_bool(v, idx + 1)?),
            _ if *ty == Type::INT2 => QueryParam::Int16(parse_i16(v, idx + 1)?),
            _ if *ty == Type::INT4 => QueryParam::Int32(parse_i32(v, idx + 1)?),
            _ if *ty == Type::INT8 => QueryParam::Int64(parse_i64(v, idx + 1)?),
            _ if *ty == Type::FLOAT4 => QueryParam::Float32(parse_f32(v, idx + 1)?),
            _ if *ty == Type::FLOAT8 => QueryParam::Float(parse_f64(v, idx + 1)?),
            _ if *ty == Type::NUMERIC => QueryParam::Float(parse_f64(v, idx + 1)?),
            _ if *ty == Type::JSON || *ty == Type::JSONB => QueryParam::Json(Json(v.clone())),
            _ => QueryParam::Text(parse_text(v)),
        };
        params.push(p);
    }
    Ok(params)
}

fn build_param_refs(params: &[QueryParam]) -> Vec<&(dyn ToSql + Sync)> {
    params
        .iter()
        .map(|p| match p {
            QueryParam::Null(v) => v as &(dyn ToSql + Sync),
            QueryParam::Bool(v) => v as &(dyn ToSql + Sync),
            QueryParam::Int16(v) => v as &(dyn ToSql + Sync),
            QueryParam::Int32(v) => v as &(dyn ToSql + Sync),
            QueryParam::Int64(v) => v as &(dyn ToSql + Sync),
            QueryParam::Float32(v) => v as &(dyn ToSql + Sync),
            QueryParam::Float(v) => v as &(dyn ToSql + Sync),
            QueryParam::Text(v) => v as &(dyn ToSql + Sync),
            QueryParam::Json(v) => v as &(dyn ToSql + Sync),
        })
        .collect()
}

fn parse_bool(v: &Value, pos: usize) -> Result<bool, ExecError> {
    match v {
        Value::Bool(b) => Ok(*b),
        Value::String(s) => s
            .parse::<bool>()
            .map_err(|_| ExecError::InvalidParams(format!("param ${pos} cannot parse as bool"))),
        _ => Err(ExecError::InvalidParams(format!(
            "param ${pos} cannot parse as bool"
        ))),
    }
}

fn parse_i16(v: &Value, pos: usize) -> Result<i16, ExecError> {
    let n = parse_i64(v, pos)?;
    i16::try_from(n)
        .map_err(|_| ExecError::InvalidParams(format!("param ${pos} out of range for int2")))
}

fn parse_i32(v: &Value, pos: usize) -> Result<i32, ExecError> {
    let n = parse_i64(v, pos)?;
    i32::try_from(n)
        .map_err(|_| ExecError::InvalidParams(format!("param ${pos} out of range for int4")))
}

fn parse_i64(v: &Value, pos: usize) -> Result<i64, ExecError> {
    match v {
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i)
            } else if let Some(u) = n.as_u64() {
                i64::try_from(u).map_err(|_| {
                    ExecError::InvalidParams(format!("param ${pos} out of range for int8"))
                })
            } else {
                Err(ExecError::InvalidParams(format!(
                    "param ${pos} cannot parse as int8"
                )))
            }
        }
        Value::String(s) => s
            .parse::<i64>()
            .map_err(|_| ExecError::InvalidParams(format!("param ${pos} cannot parse as int8"))),
        _ => Err(ExecError::InvalidParams(format!(
            "param ${pos} cannot parse as int8"
        ))),
    }
}

fn parse_f32(v: &Value, pos: usize) -> Result<f32, ExecError> {
    let n = parse_f64(v, pos)?;
    Ok(n as f32)
}

fn parse_f64(v: &Value, pos: usize) -> Result<f64, ExecError> {
    match v {
        Value::Number(n) => n.as_f64().ok_or_else(|| {
            ExecError::InvalidParams(format!("param ${pos} cannot parse as float8"))
        }),
        Value::String(s) => s
            .parse::<f64>()
            .map_err(|_| ExecError::InvalidParams(format!("param ${pos} cannot parse as float8"))),
        _ => Err(ExecError::InvalidParams(format!(
            "param ${pos} cannot parse as float8"
        ))),
    }
}

fn parse_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn validate_param_count(expected: usize, actual: usize) -> Result<(), ExecError> {
    if expected == actual {
        return Ok(());
    }
    Err(ExecError::InvalidParams(format!(
        "placeholder count mismatch: sql requires {expected}, params provided {actual}"
    )))
}

fn row_to_json_fallback(row: &tokio_postgres::Row) -> Value {
    let mut map = serde_json::Map::new();
    for (idx, col) in row.columns().iter().enumerate() {
        let value = decode_row_value_fallback(row, idx, col.type_());
        map.insert(col.name().to_string(), value);
    }
    Value::Object(map)
}

fn decode_row_value_fallback(row: &tokio_postgres::Row, idx: usize, ty: &Type) -> Value {
    match *ty {
        Type::BOOL => row
            .try_get::<_, Option<bool>>(idx)
            .ok()
            .flatten()
            .map(Value::Bool)
            .unwrap_or(Value::Null),
        Type::INT2 => row
            .try_get::<_, Option<i16>>(idx)
            .ok()
            .flatten()
            .map(|v| json!(v))
            .unwrap_or(Value::Null),
        Type::INT4 => row
            .try_get::<_, Option<i32>>(idx)
            .ok()
            .flatten()
            .map(|v| json!(v))
            .unwrap_or(Value::Null),
        Type::INT8 => row
            .try_get::<_, Option<i64>>(idx)
            .ok()
            .flatten()
            .map(|v| json!(v))
            .unwrap_or(Value::Null),
        Type::FLOAT4 => row
            .try_get::<_, Option<f32>>(idx)
            .ok()
            .flatten()
            .and_then(|v| serde_json::Number::from_f64(v as f64).map(Value::Number))
            .unwrap_or(Value::Null),
        Type::FLOAT8 => row
            .try_get::<_, Option<f64>>(idx)
            .ok()
            .flatten()
            .and_then(|v| serde_json::Number::from_f64(v).map(Value::Number))
            .unwrap_or(Value::Null),
        Type::JSON | Type::JSONB => row
            .try_get::<_, Option<Json<Value>>>(idx)
            .ok()
            .flatten()
            .map(|v| v.0)
            .unwrap_or(Value::Null),
        _ => {
            if let Ok(Some(s)) = row.try_get::<_, Option<String>>(idx) {
                return Value::String(s);
            }
            if let Ok(Some(v)) = row.try_get::<_, Option<i64>>(idx) {
                return json!(v);
            }
            if let Ok(Some(v)) = row.try_get::<_, Option<f64>>(idx) {
                if let Some(n) = serde_json::Number::from_f64(v) {
                    return Value::Number(n);
                }
            }
            Value::String(format!("<unhandled_type:{}>", ty.name()))
        }
    }
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

#[cfg(test)]
#[path = "../tests/support/unit_db.rs"]
mod tests;
