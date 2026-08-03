use super::*;

fn raw_args(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_string()).collect()
}

fn registry() -> agent_first_data::BuiltCliSpec {
    match build_cli("afpsql") {
        Ok(cli) => cli,
        Err(error) => panic!("registry must build: {error}"),
    }
}

/// Resolve one argv against the registry, or report why it was rejected.
fn resolve(args: &[&str]) -> Result<ResolvedInvocation, agent_first_data::CliError> {
    match registry().resolve_from(args)? {
        CliOutcome::Run(invocation) => Ok(invocation),
        _ => panic!("{args:?} did not resolve to a run"),
    }
}

fn shape_of(args: &[&str]) -> String {
    match resolve(args) {
        Ok(invocation) => invocation.combination_id().to_string(),
        Err(error) => panic!("{args:?} was rejected: {}", error.message),
    }
}

fn rejection(args: &[&str]) -> agent_first_data::CliErrorRule {
    match registry().resolve_from(args) {
        Err(error) => error.rule,
        Ok(_) => panic!("{args:?} should have been rejected"),
    }
}

/// The session one query invocation resolves to.
fn query_session(args: &[&str]) -> SessionConfig {
    let invocation = match resolve(args) {
        Ok(invocation) => invocation,
        Err(error) => panic!("{args:?} was rejected: {}", error.message),
    };
    match run_query(&invocation) {
        Ok(Mode::Cli(request)) => request.session,
        Ok(_) => panic!("{args:?} is not a query"),
        Err(error) => panic!("{args:?} failed: {}", error.message),
    }
}

/// Why one query invocation was refused after the registry accepted its shape.
fn query_session_error(args: &[&str]) -> ParseError {
    let invocation = match resolve(args) {
        Ok(invocation) => invocation,
        Err(error) => panic!("{args:?} was rejected by the registry: {}", error.message),
    };
    match run_query(&invocation) {
        Ok(_) => panic!("{args:?} should have been rejected"),
        Err(error) => error,
    }
}

