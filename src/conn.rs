use crate::types::{RuntimeConfig, SessionConfig};
use tokio_postgres::Config;

pub fn resolve_session_name(cfg: &RuntimeConfig, requested: Option<&str>) -> String {
    requested
        .map(std::string::ToString::to_string)
        .unwrap_or_else(|| cfg.default_session.clone())
}

pub fn resolve_pg_config(cfg: &SessionConfig) -> Result<Config, String> {
    if let Some(dsn) = cfg
        .dsn_secret
        .clone()
        .or_else(|| std::env::var("AFPSQL_DSN_SECRET").ok())
    {
        return dsn.parse().map_err(|e| format!("invalid dsn: {e}"));
    }

    if let Some(conninfo) = cfg
        .conninfo_secret
        .clone()
        .or_else(|| std::env::var("AFPSQL_CONNINFO_SECRET").ok())
    {
        return conninfo
            .parse()
            .map_err(|e| format!("invalid conninfo: {e}"));
    }

    let host = cfg
        .host
        .clone()
        .or_else(|| std::env::var("AFPSQL_HOST").ok())
        .or_else(|| std::env::var("PGHOST").ok())
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let port = cfg
        .port
        .or_else(|| {
            std::env::var("AFPSQL_PORT")
                .ok()
                .and_then(|s| s.parse().ok())
        })
        .or_else(|| std::env::var("PGPORT").ok().and_then(|s| s.parse().ok()))
        .unwrap_or(5432);
    let user = cfg
        .user
        .clone()
        .or_else(|| std::env::var("AFPSQL_USER").ok())
        .or_else(|| std::env::var("PGUSER").ok())
        .unwrap_or_else(|| "postgres".to_string());
    let dbname = cfg
        .dbname
        .clone()
        .or_else(|| std::env::var("AFPSQL_DBNAME").ok())
        .or_else(|| std::env::var("PGDATABASE").ok())
        .unwrap_or_else(|| "postgres".to_string());
    let password = cfg
        .password_secret
        .clone()
        .or_else(|| std::env::var("AFPSQL_PASSWORD_SECRET").ok());

    let mut pg_cfg = Config::new();
    pg_cfg.host(host).port(port).user(user).dbname(dbname);
    if let Some(pw) = password {
        pg_cfg.password(pw);
    }
    Ok(pg_cfg)
}

#[cfg(test)]
#[path = "../tests/support/unit_conn.rs"]
mod tests;
