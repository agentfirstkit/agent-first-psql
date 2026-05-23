use super::*;

#[test]
fn parse_params_order_and_types() {
    let p_res = parse_params(&["2=active".to_string(), "1=42".to_string()]);
    assert!(p_res.is_ok());
    if let Ok(p) = p_res {
        assert_eq!(p[0], Value::Number(42.into()));
        assert_eq!(p[1], Value::String("active".to_string()));
    }
}

#[test]
fn parse_params_missing_index_errors() {
    let err_res = parse_params(&["2=active".to_string()]);
    assert!(err_res.is_err());
    if let Err(err) = err_res {
        assert!(err.contains("missing parameter index 1"));
    }
}

#[test]
fn parse_params_index_starts_from_one() {
    let err_res = parse_params(&["0=x".to_string()]);
    assert!(err_res.is_err());
    if let Err(err) = err_res {
        assert!(err.contains("start at 1"));
    }
}

#[test]
fn parse_params_invalid_shape() {
    let err_res = parse_params(&["abc".to_string()]);
    assert!(err_res.is_err());
    if let Err(err) = err_res {
        assert!(err.contains("expected N=value"));
    }
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
fn clap_log_flag_accepts_startup() {
    let cli_res =
        AfdCli::try_parse_from(["afpsql", "--mode", "pipe", "--log", "startup,query.error"]);
    assert!(cli_res.is_ok());
    if let Ok(cli) = cli_res {
        assert_eq!(
            parse_log_categories(&cli.log),
            vec!["startup".to_string(), "query.error".to_string()]
        );
    }
}

#[test]
fn clap_accepts_psql_admin_subcommands() {
    let cli_res =
        AfdCli::try_parse_from(["afpsql", "psql", "status", "--bin-dir", "/tmp/afpsql-bin"]);
    assert!(cli_res.is_ok());
    if let Ok(cli) = cli_res {
        assert!(matches!(
            cli.command,
            Some(AfdCommand::Psql(PsqlCommand {
                action: PsqlCliAction::Status(_)
            }))
        ));
    }
}

#[test]
fn startup_requested_detects_raw_log_entries() {
    assert!(startup_requested_from_raw(&[
        "afpsql".to_string(),
        "--log".to_string(),
        "startup".to_string(),
    ]));
    assert!(startup_requested_from_raw(&[
        "afpsql".to_string(),
        "--log=all".to_string(),
    ]));
    assert!(!startup_requested_from_raw(&[
        "afpsql".to_string(),
        "--log".to_string(),
        "query.error".to_string(),
    ]));
}

#[test]
fn resolve_secret_value_from_env_and_errors() {
    let path = std::env::var("PATH");
    assert!(path.is_ok());
    if let Ok(path) = path {
        let resolved = resolve_secret_value("--dsn-secret", None, Some("PATH"));
        assert_eq!(resolved, Ok(Some(path)));
    }

    let conflict = resolve_secret_value("--dsn-secret", Some("direct".to_string()), Some("PATH"));
    assert!(conflict.is_err());

    let missing_name = format!("AFPSQL_TEST_MISSING_{}", std::process::id());
    let missing = resolve_secret_value("--dsn-secret", None, Some(&missing_name));
    assert!(missing.is_err());
}

#[test]
fn load_sql_validation() {
    assert!(load_sql(Some("select 1".to_string()), None).is_ok());
    assert!(load_sql(Some("x".to_string()), Some("y".to_string())).is_err());
    assert!(load_sql(None, None).is_err());
}

#[test]
fn parse_psql_mode_accepts_secret_env_flags() {
    let path = std::env::var("PATH");
    assert!(path.is_ok());
    let raw = vec![
        "afpsql".to_string(),
        "--mode".to_string(),
        "psql".to_string(),
        "-c".to_string(),
        "select 1".to_string(),
        "--dsn-secret-env".to_string(),
        "PATH".to_string(),
        "--password-secret-env".to_string(),
        "PATH".to_string(),
    ];
    let mode_res = parse_psql_mode(&raw);
    assert!(mode_res.is_ok());
    if let (Ok(mode), Ok(path)) = (mode_res, path) {
        assert!(matches!(mode, Mode::Cli(_)));
        if let Mode::Cli(req) = mode {
            assert_eq!(req.session.dsn_secret.as_deref(), Some(path.as_str()));
            assert_eq!(req.session.password_secret.as_deref(), Some(path.as_str()));
            assert_eq!(
                req.startup_args["dsn_secret_env"],
                serde_json::json!("PATH")
            );
            assert_eq!(
                req.startup_args["password_secret_env"],
                serde_json::json!("PATH")
            );
        }
    }
}

#[test]
fn parse_psql_mode_all_flags_and_sql_file() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("afpsql_sql_{}.sql", std::process::id()));
    assert!(std::fs::write(&path, "select $1::int").is_ok());
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
    let mode_res = parse_psql_mode(&raw);
    assert!(mode_res.is_ok());
    if let Ok(mode) = mode_res {
        assert!(matches!(mode, Mode::Cli(_)));
        if let Mode::Cli(req) = mode {
            assert_eq!(req.sql.trim(), "select $1::int");
            assert_eq!(req.params.len(), 1);
            assert_eq!(req.startup_args["param_count"], serde_json::json!(1));
            assert!(req.startup_args.get("param").is_none());
            assert!(matches!(req.output, OutputFormat::Plain));
            assert_eq!(req.session.host.as_deref(), Some("localhost"));
            assert_eq!(req.session.user.as_deref(), Some("roger"));
            assert_eq!(req.session.dbname.as_deref(), Some("postgres"));
            assert!(req.session.conninfo_secret.is_some());
        }
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
    let mode_res = parse_psql_mode(&raw);
    assert!(mode_res.is_ok());
    if let Ok(mode) = mode_res {
        assert!(matches!(mode, Mode::Cli(_)));
        if let Mode::Cli(req) = mode {
            assert_eq!(
                req.session.dsn_secret.as_deref(),
                Some("postgresql://localhost/postgres")
            );
        }
    }

    let bad = vec![
        "afpsql".to_string(),
        "--mode".to_string(),
        "psql".to_string(),
        "--bad".to_string(),
    ];
    let err_res = parse_psql_mode(&bad);
    assert!(err_res.is_err());
    if let Err(err) = err_res {
        assert!(err.contains("unsupported psql-mode argument"));
    }
}

