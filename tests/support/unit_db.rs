use super::*;
use crate::types::{QueryOptions, RuntimeConfig};

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
            opts: &RuntimeConfig::default().resolve_options(&QueryOptions::default()),
            cancel_slot: None,
        })
        .await;
    assert!(matches!(out, Err(ExecError::Connect(_))));
}

fn test_dsn() -> Option<String> {
    std::env::var("AFPSQL_TEST_DSN_SECRET")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .ok()
}

#[tokio::test]
async fn postgres_executor_success_and_sql_error() {
    let Some(dsn) = test_dsn() else {
        return;
    };
    let exec = PostgresExecutor::new();
    let cfg = SessionConfig {
        dsn_secret: Some(dsn),
        ..Default::default()
    };
    let opts = RuntimeConfig::default().resolve_options(&QueryOptions::default());

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
