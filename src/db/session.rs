use crate::conn::resolve_pg_config;
use crate::types::{SessionConfig, TransportKind};

use super::errors::{map_connect_error, ConnectError, ExecError};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{Mutex, RwLock};

pub type CancelSlot = Arc<CancelState>;
pub(super) type SessionMap = RwLock<HashMap<String, Arc<SessionEntry>>>;
const SESSION_SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

pub struct CancelState {
    token: Mutex<Option<tokio_postgres::CancelToken>>,
    backend_pid: Mutex<Option<i32>>,
    session_cfg: Mutex<Option<SessionConfig>>,
    cancelled: AtomicBool,
}

pub(super) struct SessionEntry {
    pub(super) client: Mutex<Option<SessionClient>>,
}

pub(super) struct SessionClient {
    pub(super) client: Option<tokio_postgres::Client>,
    pub(super) backend_pid: i32,
    connection_task: Option<tokio::task::JoinHandle<()>>,
    ssh_tunnel: Option<crate::ssh_transport::SshTunnelGuard>,
    ssh_bridge: Option<crate::ssh_transport::SshBridgeGuard>,
    container_bridge: Option<crate::container_transport::ContainerBridgeGuard>,
}

pub(super) struct TransportSelection {
    pub(super) duration_ms: u64,
}

impl Drop for SessionClient {
    fn drop(&mut self) {
        if let Some(task) = self.connection_task.as_ref() {
            task.abort();
        }
    }
}

pub fn new_cancel_slot() -> CancelSlot {
    Arc::new(CancelState {
        token: Mutex::new(None),
        backend_pid: Mutex::new(None),
        session_cfg: Mutex::new(None),
        cancelled: AtomicBool::new(false),
    })
}

pub async fn cancel_query(slot: &CancelSlot) -> Result<bool, String> {
    slot.cancelled.store(true, Ordering::SeqCst);
    let token = slot.token.lock().await.clone();
    let token_attempted = token.is_some();
    let token_error = match token {
        Some(token) => {
            let tls =
                make_supported_tls().map_err(|e| format!("create TLS connector failed: {e}"))?;
            token
                .cancel_query(tls)
                .await
                .err()
                .map(|e| format!("server-side cancel failed: {e}"))
        }
        None => None,
    };

    let backend_cancel = cancel_backend_from_slot(slot).await;
    if matches!(backend_cancel, Ok(true)) {
        return Ok(true);
    }

    if let Some(token_error) = token_error {
        if let Err(backend_error) = backend_cancel {
            return Err(format!("{token_error}; fallback {backend_error}"));
        }
        return Err(token_error);
    }

    if token_attempted {
        return Ok(true);
    }

    backend_cancel
}

impl CancelState {
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    pub(super) async fn set_context(
        &self,
        token: tokio_postgres::CancelToken,
        backend_pid: i32,
        session_cfg: &SessionConfig,
    ) {
        *self.token.lock().await = Some(token);
        *self.backend_pid.lock().await = Some(backend_pid);
        *self.session_cfg.lock().await = Some(session_cfg.clone());
    }
}

pub(super) fn new_session_map() -> SessionMap {
    RwLock::new(HashMap::new())
}

pub(super) async fn get_session(sessions: &SessionMap, session_name: &str) -> Arc<SessionEntry> {
    if let Some(entry) = sessions.read().await.get(session_name) {
        return entry.clone();
    }

    let mut sessions = sessions.write().await;
    sessions
        .entry(session_name.to_string())
        .or_insert_with(|| {
            Arc::new(SessionEntry {
                client: Mutex::new(None),
            })
        })
        .clone()
}

pub(super) async fn shutdown_all_sessions(sessions: &SessionMap) {
    let entries: Vec<Arc<SessionEntry>> = sessions.write().await.drain().map(|(_, v)| v).collect();
    shutdown_entries(entries).await;
}

pub(super) async fn remove_sessions(sessions: &SessionMap, session_names: &[String]) {
    if session_names.is_empty() {
        return;
    }
    let entries: Vec<Arc<SessionEntry>> = {
        let mut sessions = sessions.write().await;
        session_names
            .iter()
            .filter_map(|name| sessions.remove(name))
            .collect()
    };
    shutdown_entries(entries).await;
}

async fn shutdown_entries(entries: Vec<Arc<SessionEntry>>) {
    for entry in entries {
        let client = entry.client.lock().await.take();
        if let Some(client) = client {
            client.shutdown().await;
        }
    }
}

