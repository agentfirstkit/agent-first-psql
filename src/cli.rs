use std::io::Write;

use crate::types::{QueryOptions, SessionConfig};
use agent_first_data::{cli_parse_log_filters, cli_parse_output, OutputFormat};
use clap::{Parser, ValueEnum};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub enum Mode {
    Cli(CliRequest),
    Pipe(PipeInit),
}

pub struct PipeInit {
    pub output: OutputFormat,
    pub session: SessionConfig,
    pub log: Vec<String>,
    pub startup_argv: Vec<String>,
    pub startup_args: Value,
    pub startup_env: Value,
    pub startup_requested: bool,
}

pub struct CliRequest {
    pub sql: String,
    pub params: Vec<Value>,
    pub options: QueryOptions,
    pub session: SessionConfig,
    pub output: OutputFormat,
    pub log: Vec<String>,
    pub startup_argv: Vec<String>,
    pub startup_args: Value,
    pub startup_env: Value,
    pub startup_requested: bool,
    pub dry_run: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum RuntimeMode {
    Cli,
    Pipe,
    #[value(name = "psql")]
    Psql,
}

#[doc = r#"Agent-First PostgreSQL client.

### Interface Policy

- default mode is canonical agent-first CLI
- `--mode psql` is argument translation only; runtime output stays JSONL
- stdout carries protocol events; stderr is not a protocol channel

### Query Sources and Parameters

- use `--sql` for inline SQL or `--sql-file` for a file
- use repeatable `--param N=value` for positional binds
- placeholder count is validated from prepared-statement metadata, not by SQL text scanning

### Connection Sources

- `--dsn-secret` for a PostgreSQL URI
- `--conninfo-secret` for libpq-style conninfo
- or discrete `--host`, `--port`, `--user`, `--dbname`, `--password-secret`
- agent-first environment fallbacks: `AFPSQL_*`
- PostgreSQL environment fallbacks: `PGHOST`, `PGPORT`, `PGUSER`, `PGDATABASE`

### Result Shaping

- default mode buffers a bounded inline result
- use `--stream-rows` for large result sets, with `--batch-rows` and `--batch-bytes` to tune chunk size
- `--output json|yaml|plain` changes rendering only, not the runtime schema

### Examples

```text
afpsql --sql "select now() as now_rfc3339"
afpsql --sql-file ./query.sql
afpsql --sql "select * from users where id = $1" --param 1=123
afpsql --dsn-secret "postgresql://app:secret@127.0.0.1:5432/appdb" --sql "select 1"
afpsql --mode psql -h 127.0.0.1 -p 5432 -U app -d appdb -c "select 1"
afpsql --sql "select * from big_table" --stream-rows --batch-rows 1000
afpsql --mode pipe
```

### Exit Codes

- `0`: query completed successfully
- `1`: SQL error or runtime error
- `2`: invalid CLI arguments
"#]
#[derive(Parser)]
#[command(name = "afpsql", version, verbatim_doc_comment)]
pub struct AfdCli {
    /// Inline SQL string to execute.
    #[arg(long, help_heading = "Query")]
    sql: Option<String>,
    /// Read SQL from a file.
    #[arg(long = "sql-file", help_heading = "Query")]
    sql_file: Option<String>,
    /// Positional bind parameter in `N=value` form. Repeat for additional parameters.
    #[arg(long = "param", help_heading = "Query")]
    param: Vec<String>,
    /// Stream large result sets as `result_rows` batches instead of a single inline result.
    #[arg(long = "stream-rows", help_heading = "Query")]
    stream_rows: bool,
    /// Maximum rows per streamed batch.
    #[arg(long = "batch-rows", help_heading = "Query")]
    batch_rows: Option<usize>,
    /// Soft byte target per streamed batch.
    #[arg(long = "batch-bytes", help_heading = "Query")]
    batch_bytes: Option<usize>,
    /// Per-query statement timeout in milliseconds.
    #[arg(long = "statement-timeout-ms", help_heading = "Query")]
    statement_timeout_ms: Option<u64>,
    /// Per-query lock timeout in milliseconds.
    #[arg(long = "lock-timeout-ms", help_heading = "Query")]
    lock_timeout_ms: Option<u64>,
    /// Maximum inline rows before returning `result_too_large`.
    #[arg(long = "inline-max-rows", help_heading = "Query")]
    inline_max_rows: Option<usize>,
    /// Maximum inline payload bytes before returning `result_too_large`.
    #[arg(long = "inline-max-bytes", help_heading = "Query")]
    inline_max_bytes: Option<usize>,
    /// Force the query to run in a read-only transaction.
    #[arg(long = "read-only", help_heading = "Query")]
    read_only: bool,
    /// Preview the query without executing it
    #[arg(long, help_heading = "Query")]
    dry_run: bool,

