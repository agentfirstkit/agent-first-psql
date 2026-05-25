use super::*;
use crate::types::{ColumnInfo, QueryOptions, RuntimeConfig};

#[path = "env.rs"]
mod test_env;

#[test]
fn parse_helpers_error_paths() {
    assert!(matches!(
        parse_bool(&Value::String("x".to_string()), 1),
        Err(ExecError::InvalidParams(_))
    ));
    assert!(matches!(
        parse_i16(&serde_json::json!(99999), 1),
        Err(ExecError::InvalidParams(_))
    ));
    assert!(matches!(
        parse_i32(&serde_json::json!(i64::MAX), 1),
        Err(ExecError::InvalidParams(_))
    ));
    assert!(matches!(
        parse_i64(&serde_json::json!(u64::MAX), 1),
        Err(ExecError::InvalidParams(_))
    ));
    assert!(matches!(
        parse_f64(&Value::String("x".to_string()), 1),
        Err(ExecError::InvalidParams(_))
    ));
    assert_eq!(parse_text(&Value::Null), "");
}

#[test]
fn build_params_types() {
    let values = vec![
        Value::Null,
        Value::String("true".to_string()),
        Value::String("7".to_string()),
        Value::String("8".to_string()),
        Value::String("9".to_string()),
        Value::String("1.5".to_string()),
        Value::String("2.5".to_string()),
        serde_json::json!({"a":1}),
        Value::String("x".to_string()),
    ];
    let tys = vec![
        Type::TEXT,
        Type::BOOL,
        Type::INT2,
        Type::INT4,
        Type::INT8,
        Type::FLOAT4,
        Type::NUMERIC,
        Type::JSONB,
        Type::VARCHAR,
    ];
    let params_res = build_params(&values, &tys);
    assert!(params_res.is_ok());
    if let Ok(params) = params_res {
        let refs = build_param_refs(&params);
        assert_eq!(refs.len(), 9);
    }
}

#[test]
fn anynull_to_sql() {
    let n = AnyNull;
    let mut out = bytes::BytesMut::new();
    let is_null_res = n.to_sql(&Type::TEXT, &mut out);
    assert!(is_null_res.is_ok());
    if let Ok(is_null) = is_null_res {
        assert!(matches!(is_null, tokio_postgres::types::IsNull::Yes));
    }
}

#[tokio::test]
async fn postgres_executor_connect_error() {
    let exec = PostgresExecutor::new();
    let cfg = SessionConfig {
        dsn_secret: Some("postgresql://127.0.0.1:1/postgres".to_string()),
        ..Default::default()
    };
    let out = exec
        .execute(ExecRequest {
            session_name: "default",
            session_cfg: &cfg,
            sql: "select 1",
            params: &[],
            opts: &test_options(),
            cancel_slot: None,
        })
        .await;
    assert!(matches!(out, Err(ExecError::Connect(_))));
}

#[tokio::test]
async fn postgres_executor_rejects_unsupported_tls_config() {
    let exec = PostgresExecutor::new();
    let cfg = SessionConfig {
        dsn_secret: Some("postgresql://localhost/postgres?sslmode=verify-full".to_string()),
        ..Default::default()
    };
    let out = exec
        .execute(ExecRequest {
            session_name: "default",
            session_cfg: &cfg,
            sql: "select 1",
            params: &[],
            opts: &test_options(),
            cancel_slot: None,
        })
        .await;

    assert!(matches!(
        out,
        Err(ExecError::Config {
            message,
            hint: Some(_),
        }) if message.contains("verify-full")
    ));
}

fn test_dsn() -> Option<String> {
    test_env::test_dsn()
}

fn test_session_config(dsn: String) -> SessionConfig {
    SessionConfig {
        dsn_secret: Some(dsn),
        ..Default::default()
    }
}

fn test_options() -> crate::types::ResolvedOptions {
    RuntimeConfig::default()
        .resolve_options_for_session(&QueryOptions::default(), &SessionConfig::default())
        .unwrap_or_else(|_| RuntimeConfig::default().resolve_options(&QueryOptions::default()))
}

