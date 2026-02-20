use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

fn test_dsn() -> String {
    std::env::var("AFPSQL_TEST_DSN_SECRET")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .unwrap_or_else(|_| "postgresql://localhost/postgres".to_string())
}

fn bin() -> PathBuf {
    let exe = std::env::current_exe().expect("current exe");
    let debug_dir = exe
        .parent()
        .and_then(|p| p.parent())
        .expect("target debug dir");
    debug_dir.join("afpsql")
}

#[test]
fn psql_mode_only_translates_supported_cli_flags() {
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
    assert_eq!(v["rows"][0]["n"], 7);

    let unsupported = Command::new(bin())
        .arg("--mode")
        .arg("psql")
        .arg("--set")
        .arg("ON_ERROR_STOP=1")
        .output()
        .expect("run afpsql");
    assert_eq!(unsupported.status.code(), Some(2));
    let v: Value = serde_json::from_slice(&unsupported.stdout).expect("json output");
    assert_eq!(v["error_code"], "invalid_request");
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
    assert_eq!(v["code"], "error");
    assert_eq!(v["error_code"], "invalid_request");
    assert!(String::from_utf8_lossy(&out.stderr).trim().is_empty());
}
