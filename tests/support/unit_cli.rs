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
fn inspect_databases_includes_size_and_connection_facts() {
    let (sql, params) = build_inspect_sql(InspectAction::Databases(InspectDatabasesArgs {
        all: false,
    }));
    assert!(params.is_empty());
    for needle in [
        "pg_database_size",
        "datcollate",
        "datctype",
        "datistemplate",
        "datallowconn",
        "datconnlimit",
        "numbackends",
        "has_database_privilege",
    ] {
        assert!(
            sql.contains(needle),
            "databases SQL missing {needle}: {sql}"
        );
    }
    // Default hides templates.
    assert!(sql.contains("where not d.datistemplate"));
}

#[test]
fn inspect_databases_all_includes_templates() {
    let (sql, _) = build_inspect_sql(InspectAction::Databases(InspectDatabasesArgs { all: true }));
    // --all drops the template filter so template0/template1 appear.
    assert!(!sql.contains("where not d.datistemplate"), "SQL: {sql}");
}

#[test]
fn inspect_database_summarizes_object_counts() {
    let (sql, params) = build_inspect_sql(InspectAction::Database);
    assert!(params.is_empty());
    for needle in [
        "current_database()",
        "as schemas",
        "as tables",
        "as views",
        "as materialized_views",
        "as sequences",
        "pg_database_size(current_database())",
    ] {
        assert!(sql.contains(needle), "database SQL missing {needle}: {sql}");
    }
}

#[test]
fn inspect_schemas_includes_counts_and_size() {
    let (sql, params) = build_inspect_sql(InspectAction::Schemas);
    assert!(params.is_empty());
    assert!(sql.contains("pg_namespace"));
    assert!(sql.contains("as tables") && sql.contains("as size"));
}

#[test]
fn inspect_schema_exports_full_metadata_snapshot() {
    let (sql, params) = build_inspect_sql(InspectAction::Schema(InspectSchemaArgs {
        schema: "app".to_string(),
        like: Some("order%".to_string()),
    }));
    assert_eq!(params[0], Value::String("app".to_string()));
    assert_eq!(params[1], Value::String("order%".to_string()));
    for needle in [
        "with relation_filter",
        "'extension'::text as kind",
        "'column'::text as kind",
        "'constraint'::text as kind",
        "'index'::text as kind",
        "'trigger'::text as kind",
        "'function'::text as kind",
        "pg_get_serial_sequence",
        "c.relname like $2",
    ] {
        assert!(sql.contains(needle), "schema SQL missing {needle}: {sql}");
    }
}

#[test]
fn inspect_snapshot_uses_same_full_metadata_shape() {
    let (sql, params) = build_inspect_sql(InspectAction::Snapshot(InspectSchemaArgs {
        schema: "public".to_string(),
        like: None,
    }));
    assert_eq!(params[0], Value::String("public".to_string()));
    assert_eq!(params[1], Value::Null);
    assert!(sql.contains("select * from snapshot"));
    assert!(sql.contains("order by case kind"));
}

#[test]
fn inspect_tables_includes_owner_rows_and_size() {
    let (sql, params) = build_inspect_sql(InspectAction::Tables(InspectTablesArgs {
        schema: "public".to_string(),
        like: Some("foo%".to_string()),
    }));
    assert_eq!(params.len(), 2);
    assert!(sql.contains("estimated_rows"));
    assert!(sql.contains("pg_total_relation_size"));
    assert!(sql.contains("c.relname like $2"));
}

#[test]
fn inspect_indexes_can_include_builtin_usage_stats() {
    let (sql, params) = build_inspect_sql(InspectAction::Indexes(InspectIndexesArgs {
        schema: "ignored".to_string(),
        table: Some("app.orders".to_string()),
        stats: true,
    }));
    assert_eq!(params[0], Value::String("app".to_string()));
    assert_eq!(params[1], Value::String("orders".to_string()));
    for needle in [
        "pg_get_indexdef",
        "pg_relation_size",
        "pg_stat_user_indexes",
        "index_scan_count",
        "index_tuple_read_count",
        "tc.relname = $2",
    ] {
        assert!(sql.contains(needle), "indexes SQL missing {needle}: {sql}");
    }
}

#[test]
fn inspect_indexes_without_stats_omits_stats_view() {
    let (sql, params) = build_inspect_sql(InspectAction::Indexes(InspectIndexesArgs {
        schema: "public".to_string(),
        table: None,
        stats: false,
    }));
    assert_eq!(params, vec![Value::String("public".to_string())]);
    assert!(!sql.contains("pg_stat_user_indexes"), "SQL: {sql}");
    assert!(!sql.contains("index_scan_count"), "SQL: {sql}");
}

