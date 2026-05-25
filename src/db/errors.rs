#[derive(Debug)]
pub enum ExecError {
    Cancelled,
    Connect(String),
    Config {
        message: String,
        hint: Option<String>,
    },
    InvalidParams(String),
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

impl From<crate::conn::ConnectionConfigError> for ExecError {
    fn from(err: crate::conn::ConnectionConfigError) -> Self {
        ExecError::Config {
            message: err.message().to_string(),
            hint: err.hint().map(std::string::ToString::to_string),
        }
    }
}