fn write_test_options() -> crate::types::ResolvedOptions {
    RuntimeConfig::default()
        .resolve_options_for_session(
            &QueryOptions {
                permission: Some(crate::types::Permission::Write),
                ..Default::default()
            },
            &SessionConfig::default(),
        )
        .unwrap_or_else(|_| RuntimeConfig::default().resolve_options(&QueryOptions::default()))
}

async fn query_rows(
    exec: &PostgresExecutor,
    cfg: &SessionConfig,
    session_name: &str,
    sql: &str,
) -> Result<Vec<Value>, ExecError> {
    let opts = test_options();
    match exec
        .execute(ExecRequest {
            session_name,
            session_cfg: cfg,
            sql,
            params: &[],
            opts: &opts,
            cancel_slot: None,
        })
        .await?
    {
        ExecOutcome::Rows { rows, .. } => Ok(rows),
        ExecOutcome::Command { .. } => Err(ExecError::Internal(
            "expected rows, got command outcome".to_string(),
        )),
    }
}

async fn execute_command(
    exec: &PostgresExecutor,
    cfg: &SessionConfig,
    session_name: &str,
    sql: &str,
) -> Result<(), ExecError> {
    let opts = write_test_options();
    match exec
        .execute(ExecRequest {
            session_name,
            session_cfg: cfg,
            sql,
            params: &[],
            opts: &opts,
            cancel_slot: None,
        })
        .await?
    {
        ExecOutcome::Command { .. } => Ok(()),
        ExecOutcome::Rows { .. } => Err(ExecError::Internal(
            "expected command, got rows outcome".to_string(),
        )),
    }
}

async fn query_i64(
    exec: &PostgresExecutor,
    cfg: &SessionConfig,
    session_name: &str,
    sql: &str,
    field: &str,
) -> Result<i64, ExecError> {
    let rows = query_rows(exec, cfg, session_name, sql).await?;
    rows.first()
        .and_then(|row| row.get(field))
        .and_then(Value::as_i64)
        .ok_or_else(|| ExecError::Internal(format!("missing integer field: {field}")))
}

async fn query_value(
    exec: &PostgresExecutor,
    cfg: &SessionConfig,
    session_name: &str,
    sql: &str,
    field: &str,
) -> Result<Value, ExecError> {
    let rows = query_rows(exec, cfg, session_name, sql).await?;
    rows.first()
        .and_then(|row| row.get(field))
        .cloned()
        .ok_or_else(|| ExecError::Internal(format!("missing field: {field}")))
}

#[tokio::test]
async fn postgres_executor_success_and_sql_error() {
    let Some(dsn) = test_dsn() else {
        return;
    };
    let exec = PostgresExecutor::new();
    let cfg = test_session_config(dsn);
    let opts = test_options();

    let out_res = exec
        .execute(ExecRequest {
            session_name: "default",
            session_cfg: &cfg,
            sql: "select 1 as n",
            params: &[],
            opts: &opts,
            cancel_slot: None,
        })
        .await;
    assert!(out_res.is_ok());
    if let Ok(out) = out_res {
        assert!(matches!(out, ExecOutcome::Rows { .. }));
    }

    let err = exec
        .execute(ExecRequest {
            session_name: "default",
            session_cfg: &cfg,
            sql: "select $1::int",
            params: &[Value::String("x".to_string())],
            opts: &opts,
            cancel_slot: None,
        })
        .await;
    assert!(matches!(err, Err(ExecError::InvalidParams(_))));

    let err = exec
        .execute(ExecRequest {
            session_name: "default",
            session_cfg: &cfg,
            sql: "select * from non_existing_table_afpsql_cov",
            params: &[],
            opts: &opts,
            cancel_slot: None,
        })
        .await;
    assert!(matches!(err, Err(ExecError::Sql { .. })));
}

