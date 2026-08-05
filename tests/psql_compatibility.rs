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

fn split_error_event(output: &std::process::Output) -> Value {
    assert!(
        String::from_utf8_lossy(&output.stdout).trim().is_empty(),
        "default split routing wrote an error to stdout"
    );
    let value: Value = serde_json::from_slice(&output.stderr).expect("JSON error event");
    assert_strict_event(&value);
    assert_eq!(value["kind"], "error");
    value
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
fn canonical_long_version_emits_structured_version_event() {
    let out = Command::new(bin())
        .arg("--version")
        .output()
        .expect("run afpsql --version");
    assert!(out.status.success());
    let value: Value = serde_json::from_slice(&out.stdout).expect("JSON version event");
    assert_strict_event(&value);
    assert_eq!(value["kind"], "result");
    assert_eq!(value["result"]["code"], "version");
    assert_eq!(value["result"]["name"], "afpsql");
    assert!(String::from_utf8_lossy(&out.stderr).trim().is_empty());
}

#[test]
fn canonical_help_is_one_complete_answer_with_no_short_spellings() {
    let out = Command::new(bin())
        .arg("--help")
        .output()
        .expect("run afpsql --help");
    assert!(out.status.success());
    let value: Value = serde_json::from_slice(&out.stdout).expect("JSON help event");
    assert_strict_event(&value);
    let help = &value["result"]["help"];
    assert_eq!(help["schema"], "cli-help-v2");
    assert_eq!(help["command_path"], "afpsql");

    let shapes = help["shapes"].as_array().expect("root shapes");
    let ids: Vec<&str> = shapes
        .iter()
        .filter_map(|shape| shape["id"].as_str())
        .collect();
    assert_eq!(
        ids,
        vec![
            "query-inline",
            "query-file",
            "query-inline-stream",
            "query-file-stream",
            "pipe",
            "psql-translation",
        ]
    );
    // One round trip: the connecting arguments and the injected output
    // arguments are in the same answer as the shape that accepts them.
    let inline = shapes[0]["usage"].as_str().unwrap_or_default();
    for expected in [
        "afpsql --sql <SQL>",
        "[--host <HOST>]",
        "[--output <json|yaml|plain>]",
    ] {
        assert!(
            inline.contains(expected),
            "{expected} missing from {inline}"
        );
    }
    // There is no short syntax anywhere in the registry to advertise.
    let rendered = help.to_string();
    for short in ["\"-h\"", "\"-V\"", "\"-o\""] {
        assert!(!rendered.contains(short), "{short} appears in {rendered}");
    }
}

#[test]
fn canonical_mode_rejects_psql_compatibility_shorts() {
    for short in ["-h", "-V", "-o"] {
        let out = Command::new(bin())
            .arg(short)
            .output()
            .expect("run afpsql compatibility short");
        assert_eq!(out.status.code(), Some(2));
        let value = split_error_event(&out);
        // Not a rejected alias — the registry has no short syntax at all. The
        // rejection classifies the token without quoting it back, so
        // `error.code` and the hint are what the caller acts on.
        assert_eq!(value["error"]["code"], "cli_unknown_argument");
        assert_eq!(value["error"]["message"], "unknown short argument");
    }
}

#[test]
fn streaming_modes_have_no_split_routing_to_ask_for() {
    // An ordered event stream must stay on one stream; splitting it across
    // stdout and stderr would lose the ordering that makes it a stream. That is
    // the streaming shapes' output contract, not a check they run once started,
    // so `split` is simply not one of their destinations.
    for args in [
        vec!["--stream-rows", "--output-to", "split", "--sql", "select 1"],
        vec!["--mode", "pipe", "--output-to", "split"],
        vec!["--mode=pipe", "--output-to=split"],
    ] {
        let out = Command::new(bin())
            .args(&args)
            .output()
            .expect("run afpsql streaming split rejection");
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let value = split_error_event(&out);
        assert_eq!(
            value["error"]["code"], "cli_invalid_argument_value",
            "{args:?}"
        );
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("expected one of stdout, stderr"),
            "{args:?}: {value}"
        );
    }
}

