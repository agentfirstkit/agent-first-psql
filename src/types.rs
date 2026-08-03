use agent_first_data::LogFilters;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Serialize `LogFilters` as its plain string array for `Output::Config` and
/// re-normalize on the (currently unused) deserialize path via afdata's parser,
/// so afpsql never keeps a second log-filter representation.
mod log_filters_serde {
    use agent_first_data::{LogFilters, cli_parse_log_filters};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(
        filters: &LogFilters,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        filters.as_slice().serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<LogFilters, D::Error> {
        let raw = Vec::<String>::deserialize(deserializer)?;
        Ok(cli_parse_log_filters(&raw))
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "code", deny_unknown_fields)]
pub enum Input {
    #[serde(rename = "query")]
    Query {
        id: String,
        #[serde(default)]
        session: Option<String>,
        sql: String,
        #[serde(default)]
        params: Vec<Value>,
        #[serde(default)]
        options: QueryOptions,
    },
    #[serde(rename = "config")]
    Config(ConfigPatch),
    #[serde(rename = "cancel")]
    Cancel { id: String },
    #[serde(rename = "ping")]
    Ping,
    #[serde(rename = "close")]
    Close,
    #[serde(rename = "session_info")]
    SessionInfo {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        session: Option<String>,
    },
    /// Open an explicit transaction on the named session. Subsequent
    /// `query` requests on that session run on the open transaction
    /// (no implicit `BEGIN..COMMIT` wrap) until `commit` or `rollback`.
    #[serde(rename = "begin")]
    Begin {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        session: Option<String>,
        /// When true, send `BEGIN READ ONLY`. Read-only is the default;
        /// read-write transactions require an explicit false value and a
        /// matching write permission.
        #[serde(default = "default_true")]
        read_only: bool,
        /// Pass `--permission write` (or matching ssh-write/container-write)
        /// to allow `BEGIN` on a session that defaults to read-only. Without
        /// it, an implicit-read session rejects the begin.
        #[serde(default)]
        permission: Option<Permission>,
    },
    #[serde(rename = "commit")]
    Commit {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        session: Option<String>,
    },
    #[serde(rename = "rollback")]
    Rollback {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        session: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Permission {
    #[serde(rename = "read")]
    Read,
    #[serde(rename = "write")]
    Write,
    #[serde(rename = "ssh-read")]
    SshRead,
    #[serde(rename = "ssh-write")]
    SshWrite,
    #[serde(rename = "container-read")]
    ContainerRead,
    #[serde(rename = "container-write")]
    ContainerWrite,
}

impl Permission {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::SshRead => "ssh-read",
            Self::SshWrite => "ssh-write",
            Self::ContainerRead => "container-read",
            Self::ContainerWrite => "container-write",
        }
    }

    pub fn is_read_only(self) -> bool {
        matches!(self, Self::Read | Self::SshRead | Self::ContainerRead)
    }

    pub fn allows_ssh(self) -> bool {
        matches!(self, Self::SshRead | Self::SshWrite)
    }

    pub fn allows_container(self) -> bool {
        matches!(self, Self::ContainerRead | Self::ContainerWrite)
    }
}

fn default_true() -> bool {
    true
}

impl std::str::FromStr for Permission {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "read" => Ok(Self::Read),
            "write" => Ok(Self::Write),
            "ssh-read" => Ok(Self::SshRead),
            "ssh-write" => Ok(Self::SshWrite),
            "container-read" => Ok(Self::ContainerRead),
            "container-write" => Ok(Self::ContainerWrite),
            _ => Err(format!(
                "invalid permission `{value}`; expected read, write, ssh-read, ssh-write, container-read, or container-write"
            )),
        }
    }
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct QueryOptions {
    #[serde(default)]
    pub stream_rows: bool,
    pub batch_rows: Option<usize>,
    pub batch_bytes: Option<usize>,
    pub statement_timeout_ms: Option<u64>,
    pub lock_timeout_ms: Option<u64>,
    pub permission: Option<Permission>,
    pub inline_max_rows: Option<usize>,
    pub inline_max_bytes: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "code")]
