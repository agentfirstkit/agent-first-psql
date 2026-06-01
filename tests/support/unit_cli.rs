use super::*;

fn raw_args(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_string()).collect()
}

#[test]
fn parse_params_order_and_types() {
    let p_res = parse_params(&["2=active".to_string(), "1=42".to_string()]);
    assert!(p_res.is_ok());
    if let Ok(p) = p_res {
        // CLI --param values are passed as strings; PostgreSQL coerces based
        // on the prepared statement's parameter type. This preserves leading
        // zeros, signs, and NUMERIC precision.
        assert_eq!(p[0], Value::String("42".to_string()));
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
fn parse_params_duplicate_index_errors() {
    let err_res = parse_params(&["1=old".to_string(), "1=new".to_string()]);
    assert!(err_res.is_err());
    if let Err(err) = err_res {
        assert!(err.contains("duplicate parameter index 1"));
    }
}

#[test]
fn parse_params_too_many_entries_errors() {
    let entries = vec!["1=x".to_string(); MAX_PARAMS + 1];
    let err_res = parse_params(&entries);
    assert!(err_res.is_err());
    if let Err(err) = err_res {
        assert!(err.contains("maximum params"));
    }
}

#[test]
fn parse_params_index_over_limit_errors_before_allocation() {
    let err_res = parse_params(&[format!("{}=x", MAX_PARAMS + 1)]);
    assert!(err_res.is_err());
    if let Err(err) = err_res {
        assert!(err.contains("exceeds maximum params"));
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
    // Numeric-looking strings stay as strings so PG receives the literal as
    // written. This preserves leading zeros and NUMERIC precision.
    assert_eq!(parse_param_value("42"), Value::String("42".to_string()));
    assert_eq!(
        parse_param_value("00123"),
        Value::String("00123".to_string())
    );
    assert_eq!(parse_param_value("1.5"), Value::String("1.5".to_string()));
    assert_eq!(
        parse_param_value("12345.6789012345"),
        Value::String("12345.6789012345".to_string())
    );
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
fn clap_accepts_skill_admin_subcommands() {
    let cli_res = AfdCli::try_parse_from([
        "afpsql",
        "skill",
        "install",
        "--agent",
        "claude-code",
        "--scope",
        "project",
        "--force",
    ]);
    assert!(cli_res.is_ok());
    if let Ok(cli) = cli_res {
        assert!(matches!(
            cli.command,
            Some(AfdCommand::Skill(SkillCommand {
                action: SkillCliAction::Install(_)
            }))
        ));
    }
}

#[test]
fn clap_accepts_global_output_after_admin_subcommands() {
    let cli_res = AfdCli::try_parse_from(["afpsql", "skill", "status", "--output", "yaml"]);
    assert!(cli_res.is_ok());
    if let Ok(cli) = cli_res {
        assert_eq!(cli.output, "yaml");
    }
}

#[test]
fn clap_accepts_ssh_transport_flags() {
    let cli_res = AfdCli::try_parse_from([
        "afpsql",
        "--ssh",
        "user@example.com",
        "--ssh-option",
        "ProxyJump=bastion",
        "--ssh-local-host",
        "127.0.0.1",
        "--ssh-local-port",
        "15432",
        "--ssh-remote-socket",
        "/var/run/postgresql/.s.PGSQL.5432",
        "--ssh-sudo-user",
        "postgres",
        "--sql",
        "select 1",
    ]);
    assert!(cli_res.is_ok());
    if let Ok(cli) = cli_res {
        assert_eq!(cli.ssh.as_deref(), Some("user@example.com"));
        assert_eq!(cli.ssh_options, vec!["ProxyJump=bastion".to_string()]);
        assert_eq!(cli.ssh_local_host.as_deref(), Some("127.0.0.1"));
        assert_eq!(cli.ssh_local_port, Some(15432));
        assert_eq!(
            cli.ssh_remote_socket.as_deref(),
            Some("/var/run/postgresql/.s.PGSQL.5432")
        );
        assert_eq!(cli.ssh_sudo_user.as_deref(), Some("postgres"));
    }
}

#[test]
fn clap_accepts_container_transport_flags() {
    let cli_res = AfdCli::try_parse_from([
        "afpsql",
        "--container",
        "pg",
        "--container-driver",
        "kubectl",
        "--container-namespace",
        "prod",
        "--container-context",
        "cluster-a",
        "--container-pod-container",
        "postgres",
        "--container-user",
        "postgres",
        "--sql",
        "select 1",
    ]);
    assert!(cli_res.is_ok());
    if let Ok(cli) = cli_res {
        assert_eq!(cli.container.as_deref(), Some("pg"));
        assert_eq!(cli.container_driver.as_deref(), Some("kubectl"));
        assert_eq!(cli.container_namespace.as_deref(), Some("prod"));
        assert_eq!(cli.container_context.as_deref(), Some("cluster-a"));
        assert_eq!(cli.container_pod_container.as_deref(), Some("postgres"));
        assert_eq!(cli.container_user.as_deref(), Some("postgres"));
    }
}

#[test]
fn clap_accepts_ssh_plus_container_transport_flags() {
    let cli_res = AfdCli::try_parse_from([
        "afpsql",
        "--ssh",
        "root@example.com",
        "--ssh-option",
        "ProxyJump=bastion",
        "--container",
        "pg",
        "--container-driver",
        "podman",
        "--sql",
        "select 1",
    ]);
    assert!(cli_res.is_ok());
    if let Ok(cli) = cli_res {
        assert_eq!(cli.ssh.as_deref(), Some("root@example.com"));
        assert_eq!(cli.ssh_options, vec!["ProxyJump=bastion".to_string()]);
        assert_eq!(cli.container.as_deref(), Some("pg"));
        assert_eq!(cli.container_driver.as_deref(), Some("podman"));
    }
}

#[test]
fn clap_accepts_permission_flag() {
    let cli_res = AfdCli::try_parse_from([
        "afpsql",
        "--permission",
        "container-write",
        "--sql",
        "select 1",
    ]);
    assert!(cli_res.is_ok());
    if let Ok(cli) = cli_res {
        assert_eq!(cli.permission, Some(Permission::ContainerWrite));
    }
}

#[test]
fn clap_accepts_sql_values_that_look_like_flags() {
    let cli_res = AfdCli::try_parse_from(["afpsql", "--sql", "--mode=psql", "--dry-run"]);
    assert!(cli_res.is_ok());
    if let Ok(cli) = cli_res {
        assert_eq!(cli.sql.as_deref(), Some("--mode=psql"));
        assert!(cli.dry_run);
    }

    let help_sql_res = AfdCli::try_parse_from(["afpsql", "--sql", "--help-markdown", "--dry-run"]);
    assert!(help_sql_res.is_ok());
    if let Ok(cli) = help_sql_res {
        assert_eq!(cli.sql.as_deref(), Some("--help-markdown"));
        assert!(cli.dry_run);
    }
}

#[test]
fn clap_rejects_removed_read_only_flag() {
    let cli_res = AfdCli::try_parse_from(["afpsql", "--read-only", "--sql", "select 1"]);
    assert!(cli_res.is_err());
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
fn top_level_mode_scan_ignores_option_values() {
    assert!(is_psql_mode_requested(&raw_args(&[
        "afpsql", "--mode", "psql", "-c", "select 1",
    ])));
    assert!(is_psql_mode_requested(&raw_args(&[
        "afpsql",
        "--mode=psql",
        "-c",
        "select 1",
    ])));

    assert!(!is_psql_mode_requested(&raw_args(&[
        "afpsql",
        "--sql",
        "--mode=psql",
        "--dry-run",
    ])));
    assert!(!is_psql_mode_requested(&raw_args(&[
        "afpsql",
        "--sql=--mode=psql",
        "--dry-run",
    ])));
    assert!(!is_psql_mode_requested(&raw_args(&[
        "afpsql",
        "psql",
        "status",
        "--bin-dir",
        "--mode=psql",
    ])));
}

#[test]
fn top_level_help_markdown_scan_ignores_option_values() {
    assert!(top_level_help_markdown_requested(&raw_args(&[
        "afpsql",
        "--help-markdown",
    ])));
    assert!(!top_level_help_markdown_requested(&raw_args(&[
        "afpsql",
        "--sql",
        "--help-markdown",
        "--dry-run",
    ])));
    assert!(!top_level_help_markdown_requested(&raw_args(&[
        "afpsql",
        "--sql=--help-markdown",
    ])));
}

#[test]
fn startup_payload_summarizes_sql_without_text() {
    let args = startup_args(
        "cli",
        Some("-- sensitive comment\nselect 'secret-value' as token"),
        None,
        0,
    );
    assert_eq!(args["mode"], "cli");
    assert_eq!(args["sql"]["present"], true);
    assert_eq!(args["sql"]["source"], "inline");
    assert_eq!(args["sql"]["operation"], "select");
    assert_eq!(
        args["sql"]["bytes"],
        serde_json::json!("-- sensitive comment\nselect 'secret-value' as token".len())
    );
    let rendered = serde_json::to_string(&args).unwrap_or_default();
    assert!(!rendered.contains("secret-value"));
    assert!(!rendered.contains("sensitive comment"));
}

#[test]
fn startup_env_snapshot_records_presence_only() {
    let key = "PGPASSWORD";
    let old = std::env::var_os(key);
    std::env::set_var(key, "pg-secret-for-startup-test");
    let env = startup_env_snapshot();
    match old {
        Some(value) => std::env::set_var(key, value),
        None => std::env::remove_var(key),
    }

    let entries = env.as_array();
    assert!(entries.is_some(), "startup env must be an array");
    let entry = entries
        .map(|entries| entries.iter().find(|entry| entry["key"] == key))
        .unwrap_or(None);
    assert!(entry.is_some(), "PGPASSWORD presence must be reported");
    if let Some(entry) = entry {
        assert_eq!(entry["present"], true);
        assert!(entry.get("value").is_none());
    }
    assert!(!serde_json::to_string(&env)
        .unwrap_or_default()
        .contains("pg-secret-for-startup-test"));
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
fn load_sql_rejects_oversized_inline_sql() {
    let err_res = load_sql(Some("x".repeat(MAX_SQL_BYTES + 1)), None);
    assert!(err_res.is_err());
    if let Err(err) = err_res {
        assert!(err.contains("maximum SQL size"));
    }
}

#[test]
fn load_sql_rejects_oversized_file() {
    let path = temp_sql_path("oversized");
    let write_res = std::fs::write(&path, "x".repeat(MAX_SQL_BYTES + 1));
    assert!(write_res.is_ok());

    let err_res = load_sql(None, Some(path.to_string_lossy().into_owned()));
    let _ = std::fs::remove_file(&path);

    assert!(err_res.is_err());
    if let Err(err) = err_res {
        assert!(err.contains("maximum SQL size"));
    }
}

fn temp_sql_path(name: &str) -> std::path::PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("afpsql-{name}-{}-{unique}.sql", std::process::id()))
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
            assert!(req.startup_args.get("dsn_secret_env").is_none());
            assert!(req.startup_args.get("password_secret_env").is_none());
            assert_eq!(req.startup_args["sql"]["operation"], "select");
            assert_eq!(req.startup_args["sql"]["source"], "inline");
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
        "--output-format".to_string(),
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
            assert_eq!(req.startup_args["sql"]["source"], serde_json::json!("file"));
            assert_eq!(
                req.startup_args["sql"]["operation"],
                serde_json::json!("select")
            );
            assert!(req.startup_args.get("sql_file").is_none());
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
            assert_eq!(req.options.permission, Some(Permission::Write));
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
fn parse_psql_mode_accepts_container_transport() {
    let raw = vec![
        "afpsql".to_string(),
        "--mode".to_string(),
        "psql".to_string(),
        "--container".to_string(),
        "pg".to_string(),
        "--container-driver".to_string(),
        "compose".to_string(),
        "--container-compose-file".to_string(),
        "compose.yml".to_string(),
        "--container-compose-project".to_string(),
        "demo".to_string(),
        "--container-user".to_string(),
        "postgres".to_string(),
        "-c".to_string(),
        "select 1".to_string(),
    ];
    let mode_res = parse_psql_mode(&raw);
    assert!(mode_res.is_ok());
    if let Ok(Mode::Cli(req)) = mode_res {
        assert_eq!(req.session.container.target.as_deref(), Some("pg"));
        assert_eq!(req.session.container.driver.as_deref(), Some("compose"));
        assert_eq!(
            req.session.container.compose_files,
            vec!["compose.yml".to_string()]
        );
        assert_eq!(
            req.session.container.compose_project.as_deref(),
            Some("demo")
        );
        assert_eq!(req.session.container.user.as_deref(), Some("postgres"));
        assert_eq!(req.options.permission, Some(Permission::ContainerWrite));
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
            assert_eq!(req.params, vec![serde_json::json!("7")]);
        }
    }
}

#[test]
fn parse_psql_mode_accepts_long_aliases_clusters_and_behavior_vars() {
    let raw = vec![
        "afpsql".to_string(),
        "--mode=psql".to_string(),
        "-qAtX".to_string(),
        "--host=localhost".to_string(),
        "--port".to_string(),
        "5432".to_string(),
        "--username".to_string(),
        "roger".to_string(),
        "--dbname".to_string(),
        "postgres".to_string(),
        "--command".to_string(),
        "select $1::int as n".to_string(),
        "--set".to_string(),
        "ON_ERROR_STOP=1".to_string(),
        "--variable".to_string(),
        "1=5".to_string(),
    ];
    let mode_res = parse_psql_mode(&raw);
    assert!(mode_res.is_ok());
    if let Ok(Mode::Cli(req)) = mode_res {
        assert_eq!(req.sql, "select $1::int as n");
        assert_eq!(req.params, vec![serde_json::json!("5")]);
        assert_eq!(req.session.host.as_deref(), Some("localhost"));
        assert_eq!(req.session.port, Some(5432));
        assert_eq!(req.session.user.as_deref(), Some("roger"));
        assert_eq!(req.session.dbname.as_deref(), Some("postgres"));
    }
}

#[test]
fn parse_psql_mode_rejects_afpsql_ssh_extensions() {
    let raw = vec![
        "afpsql".to_string(),
        "--mode".to_string(),
        "psql".to_string(),
        "-c".to_string(),
        "select 1".to_string(),
        "--afpsql-ssh".to_string(),
        "user@example.com".to_string(),
        "--afpsql-ssh-option".to_string(),
        "ProxyJump=bastion".to_string(),
        "--afpsql-ssh-local-port".to_string(),
        "15432".to_string(),
        "--afpsql-ssh-remote-socket".to_string(),
        "/var/run/postgresql/.s.PGSQL.5432".to_string(),
        "--afpsql-ssh-sudo-user".to_string(),
        "postgres".to_string(),
    ];
    let mode_res = parse_psql_mode(&raw);
    assert!(mode_res.is_err());
    if let Err(err) = mode_res {
        assert!(err.contains("unsupported psql-mode argument"));
        assert!(err.contains("--afpsql-ssh"));
    }
}

#[test]
fn parse_psql_mode_rejects_permission_extensions() {
    for flag in ["--permission", "--afpsql-permission", "--afpsql-read-only"] {
        let raw = vec![
            "afpsql".to_string(),
            "--mode".to_string(),
            "psql".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
            flag.to_string(),
            "write".to_string(),
        ];
        let mode_res = parse_psql_mode(&raw);
        assert!(matches!(mode_res, Err(err) if err.contains(flag)));
    }
}

#[test]
fn parse_psql_mode_positionals_fill_dbname_and_username() {
    let raw = vec![
        "afpsql".to_string(),
        "--mode".to_string(),
        "psql".to_string(),
        "-c".to_string(),
        "select 1".to_string(),
        "appdb".to_string(),
        "appuser".to_string(),
    ];
    let mode_res = parse_psql_mode(&raw);
    assert!(mode_res.is_ok());
    if let Ok(Mode::Cli(req)) = mode_res {
        assert_eq!(req.session.dbname.as_deref(), Some("appdb"));
        assert_eq!(req.session.user.as_deref(), Some("appuser"));
    }
}

#[test]
fn parse_psql_mode_interactive_flags_are_parsed_as_unsupported_mode() {
    for (flag, expected) in [
        ("-W", "password"),
        ("--password", "password"),
        ("-s", "single-step"),
        ("--single-step", "single-step"),
        ("-S", "single-line"),
        ("--single-line", "single-line"),
    ] {
        let raw = vec![
            "afpsql".to_string(),
            "--mode".to_string(),
            "psql".to_string(),
            flag.to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ];
        let mode_res = parse_psql_mode(&raw);
        assert!(matches!(
            mode_res,
            Ok(Mode::PsqlUnsupported(PsqlUnsupportedRequest { reason }))
                if reason.contains(expected)
        ));
    }

    let no_command = vec![
        "afpsql".to_string(),
        "--mode".to_string(),
        "psql".to_string(),
    ];
    let no_command_res = parse_psql_mode(&no_command);
    assert!(matches!(
        no_command_res,
        Ok(Mode::PsqlUnsupported(PsqlUnsupportedRequest { reason }))
            if reason.contains("no -c")
    ));
}

#[test]
fn parse_psql_mode_accepts_all_official_no_value_noninteractive_options() {
    for flag in [
        "-a",
        "--echo-all",
        "-A",
        "--no-align",
        "-b",
        "--echo-errors",
        "--csv",
        "-e",
        "--echo-queries",
        "-E",
        "--echo-hidden",
        "-H",
        "--html",
        "-n",
        "--no-readline",
        "-q",
        "--quiet",
        "-t",
        "--tuples-only",
        "-w",
        "--no-password",
        "-x",
        "--expanded",
        "-X",
        "--no-psqlrc",
        "-z",
        "--field-separator-zero",
        "-0",
        "--record-separator-zero",
        "-1",
        "--single-transaction",
    ] {
        let raw = vec![
            "afpsql".to_string(),
            "--mode".to_string(),
            "psql".to_string(),
            flag.to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ];
        assert!(parse_psql_mode(&raw).is_ok(), "{flag} should parse");
    }

    for flag in ["-l", "--list"] {
        let raw = vec![
            "afpsql".to_string(),
            "--mode".to_string(),
            "psql".to_string(),
            flag.to_string(),
        ];
        let mode_res = parse_psql_mode(&raw);
        assert!(mode_res.is_ok(), "{flag} should parse");
        if let Ok(Mode::Cli(req)) = mode_res {
            assert!(req.sql.contains("pg_catalog.pg_database"));
        }
    }
}

#[test]
fn parse_psql_mode_accepts_all_official_value_options_and_aliases() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("afpsql_all_opts_{}.sql", std::process::id()));
    assert!(std::fs::write(&path, "select 1").is_ok());
    let path = path.to_string_lossy().to_string();

    let ok_cases: Vec<Vec<String>> = vec![
        vec!["-c".to_string(), "select 1".to_string()],
        vec!["-cselect 1".to_string()],
        vec!["--command".to_string(), "select 1".to_string()],
        vec!["--command=select 1".to_string()],
        vec!["-f".to_string(), path.clone()],
        vec![format!("-f{path}")],
        vec!["--file".to_string(), path.clone()],
        vec![format!("--file={path}")],
        vec![
            "-F".to_string(),
            "|".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ],
        vec!["-F|".to_string(), "-c".to_string(), "select 1".to_string()],
        vec![
            "--field-separator".to_string(),
            "|".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ],
        vec![
            "--field-separator=|".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ],
        vec![
            "-h".to_string(),
            "localhost".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ],
        vec![
            "-hlocalhost".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ],
        vec![
            "--host".to_string(),
            "localhost".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ],
        vec![
            "--host=localhost".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ],
        vec![
            "-p".to_string(),
            "5432".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ],
        vec![
            "-p5432".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ],
        vec![
            "--port".to_string(),
            "5432".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ],
        vec![
            "--port=5432".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ],
        vec![
            "-P".to_string(),
            "format=csv".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ],
        vec![
            "-Pformat=csv".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ],
        vec![
            "--pset".to_string(),
            "format=csv".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ],
        vec![
            "--pset=format=csv".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ],
        vec![
            "-R".to_string(),
            "\\n".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ],
        vec![
            "-R\\n".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ],
        vec![
            "--record-separator".to_string(),
            "\\n".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ],
        vec![
            "--record-separator=\\n".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ],
        vec![
            "-T".to_string(),
            "class=x".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ],
        vec![
            "-Tclass=x".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ],
        vec![
            "--table-attr".to_string(),
            "class=x".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ],
        vec![
            "--table-attr=class=x".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ],
        vec![
            "-U".to_string(),
            "roger".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ],
        vec![
            "-Uroger".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ],
        vec![
            "--username".to_string(),
            "roger".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ],
        vec![
            "--username=roger".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ],
        vec![
            "--user=roger".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ],
        vec![
            "-v".to_string(),
            "1=7".to_string(),
            "-c".to_string(),
            "select $1".to_string(),
        ],
        vec![
            "-v1=7".to_string(),
            "-c".to_string(),
            "select $1".to_string(),
        ],
        vec![
            "--set".to_string(),
            "ON_ERROR_STOP=1".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ],
        vec![
            "--set=ON_ERROR_STOP=1".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ],
        vec![
            "--variable".to_string(),
            "1=7".to_string(),
            "-c".to_string(),
            "select $1".to_string(),
        ],
        vec![
            "--variable=1=7".to_string(),
            "-c".to_string(),
            "select $1".to_string(),
        ],
    ];

    for extra_args in ok_cases {
        let mut raw = vec![
            "afpsql".to_string(),
            "--mode".to_string(),
            "psql".to_string(),
        ];
        raw.extend(extra_args.clone());
        assert!(parse_psql_mode(&raw).is_ok(), "{extra_args:?} should parse");
    }

    let _ = std::fs::remove_file(path);
}

#[test]
fn parse_psql_mode_dbname_accepts_database_conninfo_or_uri_forms() {
    for (args, expected) in [
        (vec!["-d", "appdb"], ("dbname", Some("appdb"), None, None)),
        (
            vec!["-dpostgresql://localhost/appdb"],
            ("dsn", None, Some("postgresql://localhost/appdb"), None),
        ),
        (
            vec!["--dbname", "host=localhost dbname=appdb"],
            ("conninfo", None, None, Some("host=localhost dbname=appdb")),
        ),
        (
            vec!["--dbname=postgresql://localhost/appdb"],
            ("dsn", None, Some("postgresql://localhost/appdb"), None),
        ),
    ] {
        let mut raw = vec![
            "afpsql".to_string(),
            "--mode".to_string(),
            "psql".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ];
        raw.extend(args.iter().map(|s| s.to_string()));
        let mode_res = parse_psql_mode(&raw);
        assert!(mode_res.is_ok(), "{args:?} should parse as {}", expected.0);
        if let Ok(Mode::Cli(req)) = mode_res {
            assert_eq!(req.session.dbname.as_deref(), expected.1);
            assert_eq!(req.session.dsn_secret.as_deref(), expected.2);
            assert_eq!(req.session.conninfo_secret.as_deref(), expected.3);
        }
    }
}

#[test]
fn parse_psql_mode_file_routing_options_are_supported() {
    for (args, output_file, log_file) in [
        (vec!["-o", "/tmp/out.txt"], Some("/tmp/out.txt"), None),
        (vec!["-o/tmp/out.txt"], Some("/tmp/out.txt"), None),
        (vec!["--output", "/tmp/out.txt"], Some("/tmp/out.txt"), None),
        (vec!["--output=/tmp/out.txt"], Some("/tmp/out.txt"), None),
        (vec!["-L", "/tmp/log.txt"], None, Some("/tmp/log.txt")),
        (vec!["-L/tmp/log.txt"], None, Some("/tmp/log.txt")),
        (
            vec!["--log-file", "/tmp/log.txt"],
            None,
            Some("/tmp/log.txt"),
        ),
        (vec!["--log-file=/tmp/log.txt"], None, Some("/tmp/log.txt")),
    ] {
        let mut raw = vec![
            "afpsql".to_string(),
            "--mode".to_string(),
            "psql".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ];
        raw.extend(args.iter().map(|s| s.to_string()));
        let mode_res = parse_psql_mode(&raw);
        assert!(mode_res.is_ok(), "{args:?} should parse");
        if let Ok(Mode::Cli(req)) = mode_res {
            assert_eq!(req.output_file.as_deref(), output_file);
            assert_eq!(req.log_file.as_deref(), log_file);
        }
    }
}

#[test]
fn parse_psql_mode_long_output_accepts_format_alias() {
    let raw = vec![
        "afpsql".to_string(),
        "--mode".to_string(),
        "psql".to_string(),
        "-c".to_string(),
        "select 1".to_string(),
        "--output".to_string(),
        "json".to_string(),
    ];
    let mode_res = parse_psql_mode(&raw);
    assert!(mode_res.is_ok());
    if let Ok(Mode::Cli(req)) = mode_res {
        assert!(matches!(req.output, OutputFormat::Json));
        assert_eq!(req.output_file, None);
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
