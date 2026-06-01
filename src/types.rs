use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

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
        /// When true, send `BEGIN READ ONLY`. Default is read-write so the
        /// caller can run writes; per-query permission still gates the SQL.
        #[serde(default)]
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
#[allow(dead_code)]
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

#[derive(Debug, Serialize, Clone, Default)]
pub struct SessionConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dsn_secret: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conninfo_secret: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dbname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password_secret: Option<String>,
    #[serde(flatten)]
    pub ssh: SshConfig,
    #[serde(flatten)]
    pub container: ContainerConfig,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct SshConfig {
    #[serde(rename = "ssh", skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
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
            || !self.options.is_empty()
            || self.local_host.is_some()
            || self.local_port.is_some()
            || self.remote_socket.is_some()
            || self.sudo_user.is_some()
    }

    pub fn has_tunnel_or_bridge_options(&self) -> bool {
        self.local_host.is_some()
            || self.local_port.is_some()
            || self.remote_socket.is_some()
            || self.sudo_user.is_some()
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct ContainerConfig {
    #[serde(rename = "container", skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(rename = "container_driver", skip_serializing_if = "Option::is_none")]
    pub driver: Option<String>,
    #[serde(rename = "container_runtime", skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    #[serde(rename = "container_user", skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(
        rename = "container_namespace",
        skip_serializing_if = "Option::is_none"
    )]
    pub namespace: Option<String>,
    #[serde(rename = "container_context", skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(
        rename = "container_compose_files",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub compose_files: Vec<String>,
    #[serde(
        rename = "container_compose_project",
        skip_serializing_if = "Option::is_none"
    )]
    pub compose_project: Option<String>,
    #[serde(
        rename = "container_pod_container",
        skip_serializing_if = "Option::is_none"
    )]
    pub pod_container: Option<String>,
}

impl ContainerConfig {
    pub fn has_transport_fields(&self) -> bool {
        self.target.is_some()
            || self.driver.is_some()
            || self.runtime.is_some()
            || self.user.is_some()
            || self.namespace.is_some()
            || self.context.is_some()
            || !self.compose_files.is_empty()
            || self.compose_project.is_some()
            || self.pod_container.is_some()
    }
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
    container: Option<String>,
    #[serde(default)]
    container_driver: Option<String>,
    #[serde(default)]
    container_runtime: Option<String>,
    #[serde(default)]
    container_user: Option<String>,
    #[serde(default)]
    container_namespace: Option<String>,
    #[serde(default)]
    container_context: Option<String>,
    #[serde(default)]
    container_compose_files: Vec<String>,
    #[serde(default)]
    container_compose_project: Option<String>,
    #[serde(default)]
    container_pod_container: Option<String>,
}

impl From<SessionConfigFlat> for SessionConfig {
    fn from(flat: SessionConfigFlat) -> Self {
        Self {
            dsn_secret: flat.dsn_secret,
            conninfo_secret: flat.conninfo_secret,
            host: flat.host,
            port: flat.port,
            user: flat.user,
            dbname: flat.dbname,
            password_secret: flat.password_secret,
            ssh: SshConfig {
                destination: flat.ssh,
                options: flat.ssh_options,
                local_host: flat.ssh_local_host,
                local_port: flat.ssh_local_port,
                remote_socket: flat.ssh_remote_socket,
                sudo_user: flat.ssh_sudo_user,
            },
            container: ContainerConfig {
                target: flat.container,
                driver: flat.container_driver,
                runtime: flat.container_runtime,
                user: flat.container_user,
                namespace: flat.container_namespace,
                context: flat.container_context,
                compose_files: flat.container_compose_files,
                compose_project: flat.container_compose_project,
                pod_container: flat.container_pod_container,
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
        let uses_ssh = self.uses_ssh_transport();
        let uses_container = self.uses_container_transport();
        match (uses_ssh, uses_container) {
            (false, false) => Ok(TransportKind::Direct),
            (true, false) => Ok(TransportKind::Ssh),
            (false, true) => Ok(TransportKind::Container),
            // --ssh + --container means "run container exec on that remote host".
            // The PostgreSQL connection still crosses the container boundary.
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
    #[serde(default)]
    pub log: Vec<String>,
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
            log: vec![],
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
    pub options: PatchField<Vec<String>>,
    pub local_host: PatchField<String>,
    pub local_port: PatchField<u16>,
    pub remote_socket: PatchField<String>,
    pub sudo_user: PatchField<String>,
}

#[derive(Debug, Default)]
pub struct ContainerConfigPatch {
    pub target: PatchField<String>,
    pub driver: PatchField<String>,
    pub runtime: PatchField<String>,
    pub user: PatchField<String>,
    pub namespace: PatchField<String>,
    pub context: PatchField<String>,
    pub compose_files: PatchField<Vec<String>>,
    pub compose_project: PatchField<String>,
    pub pod_container: PatchField<String>,
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
    container: PatchField<String>,
    #[serde(default)]
    container_driver: PatchField<String>,
    #[serde(default)]
    container_runtime: PatchField<String>,
    #[serde(default)]
    container_user: PatchField<String>,
    #[serde(default)]
    container_namespace: PatchField<String>,
    #[serde(default)]
    container_context: PatchField<String>,
    #[serde(default)]
    container_compose_files: PatchField<Vec<String>>,
    #[serde(default)]
    container_compose_project: PatchField<String>,
    #[serde(default)]
    container_pod_container: PatchField<String>,
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
                options: flat.ssh_options,
                local_host: flat.ssh_local_host,
                local_port: flat.ssh_local_port,
                remote_socket: flat.ssh_remote_socket,
                sudo_user: flat.ssh_sudo_user,
            },
            container: ContainerConfigPatch {
                target: flat.container,
                driver: flat.container_driver,
                runtime: flat.container_runtime,
                user: flat.container_user,
                namespace: flat.container_namespace,
                context: flat.container_context,
                compose_files: flat.container_compose_files,
                compose_project: flat.container_compose_project,
                pod_container: flat.container_pod_container,
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
#[allow(dead_code)]
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
