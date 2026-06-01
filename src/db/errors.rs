#[derive(Debug)]
pub enum ExecError {
    Cancelled,
    Connect(Box<ConnectError>),
    Config {
        message: String,
        hint: Option<String>,
    },
    InvalidParams(String),
    /// Reserved variant: the inline path now soft-truncates instead of
    /// erroring, but this variant is preserved so future callers (e.g. a
    /// dedicated row-limit gate) can opt back into the hard-fail shape.
    #[allow(dead_code)]
    ResultTooLarge {
        row_count: usize,
        payload_bytes: usize,
    },
    Sql {
        sqlstate: String,
        message: String,
        detail: Option<String>,
        hint: Option<String>,
        position: Option<String>,
    },
    Internal(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectError {
    pub error: String,
    pub sqlstate: Option<String>,
    pub message: Option<String>,
    pub detail: Option<String>,
    pub hint: Option<String>,
    pub retryable: bool,
}

impl ConnectError {
    pub fn new(error: impl Into<String>) -> Self {
        let error = error.into();
        Self {
            hint: connect_hint_for_message(&error).or_else(|| Some(default_connect_hint())),
            retryable: connect_retryable_for_message(&error),
            error,
            sqlstate: None,
            message: None,
            detail: None,
        }
    }

    pub fn from_pg_error(prefix: &str, err: tokio_postgres::Error) -> Self {
        if let Some(db) = err.as_db_error() {
            let sqlstate = db.code().code().to_string();
            let pg_hint = db.hint().map(std::string::ToString::to_string);
            let hint = pg_hint
                .clone()
                .or_else(|| connect_hint_for_sqlstate(&sqlstate, db.message()));
            return Self {
                error: format!("{prefix}: {}", db.message()),
                sqlstate: Some(sqlstate),
                message: Some(db.message().to_string()),
                detail: db.detail().map(std::string::ToString::to_string),
                hint,
                retryable: connect_retryable_for_sqlstate(db.code().code()),
            };
        }

        let error = format!("{prefix}: {}", format_error_chain(&err));
        Self {
            hint: connect_hint_for_message(&error).or_else(|| Some(default_connect_hint())),
            retryable: connect_retryable_for_message(&error),
            error,
            sqlstate: None,
            message: None,
            detail: None,
        }
    }
}

impl From<String> for ConnectError {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<&str> for ConnectError {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

pub(super) fn map_pg_error(err: tokio_postgres::Error) -> ExecError {
    if let Some(db) = err.as_db_error() {
        return ExecError::Sql {
            sqlstate: db.code().code().to_string(),
            message: db.message().to_string(),
            detail: db.detail().map(std::string::ToString::to_string),
            hint: db.hint().map(std::string::ToString::to_string),
            position: db.position().map(|p| match p {
                tokio_postgres::error::ErrorPosition::Original(pos) => pos.to_string(),
                tokio_postgres::error::ErrorPosition::Internal { position, .. } => {
                    position.to_string()
                }
            }),
        };
    }
    ExecError::Internal(err.to_string())
}

pub(super) fn map_connect_error(err: tokio_postgres::Error) -> ExecError {
    ExecError::Connect(Box::new(ConnectError::from_pg_error("connect failed", err)))
}

impl From<crate::conn::ConnectionConfigError> for ExecError {
    fn from(err: crate::conn::ConnectionConfigError) -> Self {
        ExecError::Config {
            message: err.message().to_string(),
            hint: err.hint().map(std::string::ToString::to_string),
        }
    }
}

fn format_error_chain(err: &(dyn std::error::Error + 'static)) -> String {
    let mut out = err.to_string();
    let mut source = err.source();
    while let Some(err) = source {
        let part = err.to_string();
        if !part.is_empty() && !out.contains(&part) {
            out.push_str(": ");
            out.push_str(&part);
        }
        source = err.source();
    }
    out
}

fn connect_retryable_for_sqlstate(sqlstate: &str) -> bool {
    sqlstate.starts_with("08")
        || matches!(
            sqlstate,
            "57P03" // cannot_connect_now
                | "53300" // too_many_connections
                | "53400" // configuration_limit_exceeded
                | "58000" // system_error
                | "58030" // io_error
        )
}

fn connect_hint_for_sqlstate(sqlstate: &str, message: &str) -> Option<String> {
    let hint = match sqlstate {
        "28P01" => "password authentication failed; check --user and --password-secret-env PGPASSWORD, or use an authentication method accepted by pg_hba.conf",
        "28000" => {
            if message.contains("role") && message.contains("does not exist") {
                "PostgreSQL rejected the role; check --user/PGUSER, create the role, or for local peer auth use a matching OS user or --ssh-sudo-user postgres with --ssh-remote-socket"
            } else {
                "PostgreSQL authentication or authorization failed; check pg_hba.conf, --user/PGUSER, database access, and whether peer/password auth is expected"
            }
        }
        "3D000" => "database does not exist; check --dbname/PGDATABASE or connect to the postgres maintenance database to inspect available databases",
        "57P03" => "PostgreSQL is not accepting connections yet; retry after the service finishes starting or leaves recovery/maintenance",
        "53300" => "PostgreSQL has too many active connections; wait, terminate idle sessions, or raise max_connections/pool limits",
        "53400" => "PostgreSQL rejected the connection because a configured limit was exceeded; inspect server logs and connection limits",
        state if state.starts_with("08") => "connection exception from PostgreSQL; check host/port/socket path, SSH tunnel, listener status, and network reachability",
        _ => return None,
    };
    Some(hint.to_string())
}

fn connect_hint_for_message(message: &str) -> Option<String> {
    if message.contains("password missing") {
        return Some("PostgreSQL requested password authentication but no password was provided; set --password-secret-env PGPASSWORD or --password-secret, or use a peer/socket authentication path".to_string());
    }
    if message.contains("error connecting to server") && message.contains("Operation not permitted")
    {
        return Some("local sandbox or OS policy blocked opening the PostgreSQL connection; in Codex request escalation, or check host/port/socket reachability outside the sandbox".to_string());
    }
    if message.contains("allocate ssh local port") && message.contains("Operation not permitted") {
        return Some("local sandbox or OS policy blocked binding the SSH tunnel port; in Codex request escalation, or use SSH sudo Unix-socket bridge mode when appropriate".to_string());
    }
    if message.contains("SSH transport currently supports discrete connection fields") {
        return Some("with --ssh, pass discrete connection fields such as --host/--port/--user/--dbname/--password-secret-env instead of --dsn-secret or --conninfo-secret".to_string());
    }
    if message.contains("container bridge") || message.contains("container transport") {
        return Some("check the container target, runtime access, driver selection, and whether PostgreSQL is listening on the requested container-internal host/port or socket".to_string());
    }
    if message.contains("explicit remote PostgreSQL Unix socket") {
        return Some("pass --ssh-remote-socket /var/run/postgresql/.s.PGSQL.5432, or set --host/PGHOST to the remote socket directory when not using sudo bridge mode".to_string());
    }
    if message.contains(".s.PGSQL") && message.contains("No such file or directory") {
        return Some("the PostgreSQL Unix socket path does not exist; check the server socket directory, port-derived socket filename, and whether PostgreSQL is running".to_string());
    }
    if message.contains("ssh tunnel") || message.contains("start ssh") {
        return Some("check SSH reachability/options and whether PostgreSQL is listening on the requested remote host/port or socket".to_string());
    }
    None
}

fn connect_retryable_for_message(message: &str) -> bool {
    !(message.contains("password missing")
        || message.contains("SSH transport currently supports discrete connection fields")
        || message.contains("explicit remote PostgreSQL Unix socket"))
}

fn default_connect_hint() -> String {
    "check --host/--port or PGHOST/PGPORT; for remote local-only PostgreSQL use --ssh user@server; for container-local PostgreSQL use --container TARGET; for containers on an SSH host combine --ssh user@server --container TARGET; for sudo-only Unix-socket access use --ssh-sudo-user with an explicit --ssh-remote-socket, or set --host/PGHOST to the remote socket directory".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_hints_classify_common_sqlstates() {
        let auth = connect_hint_for_sqlstate("28P01", "password authentication failed");
        assert!(auth
            .as_deref()
            .unwrap_or_default()
            .contains("password authentication failed"));
        assert!(!connect_retryable_for_sqlstate("28P01"));

        let missing_role = connect_hint_for_sqlstate("28000", "role \"root\" does not exist");
        assert!(missing_role
            .as_deref()
            .unwrap_or_default()
            .contains("--user"));
        assert!(!connect_retryable_for_sqlstate("28000"));

        let db = connect_hint_for_sqlstate("3D000", "database does not exist");
        assert!(db.as_deref().unwrap_or_default().contains("--dbname"));
        assert!(!connect_retryable_for_sqlstate("3D000"));

        let startup = connect_hint_for_sqlstate("57P03", "cannot connect now");
        assert!(startup.as_deref().unwrap_or_default().contains("retry"));
        assert!(connect_retryable_for_sqlstate("57P03"));
    }

    #[test]
    fn connect_hints_classify_transport_messages() {
        let password = ConnectError::new("connect failed: invalid configuration: password missing");
        assert!(password
            .hint
            .as_deref()
            .unwrap_or_default()
            .contains("PGPASSWORD"));
        assert!(!password.retryable);

        let sandbox = connect_hint_for_message(
            "allocate ssh local port on 127.0.0.1 failed: Operation not permitted",
        );
        assert!(sandbox.as_deref().unwrap_or_default().contains("sandbox"));

        let tcp_sandbox = connect_hint_for_message(
            "connect failed: error connecting to server: Operation not permitted (os error 1)",
        );
        assert!(tcp_sandbox
            .as_deref()
            .unwrap_or_default()
            .contains("sandbox"));

        let socket = connect_hint_for_message(
            "--ssh-sudo-user requires an explicit remote PostgreSQL Unix socket",
        );
        assert!(socket
            .as_deref()
            .unwrap_or_default()
            .contains("--ssh-remote-socket"));
    }
}