    /// PostgreSQL DSN URI. Redacted in structured output.
    #[arg(long = "dsn-secret", help_heading = "Connection")]
    dsn_secret: Option<String>,
    /// libpq-style conninfo string. Redacted in structured output.
    #[arg(long = "conninfo-secret", help_heading = "Connection")]
    conninfo_secret: Option<String>,
    /// PostgreSQL host.
    #[arg(long, help_heading = "Connection")]
    host: Option<String>,
    /// PostgreSQL port.
    #[arg(long, help_heading = "Connection")]
    port: Option<u16>,
    /// PostgreSQL user name.
    #[arg(long, help_heading = "Connection")]
    user: Option<String>,
    /// PostgreSQL database name.
    #[arg(long, help_heading = "Connection")]
    dbname: Option<String>,
    /// PostgreSQL password. Redacted in structured output.
    #[arg(long = "password-secret", help_heading = "Connection")]
    password_secret: Option<String>,

    /// Output format: json (default), yaml, or plain.
    #[arg(long, default_value = "json", help_heading = "Runtime")]
    output: String,
    /// Diagnostic log categories.
    #[arg(long = "log", value_delimiter = ',', help_heading = "Runtime")]
    log: Vec<String>,
    /// Runtime mode: canonical cli, pipe, or `psql` translation mode.
    #[arg(long, value_enum, default_value_t = RuntimeMode::Cli, help_heading = "Runtime")]
    mode: RuntimeMode,
}

pub fn parse_args() -> Result<Mode, String> {
    let raw: Vec<String> = std::env::args().collect();
    if is_psql_mode_requested(&raw) {
        return parse_psql_mode(&raw);
    }
    let startup_requested = startup_requested_from_raw(&raw);

    let cli = match AfdCli::try_parse_from(&raw) {
        Ok(c) => c,
        Err(e) => {
            use clap::error::ErrorKind;
            if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
                let _ = writeln!(std::io::stdout(), "{e}");
                std::process::exit(0);
            }
            return Err(e.to_string());
        }
    };
    let output = parse_output(&cli.output)?;
    let log = parse_log_categories(&cli.log);
    let session = SessionConfig {
        dsn_secret: cli.dsn_secret,
        conninfo_secret: cli.conninfo_secret,
        host: cli.host,
        port: cli.port,
        user: cli.user,
        dbname: cli.dbname,
        password_secret: cli.password_secret,
    };
    let mode_name = match cli.mode {
        RuntimeMode::Cli => "cli",
        RuntimeMode::Pipe => "pipe",
        RuntimeMode::Psql => "psql",
    };
    let startup_args = json!({
        "mode": mode_name,
        "sql": &cli.sql,
        "sql_file": &cli.sql_file,
        "param": &cli.param,
        "stream_rows": cli.stream_rows,
        "batch_rows": cli.batch_rows,
        "batch_bytes": cli.batch_bytes,
        "statement_timeout_ms": cli.statement_timeout_ms,
        "lock_timeout_ms": cli.lock_timeout_ms,
        "inline_max_rows": cli.inline_max_rows,
        "inline_max_bytes": cli.inline_max_bytes,
        "read_only": cli.read_only,
        "dsn_secret": &session.dsn_secret,
        "conninfo_secret": &session.conninfo_secret,
        "host": &session.host,
        "port": session.port,
        "user": &session.user,
        "dbname": &session.dbname,
        "password_secret": &session.password_secret,
        "output": output_name(output),
        "log": &log,
    });
    let startup_env = startup_env_snapshot();