#[test]
fn parse_psql_mode_positional_dsn_does_not_short_circuit() {
    let raw = vec![
        "afpsql".to_string(),
        "--mode".to_string(),
        "psql".to_string(),
        "postgresql://localhost/postgres".to_string(),
        "-c".to_string(),
        "select $1::int as n".to_string(),
        "-v".to_string(),
        "1=7".to_string(),
    ];
    let mode_res = parse_psql_mode(&raw);
    assert!(mode_res.is_ok());
    if let Ok(mode) = mode_res {
        assert!(matches!(mode, Mode::Cli(_)));
        if let Mode::Cli(req) = mode {
            assert_eq!(
                req.session.dsn_secret.as_deref(),
                Some("postgresql://localhost/postgres")
            );
            assert_eq!(req.sql, "select $1::int as n");
            assert_eq!(req.params, vec![serde_json::json!(7)]);
        }
    }
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
    let err_res = parse_psql_mode(&bad_port);
    assert!(err_res.is_err());
    if let Err(err) = err_res {
        assert!(err.contains("invalid -p port"));
    }

    let bad_v = vec![
        "afpsql".to_string(),
        "--mode".to_string(),
        "psql".to_string(),
        "-c".to_string(),
        "select $1".to_string(),
        "-v".to_string(),
        "bad".to_string(),
    ];
    let err_res = parse_psql_mode(&bad_v);
    assert!(err_res.is_err());
    if let Err(err) = err_res {
        assert!(err.contains("expected N=value") || err.contains("invalid"));
    }
}
