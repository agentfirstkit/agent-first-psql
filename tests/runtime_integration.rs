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

fn run_write_sql(sql: &str) -> std::process::Output {
    Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--permission")
        .arg("write")
        .arg("--sql")
        .arg(sql)
        .output()
        .expect("run write sql")
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn cli_closes_backend_connection_before_exit() {
    let first = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--sql")
        .arg("select pg_backend_pid() as pid")
        .output()
        .expect("run afpsql");
    assert!(
        first.status.success(),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    let first_json: Value = serde_json::from_slice(&first.stdout).expect("json output");
    let pid = first_json["result"]["rows"][0]["pid"]
        .as_i64()
        .expect("backend pid");

    let check = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--sql")
        .arg("select count(*)::int as n from pg_stat_activity where pid = $1::int")
        .arg("--param")
        .arg(format!("1={pid}"))
        .output()
        .expect("run afpsql");
    assert!(
        check.status.success(),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    let check_json: Value = serde_json::from_slice(&check.stdout).expect("json output");
    assert_eq!(check_json["result"]["rows"][0]["n"], 0);
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
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
    assert_eq!(v["kind"], "error");
    assert_eq!(v["error"]["code"], "invalid_params");
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn cli_inline_max_rows_soft_truncates() {
    let out = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--sql")
        .arg("select x from generate_series(1,5) as x")
        .arg("--inline-max-rows")
        .arg("2")
        .output()
        .expect("run afpsql");

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
    assert_eq!(v["kind"], "result");
    assert_eq!(v["result"]["truncated"], true);
    assert_eq!(v["result"]["truncated_at_rows"], 2);
    assert_eq!(v["result"]["row_count"], 2);
    assert_eq!(v["result"]["rows"].as_array().map(|a| a.len()), Some(2));
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn cli_returning_truncation_completes_update_but_caps_rows() {
    // Inline truncation now matches PostgreSQL's own cursor semantics: the
    // UPDATE still affects every matching row, but the RETURNING projection
    // delivered to the agent is capped. The agent learns via
    // `truncated: true` that the result is partial.
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

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
    assert_eq!(v["kind"], "result");
    assert_eq!(v["result"]["truncated"], true);
    assert_eq!(v["result"]["truncated_at_rows"], 1);
    assert_eq!(v["result"]["rows"].as_array().map(|a| a.len()), Some(1));

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
    assert_eq!(v["result"]["rows"][0]["n"], 3);

    let _ = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--permission")
        .arg("write")
        .arg("--sql")
        .arg(format!("drop table if exists {table}"))
        .output();
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
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
    assert_eq!(v["kind"], "error");
    assert_eq!(v["error"]["code"], "sql_error");
    assert_eq!(v["error"]["sqlstate"], "25006");
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
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
    assert_eq!(v["kind"], "error");
    assert_eq!(v["error"]["code"], "sql_error");
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
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
    assert!(text.contains("\"code\":\"invalid_request\""));
    assert!(text.contains("\"code\":\"cancelled\"") || text.contains("no queued or running query"));
    assert!(text.contains("\"code\":\"pong\""));
    assert!(text.contains("\"code\":\"close\""));
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn cli_bytea_decodes_as_hex_string() {
    let out = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--sql")
        .arg("select '\\x48656c6c6f'::bytea as b")
        .output()
        .expect("run afpsql");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
    assert_eq!(v["result"]["rows"][0]["b"], "\\x48656c6c6f");
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn cli_text_array_decodes_as_json_array() {
    let out = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--sql")
        .arg("select array['a','b',null]::text[] as items")
        .output()
        .expect("run afpsql");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
    assert_eq!(v["result"]["rows"][0]["items"][0], "a");
    assert_eq!(v["result"]["rows"][0]["items"][1], "b");
    assert!(v["result"]["rows"][0]["items"][2].is_null());
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn cli_explain_returns_plan_json() {
    let out = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--sql")
        .arg("select 1 as one")
        .arg("--explain")
        .output()
        .expect("run afpsql --explain");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
    assert_eq!(v["kind"], "result");
    let plan = &v["result"]["rows"][0]["QUERY PLAN"][0]["Plan"];
    assert!(plan["Node Type"].is_string(), "plan missing node type: {v}");
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn cli_explain_analyze_reports_actual_timing() {
    let out = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--sql")
        .arg("select 1 as one")
        .arg("--explain-analyze")
        .output()
        .expect("run afpsql --explain-analyze");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
    let plan = &v["result"]["rows"][0]["QUERY PLAN"][0]["Plan"];
    assert!(
        plan["Actual Total Time"].is_number(),
        "plan missing Actual Total Time: {v}"
    );
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn pipe_explicit_tx_commit_persists_changes() {
    let table = format!("afpsql_pipe_tx_{}", unique_suffix());
    let payload = serde_json::json!({"code":"begin","id":"b","permission":"write"}).to_string()
        + "\n"
        + &serde_json::json!({"code":"query","id":"create","sql":format!("create table {table}(n int)"),"options":{"permission":"write"}}).to_string()
        + "\n"
        + &serde_json::json!({"code":"query","id":"ins","sql":format!("insert into {table} values (7)"),"options":{"permission":"write"}}).to_string()
        + "\n"
        + &serde_json::json!({"code":"commit","id":"c"}).to_string()
        + "\n"
        + &serde_json::json!({"code":"query","id":"check","sql":format!("select n from {table}")}).to_string()
        + "\n"
        + &serde_json::json!({"code":"query","id":"drop","sql":format!("drop table {table}"),"options":{"permission":"write"}}).to_string()
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
        .expect("write");
    let out = child.wait_with_output().expect("wait");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).expect("utf8");
    let check_line = text
        .lines()
        .find(|l| l.contains("\"id\":\"check\""))
        .expect("check event");
    let check: Value = serde_json::from_str(check_line).expect("parse check");
    assert_eq!(check["result"]["row_count"], 1);
    assert_eq!(check["result"]["rows"][0]["n"], 7);
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn pipe_explicit_tx_rollback_discards_changes() {
    let table = format!("afpsql_pipe_rb_{}", unique_suffix());
    let payload = serde_json::json!({"code":"begin","id":"b","permission":"write"}).to_string()
        + "\n"
        + &serde_json::json!({"code":"query","id":"create","sql":format!("create table {table}(n int)"),"options":{"permission":"write"}}).to_string()
        + "\n"
        + &serde_json::json!({"code":"rollback","id":"rb"}).to_string()
        + "\n"
        + &serde_json::json!({"code":"query","id":"check","sql":format!("select to_regclass('{table}')::text as exists")}).to_string()
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
        .expect("write");
    let out = child.wait_with_output().expect("wait");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).expect("utf8");
    let check_line = text
        .lines()
        .find(|l| l.contains("\"id\":\"check\""))
        .expect("check event");
    let check: Value = serde_json::from_str(check_line).expect("parse check");
    assert!(
        check["result"]["rows"][0]["exists"].is_null(),
        "table should have been rolled back: {check}"
    );
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn pipe_explicit_tx_savepoint_isolates_failed_query() {
    // A failed query inside an explicit tx wraps itself in a savepoint and
    // rolls back to it, so the user's outer tx is NOT aborted and the next
    // query still runs.
    let payload = serde_json::json!({"code":"begin","id":"b"}).to_string()
        + "\n"
        + &serde_json::json!({"code":"query","id":"qbad","sql":"select * from this_table_truly_does_not_exist"}).to_string()
        + "\n"
        + &serde_json::json!({"code":"query","id":"qgood","sql":"select 1 as ok"}).to_string()
        + "\n"
        + &serde_json::json!({"code":"rollback","id":"rb"}).to_string()
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
        .expect("write");
    let out = child.wait_with_output().expect("wait");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).expect("utf8");
    let qgood_line = text
        .lines()
        .find(|l| l.contains("\"id\":\"qgood\""))
        .expect("qgood event");
    let qgood: Value = serde_json::from_str(qgood_line).expect("parse qgood");
    assert_eq!(qgood["kind"], "result");
    assert_eq!(qgood["result"]["rows"][0]["ok"], 1);
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn cli_inspect_schemas_lists_public() {
    let out = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("inspect")
        .arg("schemas")
        .output()
        .expect("run afpsql inspect schemas");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
    assert_eq!(v["kind"], "result");
    let rows = v["result"]["rows"].as_array().expect("rows array");
    let names: Vec<&str> = rows.iter().filter_map(|r| r["schema"].as_str()).collect();
    assert!(names.contains(&"public"), "schemas: {names:?}");
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn cli_inspect_database_summarizes_connected_db() {
    let out = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("inspect")
        .arg("database")
        .output()
        .expect("run afpsql inspect database");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
    assert_eq!(v["kind"], "result");
    assert_eq!(v["result"]["row_count"], 1);
    let row = &v["result"]["rows"][0];
    assert!(row["database"].is_string(), "row: {row}");
    // Counts are present and numeric (>= 0).
    for key in [
        "schemas",
        "tables",
        "views",
        "materialized_views",
        "sequences",
    ] {
        assert!(row[key].is_number(), "missing numeric {key}: {row}");
    }
    assert!(row["size"].is_string(), "size missing: {row}");
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn cli_inspect_databases_includes_size_and_encoding() {
    let out = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("inspect")
        .arg("databases")
        .output()
        .expect("run afpsql inspect databases");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
    assert_eq!(v["kind"], "result");
    let rows = v["result"]["rows"].as_array().expect("rows array");
    let row = rows.first().expect("at least one database");
    for key in [
        "database",
        "owner",
        "encoding",
        "collate",
        "allow_connections",
    ] {
        assert!(row.get(key).is_some(), "missing {key}: {row}");
    }
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn cli_inspect_table_describes_columns() {
    let table = format!("afpsql_inspect_{}", unique_suffix());
    let create = format!("create table {table}(id int primary key, name text not null, age int)");
    let drop = format!("drop table if exists {table}");

    let setup = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--permission")
        .arg("write")
        .arg("--sql")
        .arg(&create)
        .output()
        .expect("create inspect table");
    assert!(
        setup.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&setup.stderr)
    );

    let out = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("inspect")
        .arg("table")
        .arg(&table)
        .output()
        .expect("run afpsql inspect table");
    let _ = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--permission")
        .arg("write")
        .arg("--sql")
        .arg(&drop)
        .output();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
    assert_eq!(v["result"]["row_count"], 3);
    let rows = v["result"]["rows"].as_array().expect("rows array");
    assert_eq!(rows[0]["name"], "id");
    assert_eq!(rows[0]["nullable"], false);
    assert_eq!(rows[1]["name"], "name");
    assert_eq!(rows[1]["nullable"], false);
    assert_eq!(rows[2]["name"], "age");
    assert_eq!(rows[2]["nullable"], true);
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn cli_inspect_schema_table_full_and_index_stats_export_metadata() {
    let suffix = unique_suffix();
    let schema = format!("afpsql_inspect_schema_{suffix}");
    let setup_statements = [
        format!("create schema {schema}"),
        format!(
            "create table {schema}.parent(\
             id serial primary key, \
             code text not null unique)"
        ),
        format!(
            "create table {schema}.child(\
             id bigint generated by default as identity primary key, \
             parent_id int not null references {schema}.parent(id), \
             qty int not null check (qty > 0), \
             note text default 'x')"
        ),
        format!("create index child_parent_idx on {schema}.child(parent_id)"),
        format!(
            "create function {schema}.touch_child() returns trigger \
             language plpgsql as $$ \
             begin \
               new.note = coalesce(new.note, 'x'); \
               return new; \
             end $$"
        ),
        format!(
            "create trigger child_touch \
             before insert on {schema}.child \
             for each row execute function {schema}.touch_child()"
        ),
    ];
    for sql in setup_statements {
        let out = run_write_sql(&sql);
        assert!(
            out.status.success(),
            "sql: {sql}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let schema_out = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("inspect")
        .arg("schema")
        .arg("--schema")
        .arg(&schema)
        .arg("--like")
        .arg("child")
        .output()
        .expect("run afpsql inspect schema");
    let table_full_out = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("inspect")
        .arg("table")
        .arg(format!("{schema}.child"))
        .arg("--full")
        .output()
        .expect("run afpsql inspect table --full");
    let indexes_out = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("inspect")
        .arg("indexes")
        .arg("--schema")
        .arg(&schema)
        .arg("--table")
        .arg("child")
        .arg("--stats")
        .output()
        .expect("run afpsql inspect indexes --stats");

    let _ = run_write_sql(&format!("drop schema if exists {schema} cascade"));

    for (name, out) in [
        ("schema", &schema_out),
        ("table --full", &table_full_out),
        ("indexes --stats", &indexes_out),
    ] {
        assert!(
            out.status.success(),
            "{name} stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let schema_json: Value = serde_json::from_slice(&schema_out.stdout).expect("schema json");
    let schema_rows = schema_json["result"]["rows"]
        .as_array()
        .expect("schema rows");
    for kind in ["relation", "column", "constraint", "index", "trigger"] {
        assert!(
            schema_rows.iter().any(|row| row["kind"] == kind),
            "missing {kind} row: {schema_rows:?}"
        );
    }
    let id_col = schema_rows
        .iter()
        .find(|row| row["kind"] == "column" && row["name"] == "id")
        .expect("id column row");
    assert!(
        id_col["payload"]["serial_sequence"].is_string(),
        "missing serial sequence relationship: {id_col}"
    );

    let full_json: Value = serde_json::from_slice(&table_full_out.stdout).expect("full json");
    let full_rows = full_json["result"]["rows"].as_array().expect("full rows");
    assert!(
        full_rows
            .iter()
            .any(|row| row["kind"] == "trigger" && row["name"] == "child_touch"),
        "missing trigger row: {full_rows:?}"
    );
    assert!(
        full_rows
            .iter()
            .any(|row| row["kind"] == "constraint" && row["object_type"] == "foreign key"),
        "missing foreign key row: {full_rows:?}"
    );

    let indexes_json: Value = serde_json::from_slice(&indexes_out.stdout).expect("indexes json");
    let index_rows = indexes_json["result"]["rows"]
        .as_array()
        .expect("index rows");
    let child_parent = index_rows
        .iter()
        .find(|row| row["name"] == "child_parent_idx")
        .expect("child_parent_idx row");
    assert_eq!(child_parent["method"], "btree");
    assert!(
        child_parent.get("index_scan_count").is_some(),
        "pg_stat_user_indexes counters missing: {child_parent}"
    );
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn cli_inspect_tables_filters_by_schema_and_pattern() {
    let suffix = unique_suffix();
    let table = format!("afpsql_inspect_tbl_{suffix}");
    let create = format!("create table {table}(id int)");
    let drop = format!("drop table if exists {table}");

    let setup = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--permission")
        .arg("write")
        .arg("--sql")
        .arg(&create)
        .output()
        .expect("create inspect tables");
    assert!(
        setup.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&setup.stderr)
    );

    let out = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("inspect")
        .arg("tables")
        .arg("--schema")
        .arg("public")
        .arg("--like")
        .arg(format!("afpsql_inspect_tbl_{suffix}%"))
        .output()
        .expect("run afpsql inspect tables");
    let _ = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--permission")
        .arg("write")
        .arg("--sql")
        .arg(&drop)
        .output();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
    assert_eq!(v["result"]["row_count"], 1);
    assert_eq!(v["result"]["rows"][0]["name"], table);
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn cli_dry_run_reports_param_types_and_columns() {
    let out = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--sql")
        .arg("select $1::int4 as n, $2::text as t")
        .arg("--param")
        .arg("1=5")
        .arg("--param")
        .arg("2=hi")
        .arg("--dry-run")
        .output()
        .expect("run afpsql");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
    assert_eq!(v["kind"], "result");
    assert_eq!(v["result"]["code"], "dry_run");
    assert_eq!(v["result"]["param_types"][0], "int4");
    assert_eq!(v["result"]["param_types"][1], "text");
    assert_eq!(v["result"]["columns"][0]["name"], "n");
    assert_eq!(v["result"]["columns"][1]["name"], "t");
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn cli_dry_run_surfaces_unknown_table_via_sql_error() {
    let out = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--sql")
        .arg("select * from this_table_does_not_exist_xyz")
        .arg("--dry-run")
        .output()
        .expect("run afpsql");
    assert_eq!(out.status.code(), Some(1));
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
    assert_eq!(v["kind"], "error");
    assert_eq!(v["error"]["code"], "sql_error");
    assert_eq!(v["error"]["sqlstate"], "42P01");
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn cli_param_preserves_string_form_for_text_column() {
    let out = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--sql")
        .arg("select $1::text as raw")
        .arg("--param")
        .arg("1=00123")
        .output()
        .expect("run afpsql");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
    assert_eq!(v["result"]["rows"][0]["raw"], "00123");
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn cli_numeric_param_preserves_precision() {
    // Cast result to text to bypass JSON-number precision limits; the goal is
    // to verify the bind path sent the literal to PG without f64 rounding.
    let out = Command::new(bin())
        .arg("--dsn-secret")
        .arg(test_dsn())
        .arg("--sql")
        .arg("select ($1::numeric(40,20))::text as n")
        .arg("--param")
        .arg("1=12345678901234567890.123456789012345")
        .output()
        .expect("run afpsql");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("json output");
    assert_eq!(
        v["result"]["rows"][0]["n"],
        "12345678901234567890.12345678901234500000"
    );
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn pipe_session_info_reports_connection_identity() {
    let payload = serde_json::json!({"code":"session_info"}).to_string()
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
    let info_line = text
        .lines()
        .find(|l| l.contains("\"code\":\"session_info\""))
        .expect("session_info event");
    let info: Value = serde_json::from_str(info_line).expect("parse session_info");
    let (expected_user, expected_db) = test_env::dsn_identity(&test_dsn());
    assert_eq!(info["result"]["database"], expected_db.as_str());
    assert_eq!(info["result"]["user"], expected_user.as_str());
    assert!(
        info["result"]["server_version"].is_string(),
        "server_version missing: {info_line}"
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
    agent_first_data::validate_protocol_event(&v, true).expect("strict AFDATA event");
    assert_eq!(v["kind"], "error");
    assert_eq!(v["error"]["code"], "invalid_request");
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
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

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
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

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
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

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
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
    // Exactly one outcome event for q1, either the SELECT result (handler
    // won) or the cancelled error (cancel dispatcher won). Never both.
    let outcomes_for_q1 = text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|v| v["result"]["id"] == "q1" || v["error"]["id"] == "q1")
        .filter(|v| {
            v["kind"] == "result"
                || (v["kind"] == "error"
                    && matches!(v["error"]["code"].as_str(), Some("sql_error" | "cancelled")))
        })
        .count();
    assert_eq!(
        outcomes_for_q1, 1,
        "expected one outcome for q1, got {outcomes_for_q1}; output: {text}"
    );
    assert!(text.contains("\"code\":\"close\""));
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
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

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
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

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
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
    assert!(text.contains("\"code\":\"cancelled\""), "output: {text}");
    assert!(text.contains("\"id\":\"qcheck\""), "output: {text}");
    assert!(text.contains("\"n\":0"), "output: {text}");
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
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
        all.contains("\"code\":\"connect_failed\""),
        "full output: {all}"
    );
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
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
        all.contains("\"code\":\"connect_failed\""),
        "full output: {all}"
    );
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
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
    assert!(all.contains("\"code\":\"invalid_request\""));
    assert!(all.contains("query already finished"));
    assert!(all.contains("\"code\":\"close\""));
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
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
    // The CAS guarantees exactly one *outcome* event for qrace (either
    // `result`/`sql_error` if the handler won the race, or
    // `error_code:cancelled` if the cancel dispatcher won). The second
    // cancel request gets a separate "no matching in-flight query"
    // acknowledgement, which is dispatch-level — not a query outcome.
    let outcomes_for_qrace = text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|v| v["result"]["id"] == "qrace" || v["error"]["id"] == "qrace")
        .filter(|v| {
            v["kind"] == "result"
                || (v["kind"] == "error"
                    && matches!(v["error"]["code"].as_str(), Some("sql_error" | "cancelled")))
        })
        .count();
    assert_eq!(
        outcomes_for_qrace, 1,
        "expected one outcome for qrace, got {outcomes_for_qrace}; output: {text}"
    );
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
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
            if v["result"]["rows"][0]["n"].as_i64().unwrap_or(0) > 0 {
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
    assert!(all.contains("\"code\":\"cancelled\""), "output: {all}");
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
    assert_eq!(v["result"]["rows"][0]["n"].as_i64().unwrap_or(-1), 0);
}
