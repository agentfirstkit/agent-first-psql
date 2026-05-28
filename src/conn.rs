use crate::types::{RuntimeConfig, SessionConfig};
use std::error::Error as _;
use tokio_postgres::config::SslMode;
use tokio_postgres::Config;

const SUPPORTED_SSLMODE_HINT: &str = "afpsql supports sslmode=disable, prefer, and require. It does not implement libpq verify-ca/verify-full or client certificate options yet; use psql/libpq when certificate verification or client certificates are required.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionConfigError {
    message: String,
    hint: Option<String>,
}

impl ConnectionConfigError {
    pub fn new(message: impl Into<String>, hint: Option<String>) -> Self {
        Self {
            message: message.into(),
            hint,
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn hint(&self) -> Option<&str> {
        self.hint.as_deref()
    }
}

impl std::fmt::Display for ConnectionConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ConnectionConfigError {}

pub fn resolve_session_name(cfg: &RuntimeConfig, requested: Option<&str>) -> String {
    requested
        .map(std::string::ToString::to_string)
        .unwrap_or_else(|| cfg.default_session.clone())
}

pub fn resolve_pg_config(cfg: &SessionConfig) -> Result<Config, ConnectionConfigError> {
    if let Some(dsn) = cfg
        .dsn_secret
        .clone()
        .or_else(|| std::env::var("AFPSQL_DSN_SECRET").ok())
    {
        validate_dsn_ssl_options(&dsn)?;
        return dsn.parse().map_err(|e| map_pg_config_parse_error("dsn", e));
    }

    if let Some(conninfo) = cfg
        .conninfo_secret
        .clone()
        .or_else(|| std::env::var("AFPSQL_CONNINFO_SECRET").ok())
    {
        validate_conninfo_ssl_options(&conninfo)?;
        return conninfo
            .parse()
            .map_err(|e| map_pg_config_parse_error("conninfo", e));
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
        .or_else(|| std::env::var("AFPSQL_PASSWORD_SECRET").ok())
        .or_else(|| std::env::var("PGPASSWORD").ok());

    let mut pg_cfg = Config::new();
    pg_cfg.host(host).port(port).user(user).dbname(dbname);
    if let Some(pw) = password {
        pg_cfg.password(pw);
    }
    if let Some(sslmode) = env_nonempty("PGSSLMODE") {
        apply_sslmode(&mut pg_cfg, "PGSSLMODE", &sslmode)?;
    }
    Ok(pg_cfg)
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

pub fn libpq_env_fallbacks_in_use(cfg: &SessionConfig) -> Vec<&'static str> {
    if cfg.dsn_secret.is_some() || cfg.conninfo_secret.is_some() {
        return Vec::new();
    }
    if std::env::var("AFPSQL_DSN_SECRET").is_ok() || std::env::var("AFPSQL_CONNINFO_SECRET").is_ok()
    {
        return Vec::new();
    }
    let mut used = Vec::new();
    if cfg.host.is_none()
        && std::env::var("AFPSQL_HOST").is_err()
        && env_nonempty("PGHOST").is_some()
    {
        used.push("PGHOST");
    }
    if cfg.port.is_none()
        && std::env::var("AFPSQL_PORT").is_err()
        && env_nonempty("PGPORT").is_some()
    {
        used.push("PGPORT");
    }
    if cfg.user.is_none()
        && std::env::var("AFPSQL_USER").is_err()
        && env_nonempty("PGUSER").is_some()
    {
        used.push("PGUSER");
    }
    if cfg.dbname.is_none()
        && std::env::var("AFPSQL_DBNAME").is_err()
        && env_nonempty("PGDATABASE").is_some()
    {
        used.push("PGDATABASE");
    }
    if cfg.password_secret.is_none()
        && std::env::var("AFPSQL_PASSWORD_SECRET").is_err()
        && env_nonempty("PGPASSWORD").is_some()
    {
        used.push("PGPASSWORD");
    }
    if env_nonempty("PGSSLMODE").is_some() {
        used.push("PGSSLMODE");
    }
    used
}

fn validate_dsn_ssl_options(dsn: &str) -> Result<(), ConnectionConfigError> {
    let Some(query) = dsn.split_once('?').map(|(_, query)| query) else {
        return Ok(());
    };
    let query = query.split('#').next().unwrap_or(query);
    for part in query.split('&') {
        let (key, value) = part.split_once('=').unwrap_or((part, ""));
        validate_ssl_option(key, value, "dsn")?;
    }
    Ok(())
}

fn validate_conninfo_ssl_options(conninfo: &str) -> Result<(), ConnectionConfigError> {
    for (key, value) in parse_conninfo_pairs(conninfo) {
        validate_ssl_option(&key, &value, "conninfo")?;
    }
    Ok(())
}

fn validate_ssl_option(key: &str, value: &str, source: &str) -> Result<(), ConnectionConfigError> {
    match key {
        "sslmode" => validate_sslmode(source, value),
        "sslnegotiation" if value == "postgres" => Ok(()),
        "sslnegotiation" => Err(ConnectionConfigError::new(
            format!("unsupported {source} TLS option `sslnegotiation={value}`"),
            Some("afpsql supports PostgreSQL's standard TLS negotiation path only; remove sslnegotiation=direct or use psql/libpq for PostgreSQL 17 direct TLS negotiation.".to_string()),
        )),
        "sslrootcert" | "sslcert" | "sslkey" | "sslpassword" | "sslcrl" | "sslcrldir"
        | "sslcertmode" | "sslsni" | "ssl_min_protocol_version" | "ssl_max_protocol_version"
        => Err(unsupported_ssl_option(source, key)),
        _ => Ok(()),
    }
}

fn apply_sslmode(
    pg_cfg: &mut Config,
    source: &str,
    value: &str,
) -> Result<(), ConnectionConfigError> {
    validate_sslmode(source, value)?;
    let mode = match value {
        "disable" => SslMode::Disable,
        "prefer" => SslMode::Prefer,
        "require" => SslMode::Require,
        _ => return Err(unsupported_sslmode(source, value)),
    };
    pg_cfg.ssl_mode(mode);
    Ok(())
}

fn validate_sslmode(source: &str, value: &str) -> Result<(), ConnectionConfigError> {
    match value {
        "disable" | "prefer" | "require" => Ok(()),
        _ => Err(unsupported_sslmode(source, value)),
    }
}

fn unsupported_sslmode(source: &str, value: &str) -> ConnectionConfigError {
    ConnectionConfigError::new(
        format!(
            "unsupported {source} sslmode `{value}`; supported values are disable, prefer, require"
        ),
        Some(SUPPORTED_SSLMODE_HINT.to_string()),
    )
}

fn unsupported_ssl_option(source: &str, key: &str) -> ConnectionConfigError {
    ConnectionConfigError::new(
        format!("unsupported {source} TLS option `{key}`"),
        Some(SUPPORTED_SSLMODE_HINT.to_string()),
    )
}

fn map_pg_config_parse_error(source: &str, err: tokio_postgres::Error) -> ConnectionConfigError {
    let cause = err.source().map(std::string::ToString::to_string);
    if let Some(cause) = cause.as_deref() {
        if cause == "invalid value for option `sslmode`" {
            return ConnectionConfigError::new(
                format!("unsupported {source} sslmode"),
                Some(SUPPORTED_SSLMODE_HINT.to_string()),
            );
        }
        if let Some(key) = cause
            .strip_prefix("unknown option `")
            .and_then(|rest| rest.strip_suffix('`'))
        {
            if is_unsupported_ssl_option(key) {
                return unsupported_ssl_option(source, key);
            }
        }
    }

    let detail = cause
        .map(|cause| format!("{err}: {cause}"))
        .unwrap_or_else(|| err.to_string());
    ConnectionConfigError::new(format!("invalid {source}: {detail}"), None)
}

fn is_unsupported_ssl_option(key: &str) -> bool {
    matches!(
        key,
        "sslrootcert"
            | "sslcert"
            | "sslkey"
            | "sslpassword"
            | "sslcrl"
            | "sslcrldir"
            | "sslcertmode"
            | "sslsni"
            | "ssl_min_protocol_version"
            | "ssl_max_protocol_version"
            | "sslnegotiation"
    )
}

fn parse_conninfo_pairs(input: &str) -> Vec<(String, String)> {
    let bytes = input.as_bytes();
    let mut pairs = Vec::new();
    let mut i = 0usize;

    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }

        let key_start = i;
        while i < bytes.len() && bytes[i] != b'=' && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let key = &input[key_start..i];
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            break;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }

        let mut value = String::new();
        if i < bytes.len() && bytes[i] == b'\'' {
            i += 1;
            while i < bytes.len() {
                match bytes[i] {
                    b'\\' if i + 1 < bytes.len() => {
                        i += 1;
                        value.push(bytes[i] as char);
                        i += 1;
                    }
                    b'\'' => {
                        i += 1;
                        break;
                    }
                    b => {
                        value.push(b as char);
                        i += 1;
                    }
                }
            }
        } else {
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 1;
                }
                value.push(bytes[i] as char);
                i += 1;
            }
        }

        pairs.push((key.to_string(), value));
    }

    pairs
}

#[cfg(test)]
#[path = "../tests/support/unit_conn.rs"]
mod tests;
