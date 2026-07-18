#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::Value;
use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[path = "support/env.rs"]
mod test_env;

fn readonly() -> &'static str {
    env!("CARGO_BIN_EXE_afpsql-readonly")
}

fn readwrite() -> &'static str {
    env!("CARGO_BIN_EXE_afpsql")
}

fn error(output: std::process::Output) -> Value {
    assert!(!output.status.success());
    serde_json::from_slice(&output.stdout).expect("structured error JSON")
}

fn temp_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("afpsql-readonly-{name}-{}", std::process::id()))
}

fn dsn_with_credentials(dsn: &str, user: &str, password: &str) -> String {
    let Some((scheme, rest)) = dsn.split_once("://") else {
        return dsn.to_string();
    };
    let endpoint = rest.rsplit_once('@').map_or(rest, |(_, endpoint)| endpoint);
    format!("{scheme}://{user}:{password}@{endpoint}")
}

#[test]
fn readonly_rejects_every_write_permission_before_connecting() {
    for permission in ["write", "ssh-write", "container-write"] {
        for args in [
            ["--permission", permission, "--sql", "select 1"],
            ["--sql", "select 1", "--permission", permission],
        ] {
            let value = error(
                Command::new(readonly())
                    .args(args)
                    .output()
                    .expect("run readonly binary"),
            );
            assert_eq!(value["kind"], "error");
            assert_eq!(value["error"]["code"], "invalid_request");
            assert!(
                value["error"]["hint"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("afpsql`")
            );
        }
    }
}

#[test]
fn readonly_rejects_psql_and_psql_admin() {
    let mode = error(
        Command::new(readonly())
            .args(["--mode", "psql", "-c", "select 1"])
            .output()
            .expect("run readonly binary"),
    );
    assert_eq!(mode["error"]["code"], "invalid_request");
    assert!(
        mode["error"]["hint"]
            .as_str()
            .unwrap_or_default()
            .contains("`afpsql`")
    );

    for action in ["status", "install", "uninstall"] {
        let admin = error(
            Command::new(readonly())
                .args(["psql", action])
                .output()
                .expect("run readonly binary"),
        );
        assert_eq!(admin["error"]["code"], "invalid_request");
        assert!(
            admin["error"]["hint"]
                .as_str()
                .unwrap_or_default()
                .contains("afpsql psql")
        );
    }
}

#[test]
fn readonly_allows_skill_admin_redirects_and_sql_files() {
    let skills_dir = temp_path("skills");
    fs::create_dir_all(&skills_dir).expect("create skills dir");
    for action in ["status", "install", "uninstall"] {
        let output = Command::new(readonly())
            .args([
                "skill",
                action,
                "--agent",
                "codex",
                "--skills-dir",
                skills_dir.to_str().expect("utf8 skills dir"),
            ])
            .output()
            .expect("run readonly skill admin");
        assert!(output.status.success(), "action {action}: {output:?}");
    }
    fs::remove_dir_all(&skills_dir).expect("remove skills dir");

    let output_path = temp_path("redirect");
    fs::write(&output_path, "preserve me").expect("seed output path");
    let output = Command::new(readonly())
        .args([
            "--stdout-file",
            output_path.to_str().expect("utf8 path"),
            "--sql",
            "select 1",
        ])
        .output()
        .expect("run readonly redirect");
    assert!(!output.status.success());
    let redirected = fs::read_to_string(&output_path).expect("read output path");
    assert!(redirected.contains("connect_failed"));
    assert!(!redirected.contains("unavailable in afpsql-readonly"));
    fs::remove_file(&output_path).expect("remove output path");

    let sql_path = temp_path("query.sql");
    fs::write(&sql_path, "select 1").expect("seed SQL path");
    let value = error(
        Command::new(readonly())
            .args(["--sql-file", sql_path.to_str().expect("utf8 path")])
            .output()
            .expect("run readonly SQL file"),
    );
    assert_eq!(value["error"]["code"], "connect_failed");
    fs::remove_file(&sql_path).expect("remove SQL path");
}

#[test]
fn readonly_stream_redirect_scanner_keeps_ordinary_entrypoint_semantics() {
    // The stream-redirect installer scans raw argv independently of the CLI
    // parser. Ordinary readonly intentionally grants the same host redirect
    // capability as afpsql, even when the later CLI parse fails.
    for args in [
        vec!["--sql", "--stdout-file=SMUGGLE"],
        vec!["--sql", "--stdout-file", "SMUGGLE"],
        vec!["--param", "x", "--stderr-file=SMUGGLE"],
    ] {
        let target = temp_path("smuggled-redirect");
        let _ = fs::remove_file(&target);
        let target_text = target.to_str().expect("utf8 path");
        let resolved: Vec<String> = args
            .iter()
            .map(|arg| arg.replace("SMUGGLE", target_text))
            .collect();
        let output = Command::new(readonly())
            .args(&resolved)
            .output()
            .expect("run readonly redirect-like value");
        assert!(!output.status.success());
        assert!(
            target.exists(),
            "redirect scanner did not create a file for {resolved:?}"
        );
        fs::remove_file(target).expect("remove redirect target");
    }
}

#[test]
fn readonly_allows_custom_transport_capabilities_before_database_boundary() {
    for option in ["ProxyCommand=false", "LocalCommand=false"] {
        let value = error(
            Command::new(readonly())
                .args(["--ssh-option", option, "--ssh", "invalid"])
                .args(["--sql", "select 1"])
                .output()
                .expect("run readonly transport override"),
        );
        assert_ne!(value["error"]["code"], "invalid_request");
    }

    #[cfg(unix)]
    {
        let runtime = temp_path("custom-runtime");
        fs::write(&runtime, "#!/bin/sh\nexit 1\n").expect("write custom runtime");
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700))
            .expect("make custom runtime executable");
        let value = error(
            Command::new(readonly())
                .args([
                    "--container-runtime",
                    runtime.to_str().expect("utf8 runtime path"),
                    "--container",
                    "invalid",
                    "--sql",
                    "select 1",
                ])
                .output()
                .expect("run readonly custom runtime"),
        );
        assert_ne!(value["error"]["code"], "invalid_request");
        fs::remove_file(runtime).expect("remove custom runtime");
    }
}