pub enum Output {
    #[serde(rename = "result")]
    Result {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        command_tag: String,
        columns: Vec<ColumnInfo>,
        rows: Vec<Value>,
        row_count: usize,
        /// True when `rows` is a prefix of the full result — emit when the
        /// inline row or byte cap was hit. Default-false serializes elided.
        #[serde(skip_serializing_if = "is_false", default)]
        truncated: bool,
        /// Inline-row cap if that's what fired.
        #[serde(skip_serializing_if = "Option::is_none")]
        truncated_at_rows: Option<usize>,
        /// Inline-byte cap if that's what fired.
        #[serde(skip_serializing_if = "Option::is_none")]
        truncated_at_bytes: Option<usize>,
        trace: Trace,
    },
    #[serde(rename = "result_start")]
    ResultStart {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        columns: Vec<ColumnInfo>,
    },
    #[serde(rename = "result_rows")]
    ResultRows {
        id: String,
        rows: Vec<Value>,
        rows_batch_count: usize,
    },
    #[serde(rename = "result_end")]
    ResultEnd {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        command_tag: String,
        trace: Trace,
    },
    #[serde(rename = "sql_error")]
    SqlError {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        sqlstate: String,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        hint: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        position: Option<String>,
        retryable: bool,
        trace: Trace,
    },
    #[serde(rename = "error")]
    Error {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        error_code: String,
        error: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        sqlstate: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        hint: Option<String>,
        retryable: bool,
        trace: Trace,
    },
    #[serde(rename = "dry_run")]
    DryRun {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        sql: String,
        params: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        /// Inferred PostgreSQL parameter types in placeholder order
        /// (`$1`, `$2`, ...). Populated when the server-side PREPARE succeeds.
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        param_types: Vec<String>,
        /// Output columns inferred from the prepared statement
        /// (empty for non-SELECT statements).
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        columns: Vec<ColumnInfo>,
        trace: Trace,
    },
    #[serde(rename = "config")]
    Config(RuntimeConfig),
    #[serde(rename = "pong")]
    Pong { trace: PongTrace },
    #[serde(rename = "close")]
    Close { message: String, trace: CloseTrace },
    #[serde(rename = "session_info")]
    SessionInfo {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        session: String,
        transport_kind: String,
        permission_default: String,
        stream_rows_default: bool,
        batch_rows: usize,
        batch_bytes: usize,
        inline_max_rows: usize,
        inline_max_bytes: usize,
        statement_timeout_ms: u64,
        lock_timeout_ms: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        database: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        user: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        host: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        port: Option<u16>,
        #[serde(skip_serializing_if = "Option::is_none")]
        server_version: Option<String>,
        trace: Trace,
    },
    #[serde(rename = "log")]
    Log {
        event: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error_code: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        command_tag: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        config: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        args: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        env: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        chain: Option<String>,
        trace: Trace,
    },
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Serialize, Clone)]
pub struct ColumnInfo {
    pub name: String,
    #[serde(rename = "type")]
    pub type_name: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct Trace {
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_bytes: Option<usize>,
}

impl Trace {
    pub fn only_duration(duration_ms: u64) -> Self {
        Self {
            duration_ms,
            row_count: None,
            payload_bytes: None,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct PongTrace {
    pub uptime_s: u64,
    pub requests_total: u64,
    pub in_flight: usize,
}

#[derive(Debug, Serialize)]
pub struct CloseTrace {
    pub uptime_s: u64,
    pub requests_total: u64,
}

#[derive(Debug, Clone, Default)]
pub struct SessionConfig {
    pub dsn_secret: Option<String>,
    pub conninfo_secret: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub dbname: Option<String>,
    pub password_secret: Option<String>,
    pub ssh: SshConfig,
    pub container: ContainerConfig,
    /// Set only when an administrator-locked readonly profile supplied this
    /// session, meaning the endpoint is the administrator's and environment
    /// variables must not redirect it.
    ///
    /// Deliberately absent from `SessionConfigFlat`, so it can never be set by
    /// profile JSON or by a pipe session patch — only by the code path that
    /// loads a locked profile.
    pub profile_pinned: bool,
}

#[derive(Serialize)]
struct SafeSessionConfig<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    dsn_secret: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    conninfo_secret: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    host: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dbname: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    password_secret: Option<&'static str>,
    #[serde(flatten)]
    ssh: &'a SshConfig,
    #[serde(flatten)]
    container: &'a ContainerConfig,
}

impl Serialize for SessionConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        SafeSessionConfig {
            dsn_secret: self.dsn_secret.as_ref().map(|_| "***"),
            conninfo_secret: self.conninfo_secret.as_ref().map(|_| "***"),
            host: self.host.as_deref(),
            port: self.port,
            user: self.user.as_deref(),
            dbname: self.dbname.as_deref(),
            password_secret: self.password_secret.as_ref().map(|_| "***"),
            ssh: &self.ssh,
            container: &self.container,
        }
        .serialize(serializer)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct SshConfig {
    #[serde(rename = "ssh", skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    #[serde(rename = "ssh_via", default, skip_serializing_if = "Vec::is_empty")]
    pub via: Vec<String>,
    #[serde(rename = "ssh_options", default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    #[serde(rename = "ssh_local_host", skip_serializing_if = "Option::is_none")]
    pub local_host: Option<String>,
    #[serde(rename = "ssh_local_port", skip_serializing_if = "Option::is_none")]
    pub local_port: Option<u16>,
    #[serde(rename = "ssh_remote_socket", skip_serializing_if = "Option::is_none")]
    pub remote_socket: Option<String>,
    #[serde(rename = "ssh_sudo_user", skip_serializing_if = "Option::is_none")]
    pub sudo_user: Option<String>,
}

impl SshConfig {
    pub fn has_transport_fields(&self) -> bool {
        self.destination.is_some()
            || !self.via.is_empty()
            || !self.options.is_empty()
            || self.local_host.is_some()
            || self.local_port.is_some()
            || self.remote_socket.is_some()
            || self.sudo_user.is_some()
    }

    pub fn has_tunnel_or_bridge_options(&self) -> bool {
        self.local_host.is_some()
            || self.local_port.is_some()
            || !self.via.is_empty()
            || self.remote_socket.is_some()
            || self.sudo_user.is_some()
    }
}

/// The five container exec drivers.
///
/// A driver is never named directly. It is inferred from which
/// `--container-<driver>-*` flag family (or matching session field family) the
/// caller used, so an option a driver cannot express has no spelling at all
/// rather than a runtime rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerDriver {
    Docker,
    Podman,
    Nerdctl,
    Compose,
    Kubectl,
}

impl ContainerDriver {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Docker => "docker",
            Self::Podman => "podman",
            Self::Nerdctl => "nerdctl",
            Self::Compose => "compose",
            Self::Kubectl => "kubectl",
        }
    }

