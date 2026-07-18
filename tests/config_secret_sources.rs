#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::Value;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

#[path = "support/env.rs"]
mod test_env;

const DSN_CANARY: &str = "AFPSQL_DSN_CONFIG_CANARY";
const CONNINFO_CANARY: &str = "AFPSQL_CONNINFO_CONFIG_CANARY";
const PASSWORD_CANARY: &str = "AFPSQL_PASSWORD_CONFIG_CANARY";

fn afpsql() -> &'static str {
    env!("CARGO_BIN_EXE_afpsql")
}

fn readonly() -> &'static str {
    env!("CARGO_BIN_EXE_afpsql-readonly")
}

fn temp_config(name: &str, extension: &str, content: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "afpsql-config-source-{name}-{}.{extension}",
        std::process::id()
    ));
    std::fs::write(&path, content).expect("write config source");
    path
}

fn combined(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_no_canaries(output: &Output) {
    let rendered = combined(output);
    for canary in [DSN_CANARY, CONNINFO_CANARY, PASSWORD_CANARY] {
        assert!(
            !rendered.contains(canary),
            "output leaked {canary}: {rendered}"
        );
    }
}

fn first_event(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("structured JSON event")
}

#[test]
fn canonical_config_sources_resolve_before_all_cli_paths_without_leaking() {
    let path = temp_config(
        "canonical",
        "json",
        &format!(
            r#"{{"database":{{"url":"postgresql://user:{DSN_CANARY}@127.0.0.1:1/db","conninfo":"host=127.0.0.1 port=1 user=test password={CONNINFO_CANARY}","password":"{PASSWORD_CANARY}"}}}}"#
        ),
    );
    let path_text = path.to_str().expect("utf8 path");
    for suffix in [
        vec!["--sql", "select 1"],
        vec!["inspect", "schemas"],
        vec!["--dry-run", "--sql", "select 1"],
        vec!["--explain", "--sql", "select 1"],
        vec!["--explain-analyze", "--sql", "select 1"],
    ] {
        let output = Command::new(afpsql())
            .args(["--dsn-secret-config", path_text, "database.url"])
            .args(suffix)
            .output()
            .expect("run canonical config source");
        assert!(!output.status.success());
        assert_eq!(first_event(&output)["error"]["code"], "connect_failed");
        assert_no_canaries(&output);
    }

    for (flag, dot_path) in [
        ("--conninfo-secret-config", "database.conninfo"),
        ("--password-secret-config", "database.password"),
    ] {
        let output = Command::new(afpsql())
            .args([flag, path_text, dot_path])
            .args(["--host", "127.0.0.1", "--port", "1", "--sql", "select 1"])
            .output()
            .expect("run config secret slot");
        assert!(!output.status.success());
        assert_no_canaries(&output);
    }
    std::fs::remove_file(path).expect("remove config source");
}

#[test]
fn pipe_startup_config_is_read_once_and_serializes_only_redaction() {
    let path = temp_config(
        "pipe",
        "env",
        &format!("DATABASE_URL=postgresql://user:{DSN_CANARY}@127.0.0.1:1/db\n"),
    );
    let path_text = path.to_str().expect("utf8 path");
    let mut child = Command::new(afpsql())
        .args([
            "--mode",
            "pipe",
            "--dsn-secret-config",
            path_text,
            "DATABASE_URL",
            "--log",
            "startup",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn pipe");
    use std::io::{BufRead, Read, Write};
    let stdout = child.stdout.take().expect("pipe stdout");
    let mut reader = std::io::BufReader::new(stdout);
    let mut startup = String::new();
    reader
        .read_line(&mut startup)
        .expect("read startup event after source resolution");
    assert!(startup.contains("\"event\":\"startup\""));

    std::fs::write(
        &path,
        "DATABASE_URL=postgresql://changed:ROTATED_CANARY@127.0.0.1:2/db\n",
    )
    .expect("rotate source after startup");
    child
        .stdin
        .as_mut()
        .expect("pipe stdin")
        .write_all(b"{\"code\":\"config\"}\n{\"code\":\"close\"}\n")
        .expect("write pipe requests");
    child.stdin.take();
    let status = child.wait().expect("wait pipe");
    assert!(status.success());
    let mut rest = String::new();
    reader.read_to_string(&mut rest).expect("read pipe events");
    let rendered = format!("{startup}{rest}");
    assert!(rendered.contains("\"dsn_secret\":\"***\""));
    assert!(rendered.contains(path_text));
    assert!(rendered.contains("DATABASE_URL"));
    assert!(!rendered.contains(DSN_CANARY));
    assert!(!rendered.contains("ROTATED_CANARY"));
    std::fs::remove_file(path).expect("remove config source");
}

#[test]
fn psql_translation_accepts_space_form_on_both_sides_of_command_only() {
    let path = temp_config(
        "psql",
        "env",
        &format!("DATABASE_URL=postgresql://user:{DSN_CANARY}@127.0.0.1:1/db\n"),
    );
    let path_text = path.to_str().expect("utf8 path");
    for args in [
        vec![
            "--mode=psql",
            "--dsn-secret-config",
            path_text,
            "DATABASE_URL",
            "-c",
            "select 1",
        ],
        vec![
            "--mode=psql",
            "-c",
            "select 1",
            "--dsn-secret-config",
            path_text,
            "DATABASE_URL",
        ],
    ] {
        let output = Command::new(afpsql())
            .args(args)
            .output()
            .expect("run psql translation");
        assert!(!output.status.success());
        assert_eq!(first_event(&output)["error"]["code"], "connect_failed");
        assert_no_canaries(&output);
    }

    let equals = format!("--dsn-secret-config={path_text}");
    let output = Command::new(afpsql())
        .args(["--mode=psql", &equals, "DATABASE_URL", "-c", "select 1"])
        .output()
        .expect("run rejected equals form");
    assert_eq!(output.status.code(), Some(2));
    std::fs::remove_file(path).expect("remove config source");
}

#[test]
fn ordinary_readonly_accepts_config_and_arbitrary_env_but_still_rejects_write() {
    let path = temp_config(
        "readonly",
        "yaml",
        &format!("database:\n  url: postgresql://user:{DSN_CANARY}@127.0.0.1:1/db\n"),
    );
    let path_text = path.to_str().expect("utf8 path");
    let read = Command::new(readonly())
        .args([
            "--dsn-secret-config",
            path_text,
            "database.url",
            "--sql",
            "select 1",
        ])
        .output()
        .expect("run readonly config source");
    assert_eq!(first_event(&read)["error"]["code"], "connect_failed");
    assert_no_canaries(&read);

    let write = Command::new(readonly())
        .args([
            "--dsn-secret-config",
            path_text,
            "database.url",
            "--permission",
            "write",
            "--sql",
            "select 1",
        ])
        .output()
        .expect("run readonly write attempt");
    assert!(!write.status.success());
    assert_eq!(first_event(&write)["error"]["code"], "invalid_request");
    assert_no_canaries(&write);

    let env = Command::new(readonly())
        .env(
            "CUSTOM_APPLICATION_DATABASE_URL",
            format!("postgresql://user:{DSN_CANARY}@127.0.0.1:1/db"),
        )
        .args([
            "--dsn-secret-env",
            "CUSTOM_APPLICATION_DATABASE_URL",
            "--sql",
            "select 1",
        ])
        .output()
        .expect("run arbitrary readonly env source");
    assert_eq!(first_event(&env)["error"]["code"], "connect_failed");
    assert_no_canaries(&env);
    std::fs::remove_file(path).expect("remove config source");
}

#[test]
fn config_source_errors_are_exit_two_and_never_include_source_values() {
    for (name, extension, content, path) in [
        (
            "malformed",
            "yaml",
            &format!("value: [ {DSN_CANARY}"),
            "value",
        ),
        (
            "missing",
            "json",
            &format!(r#"{{"other":"{DSN_CANARY}"}}"#),
            "value",
        ),
        (
            "type",
            "json",
            &format!(r#"{{"value":["{DSN_CANARY}"]}}"#),
            "value",
        ),
        ("empty", "json", &"{\"value\":\"\"}".to_string(), "value"),
        ("unknown", "txt", &format!("value={DSN_CANARY}"), "value"),
    ] {
        let config = temp_config(name, extension, content);
        let output = Command::new(afpsql())
            .args([
                "--dsn-secret-config",
                config.to_str().expect("utf8 path"),
                path,
                "--sql",
                "select 1",
            ])
            .output()
            .expect("run invalid config source");
        assert_eq!(
            output.status.code(),
            Some(2),
            "{name}: {}",
            combined(&output)
        );
        assert_no_canaries(&output);
        std::fs::remove_file(config).expect("remove config source");
    }

    let missing = std::env::temp_dir().join(format!(
        "afpsql-config-source-missing-file-{}.json",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&missing);
    let output = Command::new(afpsql())
        .args([
            "--dsn-secret-config",
            missing.to_str().expect("utf8 path"),
            "value",
            "--sql",
            "select 1",
        ])
        .output()
        .expect("run missing config source");
    assert_eq!(output.status.code(), Some(2));
    assert_no_canaries(&output);
}

#[test]
fn explicit_config_source_overrides_afpsql_environment_fallback() {
    let path = temp_config(
        "precedence",
        "toml",
        &format!("[database]\nurl = 'postgresql://user:{DSN_CANARY}@127.0.0.1:1/db'\n"),
    );
    let output = Command::new(afpsql())
        .env("AFPSQL_DSN_SECRET", "not-a-valid-postgresql-dsn")
        .args([
            "--dsn-secret-config",
            path.to_str().expect("utf8 path"),
            "database.url",
            "--sql",
            "select 1",
        ])
        .output()
        .expect("run source precedence");
    assert_eq!(first_event(&output)["error"]["code"], "connect_failed");
    assert_no_canaries(&output);
    std::fs::remove_file(path).expect("remove config source");
}

#[test]
fn resolved_config_secret_reaches_ssh_container_and_combined_transports() {
    let path = temp_config(
        "transports",
        "env",
        &format!("PGPASSWORD={PASSWORD_CANARY}\n"),
    );
    let path_text = path.to_str().expect("utf8 path");
    for transport in [
        vec!["--ssh", "invalid", "--ssh-option", "ConnectTimeout=1"],
        vec!["--container", "invalid", "--container-runtime", "false"],
        vec![
            "--ssh",
            "invalid",
            "--ssh-option",
            "ConnectTimeout=1",
            "--container",
            "invalid",
            "--container-runtime",
            "false",
        ],
    ] {
        let output = Command::new(afpsql())
            .args([
                "--host",
                "127.0.0.1",
                "--port",
                "1",
                "--user",
                "app",
                "--dbname",
                "app",
                "--password-secret-config",
                path_text,
                "PGPASSWORD",
                "--sql",
                "select 1",
            ])
            .args(transport)
            .output()
            .expect("run config-backed transport");
        assert!(!output.status.success());
        assert_no_canaries(&output);
        let rendered = combined(&output);
        assert!(!rendered.contains("password-secret-config"), "{rendered}");
    }
    std::fs::remove_file(path).expect("remove transport config source");
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn config_source_executes_live_canonical_psql_pipe_and_readonly_paths() {
    let dsn = test_env::required_test_dsn();
    let content = serde_json::json!({"database": {"url": dsn}}).to_string();
    let path = temp_config("live", "json", &content);
    let path_text = path.to_str().expect("utf8 path");

    for args in [
        vec!["--sql", "select 11 as n"],
        vec!["--dry-run", "--sql", "select 12 as n"],
        vec!["--explain", "--sql", "select 13 as n"],
        vec!["inspect", "schemas"],
    ] {
        let output = Command::new(afpsql())
            .args(["--dsn-secret-config", path_text, "database.url"])
            .args(args)
            .output()
            .expect("run live canonical config path");
        assert!(output.status.success(), "{}", combined(&output));
    }

    let psql = Command::new(afpsql())
        .args([
            "--mode=psql",
            "--dsn-secret-config",
            path_text,
            "database.url",
            "-c",
            "select 14 as n",
        ])
        .output()
        .expect("run live psql config path");
    assert!(psql.status.success(), "{}", combined(&psql));

    let readonly = Command::new(readonly())
        .args([
            "--dsn-secret-config",
            path_text,
            "database.url",
            "--sql",
            "select 15 as n",
        ])
        .output()
        .expect("run live readonly config path");
    assert!(readonly.status.success(), "{}", combined(&readonly));

    let mut pipe = Command::new(afpsql())
        .args([
            "--mode",
            "pipe",
            "--dsn-secret-config",
            path_text,
            "database.url",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn live pipe config path");
    use std::io::Write;
    pipe.stdin
        .as_mut()
        .expect("pipe stdin")
        .write_all(
            b"{\"code\":\"query\",\"id\":\"q\",\"sql\":\"select 16 as n\"}\n{\"code\":\"close\"}\n",
        )
        .expect("write live pipe request");
    let output = pipe.wait_with_output().expect("wait live pipe");
    assert!(output.status.success(), "{}", combined(&output));
    assert!(String::from_utf8_lossy(&output.stdout).contains("\"n\":16"));

    std::fs::remove_file(path).expect("remove live config source");
}