#[cfg(unix)]
#[test]
fn locked_profile_executable_rejects_connection_overrides_before_profile_loading() {
    use std::os::unix::fs::symlink;

    let alias = temp_path("bin").join("afpsql-readonly-production");
    fs::create_dir_all(alias.parent().expect("alias parent")).expect("create alias directory");
    symlink(readonly(), &alias).expect("create locked profile alias");
    let value = error(
        Command::new(&alias)
            .args(["--host", "attacker.example", "--sql", "select 1"])
            .output()
            .expect("run locked profile alias"),
    );
    assert_eq!(value["error"]["code"], "invalid_request");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("cannot override")
    );
    fs::remove_file(&alias).expect("remove alias");
    fs::remove_dir(alias.parent().expect("alias parent")).expect("remove alias directory");
}

#[test]
fn readonly_pipe_rejects_write_begin_and_query() {
    let payload = concat!(
        r#"{"code":"begin","id":"b","read_only":false,"permission":"write"}"#,
        "\n",
        r#"{"code":"query","id":"q","sql":"select 1","options":{"permission":"write"}}"#,
        "\n",
        r#"{"code":"close"}"#,
        "\n",
    );
    let mut child = Command::new(readonly())
        .args(["--mode", "pipe"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn readonly pipe");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write input");
    let output = child.wait_with_output().expect("wait");
    assert!(output.status.success());
    let events: Vec<Value> = String::from_utf8(output.stdout)
        .expect("utf8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("json event"))
        .collect();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["error"]["code"] == "invalid_request")
            .count(),
        2
    );
}

