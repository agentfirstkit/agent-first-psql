#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[path = "support/env.rs"]
mod test_env;

const POSTGRES_ALIAS: &str = "postgres";

// `#[ignore]` is what keeps this out of a plain `cargo test`; the container
// leg opts in with `--ignored`. Once it is running it must assert or fail —
// an early return on a missing environment variable would report a green run
// that exercised nothing, which is how the fixed entrypoint exists to prevent.
#[test]
#[ignore]
fn docker_container_transport_select_one() {
    assert_eq!(
        test_env::env_value("AFPSQL_E2E").as_deref(),
        Some("1"),
        "the container e2e needs AFPSQL_E2E=1; run it through `scripts/test.sh container` \
         or `scripts/test.sh all` rather than cargo directly"
    );

    let suffix = std::process::id().to_string();
    let network = format!("afpsql-e2e-net-{suffix}");
    let postgres_name = format!("afpsql-e2e-pg-{suffix}");
    let bridge_name = format!("afpsql-e2e-bridge-{suffix}");
    let postgres_image = test_env::env_value("AFPSQL_E2E_POSTGRES_IMAGE")
        .unwrap_or_else(|| "postgres:16".to_string());
    let bridge_image = test_env::env_value("AFPSQL_E2E_BRIDGE_IMAGE")
        .unwrap_or_else(|| "ubuntu:22.04".to_string());
    let _guard = DockerE2eGuard {
        containers: vec![postgres_name.clone(), bridge_name.clone()],
        network: network.clone(),
    };

    docker_success(["network", "create", &network], "create docker network");
    docker_success(
        [
            "run",
            "-d",
            "--rm",
            "--name",
            &postgres_name,
            "--network",
            &network,
            "--network-alias",
            POSTGRES_ALIAS,
            "-p",
            "127.0.0.1::5432",
            "-e",
            "POSTGRES_USER=test",
            "-e",
            "POSTGRES_PASSWORD=test",
            "-e",
            "POSTGRES_DB=test",
            &postgres_image,
        ],
        "start postgres container",
    );
    docker_success(
        [
            "run",
            "-d",
            "--rm",
            "--name",
            &bridge_name,
            "--network",
            &network,
            &bridge_image,
            "sh",
            "-c",
            "sleep 300",
        ],
        "start bridge container",
    );

    assert!(
        wait_for_postgres(&postgres_name),
        "postgres container did not become ready"
    );
    assert!(
        bridge_has_interpreter(&bridge_name),
        "bridge container must provide sh plus python3, python, or perl"
    );

    let published_postgres_port = docker_mapped_port(&postgres_name, "5432/tcp");

    let readonly = env!("CARGO_BIN_EXE_afpsql-readonly");
    let output_result = Command::new(readonly)
        .args([
            "--container-docker-name",
            &bridge_name,
            "--host",
            POSTGRES_ALIAS,
            "--port",
            "5432",
            "--user",
            "test",
            "--dbname",
            "test",
            "--password",
            "test",
            "--sql",
            "select 1 as n",
        ])
        .output();
    assert!(
        output_result.is_ok(),
        "run afpsql-readonly failed: {:?}",
        output_result.as_ref().err()
    );
    let output = match output_result {
        Ok(output) => output,
        Err(_) => return,
    };

    assert!(
        output.status.success(),
        "afpsql-readonly failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(r#""kind":"result""#), "{stdout}");
    assert!(stdout.contains(r#""row_count":1"#), "{stdout}");

    let write_result = Command::new(readonly)
        .args([
            "--container-docker-name",
            &bridge_name,
            "--host",
            POSTGRES_ALIAS,
            "--port",
            "5432",
            "--user",
            "test",
            "--dbname",
            "test",
            "--password",
            "test",
            "--permission",
            "container-write",
            "--sql",
            "select 1",
        ])
        .output();
    assert!(
        write_result.is_ok(),
        "run readonly container write failed: {:?}",
        write_result.as_ref().err()
    );
    let write = match write_result {
        Ok(output) => output,
        Err(_) => return,
    };
    assert!(!write.status.success());
    assert!(
        String::from_utf8_lossy(&write.stdout).trim().is_empty(),
        "default split routing wrote an error to stdout: {}",
        String::from_utf8_lossy(&write.stdout)
    );
    assert!(
        String::from_utf8_lossy(&write.stderr).contains(r#""code":"invalid_request""#),
        "stderr: {}",
        String::from_utf8_lossy(&write.stderr)
    );

    assert_explicit_transaction_boundaries_and_types(&bridge_name);
    assert_encoding_fallback_executes_once(&bridge_name);

    assert_readonly_policy_rejects(
        [
            "--container-docker-name",
            &bridge_name,
            "--container-docker-runtime",
            "false",
            "--sql",
            "select 1",
        ],
        "custom container runtime",
    );

    if let Some(ssh_destination) = test_env::env_value("AFPSQL_E2E_SSH") {
        assert_readonly_policy_rejects(
            [
                "--ssh",
                &ssh_destination,
                "--ssh-option",
                "ProxyCommand=false",
                "--sql",
                "select 1",
            ],
            "ProxyCommand",
        );
        assert_readonly_success(
            [
                "--ssh",
                &ssh_destination,
                "--host",
                "127.0.0.1",
                "--port",
                &published_postgres_port,
                "--user",
                "test",
                "--dbname",
                "test",
                "--password",
                "test",
                "--sql",
                "select 2 as n",
            ],
            "SSH readonly",
        );

        if let Some(proxy_jump) = test_env::env_value("AFPSQL_E2E_SSH_PROXY_JUMP") {
            let proxy_jump_option = format!("ProxyJump={proxy_jump}");
            assert_readonly_success(
                [
                    "--ssh",
                    &ssh_destination,
                    "--ssh-option",
                    &proxy_jump_option,
                    "--host",
                    "127.0.0.1",
                    "--port",
                    &published_postgres_port,
                    "--user",
                    "test",
                    "--dbname",
                    "test",
                    "--password",
                    "test",
                    "--sql",
                    "select 3 as n",
                ],
                "ProxyJump readonly",
            );
        }

        assert_readonly_success(
            [
                "--ssh",
                &ssh_destination,
                "--container-docker-name",
                &bridge_name,
                "--host",
                POSTGRES_ALIAS,
                "--port",
                "5432",
                "--user",
                "test",
                "--dbname",
                "test",
                "--password",
                "test",
                "--sql",
                "select 4 as n",
            ],
            "SSH plus container readonly",
        );
    }
}

fn container_write(bridge_name: &str, sql: &str) -> std::process::Output {
    container_write_logging(bridge_name, sql, Some("query.row_encoding_degraded"))
}

fn container_write_logging(
    bridge_name: &str,
    sql: &str,
    log_filter: Option<&str>,
) -> std::process::Output {
    let mut args = vec![
        "--container-docker-name",
        bridge_name,
        "--host",
        POSTGRES_ALIAS,
        "--port",
        "5432",
        "--user",
        "test",
        "--dbname",
        "test",
        "--password",
        "test",
        "--permission",
        "container-write",
    ];
    if let Some(filter) = log_filter {
        args.push("--log");
        args.push(filter);
    }
    args.push("--sql");
    args.push(sql);
    Command::new(env!("CARGO_BIN_EXE_afpsql"))
        .args(args)
        .output()
        .expect("run container write")
}

fn assert_encoding_fallback_executes_once(bridge_name: &str) {
    let table = format!("afpsql_fallback_once_{}", std::process::id());
    let create = container_write(bridge_name, &format!("create table {table}(n int)"));
    assert!(
        create.status.success(),
        "create fallback fixture: {}",
        String::from_utf8_lossy(&create.stderr)
    );

    let explain = container_write(
        bridge_name,
        &format!("explain analyze insert into {table} values (1) returning n"),
    );
    assert!(
        explain.status.success(),
        "fallback statement: {}",
        String::from_utf8_lossy(&explain.stderr)
    );
    assert!(
        String::from_utf8_lossy(&explain.stderr)
            .contains("\"event\":\"query.row_encoding_degraded\""),
        "fallback was not observable: {}",
        String::from_utf8_lossy(&explain.stderr)
    );

    let count = container_write(
        bridge_name,
        &format!("select count(*)::int as count from {table}"),
    );
    assert!(
        count.status.success(),
        "count fallback effects: {}",
        String::from_utf8_lossy(&count.stderr)
    );
    let event: serde_json::Value =
        serde_json::from_slice(&count.stdout).expect("count result event");
    assert_eq!(
        event["result"]["rows"][0]["count"], 1,
        "fallback statement executed more than once"
    );

    // The degradation notice is a log event, so it must honor --log like every
    // other one. An agent that asked for no logs must not get one injected into
    // its stream. `explain` triggers the same wrap failure without writing.
    let unlogged = container_write_logging(bridge_name, "explain select 1", None);
    assert!(
        unlogged.status.success(),
        "unlogged fallback statement: {}",
        String::from_utf8_lossy(&unlogged.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&unlogged.stderr).contains("query.row_encoding_degraded"),
        "degraded event ignored the log filter: {}",
        String::from_utf8_lossy(&unlogged.stderr)
    );

    let drop_table = container_write(bridge_name, &format!("drop table {table}"));
    assert!(drop_table.status.success(), "drop fallback fixture");
}

fn assert_explicit_transaction_boundaries_and_types(bridge_name: &str) {
    let requests = [
        serde_json::json!({"code":"begin","id":"default_begin"}),
        serde_json::json!({
            "code":"query",
            "id":"default_begin_write",
            "sql":"create temporary table afpsql_must_not_exist(n int)",
            "options":{"permission":"container-write"}
        }),
        serde_json::json!({"code":"rollback","id":"default_rollback"}),
        serde_json::json!({
            "code":"begin",
            "id":"write_begin",
            "read_only":false,
            "permission":"container-write"
        }),
        serde_json::json!({
            "code":"query",
            "id":"unacknowledged_write_tx",
            "sql":"select 1",
            "options":{"permission":"container-read"}
        }),
        serde_json::json!({"code":"rollback","id":"write_rollback"}),
        serde_json::json!({"code":"begin","id":"typed_begin"}),
        serde_json::json!({
            "code":"query",
            "id":"typed",
            "sql":"select 12.34::numeric as amount, '2026-07-31 10:00:00+00'::timestamptz as created_at, '123e4567-e89b-12d3-a456-426614174000'::uuid as id;"
        }),
        serde_json::json!({"code":"rollback","id":"typed_rollback"}),
        serde_json::json!({"code":"begin","id":"utility_begin"}),
        serde_json::json!({"code":"query","id":"utility_explain","sql":"explain select 1"}),
        serde_json::json!({"code":"query","id":"utility_show","sql":"show server_version"}),
        serde_json::json!({"code":"rollback","id":"utility_rollback"}),
        serde_json::json!({"code":"close"}),
    ]
    .into_iter()
    .map(|request| request.to_string())
    .collect::<Vec<_>>()
    .join("\n");

    let mut child = Command::new(env!("CARGO_BIN_EXE_afpsql"))
        .args([
            "--mode",
            "pipe",
            "--output-to",
            "stdout",
            "--container-docker-name",
            bridge_name,
            "--host",
            POSTGRES_ALIAS,
            "--port",
            "5432",
            "--user",
            "test",
            "--dbname",
            "test",
            "--password",
            "test",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn afpsql transaction e2e");
    child
        .stdin
        .as_mut()
        .expect("pipe stdin")
        .write_all(format!("{requests}\n").as_bytes())
        .expect("write transaction requests");
    let output = child.wait_with_output().expect("wait transaction e2e");
    assert!(
        output.status.success(),
        "transaction e2e failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events = String::from_utf8(output.stdout)
        .expect("utf8 events")
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("JSON event"))
        .collect::<Vec<_>>();
    let event = |id: &str| {
        events
            .iter()
            .find(|value| value["result"]["id"] == id || value["error"]["id"] == id)
            .unwrap_or_else(|| panic!("missing {id}: {events:?}"))
    };
    assert_eq!(
        event("default_begin_write")["error"]["sqlstate"],
        "25006",
        "begin without fields must be read-only"
    );
    assert_eq!(
        event("unacknowledged_write_tx")["error"]["code"],
        "invalid_request",
        "each query in a read-write transaction must acknowledge write permission"
    );
    let typed = &event("typed")["result"]["rows"][0];
    assert_eq!(typed["amount"], 12.34);
    assert_eq!(typed["created_at"], "2026-07-31T10:00:00+00:00");
    assert_eq!(typed["id"], "123e4567-e89b-12d3-a456-426614174000");
    // Utility statements cannot sit inside the `to_jsonb` CTE, so inside an
    // explicit transaction they must reach the direct decoder the same way they
    // do outside one. Without that fallback they fail as a wrapper syntax error
    // whose position points into SQL the caller never sent.
    assert!(
        event("utility_explain")["result"]["rows"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty()),
        "EXPLAIN inside an explicit transaction must fall back to the direct decoder: {:?}",
        event("utility_explain")
    );
    assert!(
        event("utility_show")["result"]["rows"][0]["server_version"].is_string(),
        "SHOW inside an explicit transaction must return its value: {:?}",
        event("utility_show")
    );
}

fn assert_readonly_policy_rejects<const N: usize>(args: [&str; N], context: &str) {
    let output = Command::new(env!("CARGO_BIN_EXE_afpsql-readonly"))
        .args(args)
        .output()
        .expect("run readonly runtime case");
    assert!(!output.status.success(), "{context} unexpectedly succeeded");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(r#""kind":"error""#) && stderr.contains(r#""code":"invalid_request""#),
        "{context} was not rejected by readonly policy: {stderr}"
    );
}

fn assert_readonly_success<const N: usize>(args: [&str; N], context: &str) {
    let output = Command::new(env!("CARGO_BIN_EXE_afpsql-readonly"))
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("{context} failed to start: {error}"));
    assert!(
        output.status.success(),
        "{context} failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains(r#""kind":"result""#),
        "{context} stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

fn docker_mapped_port(container: &str, port: &str) -> String {
    let output = Command::new("docker")
        .args(["port", container, port])
        .output()
        .expect("query Docker mapped port");
    assert!(output.status.success());
    let mapping = String::from_utf8(output.stdout).expect("Docker port output is UTF-8");
    mapping
        .trim()
        .rsplit_once(':')
        .map(|(_, port)| port.to_string())
        .expect("Docker port mapping contains a port")
}

fn docker_success<const N: usize>(args: [&str; N], context: &str) {
    let output_result = Command::new("docker").args(args).output();
    assert!(
        output_result.is_ok(),
        "{context} failed: {:?}",
        output_result.as_ref().err()
    );
    let output = match output_result {
        Ok(output) => output,
        Err(_) => return,
    };
    assert!(
        output.status.success(),
        "{context} failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn wait_for_postgres(name: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        let status = Command::new("docker")
            .args(["exec", name, "pg_isready", "-U", "test", "-d", "test"])
            .status();
        if matches!(status, Ok(status) if status.success()) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

fn bridge_has_interpreter(name: &str) -> bool {
    let status = Command::new("docker")
        .args([
            "exec",
            name,
            "sh",
            "-c",
            "command -v python3 >/dev/null 2>&1 || command -v python >/dev/null 2>&1 || command -v perl >/dev/null 2>&1",
        ])
        .status();
    matches!(status, Ok(status) if status.success())
}

struct DockerE2eGuard {
    containers: Vec<String>,
    network: String,
}

impl Drop for DockerE2eGuard {
    fn drop(&mut self) {
        for name in &self.containers {
            let _ = Command::new("docker").args(["rm", "-f", name]).status();
        }
        let _ = Command::new("docker")
            .args(["network", "rm", &self.network])
            .status();
    }
}
