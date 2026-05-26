use std::io::{Read, Write};

use crate::limits::{MAX_PARAMS, MAX_SQL_BYTES};
use crate::types::{Permission, QueryOptions, SessionConfig};
use agent_first_data::{cli_parse_log_filters, cli_parse_output, OutputFormat};
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use serde_json::{json, Value};
use std::collections::{btree_map::Entry, BTreeMap};

const STARTUP_ENV_KEYS: &[&str] = &[
    "AFPSQL_DSN_SECRET",
    "AFPSQL_CONNINFO_SECRET",
    "AFPSQL_HOST",
    "AFPSQL_PORT",
    "AFPSQL_USER",
    "AFPSQL_DBNAME",
    "AFPSQL_PASSWORD_SECRET",
    "AFPSQL_SSH",
    "AFPSQL_SSH_LOCAL_HOST",
    "AFPSQL_SSH_LOCAL_PORT",
    "AFPSQL_SSH_REMOTE_SOCKET",
    "AFPSQL_SSH_SUDO_USER",
    "PGHOST",
    "PGPORT",
    "PGUSER",
    "PGDATABASE",
    "PGPASSWORD",
    "PGSSLMODE",
];

pub enum Mode {
    Cli(CliRequest),
    Pipe(PipeInit),
    PsqlAdmin(PsqlAdminRequest),
    SkillAdmin(SkillAdminRequest),
    PsqlUnsupported(PsqlUnsupportedRequest),
}

pub struct PipeInit {
    pub output: OutputFormat,
    pub session: SessionConfig,
    pub log: Vec<String>,
    pub startup_args: Value,
    pub startup_env: Value,
    pub startup_requested: bool,
}

#[derive(Debug, Clone)]
pub struct PsqlAdminRequest {
    pub action: PsqlAdminAction,
    pub output: OutputFormat,
}

#[derive(Debug, Clone)]
pub enum PsqlAdminAction {
    Status { bin_dir: Option<String> },
    Install { bin_dir: Option<String> },
    Uninstall { bin_dir: Option<String> },
}

#[derive(Debug, Clone)]
pub struct SkillAdminRequest {
    pub action: SkillAdminAction,
    pub output: OutputFormat,
}

#[derive(Debug, Clone)]
pub enum SkillAdminAction {
    Status(SkillAdminOptions),
    Install(SkillAdminOptions),
    Uninstall(SkillAdminOptions),
}

