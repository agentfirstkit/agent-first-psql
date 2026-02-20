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
    let params = build_params(&values, &tys).expect("build params");
    let refs = build_param_refs(&params);
    assert_eq!(refs.len(), 9);
}

#[test]
fn anynull_to_sql() {
    let n = AnyNull;
    let mut out = bytes::BytesMut::new();
    let is_null = n.to_sql(&Type::TEXT, &mut out).expect("to_sql");
    assert!(matches!(is_null, tokio_postgres::types::IsNull::Yes));
}

#[tokio::test]
async fn postgres_executor_connect_error() {
    let exec = PostgresExecutor::new();
    let cfg = SessionConfig {
        dsn_secret: Some("postgresql://127.0.0.1:1/postgres".to_string()),
        ..Default::default()
    };
    let out = exec
        .execute(
            "default",
            &cfg,
            "select 1",
            &[],
            &RuntimeConfig::default().resolve_options(&QueryOptions::default()),
        )
        .await;
    assert!(matches!(out, Err(ExecError::Connect(_))));
}

fn test_dsn() -> String {
    std::env::var("AFPSQL_TEST_DSN_SECRET")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .unwrap_or_else(|_| "postgresql://localhost/postgres".to_string())
}

#[tokio::test]
async fn postgres_executor_success_and_sql_error() {
    let exec = PostgresExecutor::new();
    let cfg = SessionConfig {
        dsn_secret: Some(test_dsn()),
        ..Default::default()
    };
    let opts = RuntimeConfig::default().resolve_options(&QueryOptions::default());

    let out = exec
        .execute("default", &cfg, "select 1 as n", &[], &opts)
        .await
        .expect("ok");
    assert!(matches!(out, ExecOutcome::Rows(_)));

    let err = exec
        .execute(
            "default",
            &cfg,
            "select $1::int",
            &[Value::String("x".to_string())],
            &opts,
        )
        .await;
    assert!(matches!(err, Err(ExecError::InvalidParams(_))));

    let err = exec
        .execute(
            "default",
            &cfg,
            "select * from non_existing_table_afpsql_cov",
            &[],
            &opts,
        )
        .await;
    assert!(matches!(err, Err(ExecError::Sql { .. })));
}
