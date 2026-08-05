#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::Value;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

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

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn stream_rows_keeps_the_whole_result_stream_on_stdout() {
    // A streamed query is an ordered event stream: the row batches carry the
    // payload the caller captures, so they must not land on the diagnostic
    // stream while only the terminator reaches stdout.
    let out = Command::new(bin())
        .arg("--dsn")
        .arg(test_dsn())
        .arg("--stream-rows")
        .arg("--sql")
        .arg("select generate_series(1,3) as n")
        .output()
        .expect("run afpsql stream-rows");
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).trim().is_empty(),
        "streaming split the event stream onto stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let text = String::from_utf8(out.stdout).expect("utf8");
    let events = text
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("json event"))
        .collect::<Vec<_>>();
    for event in &events {
        agent_first_data::validate_protocol_event(event, true).expect("strict AFDATA event");
    }

    let rows = events
        .iter()
        .filter(|event| event["progress"]["code"] == "result_rows")
        .flat_map(|event| {
            event["progress"]["rows"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 3, "row batches missing from stdout: {text}");

    // Exactly one terminal event, and it comes last.
    let terminal = events
        .iter()
        .filter(|event| event["kind"] == "result" || event["kind"] == "error")
        .count();
    assert_eq!(terminal, 1, "{text}");
    assert_eq!(
        events.last().expect("events")["result"]["code"],
        "result_end"
    );
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn pipe_mode_keeps_failures_in_stream_order_on_stdout() {
    // A consumer reading the pipe stream must see a failed request in its
    // original position, not lose it to a second stream.
    let payload = [
        serde_json::json!({"code":"query","id":"q1","sql":"select 1 as n"}).to_string(),
        serde_json::json!({"code":"query","id":"q2","sql":"select bad_col"}).to_string(),
        serde_json::json!({"code":"query","id":"q3","sql":"select 3 as n"}).to_string(),
        serde_json::json!({"code":"close"}).to_string(),
    ]
    .join("\n")
        + "\n";

    let mut child = Command::new(bin())
        .arg("--mode")
        .arg("pipe")
        .arg("--dsn")
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
        String::from_utf8_lossy(&out.stderr).trim().is_empty(),
        "pipe mode split the event stream onto stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).expect("utf8");
    let ids = text
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("json event"))
        .filter_map(|event| {
            event["result"]["id"]
                .as_str()
                .or_else(|| event["error"]["id"].as_str())
                .map(str::to_string)
        })
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["q1", "q2", "q3"], "{text}");
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn afd_cli_param_binding_query() {
    let out = Command::new(bin())
        .arg("--dsn")
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
    agent_first_data::validate_protocol_event(&v, true).expect("strict AFDATA event");
    assert_eq!(v["kind"], "result");
    assert_eq!(v["result"]["rows"][0]["n"], 42);
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn psql_mode_translates_v_params() {
    let out = Command::new(bin())
        .arg("--mode")
        .arg("psql")
        .arg("--dsn")
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
    agent_first_data::validate_protocol_event(&v, true).expect("strict AFDATA event");
    assert_eq!(v["kind"], "result");
    assert_eq!(v["result"]["rows"][0]["n"], 7);
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
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
        .arg("--dsn")
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

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
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
        .arg("--dsn")
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

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
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
        .arg("--dsn")
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

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
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
    agent_first_data::validate_protocol_event(&v, true).expect("strict AFDATA event");
    assert_eq!(v["kind"], "result");
    assert_eq!(v["result"]["rows"][0]["n"], 3);
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn psql_mode_positional_dsn_before_sql_flag() {
    let out = Command::new(bin())
        .arg("--mode")
        .arg("psql")
        .arg(test_dsn())
        .arg("-c")
        .arg("select 4 as n")
        .output()
        .expect("run afpsql");
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
    agent_first_data::validate_protocol_event(&v, true).expect("strict AFDATA event");
    assert_eq!(v["kind"], "result");
    assert_eq!(v["result"]["rows"][0]["n"], 4);
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn pipe_rejects_duplicate_active_query_id() {
    let payload = serde_json::json!({
        "code": "query",
        "id": "dup1",
        "sql": "select pg_sleep(1)"
    })
    .to_string()
        + "\n"
        + &serde_json::json!({
            "code": "query",
            "id": "dup1",
            "sql": "select 1 as n"
        })
        .to_string()
        + "\n"
        + &serde_json::json!({"code":"close"}).to_string()
        + "\n";

    let mut child = Command::new(bin())
        .arg("--mode")
        .arg("pipe")
        .arg("--dsn")
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
    let events = text
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("json event"))
        .collect::<Vec<_>>();
    for event in &events {
        agent_first_data::validate_protocol_event(event, true).expect("strict AFDATA event");
    }
    assert!(
        events.iter().any(|event| {
            event["kind"] == "error" && event["error"]["code"] == "invalid_request"
        })
    );
    assert!(text.contains("duplicate active query id"));
}

#[test]
fn docs_document_every_exit_code_the_binary_can_produce() {
    // `--docs` renders afdata's CLI reference and then splices in the exit-4
    // row by replacing an exact literal copy of afdata's exit-2 row. If afdata
    // ever rewords that row, `str::replace` silently matches nothing and the
    // exit code vanishes from the published reference while the `exit(4)` call
    // sites keep running. Assert the result rather than the mechanism.
    let out = Command::new(bin())
        .arg("--docs")
        .output()
        .expect("run afpsql --docs");
    assert!(
        out.status.success(),
        "--docs failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let docs = String::from_utf8(out.stdout).expect("utf8 docs");
    for code in ["| 0 |", "| 1 |", "| 2 |", "| 4 |"] {
        assert!(
            docs.contains(code),
            "exit code row {code} missing from --docs; if only `| 4 |` is absent, \
             afdata reworded the anchor row that src/cli.rs splices against"
        );
    }
    assert!(
        docs.contains("A terminal event could not be written"),
        "the exit-4 row lost its description"
    );
}

#[test]
fn an_unopenable_output_sink_is_a_defect_rather_than_a_usage_error() {
    // The published reference names `output_setup_failed` at exit 1, and the
    // difference from a `cli_*` rejection is the whole point: exit 2 tells an
    // agent to rewrite the command line, while a sink that cannot be opened
    // leaves the same command line with nowhere to write, so retrying it lands
    // in the same place. Documenting the code without emitting it would leave
    // the reference describing a binary that does not exist.
    let absent_dir =
        std::env::temp_dir().join(format!("afpsql_absent_sink_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&absent_dir);

    let out = Command::new(bin())
        .arg("--sql")
        .arg("select 1")
        .arg("--dsn")
        .arg("postgresql://127.0.0.1:1/postgres")
        .arg("--stdout-file")
        .arg(absent_dir.join("out.jsonl"))
        .output()
        .expect("run afpsql with an unopenable sink");

    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "the failure was announced on the stream that could not be opened"
    );
    let text = String::from_utf8(out.stderr).expect("utf8 stderr");
    let event: Value = serde_json::from_str(text.trim()).expect("json error event");
    agent_first_data::validate_protocol_event(&event, true).expect("strict AFDATA event");
    assert_eq!(event["error"]["code"], "output_setup_failed");
}
