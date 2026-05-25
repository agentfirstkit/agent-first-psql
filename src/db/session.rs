use crate::conn::resolve_pg_config;
use crate::types::SessionConfig;

use super::errors::ExecError;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

pub type CancelSlot = Arc<CancelState>;
pub(super) type SessionMap = RwLock<HashMap<String, Arc<SessionEntry>>>;

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
    pub(super) client: tokio_postgres::Client,
    pub(super) backend_pid: i32,
    connection_task: Option<tokio::task::JoinHandle<()>>,
    _ssh_tunnel: Option<crate::ssh_transport::SshTunnelGuard>,
    _ssh_bridge: Option<crate::ssh_transport::SshBridgeGuard>,
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

pub(super) async fn connect_session(cfg: &SessionConfig) -> Result<SessionClient, ExecError> {
    if crate::ssh_transport::needs_stdio_bridge(cfg) {
        let (client, bridge) = crate::ssh_transport::connect_stdio_bridge(cfg)
            .await
            .map_err(ExecError::Connect)?;
        let backend_pid = fetch_backend_pid(&client).await?;
        return Ok(SessionClient {
            client,
            backend_pid,
            connection_task: None,
            _ssh_tunnel: None,
            _ssh_bridge: Some(bridge),
        });
    }

    let (connect_cfg, ssh_tunnel) = crate::ssh_transport::prepare_session(cfg)
        .await
        .map_err(ExecError::Connect)?;
    let pg_cfg = resolve_pg_config(&connect_cfg).map_err(ExecError::from)?;
    let tls = make_supported_tls()
        .map_err(|e| ExecError::Connect(format!("create TLS connector failed: {e}")))?;
    let (client, connection) = pg_cfg
        .connect(tls)
        .await
        .map_err(|e| ExecError::Connect(format!("connect failed: {e}")))?;
    let connection_task = tokio::spawn(async move {
        let _ = connection.await;
    });
    let backend_pid = fetch_backend_pid(&client).await?;

    Ok(SessionClient {
        client,
        backend_pid,
        connection_task: Some(connection_task),
        _ssh_tunnel: ssh_tunnel,
        _ssh_bridge: None,
    })
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
    let row = client
        .client
        .query_one("select pg_cancel_backend($1)", &[&backend_pid])
        .await
        .map_err(|e| format!("pg_cancel_backend failed: {e}"))?;
    row.try_get(0)
        .map_err(|e| format!("decode pg_cancel_backend result failed: {e}"))
}

impl SessionClient {
    pub(super) fn is_closed(&self) -> bool {
        self.connection_task
            .as_ref()
            .map(|task| task.is_finished())
            .unwrap_or(false)
            || self
                ._ssh_bridge
                .as_ref()
                .map(crate::ssh_transport::SshBridgeGuard::is_finished)
                .unwrap_or(false)
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