/// The psql translation on its own, which is what every translation test here
/// asserts on; the routing it also decides is exercised through the binary.
fn parse_psql_mode(raw: &[String]) -> Result<Mode, String> {
    parse_psql_mode_full(raw).map(|(mode, _)| mode)
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
    assert_eq!(
        parse_param_value("text:null"),
        Value::String("null".to_string())
    );
    assert_eq!(
        parse_param_value("text:true"),
        Value::String("true".to_string())
    );
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
fn registry_accepts_extended_inspect_subcommands() {
    for (args, shape) in [
        (
            vec!["afpsql", "inspect", "schema", "--schema", "public"],
            "inspect_schema",
        ),
        (
            vec!["afpsql", "inspect", "snapshot", "--like", "foo%"],
            "inspect_snapshot",
        ),
        (
            vec![
                "afpsql", "inspect", "indexes", "--schema", "public", "--table", "users", "--stats",
            ],
            "inspect_indexes",
        ),
        (
            vec!["afpsql", "inspect", "table", "public.users", "--full"],
            "inspect_table",
        ),
    ] {
        assert_eq!(shape_of(&args), shape);
    }
}

#[test]
fn output_format_is_taken_from_the_resolved_plan() {
    for (value, expected) in [
        ("json", OutputFormat::Json),
        ("yaml", OutputFormat::Yaml),
        ("plain", OutputFormat::Plain),
    ] {
        let invocation = match resolve(&["afpsql", "--sql", "select 1", "--output", value]) {
            Ok(invocation) => invocation,
            Err(error) => panic!("--output {value} was rejected: {}", error.message),
        };
        assert_eq!(format_of(&invocation), Ok(expected));
    }
    // The closed set lives in the output contract, so an unlisted format is
    // rejected by the parser rather than by a second check in the handler.
    assert_eq!(
        rejection(&["afpsql", "--sql", "select 1", "--output", "bad"]),
        agent_first_data::CliErrorRule::InvalidArgumentValue
    );
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
fn log_filters_accept_repetition_and_comma_lists() {
    let invocation = match resolve(&[
        "afpsql",
        "--mode",
        "pipe",
        "--log",
        "startup,query.error",
        "--log",
        "transport",
    ]) {
        Ok(invocation) => invocation,
        Err(error) => panic!("--log was rejected: {}", error.message),
    };
    let entries = log_entries(&invocation);
    assert_eq!(entries, vec!["startup", "query.error", "transport"]);
    assert!(startup_requested(&entries));
    assert_eq!(
        parse_log_categories(&entries),
        agent_first_data::LogFilters::new(["startup", "query.error", "transport"])
    );
}

#[test]
fn registry_accepts_psql_admin_subcommands() {
    let invocation = match resolve(&["afpsql", "psql", "status", "--bin-dir", "/tmp/afpsql-bin"]) {
        Ok(invocation) => invocation,
        Err(error) => panic!("psql status was rejected: {}", error.message),
    };
    assert_eq!(invocation.action_id(), "psql_status");
    assert!(matches!(
        run_psql_status(&invocation),
        Ok(Mode::PsqlAdmin(PsqlAdminRequest {
            action: PsqlAdminAction::Status { bin_dir: Some(dir) },
            ..
        })) if dir == "/tmp/afpsql-bin"
    ));
    // Connection arguments belong to the commands that connect; the wrapper
    // installer is not one of them.
    assert_eq!(
        rejection(&["afpsql", "psql", "status", "--host", "db.example"]),
        agent_first_data::CliErrorRule::UnknownArgument
    );
}

#[test]
fn registry_accepts_skill_admin_subcommands() {
    let invocation = match resolve(&[
        "afpsql",
        "skill",
        "install",
        "--agent",
        "claude-code",
        "--scope",
        "workspace",
        "--force",
    ]) {
        Ok(invocation) => invocation,
        Err(error) => panic!("skill install was rejected: {}", error.message),
    };
    assert_eq!(invocation.combination_id(), "skill-install-one-agent");
    assert!(matches!(
        run_skill_install(&invocation),
        Ok(Mode::SkillAdmin(SkillAdminRequest {
            action: SkillAdminAction::Install(SkillAdminOptions {
                agent: SkillAgentSelection::ClaudeCode,
                scope: SkillScope::Workspace,
                force: true,
                ..
            }),
            ..
        }))
    ));
}

#[test]
fn output_is_injected_into_every_command() {
    let invocation = match resolve(&["afpsql", "skill", "status", "--output", "yaml"]) {
        Ok(invocation) => invocation,
        Err(error) => panic!("skill status --output yaml was rejected: {}", error.message),
    };
    assert_eq!(format_of(&invocation), Ok(OutputFormat::Yaml));
}

#[test]
fn registry_accepts_ssh_transport_flags() {
    let _env_guard = crate::test_env::env_lock();
    let session = query_session(&[
        "afpsql",
        "--ssh",
        "user@example.com",
        "--ssh-via",
        "user@jump1",
        "--ssh-via",
        "user@jump2",
        "--ssh-option",
        "ProxyJump=bastion",
        "--ssh-remote-socket",
        "/var/run/postgresql/.s.PGSQL.5432",
        "--ssh-sudo-user",
        "postgres",
        "--sql",
        "select 1",
    ]);
    assert_eq!(session.ssh.destination.as_deref(), Some("user@example.com"));
    assert_eq!(
        session.ssh.via,
        vec!["user@jump1".to_string(), "user@jump2".to_string()]
    );
    assert_eq!(session.ssh.options, vec!["ProxyJump=bastion".to_string()]);
    assert_eq!(session.ssh.local_host, None);
    assert_eq!(session.ssh.local_port, None);
    assert_eq!(
        session.ssh.remote_socket.as_deref(),
        Some("/var/run/postgresql/.s.PGSQL.5432")
    );
    assert_eq!(session.ssh.sudo_user.as_deref(), Some("postgres"));
}

#[test]
fn registry_accepts_container_transport_flags() {
    let _env_guard = crate::test_env::env_lock();
    let session = query_session(&[
        "afpsql",
        "--container-kubectl-pod",
        "pg",
        "--container-kubectl-namespace",
        "prod",
        "--container-kubectl-context",
        "cluster-a",
        "--container-kubectl-container",
        "postgres",
        "--sql",
        "select 1",
    ]);
    assert_eq!(session.container.kubectl_pod.as_deref(), Some("pg"));
    assert_eq!(session.container.kubectl_namespace.as_deref(), Some("prod"));
    assert_eq!(
        session.container.kubectl_context.as_deref(),
        Some("cluster-a")
    );
    assert_eq!(
        session.container.kubectl_container.as_deref(),
        Some("postgres")
    );
    assert_eq!(
        session.container.selected_driver(),
        Ok(Some(crate::types::ContainerDriver::Kubectl))
    );

    let compose = query_session(&[
        "afpsql",
        "--container-compose-service",
        "db",
        "--container-compose-file",
        "compose.yml",
        "--container-compose-project",
        "demo",
        "--container-compose-user",
        "postgres",
        "--container-compose-runtime",
        "docker-compose",
        "--sql",
        "select 1",
    ]);
    assert_eq!(compose.container.compose_service.as_deref(), Some("db"));
    assert_eq!(compose.container.compose_files, vec!["compose.yml"]);
    assert_eq!(compose.container.compose_project.as_deref(), Some("demo"));
    assert_eq!(compose.container.compose_user.as_deref(), Some("postgres"));
    assert_eq!(
        compose.container.compose_runtime.as_deref(),
        Some("docker-compose")
    );
}

/// `kubectl exec` has no exec-as-user option, so there is no flag that could
/// ask for one — the impossibility is in the surface, not in a runtime check.
#[test]
fn container_kubectl_family_has_no_user_flag() {
    assert_eq!(
        rejection(&[
            "afpsql",
            "--container-kubectl-pod",
            "pg",
            "--container-kubectl-user",
            "postgres",
            "--sql",
            "select 1",
        ]),
        agent_first_data::CliErrorRule::UnknownArgument
    );
}

/// The driver is inferred from the family used, so two families name two
/// drivers and there is nothing to run.
#[test]
fn container_flag_families_cannot_be_mixed() {
    let _env_guard = crate::test_env::env_lock();
    let error = query_session_error(&[
        "afpsql",
        "--container-docker-name",
        "pg",
        "--container-kubectl-pod",
        "app",
        "--sql",
        "select 1",
    ]);
    assert_eq!(error.code, "cli_invalid_argument_value");
    assert_eq!(
        error.message,
        "--container-docker-name cannot be combined with --container-kubectl-pod; each container driver has its own flag family"
    );
}

#[test]
fn registry_accepts_ssh_plus_container_transport_flags() {
    let _env_guard = crate::test_env::env_lock();
    let session = query_session(&[
        "afpsql",
        "--ssh",
        "root@example.com",
        "--ssh-option",
        "ProxyJump=bastion",
        "--container-podman-name",
        "pg",
        "--sql",
        "select 1",
    ]);
    assert_eq!(session.ssh.destination.as_deref(), Some("root@example.com"));
    assert_eq!(session.ssh.options, vec!["ProxyJump=bastion".to_string()]);
    assert_eq!(session.container.podman_name.as_deref(), Some("pg"));
    assert_eq!(
        session.container.selected_driver(),
        Ok(Some(crate::types::ContainerDriver::Podman))
    );
}

#[test]
fn registry_accepts_permission_flag() {
    let invocation = match resolve(&[
        "afpsql",
        "--permission",
        "container-write",
        "--sql",
        "select 1",
    ]) {
        Ok(invocation) => invocation,
        Err(error) => panic!("--permission was rejected: {}", error.message),
    };
    assert_eq!(permission_of(&invocation), Some(Permission::ContainerWrite));
}

#[test]
fn short_flags_do_not_exist() {
    // The registry has no short syntax at all, so `-h` is not a rejected alias
    // of `--host` — it is simply not an argument.
    for args in [
        vec!["afpsql", "-h", "db.example", "--sql", "select 1"],
        vec!["afpsql", "-V"],
        vec!["afpsql", "-o", "yaml", "--sql", "select 1"],
    ] {
        assert_eq!(
            rejection(&args),
            agent_first_data::CliErrorRule::UnknownArgument,
            "{args:?}"
        );
    }
}

#[test]
fn sql_values_that_look_like_flags_need_the_inline_form() {
    // A value is never taken from a token that starts with `-`, so a SQL string
    // that looks like a flag is written `--sql=<value>`. That is what keeps
    // `--sql --dry-run` a missing value rather than a silently swallowed flag.
    for value in ["--mode=psql", "--explain"] {
        let invocation = match resolve(&["afpsql", &format!("--sql={value}"), "--dry-run"]) {
            Ok(invocation) => invocation,
            Err(error) => panic!("--sql={value} was rejected: {}", error.message),
        };
        assert_eq!(optional_string(&invocation, "sql").as_deref(), Some(value));
        assert!(flag(&invocation, "dry_run"));
    }
    assert_eq!(
        rejection(&["afpsql", "--sql", "--dry-run"]),
        agent_first_data::CliErrorRule::MissingArgumentValue
    );
}

#[test]
fn registry_rejects_removed_read_only_flag() {
    assert_eq!(
        rejection(&["afpsql", "--read-only", "--sql", "select 1"]),
        agent_first_data::CliErrorRule::UnknownArgument
    );
}

#[test]
fn startup_is_requested_only_by_a_filter_that_selects_it() {
    assert!(startup_requested(&split_log_entries(&[
        "startup".to_string()
    ])));
    assert!(startup_requested(&split_log_entries(&["all".to_string()])));
    assert!(!startup_requested(&split_log_entries(&[
        "query.error".to_string()
    ])));
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
        "select 1",
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
fn every_registered_shape_resolves_back_to_itself() {
    // Each generated argv must resolve to the shape it came from, so an
    // overlapping or unreachable combination fails here rather than at a
    // caller's first invocation.
    let cli = registry();
    let synthetics = cli.synthetic_invocations();
    assert!(!synthetics.is_empty(), "the registry generated no fixtures");
    for synthetic in synthetics {
        let argv = synthetic.argv.clone();
        match cli.resolve_from(argv.clone()) {
            Ok(CliOutcome::Run(invocation)) => assert_eq!(
                invocation.combination_id(),
                synthetic.combination_id,
                "{argv:?} resolved to the wrong shape"
            ),
            Ok(_) => panic!("{argv:?} did not resolve to a run"),
            Err(error) => panic!("{argv:?} failed to resolve: {}", error.message),
        }
    }
}

#[test]
fn an_ordered_stream_cannot_be_dry_run_or_bounded() {
    // `--dry-run` never streams and `--inline-max-rows` bounds the buffered
    // result a stream does not build, so neither belongs to a streaming shape.
    // Both were runtime no-ops; the registry rejects them before anything runs.
    for args in [
        vec!["afpsql", "--sql", "select 1", "--stream-rows", "--dry-run"],
        vec![
            "afpsql",
            "--sql",
            "select 1",
            "--stream-rows",
            "--inline-max-rows",
            "10",
        ],
        vec!["afpsql", "--sql", "select 1", "--batch-rows", "10"],
    ] {
        assert_eq!(
            rejection(&args),
            agent_first_data::CliErrorRule::UnregisteredCombination,
            "{args:?}"
        );
    }
}

#[test]
fn one_sql_source_and_one_mode_per_invocation() {
    assert_eq!(shape_of(&["afpsql", "--sql", "select 1"]), "query-inline");
    assert_eq!(shape_of(&["afpsql", "--sql-file", "-"]), "query-file");
    assert_eq!(shape_of(&["afpsql", "--mode", "pipe"]), "pipe");
    assert_eq!(shape_of(&["afpsql", "--mode", "psql"]), "psql-translation");
    for args in [
        // Two sources for one query, and a query source in a mode that reads
        // its requests from stdin: both were runtime checks, both are shapes.
        vec!["afpsql", "--sql", "select 1", "--sql-file", "/tmp/q.sql"],
        vec!["afpsql", "--mode", "pipe", "--sql", "select 1"],
    ] {
        assert_eq!(
            rejection(&args),
            agent_first_data::CliErrorRule::UnregisteredCombination,
            "{args:?}"
        );
    }
}

#[test]
fn help_answers_one_command_completely() {
    let cli = registry();
    let CliOutcome::Help(help) = (match cli.resolve_from(["afpsql", "inspect", "table", "--help"]) {
        Ok(outcome) => outcome,
        Err(error) => panic!("scoped help was rejected: {}", error.message),
    }) else {
        panic!("expected a help outcome");
    };
    let model = help.model();
    assert_eq!(model.schema, "cli-help-v2");
    assert_eq!(model.command_path, "afpsql inspect table");

    let [shape] = model.shapes.as_slice() else {
        panic!("inspect table has exactly one shape");
    };
    assert!(
        shape.usage.contains("afpsql inspect table <NAME>"),
        "{shape:?}"
    );
    assert!(shape.usage.contains("[--full]"), "{shape:?}");
    // Connection arguments are the command's own, so one call is the whole
    // answer: there is no second level that would only be reachable by asking
    // a parent command for what this one accepts.
    assert!(shape.usage.contains("[--dsn <SOURCE>]"), "{shape:?}");

    // A group command routes rather than running, so it advertises ready-to-run
    // next calls instead of shapes.
    let CliOutcome::Help(group) = (match cli.resolve_from(["afpsql", "inspect", "--help"]) {
        Ok(outcome) => outcome,
        Err(error) => panic!("group help was rejected: {}", error.message),
    }) else {
        panic!("expected a help outcome");
    };
    assert!(group.model().shapes.is_empty());
    assert!(
        group
            .model()
            .subcommands
            .contains(&"afpsql inspect table --help".to_string()),
        "{:?}",
        group.model().subcommands
    );
}

#[test]
fn an_unknown_command_is_named_rather_than_guessed() {
    assert_eq!(
        rejection(&["afpsql", "inspect", "nope"]),
        agent_first_data::CliErrorRule::UnknownCommand
    );
    // clap's `help` pseudo-command went with clap: `help` names no command.
    assert_eq!(
        rejection(&["afpsql", "help"]),
        agent_first_data::CliErrorRule::UnknownCommand
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
fn typed_secret_source_resolves_env_and_errors() {
    let path = std::env::var("PATH");
    assert!(path.is_ok());
    if let Ok(path) = path {
        let source = TypedSecretSource::Env("PATH".to_string());
        assert_eq!(source.resolve("--dsn"), Ok(path));
    }

    let missing_name = format!("AFPSQL_TEST_MISSING_{}", std::process::id());
    let missing = TypedSecretSource::Env(missing_name).resolve("--dsn");
    assert!(missing.is_err());
}

#[test]
fn a_config_source_is_one_file_and_one_dot_path() {
    for flag in ["--dsn", "--conninfo", "--password"] {
        let source =
            TypedSecretSource::parse(flag, Some("file:config.yaml#database.url".to_string()))
                .expect("typed file source")
                .expect("source");
        let TypedSecretSource::File(reference) = source else {
            panic!("expected file source")
        };
        assert_eq!(reference.file, std::path::PathBuf::from("config.yaml"));
        assert_eq!(reference.path, "database.url");
        assert!(TypedSecretSource::parse(flag, Some("file:config.yaml".to_string())).is_err());
    }
}

#[test]
fn one_slot_is_one_typed_secret_source() {
    assert!(matches!(
        TypedSecretSource::parse("--dsn", Some("env:DATABASE_URL".to_string())),
        Ok(Some(TypedSecretSource::Env(name))) if name == "DATABASE_URL"
    ));
    assert!(matches!(
        TypedSecretSource::parse("--dsn", Some("postgresql://db/app".to_string())),
        Ok(Some(TypedSecretSource::Literal(value))) if value == "postgresql://db/app"
    ));
    assert!(TypedSecretSource::parse("--dsn", Some("env:".to_string())).is_err());
}

#[test]
fn psql_mode_takes_the_same_typed_secret_sources() {
    for (flag, slot) in [("--dsn", "dsn"), ("--conninfo", "conninfo")] {
        let source = "file:config.env#SECRET";
        let raw = raw_args(&["afpsql", "--mode=psql", flag, source, "-c", "select 1"]);
        let mut state = PsqlModeState::default();
        let mut index = 2;
        assert!(parse_psql_long_arg(&raw, &mut index, &mut state).is_ok());
        let value = match slot {
            "dsn" => state.dsn_secret,
            _ => state.conninfo_secret,
        };
        assert_eq!(value.as_deref(), Some(source));
        assert_eq!(index, raw.len() - 2);
    }
    let raw = raw_args(&[
        "afpsql",
        "--mode=psql",
        "--password=env:PGPASSWORD",
        "-c",
        "select 1",
    ]);
    let mut state = PsqlModeState::default();
    let mut index = 2;
    assert!(parse_psql_long_arg(&raw, &mut index, &mut state).is_ok());
    assert_eq!(state.password_secret.as_deref(), Some("env:PGPASSWORD"));
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
fn parse_psql_mode_accepts_typed_env_sources() {
    let path = std::env::var("PATH");
    assert!(path.is_ok());
    let raw = vec![
        "afpsql".to_string(),
        "--mode".to_string(),
        "psql".to_string(),
        "-c".to_string(),
        "select 1".to_string(),
        "--dsn".to_string(),
        "env:PATH".to_string(),
        "--password=env:PATH".to_string(),
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
        "--conninfo".to_string(),
        "host=localhost user=roger dbname=postgres".to_string(),
        "-v".to_string(),
        "1=7".to_string(),
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
            assert!(matches!(req.output, OutputFormat::Json));
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
        "--dsn".to_string(),
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
    let _env_guard = crate::test_env::env_lock();
    let raw = vec![
        "afpsql".to_string(),
        "--mode".to_string(),
        "psql".to_string(),
        "--container-compose-service".to_string(),
        "pg".to_string(),
        "--container-compose-file".to_string(),
        "compose.yml".to_string(),
        "--container-compose-project".to_string(),
        "demo".to_string(),
        "--container-compose-user".to_string(),
        "postgres".to_string(),
        "-c".to_string(),
        "select 1".to_string(),
    ];
    let mode_res = parse_psql_mode(&raw);
    assert!(mode_res.is_ok());
    if let Ok(Mode::Cli(req)) = mode_res {
        assert_eq!(req.session.container.compose_service.as_deref(), Some("pg"));
        assert_eq!(
            req.session.container.compose_files,
            vec!["compose.yml".to_string()]
        );
        assert_eq!(
            req.session.container.compose_project.as_deref(),
            Some("demo")
        );
        assert_eq!(
            req.session.container.compose_user.as_deref(),
            Some("postgres")
        );
        assert_eq!(req.options.permission, Some(Permission::ContainerWrite));
    }
}

#[test]
fn parse_psql_mode_rejects_mixed_container_flag_families() {
    let _env_guard = crate::test_env::env_lock();
    let raw = vec![
        "afpsql".to_string(),
        "--mode".to_string(),
        "psql".to_string(),
        "--container-docker-name".to_string(),
        "pg".to_string(),
        "--container-kubectl-pod".to_string(),
        "app".to_string(),
        "-c".to_string(),
        "select 1".to_string(),
    ];
    assert_eq!(
        parse_psql_mode(&raw).err(),
        Some(
            "--container-docker-name cannot be combined with --container-kubectl-pod; each container driver has its own flag family"
                .to_string()
        )
    );
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
fn parse_psql_mode_rejects_output_flags() {
    for args in [
        vec!["--output", "json"],
        vec!["--output=json"],
        vec!["-o", "/tmp/out.txt"],
    ] {
        let mut raw = vec![
            "afpsql".to_string(),
            "--mode".to_string(),
            "psql".to_string(),
            "-c".to_string(),
            "select 1".to_string(),
        ];
        raw.extend(args.iter().map(|value| value.to_string()));
        let err_res = parse_psql_mode(&raw);
        assert!(err_res.is_err(), "{args:?} unexpectedly parsed");
        if let Err(err) = err_res {
            assert!(
                err.contains("unsupported psql-mode argument"),
                "{args:?}: {err}"
            );
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