    /// The command a driver execs through when the caller sets no runtime.
    pub fn default_runtime(self) -> &'static str {
        match self {
            Self::Docker | Self::Compose => "docker",
            Self::Podman => "podman",
            Self::Nerdctl => "nerdctl",
            Self::Kubectl => "kubectl",
        }
    }

    /// The flag that names this driver's exec target.
    pub fn target_flag(self) -> &'static str {
        match self {
            Self::Docker => "--container-docker-name",
            Self::Podman => "--container-podman-name",
            Self::Nerdctl => "--container-nerdctl-name",
            Self::Compose => "--container-compose-service",
            Self::Kubectl => "--container-kubectl-pod",
        }
    }

    /// How to spell this driver's whole flag family in a message.
    pub fn flag_family(self) -> &'static str {
        match self {
            Self::Docker => "--container-docker-*",
            Self::Podman => "--container-podman-*",
            Self::Nerdctl => "--container-nerdctl-*",
            Self::Compose => "--container-compose-*",
            Self::Kubectl => "--container-kubectl-*",
        }
    }
}

/// One flag family per driver, one field per (driver, option) pair.
///
/// Which options a driver supports is expressed by the field names themselves:
/// there is no `kubectl_user` because `kubectl exec` has no exec-as-user
/// option, and no `podman_context` because Podman has no context selection.
/// The only combination left to check is that a caller stayed inside one family.
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct ContainerConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docker_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docker_user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docker_context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docker_runtime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub podman_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub podman_user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub podman_runtime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nerdctl_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nerdctl_user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nerdctl_runtime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compose_service: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compose_user: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compose_files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compose_project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compose_runtime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kubectl_pod: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kubectl_container: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kubectl_namespace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kubectl_context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kubectl_runtime: Option<String>,
}