#[test]
fn hyphen_valued_sql_is_written_inline_and_stays_a_value() {
    // A value is never taken from a token that starts with `-`, so SQL that
    // looks like a flag is written `--sql=<value>` and cannot flip the
    // invocation into event-stream mode.
    let out = Command::new(bin())
        .args(["--sql=--stream-rows", "--output-to", "split", "--dry-run"])
        .output()
        .expect("run afpsql hyphen-valued sql");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("expected one of stdout, stderr"),
        "SQL text was read as a streaming flag: {stderr}"
    );

    // Spelled with a space it is a missing value, not a swallowed flag.
    let out = Command::new(bin())
        .args(["--sql", "--stream-rows"])
        .output()
        .expect("run afpsql space-form hyphen value");
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        split_error_event(&out)["error"]["code"],
        "cli_missing_argument_value"
    );
}

#[test]
fn usage_errors_after_a_subcommand_stay_off_stdout() {
    // `--stream-rows` belongs to the root query shapes, so it is not an
    // argument of `inspect tables` at all. The rejection leaves stdout empty,
    // because a caller capturing the result stream must not find an error in it.
    let out = Command::new(bin())
        .args(["inspect", "tables", "--stream-rows"])
        .output()
        .expect("run afpsql subcommand usage error");
    assert_eq!(out.status.code(), Some(2));
    let value = split_error_event(&out);
    assert_eq!(value["error"]["code"], "cli_unknown_argument");
}

#[test]
fn psql_mode_rejects_output_flags_with_conflicting_psql_semantics() {
    for args in [
        vec![
            "--mode",
            "psql",
            "-o",
            "/tmp/afpsql-output",
            "-c",
            "select 1",
        ],
        vec![
            "--mode",
            "psql",
            "--output",
            "/tmp/afpsql-output",
            "-c",
            "select 1",
        ],
        vec!["--mode", "psql", "--output=json", "-c", "select 1"],
    ] {
        let out = Command::new(bin())
            .args(args.clone())
            .output()
            .expect("run afpsql");
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let value = split_error_event(&out);
        assert_eq!(value["error"]["code"], "invalid_request");
        assert!(
            value["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("unsupported psql-mode argument")),
            "{value}"
        );
    }
}

#[test]
fn psql_mode_interactive_usage_reports_structured_hint_on_stderr() {
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
        let v = split_error_event(&out);
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
        .arg("--dsn")
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
    assert!(output_text.trim().is_empty());
    let v: Value = serde_json::from_str(&stderr_text).expect("json error file");
    assert_strict_event(&v);
    assert_eq!(v["kind"], "error");

    let _ = std::fs::remove_file(out_path);
    let _ = std::fs::remove_file(err_path);
}

#[test]
fn ssh_transport_accepts_dsn_and_reports_precise_multi_host_error() {
    let out = Command::new(bin())
        .arg("--ssh")
        .arg("user@example.invalid")
        .arg("--dsn")
        .arg("postgresql://user:SSH_DSN_CANARY@db1:5432,db2:5433/postgres")
        .arg("--sql")
        .arg("select 1")
        .output()
        .expect("run afpsql");

    assert_eq!(out.status.code(), Some(1));
    let v = split_error_event(&out);
    assert_eq!(v["error"]["code"], "connect_failed");
    let message = v["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("single PostgreSQL host and port"));
    assert!(!message.contains("discrete connection fields"));
    assert!(!String::from_utf8_lossy(&out.stderr).contains("SSH_DSN_CANARY"));
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
    let v = split_error_event(&out);
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
        .arg("--dsn")
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
    let v = split_error_event(&unsupported);
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
        .arg("--dsn")
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
    let v = split_error_event(&out);
    assert_eq!(v["error"]["code"], "cli_unknown_argument");
}