    match cli.mode {
        RuntimeMode::Pipe => {
            return Ok(Mode::Pipe(PipeInit {
                output,
                session,
                log: log.clone(),
                startup_argv: raw,
                startup_args,
                startup_env,
                startup_requested,
            }));
        }
        RuntimeMode::Cli | RuntimeMode::Psql => {}
    }

    let sql = load_sql(cli.sql, cli.sql_file)?;
    let params = parse_params(&cli.param)?;

    let options = QueryOptions {
        stream_rows: cli.stream_rows,
        batch_rows: cli.batch_rows,
        batch_bytes: cli.batch_bytes,
        statement_timeout_ms: cli.statement_timeout_ms,
        lock_timeout_ms: cli.lock_timeout_ms,
        read_only: if cli.read_only { Some(true) } else { None },
        inline_max_rows: cli.inline_max_rows,
        inline_max_bytes: cli.inline_max_bytes,
    };

    Ok(Mode::Cli(CliRequest {
        sql,
        params,
        options,
        session,
        output,
        log,
        startup_argv: raw,
        startup_args,
        startup_env,
        startup_requested,
        dry_run: cli.dry_run,
    }))
}

fn parse_psql_mode(raw: &[String]) -> Result<Mode, String> {
    let startup_requested = startup_requested_from_raw(raw);
    let mut sql: Option<String> = None;
    let mut sql_file: Option<String> = None;
    let mut host: Option<String> = None;
    let mut port: Option<u16> = None;
    let mut user: Option<String> = None;
    let mut dbname: Option<String> = None;
    let mut dsn_secret: Option<String> = None;
    let mut conninfo_secret: Option<String> = None;
    let mut params_kv: Vec<String> = vec![];
    let mut output = OutputFormat::Json;
    let mut log_entries: Vec<String> = vec![];

    let mut i = 1usize;
    while i < raw.len() {
        match raw[i].as_str() {
            "--mode" => {
                i += 1;
                let v = raw.get(i).ok_or("--mode requires value")?;
                if v != "psql" {
                    return Err(format!("unsupported psql-mode argument: --mode {v}; only --mode psql is allowed with psql translation"));
                }
                i += 1;
            }
            other if other.starts_with("--mode=") => {
                let v = other.trim_start_matches("--mode=");
                if v != "psql" {
                    return Err(format!("unsupported psql-mode argument: {other}; only --mode=psql is allowed with psql translation"));
                }
                i += 1;
            }
            "-c" => {
                i += 1;
                let v = raw.get(i).ok_or("-c requires SQL")?;
                sql = Some(v.clone());
                i += 1;
            }
            "-f" => {
                i += 1;
                let v = raw.get(i).ok_or("-f requires file path")?;
                sql_file = Some(v.clone());
                i += 1;
            }
            "-h" => {
                i += 1;
                host = Some(raw.get(i).ok_or("-h requires value")?.clone());
                i += 1;
            }
            "-p" => {
                i += 1;
                port = Some(
                    raw.get(i)
                        .ok_or("-p requires value")?
                        .parse()
                        .map_err(|_| "invalid -p port")?,
                );
                i += 1;
            }
            "-U" => {
                i += 1;
                user = Some(raw.get(i).ok_or("-U requires value")?.clone());
                i += 1;
            }
            "-d" => {
                i += 1;
                dbname = Some(raw.get(i).ok_or("-d requires value")?.clone());
                i += 1;
            }
            "--dsn-secret" => {
                i += 1;
                dsn_secret = Some(raw.get(i).ok_or("--dsn-secret requires value")?.clone());
                i += 1;
            }
            "--conninfo-secret" => {
                i += 1;
                conninfo_secret = Some(
                    raw.get(i)
                        .ok_or("--conninfo-secret requires value")?
                        .clone(),
                );
                i += 1;
            }
            "-v" => {
                i += 1;
                params_kv.push(raw.get(i).ok_or("-v requires N=value")?.clone());
                i += 1;
            }
            "--output" => {
                i += 1;
                output = parse_output(raw.get(i).ok_or("--output requires value")?)?;
                i += 1;
            }
            "--log" => {
                i += 1;
                let values = raw.get(i).ok_or("--log requires value")?;
                for part in values.split(',') {
                    let trimmed = part.trim();
                    if !trimmed.is_empty() {
                        log_entries.push(trimmed.to_string());
                    }
                }
                i += 1;
            }
            other if other.starts_with("postgresql://") || other.starts_with("postgres://") => {
                dsn_secret = Some(other.to_string());
                i += 1;
            }
            unsupported => {
                return Err(format!(
                    "unsupported psql-mode argument: {unsupported}; only --mode psql, -c/-f/-h/-p/-U/-d/-v/--dsn-secret/--conninfo-secret/--output/--log are supported"
                ));
            }
        }
    }

    let session = SessionConfig {
        dsn_secret,
        conninfo_secret,
        host,
        port,
        user,
        dbname,
        password_secret: None,
    };

    let startup_sql = sql.clone();
    let startup_sql_file = sql_file.clone();
    let sql = load_sql(sql, sql_file)?;
    let params = parse_params(&params_kv)?;
    let startup_args = psql_startup_args(
        "psql",
        startup_sql.or_else(|| Some(sql.clone())),
        startup_sql_file,
        &params_kv,
        &session,
        output,
        &log_entries,
    );
    Ok(Mode::Cli(CliRequest {
        sql,
        params,
        options: QueryOptions::default(),
        session,
        output,
        log: parse_log_categories(&log_entries),
        startup_argv: raw.to_vec(),
        startup_args,
        startup_env: startup_env_snapshot(),
        startup_requested,
        dry_run: false,
    }))
}