pub(super) async fn connect_session(
    cfg: &SessionConfig,
) -> Result<(SessionClient, TransportSelection), ExecError> {
    let start = Instant::now();
    let transport = cfg
        .transport_kind()
        .map_err(|e| ExecError::Connect(Box::new(ConnectError::from(e))))?;
    if transport == TransportKind::Container {
        let (client, bridge) = crate::container_transport::connect_stdio_bridge(cfg)
            .await
            .map_err(|e| ExecError::Connect(Box::new(e)))?;
        let backend_pid = fetch_backend_pid(&client).await?;
        return Ok((
            SessionClient {
                client: Some(client),
                backend_pid,
                connection_task: None,
                ssh_tunnel: None,
                ssh_bridge: None,
                container_bridge: Some(bridge),
            },
            TransportSelection {
                duration_ms: start.elapsed().as_millis() as u64,
            },
        ));
    }

    if crate::ssh_transport::needs_stdio_bridge(cfg) {
        let (client, bridge) = crate::ssh_transport::connect_stdio_bridge(cfg)
            .await
            .map_err(|e| ExecError::Connect(Box::new(ConnectError::from(e))))?;
        let backend_pid = fetch_backend_pid(&client).await?;
        return Ok((
            SessionClient {
                client: Some(client),
                backend_pid,
                connection_task: None,
                ssh_tunnel: None,
                ssh_bridge: Some(bridge),
                container_bridge: None,
            },
            TransportSelection {
                duration_ms: start.elapsed().as_millis() as u64,
            },
        ));
    }

    let (connect_cfg, ssh_tunnel) = crate::ssh_transport::prepare_session(cfg)
        .await
        .map_err(|e| ExecError::Connect(Box::new(ConnectError::from(e))))?;
    let pg_cfg = resolve_pg_config(&connect_cfg).map_err(ExecError::from)?;
    let tls = make_supported_tls().map_err(|e| {
        ExecError::Connect(Box::new(ConnectError::new(format!(
            "create TLS connector failed: {e}"
        ))))
    })?;
    let (client, connection) = pg_cfg.connect(tls).await.map_err(map_connect_error)?;
    let connection_task = tokio::spawn(async move {
        let _ = connection.await;
    });
    let backend_pid = fetch_backend_pid(&client).await?;

    Ok((
        SessionClient {
            client: Some(client),
            backend_pid,
            connection_task: Some(connection_task),
            ssh_tunnel,
            ssh_bridge: None,
            container_bridge: None,
        },
        TransportSelection {
            duration_ms: start.elapsed().as_millis() as u64,
        },
    ))
}

pub(super) fn transport_chain_summary(cfg: &SessionConfig, reveal_targets: bool) -> String {
    match cfg.transport_kind().unwrap_or(TransportKind::Direct) {
        TransportKind::Direct => postgres_endpoint_summary(cfg, reveal_targets),
        TransportKind::Ssh => {
            let ssh = if reveal_targets {
                cfg.ssh
                    .destination
                    .as_deref()
                    .map(|destination| format!("ssh:{destination}"))
                    .unwrap_or_else(|| "ssh".to_string())
            } else {
                "ssh".to_string()
            };
            format!(
                "{ssh} -> {}",
                postgres_endpoint_summary(cfg, reveal_targets)
            )
        }
        TransportKind::Container => {
            let mut parts = Vec::new();
            if cfg.ssh.destination.is_some() {
                parts.push(if reveal_targets {
                    format!("ssh:{}", cfg.ssh.destination.as_deref().unwrap_or_default())
                } else {
                    "ssh".to_string()
                });
            }
            parts.push(container_exec_summary(cfg, reveal_targets));
            parts.push(postgres_endpoint_summary(cfg, reveal_targets));
            parts.join(" -> ")
        }
    }
}

fn container_exec_summary(cfg: &SessionConfig, reveal_targets: bool) -> String {
    let driver = cfg.container.driver.as_deref().unwrap_or("docker");
    let driver = match driver {
        "kubernetes" | "k8s" => "kubectl",
        "docker-compose" => "compose",
        other => other,
    };
    let target = cfg.container.target.as_deref().unwrap_or("target");
    match (reveal_targets, cfg.container.pod_container.as_deref()) {
        (true, Some(container)) if matches!(driver, "kubectl") => {
            format!("{driver} exec {target} -c {container}")
        }
        (true, _) => format!("{driver} exec {target}"),
        (false, _) => format!("{driver} exec"),
    }
}

