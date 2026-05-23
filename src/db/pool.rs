use crate::conn::resolve_pg_config;
use crate::types::SessionConfig;

use super::errors::ExecError;
use deadpool_postgres::{Manager, ManagerConfig, Pool, RecyclingMethod};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

pub type CancelSlot = Arc<Mutex<Option<tokio_postgres::CancelToken>>>;
pub(super) type PoolMap = RwLock<HashMap<String, Pool>>;

pub fn new_cancel_slot() -> CancelSlot {
    Arc::new(Mutex::new(None))
}

pub async fn cancel_query(slot: &CancelSlot) -> Result<bool, String> {
    let token = slot.lock().await.clone();
    let Some(token) = token else {
        return Ok(false);
    };
    let tls = native_tls::TlsConnector::builder()
        .build()
        .map_err(|e| format!("create TLS connector failed: {e}"))?;
    let tls = postgres_native_tls::MakeTlsConnector::new(tls);
    token
        .cancel_query(tls)
        .await
        .map_err(|e| format!("server-side cancel failed: {e}"))?;
    Ok(true)
}

pub(super) fn new_pool_map() -> PoolMap {
    RwLock::new(HashMap::new())
}

pub(super) async fn get_pool(
    pools: &PoolMap,
    session_name: &str,
    cfg: &SessionConfig,
) -> Result<Pool, ExecError> {
    if let Some(pool) = pools.read().await.get(session_name) {
        return Ok(pool.clone());
    }

    let pg_cfg = resolve_pg_config(cfg).map_err(ExecError::Connect)?;
    let tls = native_tls::TlsConnector::builder()
        .build()
        .map_err(|e| ExecError::Connect(format!("create TLS connector failed: {e}")))?;
    let tls = postgres_native_tls::MakeTlsConnector::new(tls);
    let mgr = Manager::from_config(
        pg_cfg,
        tls,
        ManagerConfig {
            recycling_method: RecyclingMethod::Fast,
        },
    );
    let pool = Pool::builder(mgr)
        .max_size(5)
        .build()
        .map_err(|e| ExecError::Connect(format!("create pool failed: {e}")))?;

    pools
        .write()
        .await
        .insert(session_name.to_string(), pool.clone());

    Ok(pool)
}
