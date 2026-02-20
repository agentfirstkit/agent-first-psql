use crate::types::{RuntimeConfig, SessionConfig};

pub fn resolve_session_name(cfg: &RuntimeConfig, requested: Option<&str>) -> String {
    requested
        .map(std::string::ToString::to_string)
        .unwrap_or_else(|| cfg.default_session.clone())
}

pub fn resolve_conn_string(cfg: &SessionConfig) -> Result<String, String> {
    if let Some(dsn) = cfg
        .dsn_secret
        .clone()
        .or_else(|| std::env::var("AFPSQL_DSN_SECRET").ok())
    {
        return Ok(dsn);
    }

    if let Some(conninfo) = cfg
        .conninfo_secret
        .clone()
        .or_else(|| std::env::var("AFPSQL_CONNINFO_SECRET").ok())
    {
        let parsed: tokio_postgres::Config = conninfo
            .parse()
            .map_err(|e| format!("invalid conninfo: {e}"))?;
        return Ok(config_to_url(&parsed));
    }

    let host = cfg
        .host
        .clone()
        .or_else(|| std::env::var("AFPSQL_HOST").ok())
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let port = cfg
        .port
        .or_else(|| {
            std::env::var("AFPSQL_PORT")
                .ok()
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or(5432);
    let user = cfg
        .user
        .clone()
        .or_else(|| std::env::var("AFPSQL_USER").ok())
        .unwrap_or_else(|| "postgres".to_string());
    let dbname = cfg
        .dbname
        .clone()
        .or_else(|| std::env::var("AFPSQL_DBNAME").ok())
        .unwrap_or_else(|| "postgres".to_string());
    let password = cfg
        .password_secret
        .clone()
        .or_else(|| std::env::var("AFPSQL_PASSWORD_SECRET").ok());

    let auth = if let Some(pw) = password {
        format!("{user}:{pw}")
    } else {
        user
    };
    Ok(format!("postgresql://{auth}@{host}:{port}/{dbname}"))
}

fn config_to_url(cfg: &tokio_postgres::Config) -> String {
    let host = cfg
        .get_hosts()
        .first()
        .map(|h| match h {
            tokio_postgres::config::Host::Tcp(s) => s.to_string(),
            #[cfg(unix)]
            tokio_postgres::config::Host::Unix(_) => "127.0.0.1".to_string(),
            #[cfg(not(unix))]
            _ => "127.0.0.1".to_string(),
        })
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let port = cfg.get_ports().first().copied().unwrap_or(5432);
    let user = cfg.get_user().unwrap_or("postgres");
    let dbname = cfg.get_dbname().unwrap_or("postgres");
    let password = cfg
        .get_password()
        .and_then(|pw| std::str::from_utf8(pw).ok())
        .map(std::string::ToString::to_string);

    let auth = if let Some(pw) = password {
        format!("{user}:{pw}")
    } else {
        user.to_string()
    };
    format!("postgresql://{auth}@{host}:{port}/{dbname}")
}

#[cfg(test)]
#[path = "../tests/support/unit_conn.rs"]
mod tests;
