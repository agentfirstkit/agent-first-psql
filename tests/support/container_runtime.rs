#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::net::TcpListener;
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestContainerRuntime {
    Apple,
    Docker,
}

pub struct PublishedPort {
    pub spec: String,
    pub host_port: String,
}

impl TestContainerRuntime {
    pub fn selected() -> Self {
        match std::env::var("AFPSQL_TEST_CONTAINER_RUNTIME").as_deref() {
            Ok("apple") => Self::Apple,
            Ok("docker") => Self::Docker,
            Ok(value) => {
                panic!("AFPSQL_TEST_CONTAINER_RUNTIME must be apple or docker, got {value}")
            }
            Err(_) if cfg!(target_os = "macos") => Self::Apple,
            Err(_) => Self::Docker,
        }
    }

    pub fn name_flag(self) -> &'static str {
        match self {
            Self::Apple => "--container-apple-name",
            Self::Docker => "--container-docker-name",
        }
    }

    pub fn runtime_flag(self) -> &'static str {
        match self {
            Self::Apple => "--container-apple-runtime",
            Self::Docker => "--container-docker-runtime",
        }
    }

    pub fn published_port(self, private_port: u16) -> PublishedPort {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("reserve loopback test port");
        let host_port = listener
            .local_addr()
            .expect("read reserved loopback test port")
            .port();
        drop(listener);
        PublishedPort {
            spec: format!("127.0.0.1:{host_port}:{private_port}"),
            host_port: host_port.to_string(),
        }
    }

    pub fn network_host(self, container: &str) -> String {
        if self == Self::Docker {
            return container.to_string();
        }
        let output = Command::new(self.binary())
            .args(["inspect", container])
            .output()
            .expect("inspect Apple test container");
        assert!(
            output.status.success(),
            "inspect Apple test container failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let document: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("Apple container inspect JSON");
        document[0]["status"]["networks"][0]["ipv4Address"]
            .as_str()
            .and_then(|address| address.split('/').next())
            .filter(|address| !address.is_empty())
            .unwrap_or_else(|| panic!("Apple test container has no IPv4 address: {document}"))
            .to_string()
    }

    pub fn success(self, args: &[&str], context: &str) {
        let output_result = Command::new(self.binary()).args(args).output();
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

    pub fn exec_status(self, container: &str, command: &[&str]) -> bool {
        let mut args = vec!["exec", container];
        args.extend_from_slice(command);
        Command::new(self.binary())
            .args(args)
            .status()
            .is_ok_and(|status| status.success())
    }

    pub fn create_network(self, name: &str) {
        self.success(&["network", "create", name], "create container network");
    }

    pub fn delete_container(self, name: &str) {
        let args = match self {
            Self::Apple => vec!["delete", "--force", name],
            Self::Docker => vec!["rm", "-f", name],
        };
        let _ = Command::new(self.binary()).args(args).status();
    }

    pub fn delete_network(self, name: &str) {
        let args = match self {
            Self::Apple => vec!["network", "delete", name],
            Self::Docker => vec!["network", "rm", name],
        };
        let _ = Command::new(self.binary()).args(args).status();
    }

    fn binary(self) -> &'static str {
        match self {
            Self::Apple => "container",
            Self::Docker => "docker",
        }
    }
}