fn postgres_endpoint_summary(cfg: &SessionConfig, reveal_targets: bool) -> String {
    let Ok(pg_cfg) = resolve_pg_config(cfg) else {
        return if reveal_targets {
            "postgres".to_string()
        } else {
            "tcp".to_string()
        };
    };
    let port = pg_cfg.get_ports().first().copied().unwrap_or(5432);
    match pg_cfg.get_hosts().first() {
        Some(tokio_postgres::config::Host::Tcp(host)) if reveal_targets => {
            if host.starts_with('/') {
                format!("unix {host}/.s.PGSQL.{port}")
            } else {
                format!("tcp {host}:{port}")
            }
        }
        Some(tokio_postgres::config::Host::Tcp(host)) if host.starts_with('/') => {
            "unix".to_string()
        }
        #[cfg(unix)]
        Some(tokio_postgres::config::Host::Unix(path)) if reveal_targets => {
            format!("unix {}/.s.PGSQL.{port}", path.to_string_lossy())
        }
        #[cfg(unix)]
        Some(tokio_postgres::config::Host::Unix(_)) => "unix".to_string(),
        _ if reveal_targets => format!("tcp 127.0.0.1:{port}"),
        _ => "tcp".to_string(),
    }
}

async fn fetch_backend_pid(client: &tokio_postgres::Client) -> Result<i32, ExecError> {
    let row = client
        .query_one("select pg_backend_pid()", &[])
        .await
        .map_err(super::errors::map_pg_error)?;
    row.try_get(0)
        .map_err(|e| ExecError::Internal(format!("decode backend pid failed: {e}")))
}

async fn cancel_backend_from_slot(slot: &CancelSlot) -> Result<bool, String> {
    let backend_pid = *slot.backend_pid.lock().await;
    let session_cfg = slot.session_cfg.lock().await.clone();
    let (Some(backend_pid), Some(session_cfg)) = (backend_pid, session_cfg) else {
        return Ok(false);
    };

    let client = connect_session(&session_cfg)
        .await
        .map_err(|e| format!("pg_cancel_backend connect failed: {e:?}"))?;
    let result = async {
        let Some(pg_client) = client.0.client.as_ref() else {
            return Err("pg_cancel_backend connection unavailable".to_string());
        };
        let row = pg_client
            .query_one("select pg_cancel_backend($1)", &[&backend_pid])
            .await
            .map_err(|e| format!("pg_cancel_backend failed: {e}"))?;
        row.try_get(0)
            .map_err(|e| format!("decode pg_cancel_backend result failed: {e}"))
    }
    .await;
    client.0.shutdown().await;
    result
}

impl SessionClient {
    pub(super) fn is_closed(&self) -> bool {
        if self.client.is_none() {
            return true;
        }
        self.connection_task
            .as_ref()
            .map(|task| task.is_finished())
            .unwrap_or(false)
            || self
                .ssh_bridge
                .as_ref()
                .map(crate::ssh_transport::SshBridgeGuard::is_finished)
                .unwrap_or(false)
            || self
                .container_bridge
                .as_ref()
                .map(crate::container_transport::ContainerBridgeGuard::is_finished)
                .unwrap_or(false)
    }

    pub(super) async fn shutdown(mut self) {
        // Let tokio-postgres send Terminate; aborting the driver leaves the
        // backend visible until TCP cleanup notices the dead client.
        drop(self.client.take());
        if let Some(task) = self.connection_task.take() {
            let mut task = task;
            if tokio::time::timeout(SESSION_SHUTDOWN_TIMEOUT, &mut task)
                .await
                .is_err()
            {
                task.abort();
            }
        }
        if let Some(tunnel) = self.ssh_tunnel.take() {
            drop(tunnel);
        }
        if let Some(bridge) = self.ssh_bridge.take() {
            bridge.shutdown(SESSION_SHUTDOWN_TIMEOUT).await;
        }
        if let Some(bridge) = self.container_bridge.take() {
            bridge.shutdown(SESSION_SHUTDOWN_TIMEOUT).await;
        }
    }
}

fn make_supported_tls() -> Result<postgres_native_tls::MakeTlsConnector, native_tls::Error> {
    let tls = native_tls::TlsConnector::builder()
        // Supported sslmode=prefer/require encrypts without certificate
        // verification. verify-ca/verify-full are rejected during config parse.
        .danger_accept_invalid_certs(true)
        .danger_accept_invalid_hostnames(true)
        .build()?;
    Ok(postgres_native_tls::MakeTlsConnector::new(tls))
}