#[test]
fn inspect_table_describes_keys_and_comments() {
    let (sql, params) = build_inspect_sql(InspectAction::Table(InspectTableArgs {
        name: "myschema.t".to_string(),
        full: false,
    }));
    assert_eq!(params[0], Value::String("myschema".to_string()));
    assert_eq!(params[1], Value::String("t".to_string()));
    assert!(sql.contains("format_type"));
    assert!(sql.contains("as primary_key"));
    assert!(sql.contains("col_description"));
}

#[test]
fn inspect_table_full_returns_snapshot_rows_for_one_table() {
    let (sql, params) = build_inspect_sql(InspectAction::Table(InspectTableArgs {
        name: "myschema.t".to_string(),
        full: true,
    }));
    assert_eq!(params[0], Value::String("myschema".to_string()));
    assert_eq!(params[1], Value::String("t".to_string()));
    assert!(sql.contains("c.relname = $2"));
    assert!(sql.contains("'constraint'::text as kind"));
    assert!(sql.contains("'index'::text as kind"));
    assert!(sql.contains("'trigger'::text as kind"));
}

#[test]
fn clap_accepts_extended_inspect_subcommands() {
    for args in [
        vec!["afpsql", "inspect", "schema", "--schema", "public"],
        vec!["afpsql", "inspect", "snapshot", "--like", "foo%"],
        vec![
            "afpsql", "inspect", "indexes", "--schema", "public", "--table", "users", "--stats",
        ],
        vec!["afpsql", "inspect", "table", "public.users", "--full"],
    ] {
        assert!(
            AfdCli::try_parse_from(args).is_ok(),
            "extended inspect command did not parse"
        );
    }
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
    assert_eq!(
        logs,
        agent_first_data::LogFilters::new(["query.result", "all"])
    );
}

