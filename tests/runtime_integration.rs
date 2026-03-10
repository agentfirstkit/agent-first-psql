use serde_json::Value;
use std::io::{BufRead, Read, Write};
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
        .env_remove("AFPSQL_DSN_SECRET")
        .env_remove("AFPSQL_CONNINFO_SECRET")
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
    use std::io::BufReader;

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

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);

    // 1. initialize
    writeln!(
        stdin,
        "{}",
        serde_json::json!({
            "jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{"protocolVersion":"2024-11-05","clientInfo":{"name":"test","version":"0.1"},"capabilities":{}}
        })
    )
    .expect("write init");
    stdin.flush().expect("flush");
    let mut line = String::new();
    reader.read_line(&mut line).expect("read init");
    assert!(line.contains("\"id\":1"));
    assert!(line.contains("\"protocolVersion\""));

    // 2. initialized notification
    writeln!(
        stdin,
        "{}",
        serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )
    .expect("write initialized");
    stdin.flush().expect("flush");

    // 3. tools/list
    writeln!(
        stdin,
        "{}",
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}})
    )
    .expect("write tools/list");
    stdin.flush().expect("flush");
    let mut line2 = String::new();
    reader.read_line(&mut line2).expect("read tools/list");
    assert!(line2.contains("\"id\":2"));
    assert!(line2.contains("\"psql_query\""));

    // 4. psql_query tool call
    writeln!(
        stdin,
        "{}",
        serde_json::json!({
            "jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"psql_query","arguments":{"sql":"select $1::int as n","params":[9],"session":"default"}}
        })
    )
    .expect("write query");
    stdin.flush().expect("flush");
    let mut line3 = String::new();
    reader.read_line(&mut line3).expect("read query result");
    assert!(line3.contains("\"id\":3"), "query response: {line3}");
    assert!(line3.contains("\"content\""), "query has content: {line3}");

    // Close stdin to shut down
    drop(stdin);
    let out = child.wait_with_output().expect("wait output");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
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
fn pipe_config_update_switches_session_connection() {
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

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = std::io::BufReader::new(stdout);

    writeln!(
        stdin,
        "{}",
        serde_json::json!({"code":"query","id":"q1","sql":"select 1 as n"})
    )
    .expect("write q1");
    stdin.flush().expect("flush q1");

    let mut line = String::new();
    let mut all = String::new();
    let mut saw_q1_result = false;
    for _ in 0..64 {
        line.clear();
        let n = reader.read_line(&mut line).expect("read q1 line");
        if n == 0 {
            break;
        }
        all.push_str(&line);
        if line.contains("\"id\":\"q1\"") && line.contains("\"code\":\"result\"") {
            saw_q1_result = true;
            break;
        }
    }
    assert!(
        saw_q1_result,
        "did not observe q1 result before config update"
    );

    writeln!(
        stdin,
        "{}",
        serde_json::json!({
            "code":"config",
            "sessions":{"default":{"dsn_secret":"postgresql://127.0.0.1:1/postgres"}}
        })
    )
    .expect("write config");
    writeln!(
        stdin,
        "{}",
        serde_json::json!({"code":"query","id":"q2","sql":"select 1 as n"})
    )
    .expect("write q2");
    writeln!(stdin, "{}", serde_json::json!({"code":"close"})).expect("write close");
    drop(stdin);

    reader.read_to_string(&mut all).expect("read remaining");

    let status = child.wait().expect("wait status");
    assert!(status.success());
    assert!(all.contains("\"code\":\"config\""));
    assert!(all.contains("\"id\":\"q2\""));
    assert!(
        all.contains("\"error_code\":\"connect_failed\""),
        "full output: {all}"
    );
}