#[tokio::test]
async fn postgres_executor_named_session_reuses_backend_and_session_state() {
    let Some(dsn) = test_dsn() else {
        return;
    };
    let exec = PostgresExecutor::new();
    let cfg = test_session_config(dsn);

    let pid_before = query_i64(
        &exec,
        &cfg,
        "default",
        "select pg_backend_pid()::bigint as pid",
        "pid",
    )
    .await;
    assert!(
        pid_before.is_ok(),
        "initial backend pid query failed: {pid_before:?}"
    );
    let pid_before = pid_before.unwrap_or_default();

    let created = execute_command(
        &exec,
        &cfg,
        "default",
        "create temp table afpsql_session_state_probe as select 42::int as marker",
    )
    .await;
    assert!(created.is_ok(), "create temp table failed: {created:?}");

    let marker = query_i64(
        &exec,
        &cfg,
        "default",
        "select marker::bigint as marker from afpsql_session_state_probe",
        "marker",
    )
    .await;
    assert!(matches!(marker, Ok(42)));

    let pid_after = query_i64(
        &exec,
        &cfg,
        "default",
        "select pg_backend_pid()::bigint as pid",
        "pid",
    )
    .await;
    assert!(matches!(pid_after, Ok(pid) if pid == pid_before));
}

#[tokio::test]
async fn postgres_executor_named_sessions_have_isolated_backends_and_state() {
    let Some(dsn) = test_dsn() else {
        return;
    };
    let exec = PostgresExecutor::new();
    let cfg = test_session_config(dsn);

    let alpha_pid = query_i64(
        &exec,
        &cfg,
        "alpha",
        "select pg_backend_pid()::bigint as pid",
        "pid",
    )
    .await;
    assert!(
        alpha_pid.is_ok(),
        "alpha backend pid query failed: {alpha_pid:?}"
    );
    let alpha_pid = alpha_pid.unwrap_or_default();

    let created = execute_command(
        &exec,
        &cfg,
        "alpha",
        "create temp table afpsql_session_isolation_probe as select 7::int as marker",
    )
    .await;
    assert!(
        created.is_ok(),
        "create alpha temp table failed: {created:?}"
    );

    let beta_pid = query_i64(
        &exec,
        &cfg,
        "beta",
        "select pg_backend_pid()::bigint as pid",
        "pid",
    )
    .await;
    assert!(
        beta_pid.is_ok(),
        "beta backend pid query failed: {beta_pid:?}"
    );
    let beta_pid = beta_pid.unwrap_or_default();
    assert_ne!(alpha_pid, beta_pid);

    let alpha_marker = query_i64(
        &exec,
        &cfg,
        "alpha",
        "select marker::bigint as marker from afpsql_session_isolation_probe",
        "marker",
    )
    .await;
    assert!(matches!(alpha_marker, Ok(7)));

    let beta_temp_table = query_value(
        &exec,
        &cfg,
        "beta",
        "select to_regclass('pg_temp.afpsql_session_isolation_probe')::text as table_name",
        "table_name",
    )
    .await;
    assert!(matches!(beta_temp_table, Ok(Value::Null)));
}

#[tokio::test]
async fn postgres_executor_invalidate_starts_new_backend_session() {
    let Some(dsn) = test_dsn() else {
        return;
    };
    let exec = PostgresExecutor::new();
    let cfg = test_session_config(dsn);

    let pid_before = query_i64(
        &exec,
        &cfg,
        "default",
        "select pg_backend_pid()::bigint as pid",
        "pid",
    )
    .await;
    assert!(
        pid_before.is_ok(),
        "initial backend pid query failed: {pid_before:?}"
    );
    let pid_before = pid_before.unwrap_or_default();

    let created = execute_command(
        &exec,
        &cfg,
        "default",
        "create temp table afpsql_session_invalidate_probe as select 9::int as marker",
    )
    .await;
    assert!(
        created.is_ok(),
        "create temp table before invalidate failed: {created:?}"
    );

    exec.invalidate_sessions(&["default".to_string()]).await;

    let pid_after = query_i64(
        &exec,
        &cfg,
        "default",
        "select pg_backend_pid()::bigint as pid",
        "pid",
    )
    .await;
    assert!(
        pid_after.is_ok(),
        "backend pid query after invalidate failed: {pid_after:?}"
    );
    let pid_after = pid_after.unwrap_or_default();
    assert_ne!(pid_before, pid_after);

    let temp_table = query_value(
        &exec,
        &cfg,
        "default",
        "select to_regclass('pg_temp.afpsql_session_invalidate_probe')::text as table_name",
        "table_name",
    )
    .await;
    assert!(matches!(temp_table, Ok(Value::Null)));
}

