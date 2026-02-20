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
fn cli_invalid_param_count_returns_error() {
    let out = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--sql")
        .arg("select $1::int")
        .output()
        .expect("run afpsql");

    assert!(!out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
    assert_eq!(v["code"], "error");
    assert_eq!(v["error_code"], "invalid_params");
}

#[test]
fn cli_result_too_large_without_streaming() {
    let out = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--sql")
        .arg("select x from generate_series(1,5) as x")
        .arg("--inline-max-rows")
        .arg("2")
        .output()
        .expect("run afpsql");

    assert!(!out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
    assert_eq!(v["code"], "error");
    assert_eq!(v["error_code"], "result_too_large");
}

#[test]
fn cli_read_only_rejects_write() {
    let out = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--sql")
        .arg("create temp table afpsql_ro_test(n int)")
        .arg("--read-only")
        .output()
        .expect("run afpsql");

    assert!(!out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
    assert_eq!(v["code"], "sql_error");
}

#[test]
fn cli_statement_timeout_triggers_sql_error() {
    let out = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--sql")
        .arg("select pg_sleep(0.20)")
        .arg("--statement-timeout-ms")
        .arg("10")
        .output()
        .expect("run afpsql");

    assert!(!out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
    assert_eq!(v["code"], "sql_error");
}

#[test]
fn pipe_handles_parse_error_cancel_ping_and_close() {
    let payload = "\n{not-json}\n".to_string()
        + &serde_json::json!({"code":"cancel","id":"missing"}).to_string()
        + "\n"
        + &serde_json::json!({"code":"ping"}).to_string()
        + "\n"
        + &serde_json::json!({"code":"close"}).to_string()
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
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).expect("utf8");
    assert!(text.contains("\"error_code\":\"invalid_request\""));
    assert!(text.contains("\"error_code\":\"cancelled\"") || text.contains("no in-flight query"));
    assert!(text.contains("\"code\":\"pong\""));
    assert!(text.contains("\"code\":\"close\""));
}

#[test]
fn mcp_initialize_list_and_query() {
    let payload = serde_json::json!({
        "jsonrpc":"2.0",
        "id":1,
        "method":"initialize",
        "params":{}
    })
    .to_string()
        + "\n"
        + &serde_json::json!({
            "jsonrpc":"2.0",
            "id":2,
            "method":"tools/list",
            "params":{}
        })
        .to_string()
        + "\n"
        + &serde_json::json!({
            "jsonrpc":"2.0",
            "id":3,
            "method":"tools/call",
            "params":{
                "name":"psql_query",
                "arguments":{
                    "sql":"select $1::int as n",
                    "params":[9],
                    "session":"default"
                }
            }
        })
        .to_string()
        + "\n"
        + &serde_json::json!({"jsonrpc":"2.0","id":4,"method":"shutdown","params":{}}).to_string()
        + "\n"
        + &serde_json::json!({"jsonrpc":"2.0","method":"exit","params":{}}).to_string()
        + "\n";

    let mut child = Command::new(bin())
        .arg("--mode")
        .arg("mcp")
        .arg("--dsn-secret")
        .arg(test_dsn())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn afpsql mode mcp");

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

    assert!(text.contains("\"id\":1"));
    assert!(text.contains("\"protocolVersion\""));
    assert!(text.contains("\"id\":2"));
    assert!(text.contains("\"psql_query\""));
    assert!(text.contains("\"id\":3"));
    assert!(text.contains("\"structuredContent\""));
    assert!(text.contains("\"ROWS 1\""));
}

#[test]
fn cli_invalid_output_returns_exit_2() {
    let out = Command::new(bin())
        .arg("--sql")
        .arg("select 1")
        .arg("--output")
        .arg("bad")
        .output()
        .expect("run afpsql");
    assert_eq!(out.status.code(), Some(2));
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
    assert_eq!(v["code"], "error");
    assert_eq!(v["error_code"], "invalid_request");
}

#[test]
fn cli_yaml_output_mode() {
    let out = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--sql")
        .arg("select 1 as n")
        .arg("--output")
        .arg("yaml")
        .output()
        .expect("run afpsql");
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).expect("utf8");
    assert!(text.contains("code: \"result\""));
}

#[test]
fn cli_plain_output_mode() {
    let out = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--sql")
        .arg("select 1 as n")
        .arg("--output")
        .arg("plain")
        .output()
        .expect("run afpsql");
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).expect("utf8");
    assert!(text.contains("result") || text.contains("code"));
}

#[test]
fn pipe_query_then_close_timeout_path() {
    let payload = serde_json::json!({
        "code": "query",
        "id": "q1",
        "sql": "select pg_sleep(10)"
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
    assert!(text.contains("\"code\":\"close\""));
}

#[test]
fn pipe_config_and_cancel_existing_query() {
    let payload = serde_json::json!({
        "code": "query",
        "id": "q1",
        "sql": "select pg_sleep(1)"
    })
    .to_string()
        + "\n"
        + &serde_json::json!({
            "code":"config",
            "inline_max_rows": 2,
            "statement_timeout_ms": 1000
        })
        .to_string()
        + "\n"
        + &serde_json::json!({"code":"cancel","id":"q1"}).to_string()
        + "\n"
        + &serde_json::json!({"code":"close"}).to_string()
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
    assert!(text.contains("\"code\":\"config\""));
    assert!(text.contains("\"error_code\":\"cancelled\"") || text.contains("\"code\":\"result\""));
    assert!(text.contains("\"code\":\"close\""));
}

#[test]
fn mcp_parse_ping_and_unknown_paths() {
    let payload = "\n{bad-json}\n".to_string()
        + &serde_json::json!({"jsonrpc":"2.0","id":11,"method":"ping","params":{}}).to_string()
        + "\n"
        + &serde_json::json!({"jsonrpc":"2.0","id":12,"method":"not-found","params":{}}).to_string()
        + "\n"
        + &serde_json::json!({"jsonrpc":"2.0","id":13,"method":"tools/call","params":{"name":"psql_config","arguments":1}}).to_string()
        + "\n"
        + &serde_json::json!({"jsonrpc":"2.0","method":"tools/call","params":{"name":"psql_query","arguments":{"sql":"select 1"}}}).to_string()
        + "\n"
        + &serde_json::json!({"jsonrpc":"2.0","id":14,"method":"tools/call","params":{"name":"psql_config","arguments":{"inline_max_rows":"bad"}}}).to_string()
        + "\n"
        + &serde_json::json!({"jsonrpc":"2.0","method":"exit","params":{}}).to_string()
        + "\n";

    let mut child = Command::new(bin())
        .arg("--mode")
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn afpsql mode mcp");

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
    assert!(text.contains("\"code\":-32700"));
    assert!(text.contains("\"id\":11"));
    assert!(text.contains("\"id\":12"));
    assert!(text.contains("method not found"));
    assert!(text.contains("\"id\":13"));
    assert!(text.contains("\"isError\":true"));
    assert!(text.contains("\"id\":14"));
    assert!(text.contains("invalid config patch"));
}

#[test]
fn pipe_cancel_race_and_long_query() {
    let payload = serde_json::json!({
        "code": "query",
        "id": "qrace",
        "sql": "select pg_sleep(2)"
    })
    .to_string()
        + "\n"
        + &serde_json::json!({"code":"cancel","id":"qrace"}).to_string()
        + "\n"
        + &serde_json::json!({"code":"cancel","id":"qrace"}).to_string()
        + "\n"
        + &serde_json::json!({"code":"close"}).to_string()
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
    assert!(text.contains("\"code\":\"close\""));
    assert!(text.contains("\"error_code\":\"cancelled\"") || text.contains("\"code\":\"result\""));
}