#[derive(Debug, Clone)]
pub struct SkillAdminOptions {
    pub agent: SkillAgentSelection,
    pub scope: SkillScope,
    pub skills_dir: Option<String>,
    pub force: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum SkillAgentSelection {
    /// Manage both Codex and Claude Code personal skills.
    All,
    /// Manage the Codex local skill under $CODEX_HOME/skills.
    Codex,
    /// Manage the Claude Code skill under ~/.claude/skills or .claude/skills.
    #[value(name = "claude-code", alias = "claude")]
    ClaudeCode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum SkillScope {
    /// Install under the user-level skills directory.
    Personal,
    /// Install under the current project's skills directory.
    Project,
}

pub struct CliRequest {
    pub sql: String,
    pub params: Vec<Value>,
    pub options: QueryOptions,
    pub session: SessionConfig,
    pub output: OutputFormat,
    pub output_file: Option<String>,
    pub log_file: Option<String>,
    pub log: Vec<String>,
    pub startup_args: Value,
    pub startup_env: Value,
    pub startup_requested: bool,
    pub dry_run: bool,
}

pub struct PsqlUnsupportedRequest {
    pub reason: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum RuntimeMode {
    Cli,
    Pipe,
    #[value(name = "psql")]
    Psql,
}

#[derive(Subcommand)]
enum AfdCommand {
    /// Manage the local psql wrapper for afpsql --mode psql.
    Psql(PsqlCommand),
    /// Manage Agent-First PSQL skills for Codex and Claude Code.
    Skill(SkillCommand),
}

#[derive(Args)]
struct PsqlCommand {
    #[command(subcommand)]
    action: PsqlCliAction,
}

#[derive(Subcommand)]
enum PsqlCliAction {
    /// Show whether the afpsql-managed psql wrapper is installed and active.
    Status(PsqlPathArgs),
    /// Install an afpsql-managed psql wrapper.
    Install(PsqlPathArgs),
    /// Remove an afpsql-managed psql wrapper.
    Uninstall(PsqlPathArgs),
}

#[derive(Args)]
struct PsqlPathArgs {
    /// Directory that contains the psql wrapper. Defaults to the afpsql executable directory.
    #[arg(long = "bin-dir")]
    bin_dir: Option<String>,
}

#[derive(Args)]
struct SkillCommand {
    #[command(subcommand)]
    action: SkillCliAction,
}

#[derive(Subcommand)]
enum SkillCliAction {
    /// Show whether the Agent-First PSQL skill is installed and valid.
    Status(SkillTargetArgs),
    /// Install the Agent-First PSQL skill.
    Install(SkillWriteArgs),
    /// Remove an afpsql-managed Agent-First PSQL skill.
    Uninstall(SkillWriteArgs),
}

#[derive(Args)]
struct SkillTargetArgs {
    /// Agent to manage. Defaults to all personal skill targets.
    #[arg(long = "agent", value_enum, default_value_t = SkillAgentSelection::All)]
    agent: SkillAgentSelection,
    /// Skill scope. Project scope is supported for Claude Code only.
    #[arg(long = "scope", value_enum, default_value_t = SkillScope::Personal)]
    scope: SkillScope,
    /// Directory that contains skill folders. Requires an explicit single --agent.
    #[arg(long = "skills-dir")]
    skills_dir: Option<String>,
}

#[derive(Args)]
struct SkillWriteArgs {
    #[command(flatten)]
    target: SkillTargetArgs,
    /// Overwrite or remove an unmanaged Agent-First PSQL skill at the target path.
    #[arg(long)]
    force: bool,
}

#[doc = r#"Agent-First PostgreSQL client.

`afpsql` gives agents a reliable PostgreSQL contract: structured stdout
events, explicit write permissions, stable pipe sessions, and machine-readable
failures.

### Interface Policy

- default mode is canonical agent-first CLI
- `--mode psql` is argument translation only; runtime output stays JSONL
- stdout carries protocol events; stderr is not a protocol channel
- native CLI and pipe mode default to read-only transactions; writes require permission

### Query Sources and Parameters

- use `--sql` for inline SQL or `--sql-file` for a file
- use repeatable `--param N=value` for positional binds
- placeholder count is validated from prepared-statement metadata, not by SQL text scanning

### Connection Sources

- `--dsn-secret` for a PostgreSQL URI
- `--conninfo-secret` for libpq-style conninfo
- or discrete `--host`, `--port`, `--user`, `--dbname`, `--password-secret`
- add `--ssh user@server` when PostgreSQL is reachable only from the server
- agent-first environment fallbacks: `AFPSQL_*`
- PostgreSQL environment fallbacks: `PGHOST`, `PGPORT`, `PGUSER`, `PGDATABASE`, `PGPASSWORD`, `PGSSLMODE`

### Result Shaping

- default mode buffers a bounded inline result
- use `--stream-rows` for large result sets, with `--batch-rows` and `--batch-bytes` to tune chunk size
- `--output json|yaml|plain` changes rendering only, not the runtime schema

### Examples

```text
afpsql --sql "select now() as now_rfc3339"
afpsql --sql-file ./query.sql
afpsql --sql "select * from users where id = $1" --param 1=123
afpsql --dsn-secret-env DATABASE_URL --sql "select 1"
afpsql --ssh user@server --host 127.0.0.1 --port 5432 --user app --dbname appdb --sql "select 1"
afpsql --mode psql -h 127.0.0.1 -p 5432 -U app -d appdb -c "select 1"
afpsql --sql "select * from big_table" --stream-rows --batch-rows 1000
afpsql --mode pipe
afpsql psql status
afpsql psql install
afpsql skill status
afpsql skill install
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
    #[arg(long, allow_hyphen_values = true, help_heading = "Query")]
    sql: Option<String>,
    /// Read SQL from a file.
    #[arg(long = "sql-file", allow_hyphen_values = true, help_heading = "Query")]
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
    /// Query permission: read, write, ssh-read, or ssh-write. Defaults to read, or ssh-read with --ssh.
    #[arg(long = "permission", value_parser = parse_permission_arg, help_heading = "Query")]
    permission: Option<Permission>,
    /// Preview the query without executing it
    #[arg(long, help_heading = "Query")]
    dry_run: bool,

    /// PostgreSQL DSN URI. Redacted in structured output.
    #[arg(long = "dsn-secret", help_heading = "Connection")]
    dsn_secret: Option<String>,
    /// Read PostgreSQL DSN URI from an environment variable.
    #[arg(long = "dsn-secret-env", help_heading = "Connection")]
    dsn_secret_env: Option<String>,
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
    /// Read PostgreSQL password from an environment variable.
    #[arg(long = "password-secret-env", help_heading = "Connection")]
    password_secret_env: Option<String>,
    /// Open an SSH transport to USER@HOST before connecting to PostgreSQL.
    #[arg(long = "ssh", help_heading = "SSH Transport")]
    ssh: Option<String>,
    /// Additional OpenSSH -o option. Repeat for multiple options.
    #[arg(long = "ssh-option", help_heading = "SSH Transport")]
    ssh_options: Vec<String>,
    /// Local bind host for the SSH tunnel.
    #[arg(long = "ssh-local-host", help_heading = "SSH Transport")]
    ssh_local_host: Option<String>,
    /// Local bind port for the SSH tunnel. Defaults to an ephemeral port.
    #[arg(long = "ssh-local-port", help_heading = "SSH Transport")]
    ssh_local_port: Option<u16>,
    /// Explicit remote PostgreSQL Unix socket path for SSH forwarding.
    #[arg(long = "ssh-remote-socket", help_heading = "SSH Transport")]
    ssh_remote_socket: Option<String>,
    /// Remote OS user for sudo -n Unix-socket bridge mode; requires an explicit socket.
    #[arg(long = "ssh-sudo-user", help_heading = "SSH Transport")]
    ssh_sudo_user: Option<String>,

    /// Output format: json (default), yaml, or plain.
    #[arg(long, default_value = "json", global = true, help_heading = "Runtime")]
    output: String,
    /// Diagnostic log categories.
    #[arg(long = "log", value_delimiter = ',', help_heading = "Runtime")]
    log: Vec<String>,
    /// Runtime mode: canonical cli, pipe, or `psql` translation mode.
    #[arg(long, value_enum, default_value_t = RuntimeMode::Cli, help_heading = "Runtime")]
    mode: RuntimeMode,

    #[command(subcommand)]
    command: Option<AfdCommand>,
}

pub fn parse_args() -> Result<Mode, String> {
    let raw: Vec<String> = std::env::args().collect();
    if is_psql_mode_requested(&raw) {
        return parse_psql_mode(&raw);
    }
    let startup_requested = startup_requested_from_raw(&raw);

    // --help: recursive plain-text help (all subcommands expanded)
    if top_level_help_requested(&raw) {
        let _ = writeln!(
            std::io::stdout(),
            "{}",
            agent_first_data::cli_render_help(&AfdCli::command(), &[])
        );
        std::process::exit(0);
    }
    // --help-markdown: Markdown for doc generation
    if top_level_help_markdown_requested(&raw) {
        let _ = writeln!(
            std::io::stdout(),
            "{}",
            agent_first_data::cli_render_help_markdown(&AfdCli::command(), &[])
        );
        std::process::exit(0);
    }

    let cli = match AfdCli::try_parse_from(&raw) {
        Ok(c) => c,
        Err(e) => {
            use clap::error::ErrorKind;
            if matches!(e.kind(), ErrorKind::DisplayVersion | ErrorKind::DisplayHelp) {
                let _ = writeln!(std::io::stdout(), "{e}");
                std::process::exit(0);
            }
            return Err(e.to_string());
        }
    };
    let output = parse_output(&cli.output)?;
    let log = parse_log_categories(&cli.log);
    let dsn_secret = resolve_secret_value(
        "--dsn-secret",
        cli.dsn_secret,
        cli.dsn_secret_env.as_deref(),
    )?;
    let password_secret = resolve_secret_value(
        "--password-secret",
        cli.password_secret,
        cli.password_secret_env.as_deref(),
    )?;
    let session = SessionConfig {
        dsn_secret,
        conninfo_secret: cli.conninfo_secret,
        host: cli.host,
        port: cli.port,
        user: cli.user,
        dbname: cli.dbname,
        password_secret,
        ssh: cli.ssh.or_else(|| std::env::var("AFPSQL_SSH").ok()),
        ssh_options: cli.ssh_options,
        ssh_local_host: cli
            .ssh_local_host
            .or_else(|| std::env::var("AFPSQL_SSH_LOCAL_HOST").ok()),
        ssh_local_port: cli.ssh_local_port.or_else(|| {
            std::env::var("AFPSQL_SSH_LOCAL_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
        }),
        ssh_remote_socket: cli
            .ssh_remote_socket
            .or_else(|| std::env::var("AFPSQL_SSH_REMOTE_SOCKET").ok()),
        ssh_sudo_user: cli
            .ssh_sudo_user
            .or_else(|| std::env::var("AFPSQL_SSH_SUDO_USER").ok()),
    };
    let mode_name = match cli.mode {
        RuntimeMode::Cli => "cli",
        RuntimeMode::Pipe => "pipe",
        RuntimeMode::Psql => "psql",
    };
    let startup_env = startup_env_snapshot();

    if let Some(command) = cli.command {
        return Ok(match command {
            AfdCommand::Psql(psql) => Mode::PsqlAdmin(PsqlAdminRequest {
                action: psql_admin_action(psql.action),
                output,
            }),
            AfdCommand::Skill(skill) => Mode::SkillAdmin(SkillAdminRequest {
                action: skill_admin_action(skill.action),
                output,
            }),
        });
    }

    match cli.mode {
        RuntimeMode::Pipe => {
            return Ok(Mode::Pipe(PipeInit {
                output,
                session,
                log: log.clone(),
                startup_args: startup_args(mode_name, None, None, 0),
                startup_env,
                startup_requested,
            }));
        }
        RuntimeMode::Cli | RuntimeMode::Psql => {}
    }

    let startup_sql_file = cli.sql_file.clone();
    let sql = load_sql(cli.sql, cli.sql_file)?;
    let params = parse_params(&cli.param)?;
    let startup_args = startup_args(
        mode_name,
        Some(&sql),
        startup_sql_file.as_deref(),
        params.len(),
    );

    let options = QueryOptions {
        stream_rows: cli.stream_rows,
        batch_rows: cli.batch_rows,
        batch_bytes: cli.batch_bytes,
        statement_timeout_ms: cli.statement_timeout_ms,
        lock_timeout_ms: cli.lock_timeout_ms,
        permission: cli.permission,
        inline_max_rows: cli.inline_max_rows,
        inline_max_bytes: cli.inline_max_bytes,
    };

    Ok(Mode::Cli(CliRequest {
        sql,
        params,
        options,
        session,
        output,
        output_file: None,
        log_file: None,
        log,
        startup_args,
        startup_env,
        startup_requested,
        dry_run: cli.dry_run,
    }))
}

fn parse_psql_mode(raw: &[String]) -> Result<Mode, String> {
    let startup_requested = startup_requested_from_raw(raw);
    let mut state = PsqlModeState::default();

    let mut i = 1usize;
    while i < raw.len() {
        let arg = raw[i].as_str();
        if arg == "--" {
            i += 1;
            while i < raw.len() {
                state.positionals.push(raw[i].clone());
                i += 1;
            }
            break;
        }
        if arg.starts_with("--") {
            parse_psql_long_arg(raw, &mut i, &mut state)?;
            continue;
        }
        if arg.starts_with('-') && arg.len() > 1 {
            parse_psql_short_arg(raw, &mut i, &mut state)?;
            continue;
        }
        state.positionals.push(raw[i].clone());
        i += 1;
    }

    if let Some(reason) = state.interactive_reason {
        return Ok(Mode::PsqlUnsupported(PsqlUnsupportedRequest { reason }));
    }

    apply_psql_positionals(&mut state)?;
    if state.list_databases {
        state.sql = Some(psql_list_databases_sql());
        state.sql_file = None;
    }
    if state.sql.is_none() && state.sql_file.is_none() {
        return Ok(Mode::PsqlUnsupported(PsqlUnsupportedRequest {
            reason: "no -c/--command, -f/--file, or -l/--list was provided".to_string(),
        }));
    }

    let dsn_secret = resolve_secret_value(
        "--dsn-secret",
        state.dsn_secret,
        state.dsn_secret_env.as_deref(),
    )?;
    let password_secret = resolve_secret_value(
        "--password-secret",
        state.password_secret,
        state.password_secret_env.as_deref(),
    )?;
    let session = SessionConfig {
        dsn_secret,
        conninfo_secret: state.conninfo_secret,
        host: state.host,
        port: state.port,
        user: state.user,
        dbname: state.dbname,
        password_secret,
        ssh: None,
        ssh_options: vec![],
        ssh_local_host: None,
        ssh_local_port: None,
        ssh_remote_socket: None,
        ssh_sudo_user: None,
    };

    let startup_sql_file = state.sql_file.clone();
    let sql = load_sql(state.sql, state.sql_file)?;
    let params = parse_params(&state.params_kv)?;
    let startup_args = psql_startup_args(PsqlStartupArgs {
        mode: "psql",
        sql: Some(&sql),
        sql_file: startup_sql_file,
        param_count: params.len(),
    });
    Ok(Mode::Cli(CliRequest {
        sql,
        params,
        options: QueryOptions {
            permission: Some(Permission::Write),
            ..Default::default()
        },
        session,
        output: state.output,
        output_file: state.output_file,
        log_file: state.log_file,
        log: parse_log_categories(&state.log_entries),
        startup_args,
        startup_env: startup_env_snapshot(),
        startup_requested,
        dry_run: false,
    }))
}

struct PsqlModeState {
    sql: Option<String>,
    sql_file: Option<String>,
    host: Option<String>,
    port: Option<u16>,
    user: Option<String>,
    dbname: Option<String>,
    dsn_secret: Option<String>,
    dsn_secret_env: Option<String>,
    conninfo_secret: Option<String>,
    password_secret: Option<String>,
    password_secret_env: Option<String>,
    params_kv: Vec<String>,
    output: OutputFormat,
    log_entries: Vec<String>,
    output_file: Option<String>,
    log_file: Option<String>,
    list_databases: bool,
    positionals: Vec<String>,
    interactive_reason: Option<String>,
}

impl Default for PsqlModeState {
    fn default() -> Self {
        Self {
            sql: None,
            sql_file: None,
            host: None,
            port: None,
            user: None,
            dbname: None,
            dsn_secret: None,
            dsn_secret_env: None,
            conninfo_secret: None,
            password_secret: None,
            password_secret_env: None,
            params_kv: vec![],
            output: OutputFormat::Json,
            log_entries: vec![],
            output_file: None,
            log_file: None,
            list_databases: false,
            positionals: vec![],
            interactive_reason: None,
        }
    }
}

impl PsqlModeState {
    fn set_sql(&mut self, sql: String, flag: &str) -> Result<(), String> {
        if self.sql.is_some() || self.sql_file.is_some() {
            return Err(format!(
                "psql mode currently supports only one -c/--command or -f/--file source; repeated source at {flag}"
            ));
        }
        self.sql = Some(sql);
        Ok(())
    }

    fn set_sql_file(&mut self, path: String, flag: &str) -> Result<(), String> {
        if self.sql.is_some() || self.sql_file.is_some() {
            return Err(format!(
                "psql mode currently supports only one -c/--command or -f/--file source; repeated source at {flag}"
            ));
        }
        self.sql_file = Some(path);
        Ok(())
    }
}

fn parse_psql_long_arg(
    raw: &[String],
    i: &mut usize,
    state: &mut PsqlModeState,
) -> Result<(), String> {
    let arg = raw[*i].as_str();
    if arg == "--mode" {
        let value = take_arg_value(raw, i, "--mode")?;
        if value != "psql" {
            return Err(format!(
                "unsupported psql-mode argument: --mode {value}; only --mode psql is allowed with psql translation"
            ));
        }
        return Ok(());
    }
    if let Some(value) = arg.strip_prefix("--mode=") {
        if value != "psql" {
            return Err(format!(
                "unsupported psql-mode argument: {arg}; only --mode=psql is allowed with psql translation"
            ));
        }
        *i += 1;
        return Ok(());
    }

    if arg == "--help" || arg.starts_with("--help=") {
        emit_psql_mode_help();
        std::process::exit(0);
    }
    if arg == "--version" {
        emit_psql_mode_version();
        std::process::exit(0);
    }

    match long_name(arg) {
        "--command" => {
            let value = take_long_arg_value(raw, i, "--command")?;
            state.set_sql(value, "--command")
        }
        "--file" => {
            let value = take_long_arg_value(raw, i, "--file")?;
            state.set_sql_file(value, "--file")
        }
        "--host" => {
            state.host = Some(take_long_arg_value(raw, i, "--host")?);
            Ok(())
        }
        "--port" => {
            state.port = Some(parse_port(
                &take_long_arg_value(raw, i, "--port")?,
                "--port",
            )?);
            Ok(())
        }
        "--username" | "--user" => {
            state.user = Some(take_long_arg_value(raw, i, long_name(arg))?);
            Ok(())
        }
        "--dbname" => {
            apply_dbname_value(state, take_long_arg_value(raw, i, "--dbname")?);
            Ok(())
        }
        "--set" | "--variable" => {
            let value = take_long_arg_value(raw, i, long_name(arg))?;
            add_psql_variable(state, value)
        }
        "--list" => {
            state.list_databases = true;
            *i += 1;
            Ok(())
        }
        "--no-password"
        | "--no-psqlrc"
        | "--no-readline"
        | "--quiet"
        | "--echo-all"
        | "--echo-errors"
        | "--echo-queries"
        | "--echo-hidden"
        | "--no-align"
        | "--csv"
        | "--html"
        | "--tuples-only"
        | "--expanded"
        | "--field-separator-zero"
        | "--record-separator-zero"
        | "--single-transaction" => {
            *i += 1;
            Ok(())
        }
        "--field-separator" | "--record-separator" | "--pset" | "--table-attr" => {
            let _ = take_long_arg_value(raw, i, long_name(arg))?;
            Ok(())
        }
        "--password" => {
            state.interactive_reason =
                Some("--password/-W requests an interactive password prompt".to_string());
            *i += 1;
            Ok(())
        }
        "--single-step" => {
            state.interactive_reason =
                Some("--single-step/-s requires interactive command confirmation".to_string());
            *i += 1;
            Ok(())
        }
        "--single-line" => {
            state.interactive_reason =
                Some("--single-line/-S is a human-interactive input mode".to_string());
            *i += 1;
            Ok(())
        }
        "--dsn-secret" => {
            state.dsn_secret = Some(take_long_arg_value(raw, i, "--dsn-secret")?);
            Ok(())
        }
        "--dsn-secret-env" => {
            state.dsn_secret_env = Some(take_long_arg_value(raw, i, "--dsn-secret-env")?);
            Ok(())
        }
        "--conninfo-secret" => {
            state.conninfo_secret = Some(take_long_arg_value(raw, i, "--conninfo-secret")?);
            Ok(())
        }
        "--password-secret" => {
            state.password_secret = Some(take_long_arg_value(raw, i, "--password-secret")?);
            Ok(())
        }
        "--password-secret-env" => {
            state.password_secret_env = Some(take_long_arg_value(raw, i, "--password-secret-env")?);
            Ok(())
        }
        "--output" => {
            let value = take_long_arg_value(raw, i, "--output")?;
            if let Ok(format) = parse_output(&value) {
                state.output = format;
            } else {
                state.output_file = Some(value);
            }
            Ok(())
        }
        "--output-format" => {
            let value = take_long_arg_value(raw, i, long_name(arg))?;
            state.output = parse_output(&value)?;
            Ok(())
        }
        "--log" => {
            let values = take_long_arg_value(raw, i, "--log")?;
            add_log_entries(state, &values);
            Ok(())
        }
        "--log-file" => {
            state.log_file = Some(take_long_arg_value(raw, i, "--log-file")?);
            Ok(())
        }
        _ => Err(format!("unsupported psql-mode argument: {arg}")),
    }
}

fn parse_psql_short_arg(
    raw: &[String],
    i: &mut usize,
    state: &mut PsqlModeState,
) -> Result<(), String> {
    let arg = raw[*i].as_str();
    let mut offset = 1usize;
    while offset < arg.len() {
        let flag = arg.as_bytes()[offset] as char;
        offset += 1;
        match flag {
            '?' => {
                emit_psql_mode_help();
                std::process::exit(0);
            }
            'V' => {
                emit_psql_mode_version();
                std::process::exit(0);
            }
            'c' => {
                let value = take_short_arg_value(raw, i, arg, offset, "-c")?;
                return state.set_sql(value, "-c");
            }
            'f' => {
                let value = take_short_arg_value(raw, i, arg, offset, "-f")?;
                return state.set_sql_file(value, "-f");
            }
            'h' => {
                state.host = Some(take_short_arg_value(raw, i, arg, offset, "-h")?);
                return Ok(());
            }
            'p' => {
                let value = take_short_arg_value(raw, i, arg, offset, "-p")?;
                state.port = Some(parse_port(&value, "-p")?);
                return Ok(());
            }
            'U' => {
                state.user = Some(take_short_arg_value(raw, i, arg, offset, "-U")?);
                return Ok(());
            }
            'd' => {
                apply_dbname_value(state, take_short_arg_value(raw, i, arg, offset, "-d")?);
                return Ok(());
            }
            'v' => {
                let value = take_short_arg_value(raw, i, arg, offset, "-v")?;
                return add_psql_variable(state, value);
            }
            'F' | 'P' | 'R' | 'T' => {
                let _ = take_short_arg_value(raw, i, arg, offset, &format!("-{flag}"))?;
                return Ok(());
            }
            'L' => {
                state.log_file = Some(take_short_arg_value(raw, i, arg, offset, "-L")?);
                return Ok(());
            }
            'o' => {
                state.output_file = Some(take_short_arg_value(raw, i, arg, offset, "-o")?);
                return Ok(());
            }
            'l' => state.list_databases = true,
            'W' => {
                state.interactive_reason =
                    Some("--password/-W requests an interactive password prompt".to_string());
            }
            's' => {
                state.interactive_reason =
                    Some("--single-step/-s requires interactive command confirmation".to_string());
            }
            'S' => {
                state.interactive_reason =
                    Some("--single-line/-S is a human-interactive input mode".to_string());
            }
            'a' | 'A' | 'b' | 'e' | 'E' | 'H' | 'n' | 'q' | 't' | 'w' | 'x' | 'X' | 'z' | '0'
            | '1' => {}
            _ => return Err(format!("unsupported psql-mode argument: -{flag}")),
        }
    }
    *i += 1;
    Ok(())
}

fn long_name(arg: &str) -> &str {
    arg.split_once('=').map(|(name, _)| name).unwrap_or(arg)
}

fn take_arg_value(raw: &[String], i: &mut usize, flag: &str) -> Result<String, String> {
    *i += 1;
    let value = raw
        .get(*i)
        .ok_or_else(|| format!("{flag} requires value"))?
        .clone();
    *i += 1;
    Ok(value)
}

fn take_long_arg_value(raw: &[String], i: &mut usize, flag: &str) -> Result<String, String> {
    let arg = raw[*i].as_str();
    if let Some((_, value)) = arg.split_once('=') {
        *i += 1;
        return Ok(value.to_string());
    }
    take_arg_value(raw, i, flag)
}

fn take_short_arg_value(
    raw: &[String],
    i: &mut usize,
    arg: &str,
    offset: usize,
    flag: &str,
) -> Result<String, String> {
    if offset < arg.len() {
        let value = arg[offset..].to_string();
        *i += 1;
        return Ok(value);
    }
    take_arg_value(raw, i, flag)
}

fn parse_port(value: &str, flag: &str) -> Result<u16, String> {
    value.parse().map_err(|_| format!("invalid {flag} port"))
}

fn add_log_entries(state: &mut PsqlModeState, values: &str) {
    for part in values.split(',') {
        let trimmed = part.trim();
        if !trimmed.is_empty() {
            state.log_entries.push(trimmed.to_string());
        }
    }
}

fn add_psql_variable(state: &mut PsqlModeState, value: String) -> Result<(), String> {
    let name = value
        .split_once('=')
        .map(|(name, _)| name)
        .unwrap_or(value.as_str());
    if name.parse::<usize>().is_ok() {
        if value.contains('=') {
            state.params_kv.push(value);
            return Ok(());
        }
        return Err(format!("invalid param '{value}', expected N=value"));
    }
    if is_psql_behavior_variable(name) {
        return Ok(());
    }
    Err(format!(
        "invalid or unsupported psql variable '{name}'; afpsql supports numeric -v N=value bind parameters, not client-side :name interpolation"
    ))
}

fn is_psql_behavior_variable(name: &str) -> bool {
    matches!(
        name.to_ascii_uppercase().as_str(),
        "ON_ERROR_STOP"
            | "ON_ERROR_ROLLBACK"
            | "QUIET"
            | "ECHO"
            | "ECHO_HIDDEN"
            | "FETCH_COUNT"
            | "VERBOSITY"
            | "SHOW_CONTEXT"
            | "HISTCONTROL"
            | "HISTFILE"
            | "HISTSIZE"
            | "IGNOREEOF"
            | "PAGER"
            | "COLUMNS"
    )
}

fn apply_psql_positionals(state: &mut PsqlModeState) -> Result<(), String> {
    let positionals = std::mem::take(&mut state.positionals);
    for value in positionals {
        if is_postgres_uri(&value) {
            state.dsn_secret = Some(value);
            continue;
        }
        if looks_like_conninfo(&value) {
            state.conninfo_secret = Some(value);
            continue;
        }
        if state.dbname.is_none() {
            state.dbname = Some(value);
            continue;
        }
        if state.user.is_none() {
            state.user = Some(value);
            continue;
        }
        return Err(format!("too many positional psql arguments: {value}"));
    }
    Ok(())
}

fn apply_dbname_value(state: &mut PsqlModeState, value: String) {
    if is_postgres_uri(&value) {
        state.dsn_secret = Some(value);
    } else if looks_like_conninfo(&value) {
        state.conninfo_secret = Some(value);
    } else {
        state.dbname = Some(value);
    }
}

fn is_postgres_uri(value: &str) -> bool {
    value.starts_with("postgresql://") || value.starts_with("postgres://")
}

fn looks_like_conninfo(value: &str) -> bool {
    value.contains('=')
}

fn psql_list_databases_sql() -> String {
    "select datname as name from pg_catalog.pg_database where datallowconn order by datname"
        .to_string()
}

fn emit_psql_mode_version() {
    let _ = writeln!(
        std::io::stdout(),
        "psql (afpsql wrapper) {}",
        env!("CARGO_PKG_VERSION")
    );
}

fn emit_psql_mode_help() {
    let _ = writeln!(
        std::io::stdout(),
        "psql (afpsql wrapper) {}\n\
Usage:\n  psql [OPTION]... [DBNAME [USERNAME]]\n\n\
Supported non-interactive forms:\n  -c, --command=SQL\n  -f, --file=FILE\n  -l, --list\n  -h/-p/-U/-d and --host/--port/--username/--dbname\n  -v N=value, --set N=value for positional bind parameters\n\n\
Output routing:\n  -o, --output=FILE writes structured output to FILE\n  -L, --log-file=FILE tees structured output to FILE\n  --output-format=json|yaml|plain changes afpsql rendering\n\n\
Human-interactive psql modes and psql meta-commands are not supported by this wrapper.",
        env!("CARGO_PKG_VERSION")
    );
}

fn top_level_help_requested(raw: &[String]) -> bool {
    raw.len() == 2 && matches!(raw.get(1).map(String::as_str), Some("--help" | "-h"))
}

fn top_level_help_markdown_requested(raw: &[String]) -> bool {
    let mut i = 1usize;
    while i < raw.len() {
        let arg = raw[i].as_str();
        if arg == "--" {
            break;
        }
        if arg == "--help-markdown" {
            return true;
        }
        if arg == "--mode" {
            i += 2;
            continue;
        }
        if top_level_arg_consumes_value(arg) {
            i += if arg.contains('=') { 1 } else { 2 };
            continue;
        }
        if arg.starts_with('-') {
            i += 1;
            continue;
        }
        break;
    }
    false
}

fn psql_admin_action(action: PsqlCliAction) -> PsqlAdminAction {
    match action {
        PsqlCliAction::Status(args) => PsqlAdminAction::Status {
            bin_dir: args.bin_dir,
        },
        PsqlCliAction::Install(args) => PsqlAdminAction::Install {
            bin_dir: args.bin_dir,
        },
        PsqlCliAction::Uninstall(args) => PsqlAdminAction::Uninstall {
            bin_dir: args.bin_dir,
        },
    }
}

fn skill_admin_action(action: SkillCliAction) -> SkillAdminAction {
    match action {
        SkillCliAction::Status(args) => SkillAdminAction::Status(skill_options(args, false)),
        SkillCliAction::Install(args) => {
            SkillAdminAction::Install(skill_options(args.target, args.force))
        }
        SkillCliAction::Uninstall(args) => {
            SkillAdminAction::Uninstall(skill_options(args.target, args.force))
        }
    }
}

fn skill_options(args: SkillTargetArgs, force: bool) -> SkillAdminOptions {
    SkillAdminOptions {
        agent: args.agent,
        scope: args.scope,
        skills_dir: args.skills_dir,
        force,
    }
}

fn is_psql_mode_requested(raw: &[String]) -> bool {
    let mut i = 1usize;
    while i < raw.len() {
        let arg = raw[i].as_str();
        if arg == "--" {
            break;
        }
        if arg == "--mode" {
            if let Some(v) = raw.get(i + 1) {
                return v == "psql";
            }
            return false;
        }
        if arg == "--mode=psql" {
            return true;
        }
        if top_level_arg_consumes_value(arg) {
            i += if arg.contains('=') { 1 } else { 2 };
            continue;
        }
        if arg.starts_with('-') {
            i += 1;
            continue;
        }
        break;
    }
    false
}

fn top_level_arg_consumes_value(arg: &str) -> bool {
    let name = arg.split_once('=').map(|(name, _)| name).unwrap_or(arg);
    matches!(
        name,
        "--sql"
            | "--sql-file"
            | "--param"
            | "--batch-rows"
            | "--batch-bytes"
            | "--statement-timeout-ms"
            | "--lock-timeout-ms"
            | "--inline-max-rows"
            | "--inline-max-bytes"
            | "--permission"
            | "--dsn-secret"
            | "--dsn-secret-env"
            | "--conninfo-secret"
            | "--host"
            | "--port"
            | "--user"
            | "--dbname"
            | "--password-secret"
            | "--password-secret-env"
            | "--ssh"
            | "--ssh-option"
            | "--ssh-local-host"
            | "--ssh-local-port"
            | "--ssh-remote-socket"
            | "--ssh-sudo-user"
            | "--output"
            | "--log"
    )
}

fn load_sql(sql: Option<String>, sql_file: Option<String>) -> Result<String, String> {
    match (sql, sql_file) {
        (Some(s), None) => validate_sql_size(s),
        (None, Some(path)) if path == "-" => {
            let stdin = std::io::stdin();
            read_limited_sql(stdin.lock(), "read --sql-file -")
        }
        (None, Some(path)) => {
            let metadata =
                std::fs::metadata(&path).map_err(|e| format!("read --sql-file failed: {e}"))?;
            if metadata.is_file() && metadata.len() > MAX_SQL_BYTES as u64 {
                return Err(sql_size_error());
            }
            let file =
                std::fs::File::open(&path).map_err(|e| format!("read --sql-file failed: {e}"))?;
            read_limited_sql(file, "read --sql-file")
        }
        (Some(_), Some(_)) => Err("--sql and --sql-file are mutually exclusive".to_string()),
        (None, None) => Err("one of --sql or --sql-file is required".to_string()),
    }
}

fn read_limited_sql<R: Read>(reader: R, context: &str) -> Result<String, String> {
    let mut buf = Vec::new();
    let mut limited = reader.take(MAX_SQL_BYTES as u64 + 1);
    limited
        .read_to_end(&mut buf)
        .map_err(|e| format!("{context} failed: {e}"))?;
    if buf.len() > MAX_SQL_BYTES {
        return Err(sql_size_error());
    }
    String::from_utf8(buf).map_err(|e| format!("{context} failed: {e}"))
}

fn validate_sql_size(sql: String) -> Result<String, String> {
    if sql.len() > MAX_SQL_BYTES {
        return Err(sql_size_error());
    }
    Ok(sql)
}

fn sql_size_error() -> String {
    format!("sql exceeds maximum size; maximum SQL size is {MAX_SQL_BYTES} bytes")
}

fn parse_output(v: &str) -> Result<OutputFormat, String> {
    cli_parse_output(v)
}

fn parse_permission_arg(v: &str) -> Result<Permission, String> {
    v.parse()
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

fn startup_env_snapshot() -> Value {
    Value::Array(
        STARTUP_ENV_KEYS
            .iter()
            .map(|key| {
                json!({
                    "key": key,
                    "present": std::env::var_os(key).is_some(),
                })
            })
            .collect(),
    )
}

fn startup_args(
    mode: &str,
    sql: Option<&str>,
    sql_file: Option<&str>,
    param_count: usize,
) -> Value {
    json!({
        "mode": mode,
        "sql": startup_sql_summary(sql, sql_file),
        "param_count": param_count,
    })
}

fn startup_sql_summary(sql: Option<&str>, sql_file: Option<&str>) -> Value {
    let Some(sql) = sql else {
        return json!({
            "present": false,
            "source": "none",
            "bytes": 0,
            "chars": 0,
            "operation": null,
        });
    };
    json!({
        "present": true,
        "source": if sql_file.is_some() { "file" } else { "inline" },
        "bytes": sql.len(),
        "chars": sql.chars().count(),
        "operation": sql_operation(sql),
    })
}

fn sql_operation(sql: &str) -> Option<String> {
    let sql = trim_leading_sql_comments(sql);
    let token: String = sql
        .chars()
        .skip_while(|c| c.is_whitespace())
        .take_while(|c| c.is_ascii_alphabetic() || *c == '_')
        .collect();
    if token.is_empty() {
        None
    } else {
        Some(token.to_ascii_lowercase())
    }
}

fn trim_leading_sql_comments(mut sql: &str) -> &str {
    loop {
        sql = sql.trim_start();
        if let Some(rest) = sql.strip_prefix("--") {
            sql = rest.split_once('\n').map(|(_, rest)| rest).unwrap_or("");
            continue;
        }
        if let Some(rest) = sql.strip_prefix("/*") {
            let Some((_, after)) = rest.split_once("*/") else {
                return "";
            };
            sql = after;
            continue;
        }
        return sql;
    }
}

struct PsqlStartupArgs<'a> {
    mode: &'a str,
    sql: Option<&'a str>,
    sql_file: Option<String>,
    param_count: usize,
}

fn psql_startup_args(args: PsqlStartupArgs<'_>) -> Value {
    startup_args(
        args.mode,
        args.sql,
        args.sql_file.as_deref(),
        args.param_count,
    )
}

fn resolve_secret_value(
    flag_name: &str,
    direct: Option<String>,
    env_name: Option<&str>,
) -> Result<Option<String>, String> {
    match (direct, env_name) {
        (Some(_), Some(_)) => Err(format!(
            "{flag_name} and {flag_name}-env are mutually exclusive"
        )),
        (Some(value), None) => Ok(Some(value)),
        (None, Some(name)) => {
            if name.is_empty() {
                return Err(format!(
                    "{flag_name}-env requires a non-empty variable name"
                ));
            }
            std::env::var(name).map(Some).map_err(|_| {
                format!("{flag_name}-env references unset environment variable: {name}")
            })
        }
        (None, None) => Ok(None),
    }
}

pub fn parse_params(entries: &[String]) -> Result<Vec<Value>, String> {
    if entries.len() > MAX_PARAMS {
        return Err(format!("too many params; maximum params is {MAX_PARAMS}"));
    }

    let mut by_index: BTreeMap<usize, Value> = BTreeMap::new();
    for entry in entries {
        let (idx, raw) = split_index_value(entry)?;
        if idx == 0 {
            return Err("param index must start at 1".to_string());
        }
        if idx > MAX_PARAMS {
            return Err(format!(
                "parameter index {idx} exceeds maximum params {MAX_PARAMS}"
            ));
        }
        match by_index.entry(idx) {
            Entry::Vacant(slot) => {
                slot.insert(parse_param_value(raw));
            }
            Entry::Occupied(_) => return Err(format!("duplicate parameter index {idx}")),
        }
    }
    if by_index.is_empty() {
        return Ok(vec![]);
    }
    let max = by_index.keys().max().copied().unwrap_or(0);
    for i in 1..=max {
        if !by_index.contains_key(&i) {
            return Err(format!("missing parameter index {i}"));
        }
    }
    Ok(by_index.into_values().collect())
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