fn is_psql_mode_requested(raw: &[String]) -> bool {
    let mut i = 1usize;
    while i < raw.len() {
        let arg = raw[i].as_str();
        if arg == "--mode" {
            if let Some(v) = raw.get(i + 1) {
                return v == "psql";
            }
            return false;
        }
        if arg == "--mode=psql" {
            return true;
        }
        i += 1;
    }
    false
}

fn load_sql(sql: Option<String>, sql_file: Option<String>) -> Result<String, String> {
    match (sql, sql_file) {
        (Some(s), None) => Ok(s),
        (None, Some(path)) => {
            std::fs::read_to_string(path).map_err(|e| format!("read --sql-file failed: {e}"))
        }
        (Some(_), Some(_)) => Err("--sql and --sql-file are mutually exclusive".to_string()),
        (None, None) => Err("one of --sql or --sql-file is required".to_string()),
    }
}

fn parse_output(v: &str) -> Result<OutputFormat, String> {
    cli_parse_output(v)
}

fn parse_log_categories(entries: &[String]) -> Vec<String> {
    cli_parse_log_filters(entries)
}

fn startup_requested_from_raw(raw: &[String]) -> bool {
    let mut i = 1usize;
    while i < raw.len() {
        if raw[i] == "--log" {
            if let Some(values) = raw.get(i + 1) {
                for part in values.split(',') {
                    let v = part.trim().to_ascii_lowercase();
                    if matches!(v.as_str(), "startup" | "all" | "*") {
                        return true;
                    }
                }
            }
            i += 2;
            continue;
        }
        if let Some(values) = raw[i].strip_prefix("--log=") {
            for part in values.split(',') {
                let v = part.trim().to_ascii_lowercase();
                if matches!(v.as_str(), "startup" | "all" | "*") {
                    return true;
                }
            }
        }
        i += 1;
    }
    false
}

