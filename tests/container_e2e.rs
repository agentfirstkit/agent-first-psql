use std::process::Command;
use std::time::{Duration, Instant};

#[path = "support/env.rs"]
mod test_env;

const POSTGRES_ALIAS: &str = "postgres";

#[test]
#[ignore]
fn docker_container_transport_select_one() {
    if test_env::env_value("AFPSQL_E2E").as_deref() != Some("1") {
        return;
    }

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

    let afpsql = env!("CARGO_BIN_EXE_afpsql");
    let output_result = Command::new(afpsql)
        .args([
            "--container",
            &bridge_name,
            "--container-driver",
            "docker",
            "--host",
            POSTGRES_ALIAS,
            "--port",
            "5432",
            "--user",
            "test",
            "--dbname",
            "test",
            "--password-secret",
            "test",
            "--sql",
            "select 1 as n",
        ])
        .output();
    assert!(
        output_result.is_ok(),
        "run afpsql failed: {:?}",
        output_result.as_ref().err()
    );
    let output = match output_result {
        Ok(output) => output,
        Err(_) => return,
    };

    assert!(
        output.status.success(),
        "afpsql failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(r#""code":"result""#), "{stdout}");
    assert!(stdout.contains(r#""row_count":1"#), "{stdout}");
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