#[test]
fn pipe_config_patch_can_clear_dsn_secret() {
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

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = std::io::BufReader::new(stdout);

    writeln!(
        stdin,
        "{}",
        serde_json::json!({"code":"query","id":"q1","sql":"select 1 as n"})
    )
    .expect("write q1");
    stdin.flush().expect("flush q1");

    let mut line = String::new();
    let mut all = String::new();
    let mut saw_q1_result = false;
    for _ in 0..64 {
        line.clear();
        let n = reader.read_line(&mut line).expect("read q1 line");
        if n == 0 {
            break;
        }
        all.push_str(&line);
        if line.contains("\"id\":\"q1\"") && line.contains("\"code\":\"result\"") {
            saw_q1_result = true;
            break;
        }
    }
    assert!(
        saw_q1_result,
        "did not observe q1 result before config update"
    );

    writeln!(
        stdin,
        "{}",
        serde_json::json!({
            "code":"config",
            "sessions":{
                "default":{
                    "dsn_secret": null,
                    "host":"127.0.0.1",
                    "port": 1,
                    "user":"postgres",
                    "dbname":"postgres"
                }
            }
        })
    )
    .expect("write config");
    writeln!(
        stdin,
        "{}",
        serde_json::json!({"code":"query","id":"q2","sql":"select 1 as n"})
    )
    .expect("write q2");
    writeln!(stdin, "{}", serde_json::json!({"code":"close"})).expect("write close");
    drop(stdin);

    reader.read_to_string(&mut all).expect("read remaining");

    let status = child.wait().expect("wait status");
    assert!(status.success());
    assert!(all.contains("\"code\":\"config\""));
    assert!(all.contains("\"id\":\"q2\""));
    assert!(
        all.contains("\"error_code\":\"connect_failed\""),
        "full output: {all}"
    );
}

#[test]
fn pipe_cancel_after_query_finished_returns_invalid_request() {
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

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = std::io::BufReader::new(stdout);

    writeln!(
        stdin,
        "{}",
        serde_json::json!({"code":"query","id":"qdone","sql":"select 1 as n"})
    )
    .expect("write qdone");
    stdin.flush().expect("flush qdone");

    let mut line = String::new();
    let mut all = String::new();
    let mut saw_result = false;
    for _ in 0..64 {
        line.clear();
        let n = reader.read_line(&mut line).expect("read qdone line");
        if n == 0 {
            break;
        }
        all.push_str(&line);
        if line.contains("\"id\":\"qdone\"") && line.contains("\"code\":\"result\"") {
            saw_result = true;
            break;
        }
    }
    assert!(saw_result, "did not observe qdone result before cancel");

    writeln!(
        stdin,
        "{}",
        serde_json::json!({"code":"cancel","id":"qdone"})
    )
    .expect("write cancel");
    writeln!(stdin, "{}", serde_json::json!({"code":"close"})).expect("write close");
    drop(stdin);

    reader.read_to_string(&mut all).expect("read remaining");

    let status = child.wait().expect("wait status");
    assert!(status.success());
    assert!(all.contains("\"id\":\"qdone\""));
    assert!(all.contains("\"error_code\":\"invalid_request\""));
    assert!(all.contains("query already finished"));
    assert!(all.contains("\"code\":\"close\""));
}

#[test]
fn mcp_protocol_error_handling() {
    use std::io::BufReader;

    let mut child = Command::new(bin())
        .arg("--mode")
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn afpsql mode mcp");

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);

    // 1. initialize
    writeln!(
        stdin,
        "{}",
        serde_json::json!({
            "jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{"protocolVersion":"2024-11-05","clientInfo":{"name":"test","version":"0.1"},"capabilities":{}}
        })
    )
    .expect("write");
    stdin.flush().expect("flush");
    let mut line = String::new();
    reader.read_line(&mut line).expect("read");
    assert!(line.contains("\"protocolVersion\""));

    // 2. initialized
    writeln!(
        stdin,
        "{}",
        serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )
    .expect("write");
    stdin.flush().expect("flush");

    // 3. psql_config with invalid type (string instead of integer for inline_max_rows)
    writeln!(
        stdin,
        "{}",
        serde_json::json!({
            "jsonrpc":"2.0","id":10,"method":"tools/call",
            "params":{"name":"psql_config","arguments":{"inline_max_rows":"bad"}}
        })
    )
    .expect("write");
    stdin.flush().expect("flush");
    let mut line2 = String::new();
    reader.read_line(&mut line2).expect("read");
    assert!(line2.contains("\"id\":10"), "error response: {line2}");
    // rmcp returns an error for invalid tool arguments
    assert!(
        line2.contains("\"error\"") || line2.contains("\"isError\""),
        "should be error: {line2}"
    );

    // Close stdin
    drop(stdin);
    let out = child.wait_with_output().expect("wait");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
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