fn output_name(output: OutputFormat) -> &'static str {
    match output {
        OutputFormat::Json => "json",
        OutputFormat::Yaml => "yaml",
        OutputFormat::Plain => "plain",
    }
}

fn startup_env_snapshot() -> Value {
    json!({
        "AFPSQL_DSN_SECRET": std::env::var("AFPSQL_DSN_SECRET").ok(),
        "AFPSQL_CONNINFO_SECRET": std::env::var("AFPSQL_CONNINFO_SECRET").ok(),
        "AFPSQL_HOST": std::env::var("AFPSQL_HOST").ok(),
        "AFPSQL_PORT": std::env::var("AFPSQL_PORT").ok(),
        "AFPSQL_USER": std::env::var("AFPSQL_USER").ok(),
        "AFPSQL_DBNAME": std::env::var("AFPSQL_DBNAME").ok(),
        "AFPSQL_PASSWORD_SECRET": std::env::var("AFPSQL_PASSWORD_SECRET").ok(),
        "PGHOST": std::env::var("PGHOST").ok(),
        "PGPORT": std::env::var("PGPORT").ok(),
        "PGUSER": std::env::var("PGUSER").ok(),
        "PGDATABASE": std::env::var("PGDATABASE").ok(),
    })
}

fn psql_startup_args(
    mode: &str,
    sql: Option<String>,
    sql_file: Option<String>,
    params_kv: &[String],
    session: &SessionConfig,
    output: OutputFormat,
    log_entries: &[String],
) -> Value {
    json!({
        "mode": mode,
        "sql": sql,
        "sql_file": sql_file,
        "param": params_kv,
        "dsn_secret": session.dsn_secret,
        "conninfo_secret": session.conninfo_secret,
        "host": session.host,
        "port": session.port,
        "user": session.user,
        "dbname": session.dbname,
        "password_secret": session.password_secret,
        "output": output_name(output),
        "log": parse_log_categories(log_entries),
    })
}

pub fn parse_params(entries: &[String]) -> Result<Vec<Value>, String> {
    let mut by_index: BTreeMap<usize, Value> = BTreeMap::new();
    for entry in entries {
        let (idx, raw) = split_index_value(entry)?;
        if idx == 0 {
            return Err("param index must start at 1".to_string());
        }
        by_index.insert(idx, parse_param_value(raw));
    }
    if by_index.is_empty() {
        return Ok(vec![]);
    }
    let max = by_index.keys().max().copied().unwrap_or(0);
    let mut out = Vec::with_capacity(max);
    for i in 1..=max {
        let v = by_index
            .remove(&i)
            .ok_or_else(|| format!("missing parameter index {i}"))?;
        out.push(v);
    }
    Ok(out)
}

fn split_index_value(entry: &str) -> Result<(usize, &str), String> {
    let mut parts = entry.splitn(2, '=');
    let left = parts.next().unwrap_or_default();
    let right = parts
        .next()
        .ok_or_else(|| format!("invalid param '{entry}', expected N=value"))?;
    let idx = left
        .parse::<usize>()
        .map_err(|_| format!("invalid param index in '{entry}'"))?;
    Ok((idx, right))
}

fn parse_param_value(v: &str) -> Value {
    if v == "null" {
        return Value::Null;
    }
    if v == "true" {
        return Value::Bool(true);
    }
    if v == "false" {
        return Value::Bool(false);
    }
    if let Ok(i) = v.parse::<i64>() {
        return Value::Number(i.into());
    }
    if let Ok(f) = v.parse::<f64>() {
        if let Some(n) = serde_json::Number::from_f64(f) {
            return Value::Number(n);
        }
    }
    Value::String(v.to_string())
}

#[cfg(test)]
#[path = "../tests/support/unit_cli.rs"]
mod tests;
