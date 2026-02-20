use super::*;

#[test]
fn parse_params_order_and_types() {
    let p = parse_params(&["2=active".to_string(), "1=42".to_string()]).unwrap();
    assert_eq!(p[0], Value::Number(42.into()));
    assert_eq!(p[1], Value::String("active".to_string()));
}

#[test]
fn parse_params_missing_index_errors() {
    let err = parse_params(&["2=active".to_string()]).unwrap_err();
    assert!(err.contains("missing parameter index 1"));
}

#[test]
fn parse_params_index_starts_from_one() {
    let err = parse_params(&["0=x".to_string()]).unwrap_err();
    assert!(err.contains("start at 1"));
}

#[test]
fn parse_params_invalid_shape() {
    let err = parse_params(&["abc".to_string()]).unwrap_err();
    assert!(err.contains("expected N=value"));
}

#[test]
fn parse_param_value_primitives() {
    assert_eq!(parse_param_value("null"), Value::Null);
    assert_eq!(parse_param_value("true"), Value::Bool(true));
    assert_eq!(parse_param_value("false"), Value::Bool(false));
    assert_eq!(parse_param_value("42"), Value::Number(42.into()));
    assert_eq!(parse_param_value("1.5"), serde_json::json!(1.5));
    assert_eq!(parse_param_value("NaN"), Value::String("NaN".to_string()));
    assert_eq!(parse_param_value("abc"), Value::String("abc".to_string()));
}

#[test]
fn parse_output_formats() {
    assert!(matches!(parse_output("json"), Ok(OutputFormat::Json)));
    assert!(matches!(parse_output("yaml"), Ok(OutputFormat::Yaml)));
    assert!(matches!(parse_output("plain"), Ok(OutputFormat::Plain)));
    assert!(parse_output("bad").is_err());
}

#[test]
fn parse_log_categories_normalizes_and_dedups() {
    let logs = parse_log_categories(&[
        " Query.Result ".to_string(),
        "query.result".to_string(),
        "".to_string(),
        "ALL".to_string(),
    ]);
    assert_eq!(logs, vec!["query.result".to_string(), "all".to_string()]);
}

#[test]
fn load_sql_validation() {
    assert!(load_sql(Some("select 1".to_string()), None).is_ok());
    assert!(load_sql(Some("x".to_string()), Some("y".to_string())).is_err());
    assert!(load_sql(None, None).is_err());
}

#[test]
fn parse_psql_mode_all_flags_and_sql_file() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("afpsql_sql_{}.sql", std::process::id()));
    std::fs::write(&path, "select $1::int").unwrap();
    let raw = vec![
        "afpsql".to_string(),
        "--mode".to_string(),
        "psql".to_string(),
        "-f".to_string(),
        path.to_string_lossy().to_string(),
        "-h".to_string(),
        "localhost".to_string(),
        "-p".to_string(),
        "5432".to_string(),
        "-U".to_string(),
        "roger".to_string(),
        "-d".to_string(),
        "postgres".to_string(),
        "--conninfo-secret".to_string(),
        "host=localhost user=roger dbname=postgres".to_string(),
        "-v".to_string(),
        "1=7".to_string(),
        "--output".to_string(),
        "plain".to_string(),
    ];
    let mode = parse_psql_mode(&raw).unwrap();
    match mode {
        Mode::Cli(req) => {
            assert_eq!(req.sql.trim(), "select $1::int");
            assert_eq!(req.params.len(), 1);
            assert!(matches!(req.output, OutputFormat::Plain));
            assert_eq!(req.session.host.as_deref(), Some("localhost"));
            assert_eq!(req.session.user.as_deref(), Some("roger"));
            assert_eq!(req.session.dbname.as_deref(), Some("postgres"));
            assert!(req.session.conninfo_secret.is_some());
        }
        _ => panic!("expected cli mode"),
    }
    let _ = std::fs::remove_file(path);
}

#[test]
fn parse_psql_mode_dsn_and_errors() {
    let raw = vec![
        "afpsql".to_string(),
        "--mode".to_string(),
        "psql".to_string(),
        "-c".to_string(),
        "select 1".to_string(),
        "--dsn-secret".to_string(),
        "postgresql://localhost/postgres".to_string(),
    ];
    let mode = parse_psql_mode(&raw).unwrap();
    match mode {
        Mode::Cli(req) => {
            assert_eq!(
                req.session.dsn_secret.as_deref(),
                Some("postgresql://localhost/postgres")
            );
        }
        _ => panic!("expected cli mode"),
    }

    let bad = vec![
        "afpsql".to_string(),
        "--mode".to_string(),
        "psql".to_string(),
        "--bad".to_string(),
    ];
    let err = parse_psql_mode(&bad).err().unwrap_or_default();
    assert!(err.contains("unsupported psql-mode argument"));
}

#[test]
fn parse_psql_mode_port_and_v_errors() {
    let bad_port = vec![
        "afpsql".to_string(),
        "--mode".to_string(),
        "psql".to_string(),
        "-p".to_string(),
        "abc".to_string(),
        "-c".to_string(),
        "select 1".to_string(),
    ];
    let err = parse_psql_mode(&bad_port).err().unwrap_or_default();
    assert!(err.contains("invalid -p port"));

    let bad_v = vec![
        "afpsql".to_string(),
        "--mode".to_string(),
        "psql".to_string(),
        "-c".to_string(),
        "select $1".to_string(),
        "-v".to_string(),
        "bad".to_string(),
    ];
    let err = parse_psql_mode(&bad_v).err().unwrap_or_default();
    assert!(err.contains("expected N=value") || err.contains("invalid"));
}