#[tokio::test]
async fn postgres_executor_rejects_duplicate_result_columns() {
    let Some(dsn) = test_dsn() else {
        return;
    };
    let exec = PostgresExecutor::new();
    let cfg = test_session_config(dsn);
    let opts = test_options();

    let out = exec
        .execute(ExecRequest {
            session_name: "default",
            session_cfg: &cfg,
            sql: "select 1 as id, 2 as id",
            params: &[],
            opts: &opts,
            cancel_slot: None,
        })
        .await;
    assert_duplicate_column_error(out, "`id`");

    let out = exec
        .execute(ExecRequest {
            session_name: "default",
            session_cfg: &cfg,
            sql: "select * from (values (1)) as a(id) join (values (2)) as b(id) on true",
            params: &[],
            opts: &opts,
            cancel_slot: None,
        })
        .await;
    assert_duplicate_column_error(out, "`id`");

    let out = exec
        .execute(ExecRequest {
            session_name: "default",
            session_cfg: &cfg,
            sql: "select 1 as a_id, 2 as b_id",
            params: &[],
            opts: &opts,
            cancel_slot: None,
        })
        .await;
    assert!(matches!(out, Ok(ExecOutcome::Rows { .. })));
}

#[derive(Default)]
struct CountingSink {
    started: bool,
    row_count: usize,
    payload_bytes: usize,
    last_n: Option<i64>,
}

#[async_trait::async_trait]
impl RowSink for CountingSink {
    async fn start(&mut self, columns: Vec<ColumnInfo>) -> Result<(), ExecError> {
        assert_eq!(columns.len(), 1);
        assert_eq!(columns[0].name, "n");
        self.started = true;
        Ok(())
    }

    async fn row(&mut self, row: Value, row_bytes: usize) -> Result<(), ExecError> {
        assert!(self.started);
        self.row_count += 1;
        self.payload_bytes += row_bytes;
        self.last_n = row.get("n").and_then(Value::as_i64);
        Ok(())
    }
}

#[tokio::test]
async fn postgres_executor_streams_large_wrapped_result() {
    let Some(dsn) = test_dsn() else {
        return;
    };
    let exec = PostgresExecutor::new();
    let cfg = test_session_config(dsn);
    let query_opts = QueryOptions {
        stream_rows: true,
        ..Default::default()
    };
    let opts = RuntimeConfig::default()
        .resolve_options_for_session(&query_opts, &cfg)
        .unwrap_or_else(|_| RuntimeConfig::default().resolve_options(&QueryOptions::default()));
    let mut sink = CountingSink::default();

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        exec.execute_streaming(
            ExecRequest {
                session_name: "default",
                session_cfg: &cfg,
                sql: "select x::int as n from generate_series(1, 20000) as x",
                params: &[],
                opts: &opts,
                cancel_slot: None,
            },
            &mut sink,
        ),
    )
    .await;

    assert!(
        result.is_ok(),
        "streaming query timed out before draining large result"
    );
    let out = match result {
        Ok(out) => out,
        Err(_) => return,
    };
    assert!(matches!(
        out,
        Ok(StreamOutcome::Rows {
            row_count: 20_000,
            payload_bytes,
        }) if payload_bytes == sink.payload_bytes
    ));
    assert_eq!(sink.row_count, 20_000);
    assert_eq!(sink.last_n, Some(20_000));
}

fn assert_duplicate_column_error(out: Result<ExecOutcome, ExecError>, column: &str) {
    assert!(matches!(
        out,
        Err(ExecError::InvalidParams(message))
            if message.contains("duplicate column name")
                && message.contains(column)
                && message.contains("AS aliases")
    ));
}
