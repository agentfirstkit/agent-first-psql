#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::Value;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[path = "support/env.rs"]
mod test_env;

fn bin() -> PathBuf {
    let exe = std::env::current_exe().expect("current exe");
    let debug_dir = exe
        .parent()
        .and_then(|p| p.parent())
        .expect("target debug dir");
    debug_dir.join("afpsql")
}

fn test_dsn() -> String {
    test_env::required_test_dsn()
}

fn run(mut cmd: Command) -> (i32, String, String) {
    let out = cmd.output().expect("run command");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn psql_mode_all_translation_paths() {
    let path = std::env::temp_dir().join(format!("afpsql_cov_{}.sql", std::process::id()));
    std::fs::write(&path, "select $1::int as n").expect("write temp sql");

    let mut cmd = Command::new(bin());
    cmd.arg("--mode")
        .arg("psql")
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("-f")
        .arg(path.to_string_lossy().to_string())
        .arg("-v")
        .arg("1=9")
        .arg("--output")
        .arg("json");
    let (code, stdout, _stderr) = run(cmd);
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(&stdout).expect("json output");
    agent_first_data::validate_protocol_event(&v, true).expect("strict AFDATA event");
    assert_eq!(v["result"]["rows"][0]["n"], 9);

    let mut bad = Command::new(bin());
    bad.arg("--mode").arg("psql").arg("--bad");
    let (code, stdout, _) = run(bad);
    assert_eq!(code, 2);
    assert!(stdout.contains("unsupported psql-mode argument"));

    let _ = std::fs::remove_file(path);
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn pipe_config_full_patch_and_close() {
    let payload = serde_json::json!({
        "code":"config",
        "default_session":"s1",
        "inline_max_rows":11,
        "inline_max_bytes":22,
        "statement_timeout_ms":33,
        "lock_timeout_ms":44,
        "log":["x"],
        "sessions": {
            "s1": {
                "dsn_secret": test_dsn(),
                "conninfo_secret": "host=localhost user=roger dbname=postgres",
                "host": "localhost",
                "port": 5432,
                "user": "roger",
                "dbname": "postgres",
                "password_secret": "pw"
            }
        }
    })
    .to_string()
        + "\n"
        + &serde_json::json!({"code":"close"}).to_string()
        + "\n";

    let mut child = Command::new(bin())
        .arg("--mode")
        .arg("pipe")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write");
    let out = child.wait_with_output().expect("wait");
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).expect("utf8");
    assert!(text.contains("\"code\":\"config\""));
    assert!(text.contains("\"default_session\":\"s1\""));
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn conn_via_env_fallback() {
    let mut cmd = Command::new(bin());
    cmd.arg("--sql")
        .arg("select 1 as n")
        .env("AFPSQL_DSN_SECRET", test_dsn());
    let (code, stdout, _stderr) = run(cmd);
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(&stdout).expect("json output");
    agent_first_data::validate_protocol_event(&v, true).expect("strict AFDATA event");
    assert_eq!(v["result"]["rows"][0]["n"], 1);
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn has_session_override_each_field_in_pipe_mode() {
    for args in [
        vec!["--dsn-secret", &test_dsn()],
        vec![
            "--conninfo-secret",
            "host=localhost user=roger dbname=postgres",
        ],
        vec!["--host", "localhost"],
        vec!["--port", "5432"],
        vec!["--user", "roger"],
        vec!["--dbname", "postgres"],
        vec!["--password-secret", "pw"],
        vec!["--container", "pg"],
    ] {
        let payload = serde_json::json!({"code":"close"}).to_string() + "\n";
        let mut cmd = Command::new(bin());
        cmd.arg("--mode").arg("pipe");
        cmd.args(args);
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn");
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(payload.as_bytes())
            .expect("write");
        let out = child.wait_with_output().expect("wait");
        assert!(out.status.success());
    }
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn cli_emits_structured_stdout_log_events_when_enabled() {
    let mut cmd = Command::new(bin());
    cmd.arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--log")
        .arg("query.result")
        .arg("--sql")
        .arg("select 1 as n");
    let (code, stdout, stderr) = run(cmd);
    assert_eq!(code, 0);
    assert!(stdout.contains("\"kind\":\"result\""));
    assert!(stdout.contains("\"kind\":\"log\""));
    assert!(stdout.contains("\"event\":\"query.result\""));
    assert!(!stdout.contains("\"event\":\"startup\""));
    assert!(stdout.contains("\"duration_ms\""));
    assert!(stderr.trim().is_empty());
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn handler_param_types_and_empty_rows() {
    let mut cmd = Command::new(bin());
    cmd.arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--sql")
        .arg("select $1::text as a, $2::boolean as b, $3::double precision as c, $4::jsonb as d, $5::jsonb as e")
        .arg("--param")
        .arg("1=NaN")
        .arg("--param")
        .arg("2=true")
        .arg("--param")
        .arg("3=1.25")
        .arg("--param")
        .arg("4=[1,2]")
        .arg("--param")
        .arg("5={\"x\":1}");
    let (code, stdout, _stderr) = run(cmd);
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(&stdout).expect("json output");
    agent_first_data::validate_protocol_event(&v, true).expect("strict AFDATA event");
    assert_eq!(v["kind"], "result");

    let mut empty = Command::new(bin());
    empty
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--sql")
        .arg("select 1 as n where false");
    let (code, stdout, _stderr) = run(empty);
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(&stdout).expect("json output");
    assert_eq!(
        v["result"]["columns"]
            .as_array()
            .map(|columns| columns.len())
            .unwrap_or(0),
        1
    );
    assert_eq!(v["result"]["columns"][0]["name"], "n");
}