impl ContainerConfig {
    pub fn has_transport_fields(&self) -> bool {
        self.family_flags().iter().any(|(_, flag)| flag.is_some())
    }

    /// Which driver family the caller used, or `None` for no container
    /// transport at all.
    ///
    /// Two families at once is the one combination this surface still has to
    /// reject, and the error names both offending flags.
    pub fn selected_driver(&self) -> Result<Option<ContainerDriver>, String> {
        let mut selected: Option<(ContainerDriver, &'static str)> = None;
        for (driver, flag) in self.family_flags() {
            let Some(flag) = flag else { continue };
            match selected {
                None => selected = Some((driver, flag)),
                Some((_, first)) => {
                    return Err(format!(
                        "{first} cannot be combined with {flag}; each container driver has its own flag family"
                    ));
                }
            }
        }
        Ok(selected.map(|(driver, _)| driver))
    }

    /// The container, service, or pod this session execs into, whichever family
    /// named it.
    pub fn target_name(&self) -> Option<&str> {
        self.docker_name
            .as_deref()
            .or(self.podman_name.as_deref())
            .or(self.nerdctl_name.as_deref())
            .or(self.compose_service.as_deref())
            .or(self.kubectl_pod.as_deref())
    }

    /// The runtime command override, whichever family named it.
    pub fn runtime_override(&self) -> Option<&str> {
        self.docker_runtime
            .as_deref()
            .or(self.podman_runtime.as_deref())
            .or(self.nerdctl_runtime.as_deref())
            .or(self.compose_runtime.as_deref())
            .or(self.kubectl_runtime.as_deref())
    }

    /// For each driver, the first flag of its family the caller set.
    fn family_flags(&self) -> [(ContainerDriver, Option<&'static str>); 5] {
        [
            (
                ContainerDriver::Docker,
                first_present(&[
                    ("--container-docker-name", self.docker_name.is_some()),
                    ("--container-docker-user", self.docker_user.is_some()),
                    ("--container-docker-context", self.docker_context.is_some()),
                    ("--container-docker-runtime", self.docker_runtime.is_some()),
                ]),
            ),
            (
                ContainerDriver::Podman,
                first_present(&[
                    ("--container-podman-name", self.podman_name.is_some()),
                    ("--container-podman-user", self.podman_user.is_some()),
                    ("--container-podman-runtime", self.podman_runtime.is_some()),
                ]),
            ),
            (
                ContainerDriver::Nerdctl,
                first_present(&[
                    ("--container-nerdctl-name", self.nerdctl_name.is_some()),
                    ("--container-nerdctl-user", self.nerdctl_user.is_some()),
                    (
                        "--container-nerdctl-runtime",
                        self.nerdctl_runtime.is_some(),
                    ),
                ]),
            ),
            (
                ContainerDriver::Compose,
                first_present(&[
                    (
                        "--container-compose-service",
                        self.compose_service.is_some(),
                    ),
                    ("--container-compose-user", self.compose_user.is_some()),
                    ("--container-compose-file", !self.compose_files.is_empty()),
                    (
                        "--container-compose-project",
                        self.compose_project.is_some(),
                    ),
                    (
                        "--container-compose-runtime",
                        self.compose_runtime.is_some(),
                    ),
                ]),
            ),
            (
                ContainerDriver::Kubectl,
                first_present(&[
                    ("--container-kubectl-pod", self.kubectl_pod.is_some()),
                    (
                        "--container-kubectl-container",
                        self.kubectl_container.is_some(),
                    ),
                    (
                        "--container-kubectl-namespace",
                        self.kubectl_namespace.is_some(),
                    ),
                    (
                        "--container-kubectl-context",
                        self.kubectl_context.is_some(),
                    ),
                    (
                        "--container-kubectl-runtime",
                        self.kubectl_runtime.is_some(),
                    ),
                ]),
            ),
        ]
    }
}

fn first_present(fields: &[(&'static str, bool)]) -> Option<&'static str> {
    fields
        .iter()
        .find(|(_, present)| *present)
        .map(|(flag, _)| *flag)
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct SessionConfigFlat {
    #[serde(default)]
    dsn_secret: Option<String>,
    #[serde(default)]
    conninfo_secret: Option<String>,
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    dbname: Option<String>,
    #[serde(default)]
    password_secret: Option<String>,
    #[serde(default)]
    ssh: Option<String>,
    #[serde(default)]
    ssh_via: Vec<String>,
    #[serde(default)]
    ssh_options: Vec<String>,
    #[serde(default)]
    ssh_local_host: Option<String>,
    #[serde(default)]
    ssh_local_port: Option<u16>,
    #[serde(default)]
    ssh_remote_socket: Option<String>,
    #[serde(default)]
    ssh_sudo_user: Option<String>,
    #[serde(default)]
    docker_name: Option<String>,
    #[serde(default)]
    docker_user: Option<String>,
    #[serde(default)]
    docker_context: Option<String>,
    #[serde(default)]
    docker_runtime: Option<String>,
    #[serde(default)]
    podman_name: Option<String>,
    #[serde(default)]
    podman_user: Option<String>,
    #[serde(default)]
    podman_runtime: Option<String>,
    #[serde(default)]
    nerdctl_name: Option<String>,
    #[serde(default)]
    nerdctl_user: Option<String>,
    #[serde(default)]
    nerdctl_runtime: Option<String>,
    #[serde(default)]
    compose_service: Option<String>,
    #[serde(default)]
    compose_user: Option<String>,
    #[serde(default)]
    compose_files: Vec<String>,
    #[serde(default)]
    compose_project: Option<String>,
    #[serde(default)]
    compose_runtime: Option<String>,
    #[serde(default)]
    kubectl_pod: Option<String>,
    #[serde(default)]
    kubectl_container: Option<String>,
    #[serde(default)]
    kubectl_namespace: Option<String>,
    #[serde(default)]
    kubectl_context: Option<String>,
    #[serde(default)]
    kubectl_runtime: Option<String>,
}

impl From<SessionConfigFlat> for SessionConfig {
    fn from(flat: SessionConfigFlat) -> Self {
        Self {
            profile_pinned: false,
            dsn_secret: flat.dsn_secret,
            conninfo_secret: flat.conninfo_secret,
            host: flat.host,
            port: flat.port,
            user: flat.user,
            dbname: flat.dbname,
            password_secret: flat.password_secret,
            ssh: SshConfig {
                destination: flat.ssh,
                via: flat.ssh_via,
                options: flat.ssh_options,
                local_host: flat.ssh_local_host,
                local_port: flat.ssh_local_port,
                remote_socket: flat.ssh_remote_socket,
                sudo_user: flat.ssh_sudo_user,
            },
            container: ContainerConfig {
                docker_name: flat.docker_name,
                docker_user: flat.docker_user,
                docker_context: flat.docker_context,
                docker_runtime: flat.docker_runtime,
                podman_name: flat.podman_name,
                podman_user: flat.podman_user,
                podman_runtime: flat.podman_runtime,
                nerdctl_name: flat.nerdctl_name,
                nerdctl_user: flat.nerdctl_user,
                nerdctl_runtime: flat.nerdctl_runtime,
                compose_service: flat.compose_service,
                compose_user: flat.compose_user,
                compose_files: flat.compose_files,
                compose_project: flat.compose_project,
                compose_runtime: flat.compose_runtime,
                kubectl_pod: flat.kubectl_pod,
                kubectl_container: flat.kubectl_container,
                kubectl_namespace: flat.kubectl_namespace,
                kubectl_context: flat.kubectl_context,
                kubectl_runtime: flat.kubectl_runtime,
            },
        }
    }
}

impl<'de> Deserialize<'de> for SessionConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        SessionConfigFlat::deserialize(deserializer).map(Self::from)
    }
}

impl SessionConfig {
    pub fn uses_ssh_transport(&self) -> bool {
        self.ssh.has_transport_fields()
    }

    pub fn uses_container_transport(&self) -> bool {
        self.container.has_transport_fields()
    }

    pub fn transport_kind(&self) -> Result<TransportKind, String> {
        // Two driver families name two drivers, so the session has no container
        // transport to select until one of them goes. Rejected here so a pipe
        // session fails on the request rather than at connect time.
        self.container.selected_driver()?;
        let uses_ssh = self.uses_ssh_transport();
        let uses_container = self.uses_container_transport();
        match (uses_ssh, uses_container) {
            (false, false) => Ok(TransportKind::Direct),
            (true, false) => Ok(TransportKind::Ssh),
            (false, true) => Ok(TransportKind::Container),
            // --ssh plus a container driver family means "run container exec on
            // that remote host". The PostgreSQL connection still crosses the
            // container boundary.
            (true, true) => Ok(TransportKind::Container),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    Direct,
    Ssh,
    Container,
}

impl TransportKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Ssh => "ssh",
            Self::Container => "container",
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RuntimeConfig {
    pub default_session: String,
    #[serde(default)]
    pub sessions: HashMap<String, SessionConfig>,
    pub inline_max_rows: usize,
    pub inline_max_bytes: usize,
    pub statement_timeout_ms: u64,
    pub lock_timeout_ms: u64,
    #[serde(default, with = "log_filters_serde")]
    pub log: LogFilters,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        let mut sessions = HashMap::new();
        sessions.insert("default".to_string(), SessionConfig::default());
        Self {
            default_session: "default".to_string(),
            sessions,
            inline_max_rows: 1000,
            inline_max_bytes: 1_048_576,
            statement_timeout_ms: 30_000,
            lock_timeout_ms: 5_000,
            log: LogFilters::default(),
        }
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ConfigPatch {
    pub default_session: Option<String>,
    pub sessions: Option<HashMap<String, SessionConfigPatch>>,
    pub inline_max_rows: Option<usize>,
    pub inline_max_bytes: Option<usize>,
    pub statement_timeout_ms: Option<u64>,
    pub lock_timeout_ms: Option<u64>,
    pub log: Option<Vec<String>>,
}

#[derive(Debug, Default)]
pub enum PatchField<T> {
    #[default]
    Missing,
    Null,
    Value(T),
}

impl<'de, T> Deserialize<'de> for PatchField<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Option::<T>::deserialize(deserializer)?;
        match value {
            Some(value) => Ok(Self::Value(value)),
            None => Ok(Self::Null),
        }
    }
}

impl<T> PatchField<T> {
    pub fn into_update(self) -> Option<Option<T>> {
        match self {
            Self::Missing => None,
            Self::Null => Some(None),
            Self::Value(value) => Some(Some(value)),
        }
    }
}

#[derive(Debug, Default)]
pub struct SessionConfigPatch {
    pub dsn_secret: PatchField<String>,
    pub conninfo_secret: PatchField<String>,
    pub host: PatchField<String>,
    pub port: PatchField<u16>,
    pub user: PatchField<String>,
    pub dbname: PatchField<String>,
    pub password_secret: PatchField<String>,
    pub ssh: SshConfigPatch,
    pub container: ContainerConfigPatch,
}

#[derive(Debug, Default)]
pub struct SshConfigPatch {
    pub destination: PatchField<String>,
    pub via: PatchField<Vec<String>>,
    pub options: PatchField<Vec<String>>,
    pub local_host: PatchField<String>,
    pub local_port: PatchField<u16>,
    pub remote_socket: PatchField<String>,
    pub sudo_user: PatchField<String>,
}

#[derive(Debug, Default)]
pub struct ContainerConfigPatch {
    pub docker_name: PatchField<String>,
    pub docker_user: PatchField<String>,
    pub docker_context: PatchField<String>,
    pub docker_runtime: PatchField<String>,
    pub podman_name: PatchField<String>,
    pub podman_user: PatchField<String>,
    pub podman_runtime: PatchField<String>,
    pub nerdctl_name: PatchField<String>,
    pub nerdctl_user: PatchField<String>,
    pub nerdctl_runtime: PatchField<String>,
    pub compose_service: PatchField<String>,
    pub compose_user: PatchField<String>,
    pub compose_files: PatchField<Vec<String>>,
    pub compose_project: PatchField<String>,
    pub compose_runtime: PatchField<String>,
    pub kubectl_pod: PatchField<String>,
    pub kubectl_container: PatchField<String>,
    pub kubectl_namespace: PatchField<String>,
    pub kubectl_context: PatchField<String>,
    pub kubectl_runtime: PatchField<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct SessionConfigPatchFlat {
    #[serde(default)]
    dsn_secret: PatchField<String>,
    #[serde(default)]
    conninfo_secret: PatchField<String>,
    #[serde(default)]
    host: PatchField<String>,
    #[serde(default)]
    port: PatchField<u16>,
    #[serde(default)]
    user: PatchField<String>,
    #[serde(default)]
    dbname: PatchField<String>,
    #[serde(default)]
    password_secret: PatchField<String>,
    #[serde(default)]
    ssh: PatchField<String>,
    #[serde(default)]
    ssh_via: PatchField<Vec<String>>,
    #[serde(default)]
    ssh_options: PatchField<Vec<String>>,
    #[serde(default)]
    ssh_local_host: PatchField<String>,
    #[serde(default)]
    ssh_local_port: PatchField<u16>,
    #[serde(default)]
    ssh_remote_socket: PatchField<String>,
    #[serde(default)]
    ssh_sudo_user: PatchField<String>,
    #[serde(default)]
    docker_name: PatchField<String>,
    #[serde(default)]
    docker_user: PatchField<String>,
    #[serde(default)]
    docker_context: PatchField<String>,
    #[serde(default)]
    docker_runtime: PatchField<String>,
    #[serde(default)]
    podman_name: PatchField<String>,
    #[serde(default)]
    podman_user: PatchField<String>,
    #[serde(default)]
    podman_runtime: PatchField<String>,
    #[serde(default)]
    nerdctl_name: PatchField<String>,
    #[serde(default)]
    nerdctl_user: PatchField<String>,
    #[serde(default)]
    nerdctl_runtime: PatchField<String>,
    #[serde(default)]
    compose_service: PatchField<String>,
    #[serde(default)]
    compose_user: PatchField<String>,
    #[serde(default)]
    compose_files: PatchField<Vec<String>>,
    #[serde(default)]
    compose_project: PatchField<String>,
    #[serde(default)]
    compose_runtime: PatchField<String>,
    #[serde(default)]
    kubectl_pod: PatchField<String>,
    #[serde(default)]
    kubectl_container: PatchField<String>,
    #[serde(default)]
    kubectl_namespace: PatchField<String>,
    #[serde(default)]
    kubectl_context: PatchField<String>,
    #[serde(default)]
    kubectl_runtime: PatchField<String>,
}

impl From<SessionConfigPatchFlat> for SessionConfigPatch {
    fn from(flat: SessionConfigPatchFlat) -> Self {
        Self {
            dsn_secret: flat.dsn_secret,
            conninfo_secret: flat.conninfo_secret,
            host: flat.host,
            port: flat.port,
            user: flat.user,
            dbname: flat.dbname,
            password_secret: flat.password_secret,
            ssh: SshConfigPatch {
                destination: flat.ssh,
                via: flat.ssh_via,
                options: flat.ssh_options,
                local_host: flat.ssh_local_host,
                local_port: flat.ssh_local_port,
                remote_socket: flat.ssh_remote_socket,
                sudo_user: flat.ssh_sudo_user,
            },
            container: ContainerConfigPatch {
                docker_name: flat.docker_name,
                docker_user: flat.docker_user,
                docker_context: flat.docker_context,
                docker_runtime: flat.docker_runtime,
                podman_name: flat.podman_name,
                podman_user: flat.podman_user,
                podman_runtime: flat.podman_runtime,
                nerdctl_name: flat.nerdctl_name,
                nerdctl_user: flat.nerdctl_user,
                nerdctl_runtime: flat.nerdctl_runtime,
                compose_service: flat.compose_service,
                compose_user: flat.compose_user,
                compose_files: flat.compose_files,
                compose_project: flat.compose_project,
                compose_runtime: flat.compose_runtime,
                kubectl_pod: flat.kubectl_pod,
                kubectl_container: flat.kubectl_container,
                kubectl_namespace: flat.kubectl_namespace,
                kubectl_context: flat.kubectl_context,
                kubectl_runtime: flat.kubectl_runtime,
            },
        }
    }
}

impl<'de> Deserialize<'de> for SessionConfigPatch {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        SessionConfigPatchFlat::deserialize(deserializer).map(Self::from)
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedOptions {
    pub stream_rows: bool,
    pub batch_rows: usize,
    pub batch_bytes: usize,
    pub statement_timeout_ms: u64,
    pub lock_timeout_ms: u64,
    pub read_only: bool,
    pub inline_max_rows: usize,
    pub inline_max_bytes: usize,
}

#[cfg(test)]
#[path = "../tests/support/unit_types.rs"]
mod tests;
