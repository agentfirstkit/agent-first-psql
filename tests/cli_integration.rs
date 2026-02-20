use serde_json::Value;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

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
fn afd_cli_param_binding_query() {
    let out = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--sql")
        .arg("select $1::int + $2::int as n")
        .arg("--param")
        .arg("1=40")
        .arg("--param")
        .arg("2=2")
        .output()
        .expect("run afpsql");

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
    assert_eq!(v["code"], "result");
    assert_eq!(v["rows"][0]["n"], 42);
}

#[test]
fn psql_mode_translates_v_params() {
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

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
    assert_eq!(v["code"], "result");
    assert_eq!(v["rows"][0]["n"], 7);
}

#[test]
fn pipe_stream_rows() {
    let payload = serde_json::json!({
        "code": "query",
        "id": "q1",
        "sql": "select x as n from generate_series(1,5) as x",
        "options": {"stream_rows": true, "batch_rows": 2}
    })
    .to_string()
        + "\n"
        + &serde_json::json!({"code": "close"}).to_string()
        + "\n";

    let mut child = Command::new(bin())
        .arg("--mode")
        .arg("pipe")
        .arg("--dsn-secret")
        .arg(test_dsn())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn afpsql");

    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write stdin");

    let out = child.wait_with_output().expect("wait output");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).expect("utf8");
    assert!(text.contains("\"code\":\"result_start\""));
    assert!(text.contains("\"code\":\"result_rows\""));
    assert!(text.contains("\"code\":\"result_end\""));
}

#[test]
fn pipe_plain_output_mode() {
    let payload = serde_json::json!({
        "code": "query",
        "id": "q1",
        "sql": "select 1 as n"
    })
    .to_string()
        + "\n"
        + &serde_json::json!({"code":"close"}).to_string()
        + "\n";

    let mut child = Command::new(bin())
        .arg("--mode")
        .arg("pipe")
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--output")
        .arg("plain")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn afpsql");

    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write stdin");

    let out = child.wait_with_output().expect("wait output");
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).expect("utf8");
    assert!(text.contains("result") || text.contains("code"));
}

#[test]
fn pipe_yaml_output_mode() {
    let payload = serde_json::json!({
        "code": "query",
        "id": "q1",
        "sql": "select 1 as n"
    })
    .to_string()
        + "\n"
        + &serde_json::json!({"code":"close"}).to_string()
        + "\n";

    let mut child = Command::new(bin())
        .arg("--mode")
        .arg("pipe")
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--output")
        .arg("yaml")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn afpsql");

    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write stdin");

    let out = child.wait_with_output().expect("wait output");
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).expect("utf8");
    assert!(text.contains("code:"));
}

#[test]
fn psql_mode_positional_dsn() {
    let out = Command::new(bin())
        .arg("--mode")
        .arg("psql")
        .arg("-c")
        .arg("select 3 as n")
        .arg(test_dsn())
        .output()
        .expect("run afpsql");
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
    assert_eq!(v["code"], "result");
    assert_eq!(v["rows"][0]["n"], 3);
}
