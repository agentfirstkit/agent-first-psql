use super::*;

#[test]
fn has_session_override_false_for_empty() {
    assert!(!has_session_override(&SessionConfig::default()));
}

#[test]
fn has_session_override_true_for_host() {
    assert!(has_session_override(&SessionConfig {
        host: Some("localhost".to_string()),
        ..Default::default()
    }));
}

#[test]
fn build_startup_log_has_afdata_fields() {
    let cfg = RuntimeConfig::default();
    let out = build_startup_log(
        Some("default"),
        &cfg,
        &serde_json::json!({"mode":"cli"}),
        &serde_json::json!({"AFPSQL_DSN_SECRET": null}),
    );
    assert!(matches!(out, Output::Log { .. }));
    if let Output::Log {
        event,
        version,
        config,
        args,
        env,
        ..
    } = out
    {
        assert_eq!(event, "startup");
        assert!(version.is_some());
        assert!(config.is_some());
        assert!(args.is_some());
        assert!(env.is_some());
    }
}

#[test]
fn startup_log_redacts_args_and_has_no_argv() {
    let cfg = RuntimeConfig::default();
    let out = build_startup_log(
        Some("default"),
        &cfg,
        &serde_json::json!({
            "mode": "cli",
            "dsn_secret": "postgresql://user:supersecret@host/db",
            "param_count": 1
        }),
        &serde_json::json!({"AFPSQL_DSN_SECRET": "postgresql://env:secret@host/db"}),
    );
    let rendered = output_fmt::render_output(&out, OutputFormat::Json);
    assert!(!rendered.contains("\"argv\""));
    assert!(!rendered.contains("supersecret"));
    assert!(!rendered.contains("postgresql://env:secret@host/db"));
    assert!(rendered.contains("\"dsn_secret\":\"***\""));
    assert!(rendered.contains("\"AFPSQL_DSN_SECRET\":\"***\""));
    assert!(rendered.contains("\"param_count\":1"));
}

#[tokio::test]
async fn read_limited_line_rejects_oversized_line_and_recovers() {
    let input = b"abcdef\nok\n";
    let mut reader = tokio::io::BufReader::new(&input[..]);

    let first = read_limited_line(&mut reader, 4).await;
    assert!(matches!(first, Ok(Some(Err(())))));

    let second = read_limited_line(&mut reader, 4).await;
    assert!(matches!(second, Ok(Some(Ok(line))) if line == "ok\n"));
}

#[test]
fn validate_query_request_rejects_limits() {
    assert!(validate_query_request("q1", "select 1", &[]).is_none());

    let long_sql = "x".repeat(MAX_SQL_BYTES + 1);
    let sql_err = validate_query_request("q1", &long_sql, &[]);
    assert!(matches!(
        sql_err,
        Some(Output::Error {
            error_code,
            ..
        }) if error_code == "invalid_request"
    ));

    let params = vec![serde_json::Value::Null; MAX_PARAMS + 1];
    let params_err = validate_query_request("q1", "select 1", &params);
    assert!(matches!(
        params_err,
        Some(Output::Error {
            error_code,
            ..
        }) if error_code == "invalid_request"
    ));
}