#[test]
fn readonly_pipe_accepts_transport_config_patches_without_write_escalation() {
    let payload = concat!(
        r#"{"code":"config","sessions":{"default":{"container":"pg","container_runtime":"custom"}}}"#,
        "\n",
        r#"{"code":"config","sessions":{"default":{"ssh":"db","ssh_options":["ProxyCommand=false"]}}}"#,
        "\n",
        r#"{"code":"close"}"#,
        "\n",
    );
    let mut child = Command::new(readonly())
        .args(["--mode", "pipe"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn readonly pipe");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write input");
    let output = child.wait_with_output().expect("wait");
    assert!(output.status.success());
    let events = String::from_utf8(output.stdout).expect("UTF-8 events");
    assert_eq!(events.matches(r#""code":"config""#).count(), 2);
    assert_eq!(events.matches(r#""code":"invalid_request""#).count(), 0);
}

#[test]
fn readonly_dry_run_rejects_transaction_control_before_connecting() {
    let value = error(
        Command::new(readonly())
            .args(["--dry-run", "--sql", "commit"])
            .output()
            .expect("run readonly dry-run"),
    );
    assert_eq!(value["error"]["code"], "invalid_request");
}

#[test]
fn readonly_help_uses_actual_binary_name() {
    let output = Command::new(readonly())
        .arg("--help")
        .output()
        .expect("run help");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("afpsql-readonly"));
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn readonly_executes_reads_and_database_rejects_unpermitted_dml() {
    let dsn = test_env::required_test_dsn();
    let read = Command::new(readonly())
        .args(["--dsn-secret", &dsn, "--sql", "select 1 as n"])
        .output()
        .expect("run readonly query");
    assert!(
        read.status.success(),
        "stdout: {}",
        String::from_utf8_lossy(&read.stdout)
    );
    let result: Value = serde_json::from_slice(&read.stdout).expect("result JSON");
    assert_eq!(result["kind"], "result");
    assert_eq!(result["result"]["rows"][0]["n"], 1);

    let sql_path = temp_path("live-query.sql");
    fs::write(&sql_path, "select 2 as n").expect("write live SQL file");
    let file_read = Command::new(readonly())
        .args([
            "--dsn-secret",
            &dsn,
            "--sql-file",
            sql_path.to_str().expect("utf8 SQL path"),
        ])
        .output()
        .expect("run readonly SQL file query");
    assert!(file_read.status.success());
    let file_result: Value =
        serde_json::from_slice(&file_read.stdout).expect("SQL file result JSON");
    assert_eq!(file_result["result"]["rows"][0]["n"], 2);
    fs::remove_file(sql_path).expect("remove live SQL file");

    let dml = Command::new(readonly())
        .args([
            "--dsn-secret",
            &dsn,
            "--sql",
            "create temp table afpsql_readonly_should_fail(n int)",
        ])
        .output()
        .expect("run readonly DDL");
    let error = error(dml);
    assert_eq!(error["error"]["code"], "sql_error");
    assert_eq!(error["error"]["sqlstate"], "25006");
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn readonly_explain_analyze_cannot_bypass_write_permission() {
    let dsn = test_env::required_test_dsn();
    let output = Command::new(readonly())
        .args([
            "--dsn-secret",
            &dsn,
            "--explain-analyze",
            "--permission",
            "write",
            "--sql",
            "insert into pg_temp.no_such_table values (1)",
        ])
        .output()
        .expect("run explain analyze");
    let error = error(output);
    assert_eq!(error["error"]["code"], "invalid_request");
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn readonly_database_transaction_and_side_effect_boundaries_are_explicit() {
    let dsn = test_env::required_test_dsn();
    let suffix = std::process::id();
    let table = format!("afpsql_ro_effect_table_{suffix}");
    let sequence = format!("afpsql_ro_effect_seq_{suffix}");
    let function = format!("afpsql_ro_effect_fn_{suffix}");
    for setup in [
        format!("create table {table}(n int)"),
        format!("create sequence {sequence}"),
        format!(
            "create function {function}() returns int language plpgsql security definer as \
             $$ begin insert into {table} values (1); return 1; end $$"
        ),
    ] {
        let setup_output = Command::new(readwrite())
            .args([
                "--dsn-secret",
                &dsn,
                "--permission",
                "write",
                "--sql",
                &setup,
            ])
            .output()
            .expect("set up readonly boundary fixture");
        assert!(
            setup_output.status.success(),
            "setup {setup} stdout: {}",
            String::from_utf8_lossy(&setup_output.stdout)
        );
    }

    for sql in [
        "set transaction read write".to_string(),
        "commit".to_string(),
        "rollback".to_string(),
        "savepoint agent_attempt".to_string(),
        "begin".to_string(),
    ] {
        let output = Command::new(readonly())
            .args(["--dsn-secret", &dsn, "--sql", &sql])
            .output()
            .expect("run readonly transaction-control query");
        let value = error(output);
        assert_eq!(
            value["error"]["code"], "invalid_request",
            "unexpected result for {sql}: {value}"
        );
    }

    for sql in [
        "create temp table afpsql_ro_temp(n int)".to_string(),
        format!("select nextval('{sequence}')"),
        format!("select {function}()"),
    ] {
        let output = Command::new(readonly())
            .args(["--dsn-secret", &dsn, "--sql", &sql])
            .output()
            .expect("run readonly boundary query");
        assert!(
            !output.status.success(),
            "unexpected success for {sql}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        let value: Value = serde_json::from_slice(&output.stdout).expect("boundary error JSON");
        assert_eq!(
            value["error"]["code"], "sql_error",
            "unexpected result for {sql}: {value}"
        );
    }

    // PostgreSQL permits these operations in a READ ONLY transaction. They are
    // intentionally documented as role/resource-governance concerns rather
    // than being hidden behind a fragile SQL keyword filter.
    for sql in [
        "notify afpsql_readonly_test",
        "select pg_advisory_xact_lock(424242)",
    ] {
        let output = Command::new(readonly())
            .args(["--dsn-secret", &dsn, "--sql", sql])
            .output()
            .expect("run allowed readonly side effect");
        assert!(
            output.status.success(),
            "{sql} stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    let check = Command::new(readonly())
        .args([
            "--dsn-secret",
            &dsn,
            "--sql",
            &format!("select count(*)::int as n from {table}"),
        ])
        .output()
        .expect("check function side effect");
    assert!(check.status.success());
    let check: Value = serde_json::from_slice(&check.stdout).expect("check JSON");
    assert_eq!(check["result"]["rows"][0]["n"], 0);

    for cleanup in [
        format!("drop function if exists {function}()"),
        format!("drop sequence if exists {sequence}"),
        format!("drop table if exists {table}"),
    ] {
        let cleanup_output = Command::new(readwrite())
            .args([
                "--dsn-secret",
                &dsn,
                "--permission",
                "write",
                "--sql",
                &cleanup,
            ])
            .output()
            .expect("clean up readonly boundary fixture");
        assert!(cleanup_output.status.success());
    }
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn dedicated_reader_role_blocks_writes_through_full_afpsql() {
    let dsn = test_env::required_test_dsn();
    let suffix = std::process::id();
    let role = format!("afpsql_reader_{suffix}");
    let password = format!("reader_pw_{suffix}");
    let table = format!("afpsql_reader_table_{suffix}");
    for setup in [
        format!(
            "create role {role} login password '{password}' \
             nosuperuser nocreatedb nocreaterole noreplication nobypassrls"
        ),
        format!("create table {table}(n int)"),
        format!("grant select on {table} to {role}"),
    ] {
        let output = Command::new(readwrite())
            .args([
                "--dsn-secret",
                &dsn,
                "--permission",
                "write",
                "--sql",
                &setup,
            ])
            .output()
            .expect("set up reader role fixture");
        assert!(
            output.status.success(),
            "setup {setup}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    let reader_dsn = dsn_with_credentials(&dsn, &role, &password);
    let read = Command::new(readwrite())
        .args([
            "--dsn-secret",
            &reader_dsn,
            "--sql",
            &format!("select count(*)::int as n from {table}"),
        ])
        .output()
        .expect("read through dedicated reader role");
    assert!(
        read.status.success(),
        "reader select: {}",
        String::from_utf8_lossy(&read.stdout)
    );

    let write = Command::new(readwrite())
        .args([
            "--dsn-secret",
            &reader_dsn,
            "--permission",
            "write",
            "--sql",
            &format!("insert into {table} values (1)"),
        ])
        .output()
        .expect("attempt write through dedicated reader role");
    let write = error(write);
    assert_eq!(write["error"]["code"], "sql_error");
    assert_eq!(write["error"]["sqlstate"], "42501");

    for cleanup in [
        format!("drop table if exists {table}"),
        format!("drop role if exists {role}"),
    ] {
        let output = Command::new(readwrite())
            .args([
                "--dsn-secret",
                &dsn,
                "--permission",
                "write",
                "--sql",
                &cleanup,
            ])
            .output()
            .expect("clean up reader role fixture");
        assert!(output.status.success());
    }
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn readonly_pipe_keeps_read_only_lifecycle_available() {
    let dsn = test_env::required_test_dsn();
    let payload = concat!(
        r#"{"code":"query","id":"default","sql":"select 1 as n"}"#,
        "\n",
        r#"{"code":"query","id":"explicit","sql":"select 2 as n","options":{"permission":"read"}}"#,
        "\n",
        r#"{"code":"begin","id":"begin","read_only":true}"#,
        "\n",
        r#"{"code":"query","id":"control","sql":"commit"}"#,
        "\n",
        r#"{"code":"query","id":"in_tx","sql":"select 3 as n"}"#,
        "\n",
        r#"{"code":"commit","id":"commit"}"#,
        "\n",
        r#"{"code":"query","id":"after_tx","sql":"select 4 as n"}"#,
        "\n",
        r#"{"code":"close"}"#,
        "\n",
    );
    let mut child = Command::new(readonly())
        .args(["--mode", "pipe", "--dsn-secret", &dsn])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn readonly pipe");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write input");
    let output = child.wait_with_output().expect("wait");
    assert!(output.status.success());
    let events: Vec<Value> = String::from_utf8(output.stdout)
        .expect("utf8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("JSON event"))
        .collect();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["error"]["code"] == "invalid_request")
            .count(),
        1
    );
    for value in [1, 2, 3, 4] {
        assert!(
            events
                .iter()
                .any(|event| event["result"]["rows"][0]["n"] == value)
        );
    }
}
