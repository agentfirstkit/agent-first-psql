use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Deserialize)]
#[serde(tag = "code")]
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
}

#[derive(Debug, Deserialize, Default, Clone)]
#[allow(dead_code)]
pub struct QueryOptions {
    #[serde(default)]
    pub stream_rows: bool,
    pub batch_rows: Option<usize>,
    pub batch_bytes: Option<usize>,
    pub statement_timeout_ms: Option<u64>,
    pub lock_timeout_ms: Option<u64>,
    pub read_only: Option<bool>,
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
        retryable: bool,
        trace: Trace,
    },
    #[serde(rename = "config")]
    Config(RuntimeConfig),
    #[serde(rename = "pong")]
    Pong { trace: PongTrace },
    #[serde(rename = "close")]
    Close { message: String, trace: CloseTrace },
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
        trace: Trace,
    },
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

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
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
pub struct ConfigPatch {
    pub default_session: Option<String>,
    pub sessions: Option<HashMap<String, SessionConfigPatch>>,
    pub inline_max_rows: Option<usize>,
    pub inline_max_bytes: Option<usize>,
    pub statement_timeout_ms: Option<u64>,
    pub lock_timeout_ms: Option<u64>,
    pub log: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Default)]
pub struct SessionConfigPatch {
    pub dsn_secret: Option<String>,
    pub conninfo_secret: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub dbname: Option<String>,
    pub password_secret: Option<String>,
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