#[test]
fn clap_log_flag_accepts_startup() {
    let cli_res =
        AfdCli::try_parse_from(["afpsql", "--mode", "pipe", "--log", "startup,query.error"]);
    assert!(cli_res.is_ok());
    if let Ok(cli) = cli_res {
        assert_eq!(
            parse_log_categories(&cli.log),
            agent_first_data::LogFilters::new(["startup", "query.error"])
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
        "workspace",
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
        "--ssh-via",
        "user@jump1",
        "--ssh-via",
        "user@jump2",
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
        assert_eq!(
            cli.ssh_via,
            vec!["user@jump1".to_string(), "user@jump2".to_string()]
        );
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

    let help_sql_res = AfdCli::try_parse_from(["afpsql", "--sql", "--explain", "--dry-run"]);
    assert!(help_sql_res.is_ok());
    if let Ok(cli) = help_sql_res {
        assert_eq!(cli.sql.as_deref(), Some("--explain"));
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
fn help_output_markdown_scan_ignores_option_values() {
    let rendered = agent_first_data::cli_handle_help_or_continue(
        &raw_args(&["afpsql", "--help", "--output", "markdown"]),
        &AfdCli::command(),
        &agent_first_data::HelpConfig::human_cli_default(),
    );
    assert!(
        matches!(&rendered, Ok(Some(md)) if md.contains("# Agent-First PSQL") && md.contains("`afpsql`")),
        "markdown help should render with the afpsql title and command name"
    );

    let non_helper = agent_first_data::cli_handle_help_or_continue(
        &raw_args(&["afpsql", "--sql", "--help", "--dry-run"]),
        &AfdCli::command(),
        &agent_first_data::HelpConfig::human_cli_default(),
    );
    assert!(
        matches!(non_helper, Ok(None)),
        "non-helper request should produce no help output"
    );
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
    let _env_guard = crate::test_env::env_lock();
    let key = "PGPASSWORD";
    let old = std::env::var_os(key);
    // SAFETY: all tests in this crate that mutate environment variables hold
    // the shared test environment lock for the full mutation window.
    unsafe { std::env::set_var(key, "pg-secret-for-startup-test") };
    let env = startup_env_snapshot();
    match old {
        // SAFETY: the shared test environment lock is still held here.
        Some(value) => unsafe { std::env::set_var(key, value) },
        // SAFETY: the shared test environment lock is still held here.
        None => unsafe { std::env::remove_var(key) },
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
    assert!(
        !serde_json::to_string(&env)
            .unwrap_or_default()
            .contains("pg-secret-for-startup-test")
    );
}

#[test]
fn resolve_secret_value_from_env_and_errors() {
    let path = std::env::var("PATH");
    assert!(path.is_ok());
    if let Ok(path) = path {
        let resolved = resolve_secret_value("--dsn-secret", None, Some("PATH"), None);
        assert_eq!(resolved, Ok(Some(path)));
    }

    let conflict = resolve_secret_value(
        "--dsn-secret",
        Some("direct".to_string()),
        Some("PATH"),
        None,
    );
    assert!(conflict.is_err());

    let missing_name = format!("AFPSQL_TEST_MISSING_{}", std::process::id());
    let missing = resolve_secret_value("--dsn-secret", None, Some(&missing_name), None);
    assert!(missing.is_err());
}

#[test]
fn clap_accepts_two_value_config_sources_and_rejects_slot_conflicts() {
    for flag in [
        "--dsn-secret-config",
        "--conninfo-secret-config",
        "--password-secret-config",
    ] {
        let parsed = AfdCli::try_parse_from([
            "afpsql",
            flag,
            "config.yaml",
            "database.url",
            "--sql",
            "select 1",
        ]);
        assert!(parsed.is_ok(), "failed to parse {flag}");

        let equals = format!("{flag}=config.yaml");
        let parsed =
            AfdCli::try_parse_from(["afpsql", &equals, "database.url", "--sql", "select 1"]);
        assert!(parsed.is_err(), "accepted unsupported {equals}");

        for invalid in [
            vec!["afpsql", flag, "config.yaml", "--sql", "select 1"],
            vec![
                "afpsql",
                flag,
                "config.yaml",
                "database.url",
                "extra",
                "--sql",
                "select 1",
            ],
        ] {
            assert!(AfdCli::try_parse_from(invalid).is_err(), "accepted {flag}");
        }
    }

    for (direct, env, config) in [
        ("--dsn-secret", "--dsn-secret-env", "--dsn-secret-config"),
        (
            "--conninfo-secret",
            "--conninfo-secret-env",
            "--conninfo-secret-config",
        ),
        (
            "--password-secret",
            "--password-secret-env",
            "--password-secret-config",
        ),
    ] {
        for args in [
            vec!["afpsql", direct, "direct", env, "SECRET_ENV"],
            vec!["afpsql", direct, "direct", config, "config.json", "value"],
            vec!["afpsql", env, "SECRET_ENV", config, "config.json", "value"],
        ] {
            assert!(AfdCli::try_parse_from(args).is_err());
        }
    }
}

#[test]
fn psql_parser_consumes_config_source_pairs_and_rejects_bad_arity() {
    for (flag, field) in [
        ("--dsn-secret-config", "dsn"),
        ("--conninfo-secret-config", "conninfo"),
        ("--password-secret-config", "password"),
    ] {
        let raw = vec![
            "afpsql".to_string(),
            "--mode=psql".to_string(),
            flag.to_string(),
            "config.env".to_string(),
            "SECRET".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ];
        let mut state = PsqlModeState::default();
        let mut index = 2;
        assert!(parse_psql_long_arg(&raw, &mut index, &mut state).is_ok());
        let reference = match field {
            "dsn" => state.dsn_secret_config,
            "conninfo" => state.conninfo_secret_config,
            _ => state.password_secret_config,
        };
        assert_eq!(
            reference.map(|value| value.path),
            Some("SECRET".to_string())
        );
        assert_eq!(index, raw.len() - 2);

        let raw = raw_args(&[
            "afpsql",
            "--mode=psql",
            &format!("{flag}=config.env"),
            "SECRET",
            "-c",
            "select 1",
        ]);
        let mut state = PsqlModeState::default();
        let mut index = 2;
        assert!(parse_psql_long_arg(&raw, &mut index, &mut state).is_err());

        let raw = raw_args(&[
            "afpsql",
            "--mode=psql",
            flag,
            "config.env",
            "-c",
            "select 1",
        ]);
        let mut state = PsqlModeState::default();
        let mut index = 2;
        assert!(parse_psql_long_arg(&raw, &mut index, &mut state).is_err());

        let raw = raw_args(&[
            "afpsql",
            "--mode=psql",
            flag,
            "config.env",
            "SECRET",
            "extra",
            "-c",
            "select 1",
        ]);
        let mut state = PsqlModeState::default();
        let mut index = 2;
        assert!(parse_psql_long_arg(&raw, &mut index, &mut state).is_err());
    }
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
fn parse_psql_mode_stream_redirect_options_are_accepted() {
    for args in [
        vec!["--stdout-file", "/tmp/out.txt"],
        vec!["--stdout-file=/tmp/out.txt"],
        vec!["--stderr-file", "/tmp/err.txt"],
        vec!["--stderr-file=/tmp/err.txt"],
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
            assert!(matches!(req.output, OutputFormat::Json));
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
    }
}

#[test]
fn parse_psql_mode_long_output_rejects_file_paths() {
    let raw = vec![
        "afpsql".to_string(),
        "--mode".to_string(),
        "psql".to_string(),
        "-c".to_string(),
        "select 1".to_string(),
        "--output".to_string(),
        "/tmp/out.txt".to_string(),
    ];
    let err_res = parse_psql_mode(&raw);
    assert!(err_res.is_err());
    if let Err(err) = err_res {
        assert!(err.contains("invalid --output format"), "{err}");
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
