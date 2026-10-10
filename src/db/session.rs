use crate::conn::{make_supported_tls, resolve_pg_config};
use crate::types::{SessionConfig, TransportKind};

use super::errors::{ConnectError, ExecError, map_connect_error};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
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
    /// Set by the first emitter (handler or cancel dispatcher) to "win" the
    /// terminal output for a query id. Guarantees the agent sees exactly one
    /// terminal event (`result` / `sql_error` / cancelled `error`) per id.
    terminal_emitted: AtomicBool,
}

pub(super) struct SessionEntry {
    pub(super) client: Mutex<Option<SessionClient>>,
    /// 0 means idle, 1 read-write, 2 read-only, and 3 failed/lost.
    /// A failed transaction blocks queries until commit/rollback settles it.
    /// While an explicit `BEGIN` is open on this session's connection, the
    /// executor bypasses its implicit `BEGIN..COMMIT` wrap and runs queries
    /// directly on the open transaction.
    pub(super) explicit_tx: AtomicU8,
}

pub(super) struct SessionClient {
    pub(super) client: Option<tokio_postgres::Client>,
    pub(super) backend_pid: i32,
    connection_task: Option<tokio::task::JoinHandle<()>>,
    ssh_bridge: Option<crate::ssh_transport::SshBridgeGuard>,
    container_bridge: Option<crate::container_transport::ContainerBridgeGuard>,
}

pub(super) struct TransportSelection {
    pub(super) duration_ms: u64,
}

struct ConnectionDriverGuard {
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for ConnectionDriverGuard {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
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
        terminal_emitted: AtomicBool::new(false),
    })
}

