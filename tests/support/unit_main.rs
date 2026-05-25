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
    let out = build_startup_log(
        Some("default"),
        &serde_json::json!({
            "mode": "cli",
            "sql": {
                "present": true,
                "source": "inline",
                "bytes": 8,
                "chars": 8,
                "operation": "select"
            },
            "param_count": 0
        }),
        &serde_json::json!([{"key": "AFPSQL_DSN_SECRET", "present": false}]),
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
        assert!(config.is_none());
        assert!(args.is_some());
        assert!(env.is_some());
    }
}

#[test]
fn startup_log_omits_raw_sql_config_and_env_values() {
    let out = build_startup_log(
        Some("default"),
        &serde_json::json!({
            "mode": "cli",
            "sql": {
                "present": true,
                "source": "inline",
                "bytes": 47,
                "chars": 47,
                "operation": "select"
            },
            "param_count": 1
        }),
        &serde_json::json!([
            {"key": "AFPSQL_DSN_SECRET", "present": true},
            {"key": "PGPASSWORD", "present": true}
        ]),
    );
    let rendered = output_fmt::render_output(&out, OutputFormat::Json);
    assert!(!rendered.contains("\"argv\""));
    assert!(!rendered.contains("\"config\""));
    assert!(!rendered.contains("postgresql://"));
    assert!(!rendered.contains("pg-secret"));
    assert!(!rendered.contains("select 'sensitive'"));
    assert!(rendered.contains("\"operation\":\"select\""));
    assert!(rendered.contains("\"bytes\":47"));
    assert!(rendered.contains("\"key\":\"PGPASSWORD\""));
    assert!(rendered.contains("\"present\":true"));
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
