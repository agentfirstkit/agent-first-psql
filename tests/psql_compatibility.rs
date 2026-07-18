#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[path = "support/env.rs"]
mod test_env;

fn test_dsn() -> String {
    test_env::required_test_dsn()
}

fn bin() -> PathBuf {
    let exe = std::env::current_exe().expect("current exe");
    let debug_dir = exe
        .parent()
        .and_then(|p| p.parent())
        .expect("target debug dir");
    debug_dir.join("afpsql")
}

fn temp_path(name: &str) -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("afpsql_{name}_{}_{}", std::process::id(), suffix))
}

fn assert_strict_event(value: &Value) {
    agent_first_data::validate_protocol_event(value, true).expect("strict AFDATA event");
}

#[test]
fn psql_mode_help_and_version_flags_are_accepted_without_database() {
    for args in [
        vec!["--mode", "psql", "--version"],
        vec!["--mode", "psql", "-V"],
        vec!["--mode", "psql", "--help"],
        vec!["--mode", "psql", "--help=commands"],
        vec!["--mode", "psql", "-?"],
    ] {
        let out = Command::new(bin())
            .args(args.clone())
            .output()
            .expect("run afpsql");
        assert!(out.status.success(), "{args:?} should exit successfully");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("psql (afpsql wrapper)"),
            "{args:?} should print psql-compatible wrapper help/version"
        );
        assert!(String::from_utf8_lossy(&out.stderr).trim().is_empty());
    }
}

#[test]
fn psql_mode_interactive_usage_reports_structured_hint_on_stdout() {
    for args in [
        vec!["--mode", "psql"],
        vec!["--mode", "psql", "-W", "-c", "select 1"],
        vec!["--mode", "psql", "--password", "-c", "select 1"],
        vec!["--mode", "psql", "-s", "-c", "select 1"],
        vec!["--mode", "psql", "--single-step", "-c", "select 1"],
        vec!["--mode", "psql", "-S", "-c", "select 1"],
        vec!["--mode", "psql", "--single-line", "-c", "select 1"],
    ] {
        let out = Command::new(bin())
            .args(args.clone())
            .output()
            .expect("run afpsql");
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(String::from_utf8_lossy(&out.stderr).trim().is_empty());
        let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
        assert_strict_event(&v);
        assert_eq!(v["kind"], "error");
        assert_eq!(v["error"]["code"], "invalid_request");
        assert!(
            v["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("unsupported psql mode")
        );
        assert!(
            v["error"]["hint"]
                .as_str()
                .unwrap_or_default()
                .contains("original psql binary directly")
        );
    }
}

#[test]
fn psql_mode_stdout_and_stderr_files_redirect_process_streams() {
    let out_path = temp_path("psql_output");
    let err_path = temp_path("psql_error");

    let out = Command::new(bin())
        .arg("--mode")
        .arg("psql")
        .arg("--dsn-secret")
        .arg("postgresql://127.0.0.1:1/postgres")
        .arg("--stdout-file")
        .arg(&out_path)
        .arg("--stderr-file")
        .arg(&err_path)
        .arg("-c")
        .arg("select 1")
        .output()
        .expect("run afpsql");

    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).trim().is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).trim().is_empty());
    let output_text = std::fs::read_to_string(&out_path).expect("read output file");
    let stderr_text = std::fs::read_to_string(&err_path).expect("read stderr file");
    let v: Value = serde_json::from_str(&output_text).expect("json output file");
    assert_strict_event(&v);
    assert_eq!(v["kind"], "error");
    assert!(stderr_text.trim().is_empty());

    let _ = std::fs::remove_file(out_path);
    let _ = std::fs::remove_file(err_path);
}

#[test]
fn ssh_transport_validation_reports_structured_error_on_stdout() {
    let out = Command::new(bin())
        .arg("--ssh")
        .arg("user@example.invalid")
        .arg("--dsn-secret")
        .arg("postgresql://127.0.0.1/postgres")
        .arg("--sql")
        .arg("select 1")
        .output()
        .expect("run afpsql");

    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).trim().is_empty());
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
    assert_strict_event(&v);
    assert_eq!(v["kind"], "error");
    assert_eq!(v["error"]["code"], "connect_failed");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("SSH transport currently supports discrete connection fields")
    );
    assert!(
        v["error"]["hint"]
            .as_str()
            .unwrap_or_default()
            .contains("--ssh")
    );
}

#[test]
fn ssh_sudo_bridge_requires_explicit_socket_with_hint() {
    let out = Command::new(bin())
        .arg("--ssh")
        .arg("user@example.invalid")
        .arg("--ssh-sudo-user")
        .arg("postgres")
        .arg("--user")
        .arg("postgres")
        .arg("--dbname")
        .arg("postgres")
        .arg("--sql")
        .arg("select 1")
        .output()
        .expect("run afpsql");

    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).trim().is_empty());
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
    assert_strict_event(&v);
    assert_eq!(v["kind"], "error");
    assert_eq!(v["error"]["code"], "connect_failed");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("explicit remote PostgreSQL Unix socket")
    );
    let hint = v["error"]["hint"].as_str().unwrap_or_default();
    assert!(hint.contains("--ssh-remote-socket"));
    assert!(hint.contains("--host/PGHOST"));
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn psql_mode_translates_supported_cli_flags() {
    let out = Command::new(bin())
        .arg("--mode")
        .arg("psql")
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("-c")
        .arg("select $1::int as n")
        .arg("-v")
        .arg("1=7")
        .output()
        .expect("run afpsql");
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
    assert_strict_event(&v);
    assert_eq!(v["kind"], "result");
    assert_eq!(v["result"]["rows"][0]["n"], 7);
}

#[test]
fn psql_mode_rejects_unsupported_set_flag_without_database() {
    let unsupported = Command::new(bin())
        .arg("--mode")
        .arg("psql")
        .arg("--set")
        .arg("ON_ERROR_STOP=1")
        .output()
        .expect("run afpsql");
    assert_eq!(unsupported.status.code(), Some(2));
    let v: Value = serde_json::from_slice(&unsupported.stdout).expect("json output");
    assert_strict_event(&v);
    assert_eq!(v["error"]["code"], "invalid_request");
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn psql_mode_keeps_write_compatible_default() {
    let out = Command::new(bin())
        .arg("--mode")
        .arg("psql")
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("-c")
        .arg("create temp table afpsql_psql_write_default(n int)")
        .output()
        .expect("run afpsql");
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
    assert_strict_event(&v);
    assert_eq!(v["kind"], "result");
}

#[test]
fn afd_mode_rejects_psql_short_flags() {
    let out = Command::new(bin())
        .arg("-c")
        .arg("select 1")
        .output()
        .expect("run afpsql");
    assert_eq!(out.status.code(), Some(2));
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
    assert_strict_event(&v);
    assert_eq!(v["kind"], "error");
    assert_eq!(v["error"]["code"], "invalid_request");
    assert!(String::from_utf8_lossy(&out.stderr).trim().is_empty());
}
