#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::Value;
use std::io::{BufRead, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
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

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}_{}", std::process::id(), nanos)
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
fn cli_returning_result_too_large_rolls_back() {
    let table = format!("afpsql_returning_limit_{}", unique_suffix());
    for sql in [
        format!("drop table if exists {table}"),
        format!("create table {table}(id int primary key, touched boolean default false)"),
        format!("insert into {table}(id) select x from generate_series(1,3) as x"),
    ] {
        let out = Command::new(bin())
            .arg("--dsn-secret")
            .arg(test_dsn())
            .arg("--permission")
            .arg("write")
            .arg("--sql")
            .arg(sql)
            .output()
            .expect("run setup sql");
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let update_sql =
        format!("update {table} set touched = true returning id, repeat('x', 16) as payload");
    let out = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--permission")
        .arg("write")
        .arg("--sql")
        .arg(update_sql)
        .arg("--inline-max-rows")
        .arg("1")
        .output()
        .expect("run update returning");

    assert!(!out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
    assert_eq!(v["code"], "error");
    assert_eq!(v["error_code"], "result_too_large");

    let check_sql = format!("select count(*)::int as n from {table} where touched");
    let out = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--sql")
        .arg(check_sql)
        .output()
        .expect("run check sql");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
    assert_eq!(v["rows"][0]["n"], 0);

    let _ = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--permission")
        .arg("write")
        .arg("--sql")
        .arg(format!("drop table if exists {table}"))
        .output();
}

#[test]
fn cli_default_permission_rejects_write() {
    let out = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--sql")
        .arg("create temp table afpsql_ro_test(n int)")
        .output()
        .expect("run afpsql");

    assert!(!out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
    assert_eq!(v["code"], "sql_error");
    assert_eq!(v["sqlstate"], "25006");
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
    assert!(
        text.contains("\"error_code\":\"cancelled\"")
            || text.contains("no queued or running query")
    );
    assert!(text.contains("\"code\":\"pong\""));
    assert!(text.contains("\"code\":\"close\""));
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
fn pipe_session_preserves_temp_table_across_queries() {
    let payload = serde_json::json!({
        "code": "query",
        "id": "qcreate",
        "sql": "create temp table afpsql_session_state(n int)",
        "options": {"permission": "write"}
    })
    .to_string()
        + "\n"
        + &serde_json::json!({
            "code": "query",
            "id": "qinsert",
            "sql": "insert into afpsql_session_state values (7)",
            "options": {"permission": "write"}
        })
        .to_string()
        + "\n"
        + &serde_json::json!({
            "code": "query",
            "id": "qselect",
            "sql": "select n from afpsql_session_state"
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
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).expect("utf8");
    assert!(text.contains("\"id\":\"qselect\""), "output: {text}");
    assert!(text.contains("\"n\":7"), "output: {text}");
}

#[test]
fn pipe_same_session_queries_are_fifo() {
    let payload = serde_json::json!({
        "code": "query",
        "id": "qslow",
        "sql": "select 'first' as label, pg_sleep(0.30)"
    })
    .to_string()
        + "\n"
        + &serde_json::json!({
            "code": "query",
            "id": "qfast",
            "sql": "select 'second' as label"
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
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).expect("utf8");
    let slow = text.find("\"id\":\"qslow\"").expect("qslow output");
    let fast = text.find("\"id\":\"qfast\"").expect("qfast output");
    assert!(slow < fast, "same-session outputs not FIFO: {text}");
}

#[test]
fn pipe_cancel_queued_query_prevents_execution() {
    let payload = serde_json::json!({
        "code": "query",
        "id": "qcreate",
        "sql": "create temp table afpsql_queued_cancel(n int)",
        "options": {"permission": "write"}
    })
    .to_string()
        + "\n"
        + &serde_json::json!({
            "code": "query",
            "id": "qslow",
            "sql": "select pg_sleep(0.30)"
        })
        .to_string()
        + "\n"
        + &serde_json::json!({
            "code": "query",
            "id": "qqueued",
            "sql": "insert into afpsql_queued_cancel values (1)",
            "options": {"permission": "write"}
        })
        .to_string()
        + "\n"
        + &serde_json::json!({"code":"cancel","id":"qqueued"}).to_string()
        + "\n"
        + &serde_json::json!({
            "code": "query",
            "id": "qcheck",
            "sql": "select count(*)::int as n from afpsql_queued_cancel"
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
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).expect("utf8");
    assert!(text.contains("\"id\":\"qqueued\""), "output: {text}");
    assert!(
        text.contains("\"error_code\":\"cancelled\""),
        "output: {text}"
    );
    assert!(text.contains("\"id\":\"qcheck\""), "output: {text}");
    assert!(text.contains("\"n\":0"), "output: {text}");
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

#[test]
fn pipe_cancel_requests_server_side_cancel_for_active_query() {
    let marker = format!("afpsql_cancel_marker_{}", unique_suffix());
    let long_sql = format!("select pg_sleep(30) /* {marker} */");
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
        serde_json::json!({"code":"query","id":"qcancel","sql":long_sql})
    )
    .expect("write long query");
    stdin.flush().expect("flush long query");

    let activity_sql = format!(
        "select count(*)::int as n from pg_stat_activity where pid <> pg_backend_pid() and state = 'active' and query like '%{marker}%'"
    );
    let mut saw_active = false;
    for _ in 0..50 {
        let out = Command::new(bin())
            .arg("--dsn-secret")
            .arg(test_dsn())
            .arg("--sql")
            .arg(&activity_sql)
            .output()
            .expect("query pg_stat_activity");
        if out.status.success() {
            let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
            if v["rows"][0]["n"].as_i64().unwrap_or(0) > 0 {
                saw_active = true;
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(saw_active, "long query did not become active");

    writeln!(
        stdin,
        "{}",
        serde_json::json!({"code":"cancel","id":"qcancel"})
    )
    .expect("write cancel");
    writeln!(stdin, "{}", serde_json::json!({"code":"close"})).expect("write close");
    drop(stdin);

    let mut all = String::new();
    reader.read_to_string(&mut all).expect("read output");
    let status = child.wait().expect("wait status");
    assert!(status.success());
    assert!(
        all.contains("\"error_code\":\"cancelled\""),
        "output: {all}"
    );
    assert!(
        all.contains("server-side cancel requested"),
        "server-side cancel hint missing: {all}"
    );

    let out = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--sql")
        .arg(&activity_sql)
        .output()
        .expect("query pg_stat_activity after cancel");
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
    assert_eq!(v["rows"][0]["n"].as_i64().unwrap_or(-1), 0);
}