pub async fn cancel_query(slot: &CancelSlot) -> Result<bool, String> {
    slot.cancelled.store(true, Ordering::SeqCst);
    let token = slot.token.lock().await.clone();
    let token_error = match token {
        Some(token) => {
            let tls =
                make_supported_tls().map_err(|e| format!("create TLS connector failed: {e}"))?;
            match token.cancel_query(tls).await {
                Ok(()) => return Ok(true),
                Err(error) => Some(format!("server-side cancel failed: {error}")),
            }
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

    backend_cancel
}

impl CancelState {
    #[cfg(test)]
    pub(crate) async fn set_fallback_context_for_test(&self, cfg: SessionConfig) {
        *self.backend_pid.lock().await = Some(1);
        *self.session_cfg.lock().await = Some(cfg);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Record that a cancel was asked for without sending anything.
    ///
    /// Used for a query the worker had not started: there is no backend to ask,
    /// and the flag is what stops the executor from starting one.
    pub fn request_cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Atomic claim for the terminal emission for this query id. Returns
    /// `true` exactly once across all concurrent callers (handler emitting
    /// result/sql_error, cancel dispatcher emitting cancelled error). The
    /// "loser" must skip its planned emission so the agent sees one
    /// terminal event per id, never two.
    pub fn claim_terminal_emit(&self) -> bool {
        self.terminal_emitted
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
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

impl SessionEntry {
    pub(super) fn explicit_tx_active(&self) -> bool {
        self.explicit_tx.load(Ordering::SeqCst) != 0
    }

    pub(super) fn explicit_tx_failed(&self) -> bool {
        self.explicit_tx.load(Ordering::SeqCst) == 3
    }

    pub(super) fn fail_explicit_tx(&self) {
        self.explicit_tx.store(3, Ordering::SeqCst);
    }

    pub(super) fn explicit_tx_read_only(&self) -> Option<bool> {
        match self.explicit_tx.load(Ordering::SeqCst) {
            1 => Some(false),
            2 => Some(true),
            _ => None,
        }
    }

    pub(super) fn set_explicit_tx(&self, read_only: Option<bool>) {
        let state = match read_only {
            None => 0,
            Some(false) => 1,
            Some(true) => 2,
        };
        self.explicit_tx.store(state, Ordering::SeqCst);
    }
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
                explicit_tx: AtomicU8::new(0),
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
                ssh_bridge: None,
                container_bridge: Some(bridge),
            },
            TransportSelection {
                duration_ms: start.elapsed().as_millis() as u64,
            },
        ));
    }

    let pg_cfg = resolve_pg_config(cfg).map_err(ExecError::from)?;
    // A refused SSH setting ends the connection attempt here. Falling through
    // to the direct connect below would reach whatever answers on the local
    // host and port instead of the endpoint the caller named behind SSH.
    let needs_ssh_bridge = crate::ssh_transport::needs_stdio_bridge(cfg)
        .map_err(|e| ExecError::Connect(Box::new(ConnectError::from(e))))?;
    if needs_ssh_bridge {
        let (client, bridge) = crate::ssh_transport::connect_stdio_bridge(cfg, &pg_cfg)
            .await
            .map_err(|e| ExecError::Connect(Box::new(ConnectError::from(e))))?;
        let backend_pid = fetch_backend_pid(&client).await?;
        return Ok((
            SessionClient {
                client: Some(client),
                backend_pid,
                connection_task: None,
                ssh_bridge: Some(bridge),
                container_bridge: None,
            },
            TransportSelection {
                duration_ms: start.elapsed().as_millis() as u64,
            },
        ));
    }

    let tls = make_supported_tls().map_err(|e| {
        ExecError::Connect(Box::new(ConnectError::new(format!(
            "create TLS connector failed: {e}"
        ))))
    })?;
    let (client, connection) = pg_cfg.connect(tls).await.map_err(map_connect_error)?;
    let mut connection_driver = ConnectionDriverGuard {
        task: Some(tokio::spawn(async move {
            let _ = connection.await;
        })),
    };
    let backend_pid = fetch_backend_pid(&client).await?;

    Ok((
        SessionClient {
            client: Some(client),
            backend_pid,
            connection_task: connection_driver.task.take(),
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
                let mut hops = cfg
                    .ssh
                    .via
                    .iter()
                    .map(|hop| format!("ssh:{hop}"))
                    .collect::<Vec<_>>();
                if let Some(destination) = cfg.ssh.destination.as_deref() {
                    hops.push(format!("ssh:{destination}"));
                }
                if hops.is_empty() {
                    "ssh".to_string()
                } else {
                    hops.join(" -> ")
                }
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
    let driver = cfg
        .container
        .selected_driver()
        .ok()
        .flatten()
        .map_or("docker", crate::types::ContainerDriver::as_str);
    let target = cfg.container.target_name().unwrap_or("target");
    // A pod container can only have come from the kubectl family, so the driver
    // needs no second check here.
    match (reveal_targets, cfg.container.kubectl_container.as_deref()) {
        (true, Some(container)) => format!("{driver} exec {target} -c {container}"),
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
        .map_err(|e| format!("pg_cancel_backend connect failed: {e}"))?;
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
        // Keep the handle in `self` until it has settled: an outer shutdown
        // deadline can cancel this future while it is awaiting the driver, and
        // Drop must still be able to abort that driver.
        if let Some(task) = self.connection_task.as_mut()
            && tokio::time::timeout(SESSION_SHUTDOWN_TIMEOUT, &mut *task)
                .await
                .is_err()
        {
            task.abort();
            let _joined = task.await;
        }
        drop(self.connection_task.take());
        if let Some(bridge) = self.ssh_bridge.take() {
            bridge.shutdown(SESSION_SHUTDOWN_TIMEOUT).await;
        }
        if let Some(bridge) = self.container_bridge.take() {
            bridge.shutdown(SESSION_SHUTDOWN_TIMEOUT).await;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn a_successful_cancel_token_does_not_open_a_fallback_connection() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let (mut initial, _) = listener.accept().await.unwrap();
            let length = initial.read_u32().await.unwrap();
            let mut startup = vec![0; (length - 4) as usize];
            initial.read_exact(&mut startup).await.unwrap();
            initial
                .write_all(&[
                    b'R', 0, 0, 0, 8, 0, 0, 0, 0, b'K', 0, 0, 0, 12, 0, 0, 0, 1, 0, 0, 0, 2, b'Z',
                    0, 0, 0, 5, b'I',
                ])
                .await
                .unwrap();
            let (mut cancel, _) = listener.accept().await.unwrap();
            assert_eq!(cancel.read_u32().await.unwrap(), 16);
            assert_eq!(cancel.read_u32().await.unwrap(), 80_877_102);
            assert_eq!(cancel.read_u32().await.unwrap(), 1);
            assert_eq!(cancel.read_u32().await.unwrap(), 2);
            drop(cancel);
            assert!(
                tokio::time::timeout(Duration::from_millis(100), listener.accept())
                    .await
                    .is_err(),
                "successful token cancellation must not open a second cancel connection"
            );
        });
        let (client, connection) = tokio_postgres::connect(
            &format!("postgresql://test@{address}/test?sslmode=disable"),
            tokio_postgres::NoTls,
        )
        .await
        .unwrap();
        let driver = tokio::spawn(connection);
        let slot = new_cancel_slot();
        *slot.token.lock().await = Some(client.cancel_token());
        *slot.backend_pid.lock().await = Some(1);
        *slot.session_cfg.lock().await = Some(SessionConfig {
            dsn_secret: Some(format!("postgresql://test@{address}/test?sslmode=disable")),
            ..Default::default()
        });
        assert!(
            tokio::time::timeout(Duration::from_secs(1), cancel_query(&slot))
                .await
                .unwrap()
                .unwrap()
        );
        peer.await.unwrap();
        drop(client);
        driver.abort();
        let _ = driver.await;
    }

    #[tokio::test]
    async fn cancelling_direct_connection_setup_closes_its_transport() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind PostgreSQL fixture");
        let address = listener.local_addr().expect("PostgreSQL fixture address");
        let (query_tx, query_rx) = tokio::sync::oneshot::channel();
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept connection");
            let length = stream.read_u32().await.expect("startup length");
            let mut startup = vec![0; (length - 4) as usize];
            stream
                .read_exact(&mut startup)
                .await
                .expect("startup payload");
            // Authenticate and become ready, then leave the first backend-pid
            // query unanswered. Dropping Client alone cannot finish the driver
            // while its pending protocol response never arrives.
            stream
                .write_all(&[b'R', 0, 0, 0, 8, 0, 0, 0, 0, b'Z', 0, 0, 0, 5, b'I'])
                .await
                .expect("PostgreSQL startup response");
            stream.read_u8().await.expect("first query message");
            query_tx.send(()).expect("query started");
            let mut bytes = [0; 1024];
            loop {
                match stream.read(&mut bytes).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        });
        let cfg = SessionConfig {
            dsn_secret: Some(format!("postgresql://test@{address}/test?sslmode=disable")),
            ..Default::default()
        };
        {
            let connecting = connect_session(&cfg);
            tokio::pin!(connecting);
            tokio::select! {
                result = &mut connecting => panic!("setup finished before cancellation: {:?}", result.err()),
                result = query_rx => result.expect("backend-pid query started"),
            }
        }
        tokio::time::timeout(std::time::Duration::from_secs(1), peer)
            .await
            .expect("cancelled setup drops the driver's socket")
            .expect("peer task completed");
    }

    struct DriverDropped(Option<tokio::sync::oneshot::Sender<()>>);

    impl Drop for DriverDropped {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _sent = sender.send(());
            }
        }
    }

    #[tokio::test]
    async fn cancelling_session_shutdown_aborts_its_connection_driver() {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        let driver = tokio::spawn(async move {
            let _dropped = DriverDropped(Some(dropped_tx));
            let _started = started_tx.send(());
            std::future::pending::<()>().await;
        });
        started_rx.await.expect("driver started");
        let client = SessionClient {
            client: None,
            backend_pid: 0,
            connection_task: Some(driver),
            ssh_bridge: None,
            container_bridge: None,
        };
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), client.shutdown(),)
                .await
                .is_err()
        );
        tokio::time::timeout(std::time::Duration::from_secs(1), dropped_rx)
            .await
            .expect("the driver is aborted when outer shutdown is cancelled")
            .expect("driver drop notification");
    }
}
