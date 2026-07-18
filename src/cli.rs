use std::io::{Read, Write};

use crate::limits::{MAX_PARAMS, MAX_SQL_BYTES};
use crate::secret_config::{SecretConfigRef, resolve_config_secret};
use crate::types::{ContainerConfig, Permission, QueryOptions, SessionConfig, SshConfig};
use agent_first_data::{LogFilters, OutputFormat, cli_parse_log_filters, cli_parse_output};
use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use serde_json::{Value, json};
use std::collections::{BTreeMap, btree_map::Entry};

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
    "AFPSQL_CONTAINER",
    "AFPSQL_CONTAINER_DRIVER",
    "AFPSQL_CONTAINER_RUNTIME",
    "AFPSQL_CONTAINER_USER",
    "AFPSQL_CONTAINER_NAMESPACE",
    "AFPSQL_CONTAINER_CONTEXT",
    "AFPSQL_CONTAINER_COMPOSE_FILE",
    "AFPSQL_CONTAINER_COMPOSE_PROJECT",
    "AFPSQL_CONTAINER_POD_CONTAINER",
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
    pub log: LogFilters,
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
    /// Manage every agent that supports the requested scope.
    All,
    /// Manage the Codex local skill under $CODEX_HOME/skills.
    Codex,
    /// Manage the Claude Code skill under ~/.claude/skills or .claude/skills.
    #[value(name = "claude-code", alias = "claude")]
    ClaudeCode,
    /// Manage the opencode skill under ~/.config/opencode/skills or .opencode/skills.
    Opencode,
    /// Manage the Hermes skill under $HERMES_HOME/skills or ~/.hermes/skills.
    Hermes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum SkillScope {
    /// Install under the user-level skills directory.
    Personal,
    /// Install under the current workspace's skills directory.
    Workspace,
}

pub struct CliRequest {
    pub sql: String,
    pub params: Vec<Value>,
    pub options: QueryOptions,
    pub session: SessionConfig,
    pub output: OutputFormat,
    pub log: LogFilters,
    pub startup_args: Value,
    pub startup_env: Value,
    pub startup_requested: bool,
    pub dry_run: bool,
    pub psql_mode: bool,
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
    /// Manage Agent-First PSQL skills for Codex, Claude Code, opencode, and Hermes.
    Skill(SkillCommand),
    /// Schema discovery: inspect databases, schemas, tables, indexes, or snapshots.
    Inspect(InspectCommand),
}

#[derive(Args)]
struct InspectCommand {
    #[command(subcommand)]
    action: InspectAction,
}

#[derive(Subcommand)]
enum InspectAction {
    /// List databases on the connected server with size, encoding, and connection facts.
    Databases(InspectDatabasesArgs),
    /// Summarize the connected database: schema/table/view/sequence counts and size.
    Database,
    /// List user-visible schemas.
    Schemas,
    /// Export full schema metadata for one schema.
    Schema(InspectSchemaArgs),
    /// Export a stable full-schema snapshot for machine consumption.
    Snapshot(InspectSchemaArgs),
    /// List tables in a schema with owner, estimated rows, and size.
    Tables(InspectTablesArgs),
    /// List views (regular and materialized) in a schema with owner.
    Views(InspectViewsArgs),
    /// List indexes with definitions, size, validity, and optional usage stats.
    Indexes(InspectIndexesArgs),
    /// Describe a table's columns: types, nullability, defaults, primary key, comments.
    Table(InspectTableArgs),
}

#[derive(Args)]
struct InspectDatabasesArgs {
    /// Include template databases (template0/template1) in the listing.
    #[arg(long = "all")]
    all: bool,
}

#[derive(Args)]
struct InspectTablesArgs {
    /// Schema to filter on. Defaults to `public`.
    #[arg(long = "schema", default_value = "public")]
    schema: String,
    /// Optional `LIKE` pattern matched against the table name (use `%` as wildcard).
    #[arg(long = "like")]
    like: Option<String>,
}

#[derive(Args)]
struct InspectSchemaArgs {
    /// Schema to inspect. Defaults to `public`.
    #[arg(long = "schema", default_value = "public")]
    schema: String,
    /// Optional `LIKE` pattern matched against relation names (use `%` as wildcard).
    #[arg(long = "like")]
    like: Option<String>,
}

#[derive(Args)]
struct InspectViewsArgs {
    /// Schema to filter on. Defaults to `public`.
    #[arg(long = "schema", default_value = "public")]
    schema: String,
    /// Optional `LIKE` pattern matched against the view name (use `%` as wildcard).
    #[arg(long = "like")]
    like: Option<String>,
}

#[derive(Args)]
struct InspectIndexesArgs {
    /// Schema to filter on. Defaults to `public`.
    #[arg(long = "schema", default_value = "public")]
    schema: String,
    /// Optional table name to filter on. Accepts `schema.table` to override --schema.
    #[arg(long = "table")]
    table: Option<String>,
    /// Include PostgreSQL's built-in pg_stat_user_indexes usage counters.
    #[arg(long = "stats")]
    stats: bool,
}

#[derive(Args)]
struct InspectTableArgs {
    /// Table name. Accepts `schema.table`; defaults to `public.NAME` when unqualified.
    name: String,
    /// Include relation, constraints, indexes, triggers, and sequence/default metadata.
    #[arg(long = "full")]
    full: bool,
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
    /// Show whether the Agent-First PSQL skill is installed, valid, and up to date.
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
    /// Skill scope.
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

#[doc = r#"`afpsql` gives agents a reliable PostgreSQL contract: structured stdout
events, first-class SSH/container transports, explicit write permissions,
stable pipe sessions, and machine-readable failures.

### Interface Policy

- default mode is canonical agent-first CLI
- `--mode psql` is argument translation only; runtime output stays JSONL
- stdout carries protocol events; stderr is not a protocol channel
- native CLI and pipe mode default to read-only transactions; writes require permission
- SSH/container transports keep afpsql local instead of running human `psql` across boundaries

### Modes

- default (native CLI): one SQL action per process — a single agent step
- `--mode pipe`: a long-lived JSONL session with `id` correlation and named sessions for multi-step work
- `--mode psql`: run existing `psql` scripts unchanged — flags are translated, runtime output stays JSONL

### Query Sources and Parameters

- use `--sql` for inline SQL or `--sql-file` for a file
- use repeatable `--param N=value` for positional binds
- placeholder count is validated from prepared-statement metadata, not by SQL text scanning

### Connection Sources

- `--dsn-secret` for a PostgreSQL URI
- `--conninfo-secret` for libpq-style conninfo
- or discrete `--host`, `--port`, `--user`, `--dbname`, `--password-secret`
- every `*-secret` flag has a `*-secret-env` partner that reads the value from a named environment variable
- every secret slot also has a `*-secret-config FILE DOT_PATH` source for JSON, TOML, YAML, or dotenv
- add `--ssh user@server` when PostgreSQL is reachable only from the server boundary
- add `--container TARGET` when PostgreSQL is reachable only from inside a container boundary
- use named container scope flags instead of raw driver option passthrough
- use `--container-driver docker|podman|nerdctl|compose|kubectl` for the exec syntax
- combine `--ssh user@server --container TARGET` for containers on an SSH host
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
afpsql --sql 'select * from users where id = $1' --param 1=123
afpsql --dsn-secret-env DATABASE_URL --sql "select 1"
afpsql --dsn-secret-config config.yaml database.url --sql "select 1"
afpsql --ssh user@server --host 127.0.0.1 --port 5432 --user app --dbname appdb --sql "select 1"
afpsql --container pg-container --dsn-secret-env DATABASE_URL --sql "select 1"
afpsql --ssh root@server --container app --host host.container.internal --port 5432 --user app --dbname appdb --sql "select 1"
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
#[command(
    name = env!("DISPLAY_NAME"),
    bin_name = "afpsql",
    version,
    verbatim_doc_comment,
    about = env!("CARGO_PKG_DESCRIPTION"),
)]
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
    /// Query permission. Defaults to read, ssh-read with --ssh, or
    /// container-read with --container.
    #[arg(long = "permission", value_enum, help_heading = "Query")]
    permission: Option<Permission>,
    /// Preview the query without executing it
    #[arg(long, help_heading = "Query")]
    dry_run: bool,
    /// Wrap the query in EXPLAIN (FORMAT JSON) and return the plan tree instead
    /// of executing the user's SQL.
    #[arg(
        long = "explain",
        help_heading = "Query",
        conflicts_with = "explain_analyze"
    )]
    explain: bool,
    /// Wrap the query in EXPLAIN (ANALYZE, FORMAT JSON, BUFFERS). The underlying
    /// SQL actually runs; writes require the matching write permission.
    #[arg(long = "explain-analyze", help_heading = "Query")]
    explain_analyze: bool,

    /// PostgreSQL DSN URI. Redacted in structured output.
    #[arg(
        long = "dsn-secret",
        global = true,
        help_heading = "Connection",
        conflicts_with_all = ["dsn_secret_env", "dsn_secret_config"]
    )]
    dsn_secret: Option<String>,
    /// Read PostgreSQL DSN URI from an environment variable.
    #[arg(
        long = "dsn-secret-env",
        global = true,
        help_heading = "Connection",
        conflicts_with = "dsn_secret_config"
    )]
    dsn_secret_env: Option<String>,
    /// Read PostgreSQL DSN URI from FILE at DOT_PATH.
    #[arg(
        long = "dsn-secret-config",
        global = true,
        help_heading = "Connection",
        value_names = ["FILE", "DOT_PATH"],
        num_args = 2
    )]
    dsn_secret_config: Option<Vec<String>>,
    /// libpq-style conninfo string. Redacted in structured output.
    #[arg(
        long = "conninfo-secret",
        global = true,
        help_heading = "Connection",
        conflicts_with_all = ["conninfo_secret_env", "conninfo_secret_config"]
    )]
    conninfo_secret: Option<String>,
    /// Read libpq-style conninfo string from an environment variable.
    #[arg(
        long = "conninfo-secret-env",
        global = true,
        help_heading = "Connection",
        conflicts_with = "conninfo_secret_config"
    )]
    conninfo_secret_env: Option<String>,
    /// Read libpq-style conninfo from FILE at DOT_PATH.
    #[arg(
        long = "conninfo-secret-config",
        global = true,
        help_heading = "Connection",
        value_names = ["FILE", "DOT_PATH"],
        num_args = 2
    )]
    conninfo_secret_config: Option<Vec<String>>,
    /// PostgreSQL host.
    #[arg(long, global = true, help_heading = "Connection")]
    host: Option<String>,
    /// PostgreSQL port.
    #[arg(long, global = true, help_heading = "Connection")]
    port: Option<u16>,
    /// PostgreSQL user name.
    #[arg(long, global = true, help_heading = "Connection")]
    user: Option<String>,
    /// PostgreSQL database name.
    #[arg(long, global = true, help_heading = "Connection")]
    dbname: Option<String>,
    /// PostgreSQL password. Redacted in structured output.
    #[arg(
        long = "password-secret",
        global = true,
        help_heading = "Connection",
        conflicts_with_all = ["password_secret_env", "password_secret_config"]
    )]
    password_secret: Option<String>,
    /// Read PostgreSQL password from an environment variable.
    #[arg(
        long = "password-secret-env",
        global = true,
        help_heading = "Connection",
        conflicts_with = "password_secret_config"
    )]
    password_secret_env: Option<String>,
    /// Read PostgreSQL password from FILE at DOT_PATH.
    #[arg(
        long = "password-secret-config",
        global = true,
        help_heading = "Connection",
        value_names = ["FILE", "DOT_PATH"],
        num_args = 2
    )]
    password_secret_config: Option<Vec<String>>,
    /// Open an SSH transport to USER@HOST before connecting to PostgreSQL.
    #[arg(long = "ssh", global = true, help_heading = "SSH Transport")]
    ssh: Option<String>,
    /// SSH hop to reach before the final --ssh destination. Repeat for multiple hops.
    #[arg(long = "ssh-via", global = true, help_heading = "SSH Transport")]
    ssh_via: Vec<String>,
    /// Additional OpenSSH -o option. Repeat for multiple options.
    #[arg(long = "ssh-option", global = true, help_heading = "SSH Transport")]
    ssh_options: Vec<String>,
    /// Local bind host for the SSH tunnel.
    #[arg(long = "ssh-local-host", global = true, help_heading = "SSH Transport")]
    ssh_local_host: Option<String>,
    /// Local bind port for the SSH tunnel. Defaults to an ephemeral port.
    #[arg(long = "ssh-local-port", global = true, help_heading = "SSH Transport")]
    ssh_local_port: Option<u16>,
    /// Explicit remote PostgreSQL Unix socket path for SSH forwarding.
    #[arg(
        long = "ssh-remote-socket",
        global = true,
        help_heading = "SSH Transport"
    )]
    ssh_remote_socket: Option<String>,
    /// Remote OS user for sudo -n Unix-socket bridge mode; requires an explicit socket.
    #[arg(long = "ssh-sudo-user", global = true, help_heading = "SSH Transport")]
    ssh_sudo_user: Option<String>,

    /// Run a container exec stdio bridge in TARGET before connecting to PostgreSQL.
    #[arg(
        long = "container",
        global = true,
        help_heading = "Container Transport"
    )]
    container: Option<String>,
    /// Container exec driver: docker, podman, nerdctl, compose, or kubectl.
    #[arg(
        long = "container-driver",
        global = true,
        help_heading = "Container Transport"
    )]
    container_driver: Option<String>,
    /// Runtime command for the selected container driver. Defaults to the driver command.
    #[arg(
        long = "container-runtime",
        global = true,
        help_heading = "Container Transport"
    )]
    container_runtime: Option<String>,
    /// OS user passed to drivers that support exec user selection.
    #[arg(
        long = "container-user",
        global = true,
        help_heading = "Container Transport"
    )]
    container_user: Option<String>,
    /// Kubernetes namespace for kubectl exec.
    #[arg(
        long = "container-namespace",
        global = true,
        help_heading = "Container Transport"
    )]
    container_namespace: Option<String>,
    /// Docker or Kubernetes context for the selected driver.
    #[arg(
        long = "container-context",
        global = true,
        help_heading = "Container Transport"
    )]
    container_context: Option<String>,
    /// Compose file passed before compose exec. Repeat for multiple files.
    #[arg(
        long = "container-compose-file",
        global = true,
        help_heading = "Container Transport"
    )]
    container_compose_files: Vec<String>,
    /// Compose project name passed before compose exec.
    #[arg(
        long = "container-compose-project",
        global = true,
        help_heading = "Container Transport"
    )]
    container_compose_project: Option<String>,
    /// Kubernetes container name for multi-container pods.
    #[arg(
        long = "container-pod-container",
        global = true,
        help_heading = "Container Transport"
    )]
    container_pod_container: Option<String>,

    /// Output format: json (default), yaml, or plain.
    #[arg(
        long,
        short = 'o',
        default_value = "json",
        global = true,
        help_heading = "Runtime"
    )]
    output: String,
    /// Redirect stdout bytes to this file.
    #[arg(
        long = "stdout-file",
        value_name = "PATH",
        global = true,
        help_heading = "Runtime"
    )]
    stdout_file: Option<String>,
    /// Redirect stderr bytes to this file.
    #[arg(
        long = "stderr-file",
        value_name = "PATH",
        global = true,
        help_heading = "Runtime"
    )]
    stderr_file: Option<String>,
    /// Diagnostic log categories (comma-separated). Categories: startup,
    /// connect, query, transport, mode; or an exact event name like
    /// `query.error`; or `all` for everything.
    #[arg(
        long = "log",
        value_delimiter = ',',
        global = true,
        help_heading = "Runtime"
    )]
    log: Vec<String>,
    /// Runtime mode: canonical cli, pipe, or `psql` translation mode.
    #[arg(long, value_enum, default_value_t = RuntimeMode::Cli, help_heading = "Runtime")]
    mode: RuntimeMode,

    #[command(subcommand)]
    command: Option<AfdCommand>,
}

pub fn parse_args(bin_name: &str) -> Result<Mode, String> {
    let raw: Vec<String> = std::env::args().collect();
    if is_psql_mode_requested(&raw) {
        return parse_psql_mode(&raw);
    }
    let startup_requested = startup_requested_from_raw(&raw);

    match agent_first_data::cli_handle_version_or_continue(
        &raw,
        bin_name,
        env!("CARGO_PKG_VERSION"),
    ) {
        Ok(Some(version)) => {
            let _ = write!(std::io::stdout(), "{version}");
            std::process::exit(0);
        }
        Ok(None) => {}
        Err(err) => {
            let stdout = std::io::stdout();
            let mut emitter = agent_first_data::CliEmitter::new(stdout.lock(), OutputFormat::Json);
            let _ = emitter.emit_error("cli_error", &err.to_string());
            std::process::exit(2);
        }
    }

    match agent_first_data::cli_handle_help_or_continue(
        &raw,
        &command_for_bin(bin_name),
        &agent_first_data::HelpConfig::human_cli_default(),
    ) {
        Ok(Some(help)) => {
            let _ = write!(std::io::stdout(), "{help}");
            std::process::exit(0);
        }
        Ok(None) => {}
        Err(err) => {
            let stdout = std::io::stdout();
            let mut emitter = agent_first_data::CliEmitter::new(stdout.lock(), OutputFormat::Json);
            let _ = emitter.emit_error("cli_error", &err.to_string());
            std::process::exit(2);
        }
    }

    let cli = match command_for_bin(bin_name)
        .try_get_matches_from(&raw)
        .and_then(|matches| AfdCli::from_arg_matches(&matches))
    {
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
    let _stream_redirect_args = (&cli.stdout_file, &cli.stderr_file);
    let output = parse_output(&cli.output)?;
    let log = parse_log_categories(&cli.log);
    let dsn_config = SecretConfigRef::from_values("--dsn-secret-config", cli.dsn_secret_config)?;
    let conninfo_config =
        SecretConfigRef::from_values("--conninfo-secret-config", cli.conninfo_secret_config)?;
    let password_config =
        SecretConfigRef::from_values("--password-secret-config", cli.password_secret_config)?;
    let connection_sources = connection_source_metadata([
        (
            "dsn",
            cli.dsn_secret.is_some(),
            cli.dsn_secret_env.as_deref(),
            dsn_config.as_ref(),
        ),
        (
            "conninfo",
            cli.conninfo_secret.is_some(),
            cli.conninfo_secret_env.as_deref(),
            conninfo_config.as_ref(),
        ),
        (
            "password",
            cli.password_secret.is_some(),
            cli.password_secret_env.as_deref(),
            password_config.as_ref(),
        ),
    ]);
    let dsn_secret = resolve_secret_value(
        "--dsn-secret",
        cli.dsn_secret,
        cli.dsn_secret_env.as_deref(),
        dsn_config.as_ref(),
    )?;
    let password_secret = resolve_secret_value(
        "--password-secret",
        cli.password_secret,
        cli.password_secret_env.as_deref(),
        password_config.as_ref(),
    )?;
    let conninfo_secret = resolve_secret_value(
        "--conninfo-secret",
        cli.conninfo_secret,
        cli.conninfo_secret_env.as_deref(),
        conninfo_config.as_ref(),
    )?;
    let session = SessionConfig {
        dsn_secret,
        conninfo_secret,
        host: cli.host,
        port: cli.port,
        user: cli.user,
        dbname: cli.dbname,
        password_secret,
        ssh: SshConfig {
            destination: cli.ssh.or_else(|| std::env::var("AFPSQL_SSH").ok()),
            via: if cli.ssh_via.is_empty() {
                parse_csv_env("AFPSQL_SSH_VIA")
            } else {
                cli.ssh_via
            },
            options: cli.ssh_options,
            local_host: cli
                .ssh_local_host
                .or_else(|| std::env::var("AFPSQL_SSH_LOCAL_HOST").ok()),
            local_port: cli.ssh_local_port.or_else(|| {
                std::env::var("AFPSQL_SSH_LOCAL_PORT")
                    .ok()
                    .and_then(|v| v.parse().ok())
            }),
            remote_socket: cli
                .ssh_remote_socket
                .or_else(|| std::env::var("AFPSQL_SSH_REMOTE_SOCKET").ok()),
            sudo_user: cli
                .ssh_sudo_user
                .or_else(|| std::env::var("AFPSQL_SSH_SUDO_USER").ok()),
        },
        container: ContainerConfig {
            target: cli
                .container
                .or_else(|| std::env::var("AFPSQL_CONTAINER").ok()),
            driver: cli
                .container_driver
                .or_else(|| std::env::var("AFPSQL_CONTAINER_DRIVER").ok()),
            runtime: cli
                .container_runtime
                .or_else(|| std::env::var("AFPSQL_CONTAINER_RUNTIME").ok()),
            user: cli
                .container_user
                .or_else(|| std::env::var("AFPSQL_CONTAINER_USER").ok()),
            namespace: cli
                .container_namespace
                .or_else(|| std::env::var("AFPSQL_CONTAINER_NAMESPACE").ok()),
            context: cli
                .container_context
                .or_else(|| std::env::var("AFPSQL_CONTAINER_CONTEXT").ok()),
            compose_files: resolve_container_compose_files(cli.container_compose_files),
            compose_project: cli
                .container_compose_project
                .or_else(|| std::env::var("AFPSQL_CONTAINER_COMPOSE_PROJECT").ok()),
            pod_container: cli
                .container_pod_container
                .or_else(|| std::env::var("AFPSQL_CONTAINER_POD_CONTAINER").ok()),
        },
    };
    let mode_name = match cli.mode {
        RuntimeMode::Cli => "cli",
        RuntimeMode::Pipe => "pipe",
        RuntimeMode::Psql => "psql",
    };
    let startup_env = startup_env_snapshot();

    if let Some(command) = cli.command {
        return match command {
            AfdCommand::Psql(psql) => Ok(Mode::PsqlAdmin(PsqlAdminRequest {
                action: psql_admin_action(psql.action),
                output,
            })),
            AfdCommand::Skill(skill) => Ok(Mode::SkillAdmin(SkillAdminRequest {
                action: skill_admin_action(skill.action),
                output,
            })),
            AfdCommand::Inspect(inspect) => {
                let (sql, params) = build_inspect_sql(inspect.action);
                let startup_args = with_connection_sources(
                    startup_args(mode_name, Some(&sql), None, params.len()),
                    &connection_sources,
                );
                Ok(Mode::Cli(CliRequest {
                    sql,
                    params,
                    options: QueryOptions::default(),
                    session,
                    output,
                    log,
                    startup_args,
                    startup_env,
                    startup_requested,
                    dry_run: false,
                    psql_mode: false,
                }))
            }
        };
    }

    match cli.mode {
        RuntimeMode::Pipe => {
            return Ok(Mode::Pipe(PipeInit {
                output,
                session,
                log: log.clone(),
                startup_args: with_connection_sources(
                    startup_args(mode_name, None, None, 0),
                    &connection_sources,
                ),
                startup_env,
                startup_requested,
            }));
        }
        RuntimeMode::Cli | RuntimeMode::Psql => {}
    }

    let startup_sql_file = cli.sql_file.clone();
    let user_sql = load_sql(cli.sql, cli.sql_file)?;
    let params = parse_params(&cli.param)?;
    let sql = if cli.explain {
        wrap_explain_sql(&user_sql, false)
    } else if cli.explain_analyze {
        wrap_explain_sql(&user_sql, true)
    } else {
        user_sql
    };
    let startup_args = with_connection_sources(
        startup_args(
            mode_name,
            Some(&sql),
            startup_sql_file.as_deref(),
            params.len(),
        ),
        &connection_sources,
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
        log,
        startup_args,
        startup_env,
        startup_requested,
        dry_run: cli.dry_run,
        psql_mode: false,
    }))
}

fn command_for_bin(bin_name: &str) -> clap::Command {
    match bin_name {
        "afpsql-readonly" => AfdCli::command()
            .name("afpsql-readonly")
            .bin_name("afpsql-readonly"),
        _ => AfdCli::command().name("afpsql").bin_name("afpsql"),
    }
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

    let connection_sources = connection_source_metadata([
        (
            "dsn",
            state.dsn_secret.is_some(),
            state.dsn_secret_env.as_deref(),
            state.dsn_secret_config.as_ref(),
        ),
        (
            "conninfo",
            state.conninfo_secret.is_some(),
            state.conninfo_secret_env.as_deref(),
            state.conninfo_secret_config.as_ref(),
        ),
        (
            "password",
            state.password_secret.is_some(),
            state.password_secret_env.as_deref(),
            state.password_secret_config.as_ref(),
        ),
    ]);
    let dsn_secret = resolve_secret_value(
        "--dsn-secret",
        state.dsn_secret,
        state.dsn_secret_env.as_deref(),
        state.dsn_secret_config.as_ref(),
    )?;
    let password_secret = resolve_secret_value(
        "--password-secret",
        state.password_secret,
        state.password_secret_env.as_deref(),
        state.password_secret_config.as_ref(),
    )?;
    let conninfo_secret = resolve_secret_value(
        "--conninfo-secret",
        state.conninfo_secret,
        state.conninfo_secret_env.as_deref(),
        state.conninfo_secret_config.as_ref(),
    )?;
    let session = SessionConfig {
        dsn_secret,
        conninfo_secret,
        host: state.host,
        port: state.port,
        user: state.user,
        dbname: state.dbname,
        password_secret,
        ssh: SshConfig::default(),
        container: ContainerConfig {
            target: state
                .container
                .or_else(|| std::env::var("AFPSQL_CONTAINER").ok()),
            driver: state
                .container_driver
                .or_else(|| std::env::var("AFPSQL_CONTAINER_DRIVER").ok()),
            runtime: state
                .container_runtime
                .or_else(|| std::env::var("AFPSQL_CONTAINER_RUNTIME").ok()),
            user: state
                .container_user
                .or_else(|| std::env::var("AFPSQL_CONTAINER_USER").ok()),
            namespace: state
                .container_namespace
                .or_else(|| std::env::var("AFPSQL_CONTAINER_NAMESPACE").ok()),
            context: state
                .container_context
                .or_else(|| std::env::var("AFPSQL_CONTAINER_CONTEXT").ok()),
            compose_files: resolve_container_compose_files(state.container_compose_files),
            compose_project: state
                .container_compose_project
                .or_else(|| std::env::var("AFPSQL_CONTAINER_COMPOSE_PROJECT").ok()),
            pod_container: state
                .container_pod_container
                .or_else(|| std::env::var("AFPSQL_CONTAINER_POD_CONTAINER").ok()),
        },
    };

    let startup_sql_file = state.sql_file.clone();
    let sql = load_sql(state.sql, state.sql_file)?;
    let params = parse_params(&state.params_kv)?;
    let startup_args = with_connection_sources(
        psql_startup_args(PsqlStartupArgs {
            mode: "psql",
            sql: Some(&sql),
            sql_file: startup_sql_file,
            param_count: params.len(),
        }),
        &connection_sources,
    );
    Ok(Mode::Cli(CliRequest {
        sql,
        params,
        options: QueryOptions {
            permission: Some(if session.uses_container_transport() {
                Permission::ContainerWrite
            } else {
                Permission::Write
            }),
            ..Default::default()
        },
        session,
        output: state.output,
        log: parse_log_categories(&state.log_entries),
        startup_args,
        startup_env: startup_env_snapshot(),
        startup_requested,
        dry_run: false,
        psql_mode: true,
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
    dsn_secret_config: Option<SecretConfigRef>,
    conninfo_secret: Option<String>,
    conninfo_secret_env: Option<String>,
    conninfo_secret_config: Option<SecretConfigRef>,
    password_secret: Option<String>,
    password_secret_env: Option<String>,
    password_secret_config: Option<SecretConfigRef>,
    container: Option<String>,
    container_driver: Option<String>,
    container_runtime: Option<String>,
    container_user: Option<String>,
    container_namespace: Option<String>,
    container_context: Option<String>,
    container_compose_files: Vec<String>,
    container_compose_project: Option<String>,
    container_pod_container: Option<String>,
    params_kv: Vec<String>,
    output: OutputFormat,
    log_entries: Vec<String>,
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
            dsn_secret_config: None,
            conninfo_secret: None,
            conninfo_secret_env: None,
            conninfo_secret_config: None,
            password_secret: None,
            password_secret_env: None,
            password_secret_config: None,
            container: None,
            container_driver: None,
            container_runtime: None,
            container_user: None,
            container_namespace: None,
            container_context: None,
            container_compose_files: vec![],
            container_compose_project: None,
            container_pod_container: None,
            params_kv: vec![],
            output: OutputFormat::Json,
            log_entries: vec![],
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
        "--dsn-secret-config" => {
            state.dsn_secret_config = Some(take_secret_config_ref(raw, i, "--dsn-secret-config")?);
            Ok(())
        }
        "--conninfo-secret" => {
            state.conninfo_secret = Some(take_long_arg_value(raw, i, "--conninfo-secret")?);
            Ok(())
        }
        "--conninfo-secret-env" => {
            state.conninfo_secret_env = Some(take_long_arg_value(raw, i, "--conninfo-secret-env")?);
            Ok(())
        }
        "--conninfo-secret-config" => {
            state.conninfo_secret_config =
                Some(take_secret_config_ref(raw, i, "--conninfo-secret-config")?);
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
        "--password-secret-config" => {
            state.password_secret_config =
                Some(take_secret_config_ref(raw, i, "--password-secret-config")?);
            Ok(())
        }
        "--container" => {
            state.container = Some(take_long_arg_value(raw, i, "--container")?);
            Ok(())
        }
        "--container-driver" => {
            state.container_driver = Some(take_long_arg_value(raw, i, "--container-driver")?);
            Ok(())
        }
        "--container-runtime" => {
            state.container_runtime = Some(take_long_arg_value(raw, i, "--container-runtime")?);
            Ok(())
        }
        "--container-user" => {
            state.container_user = Some(take_long_arg_value(raw, i, "--container-user")?);
            Ok(())
        }
        "--container-namespace" => {
            state.container_namespace = Some(take_long_arg_value(raw, i, "--container-namespace")?);
            Ok(())
        }
        "--container-context" => {
            state.container_context = Some(take_long_arg_value(raw, i, "--container-context")?);
            Ok(())
        }
        "--container-compose-file" => {
            state.container_compose_files.push(take_long_arg_value(
                raw,
                i,
                "--container-compose-file",
            )?);
            Ok(())
        }
        "--container-compose-project" => {
            state.container_compose_project =
                Some(take_long_arg_value(raw, i, "--container-compose-project")?);
            Ok(())
        }
        "--container-pod-container" => {
            state.container_pod_container =
                Some(take_long_arg_value(raw, i, "--container-pod-container")?);
            Ok(())
        }
        "--output" => {
            let value = take_long_arg_value(raw, i, "--output")?;
            state.output = parse_output(&value)?;
            Ok(())
        }
        "--stdout-file" | "--stderr-file" => {
            let _ = take_long_arg_value(raw, i, long_name(arg))?;
            Ok(())
        }
        "--log" => {
            let values = take_long_arg_value(raw, i, "--log")?;
            add_log_entries(state, &values);
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
            'o' => {
                let value = take_short_arg_value(raw, i, arg, offset, "-o")?;
                state.output = parse_output(&value)?;
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

fn take_secret_config_ref(
    raw: &[String],
    i: &mut usize,
    flag: &str,
) -> Result<SecretConfigRef, String> {
    if raw[*i].contains('=') {
        return Err(format!(
            "{flag} requires space-separated values: {flag} <FILE> <DOT_PATH>"
        ));
    }
    let file = take_long_arg_value(raw, i, flag)?;
    let path = raw
        .get(*i)
        .filter(|value| !value.starts_with('-'))
        .ok_or_else(|| format!("{flag} requires exactly two values: <FILE> <DOT_PATH>"))?
        .clone();
    *i += 1;
    if raw.get(*i).is_some_and(|value| !value.starts_with('-')) {
        return Err(format!(
            "{flag} accepts exactly two values: <FILE> <DOT_PATH>"
        ));
    }
    if file.is_empty() || path.is_empty() {
        return Err(format!(
            "{flag} requires exactly two non-empty values: <FILE> <DOT_PATH>"
        ));
    }
    Ok(SecretConfigRef {
        file: file.into(),
        path,
    })
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
Output:\n  -o, --output=json|yaml|plain changes afpsql rendering\n  --stdout-file=FILE redirects stdout bytes to FILE\n  --stderr-file=FILE redirects stderr bytes to FILE\n\n\
Human-interactive psql modes and psql meta-commands are not supported by this wrapper.",
        env!("CARGO_PKG_VERSION")
    );
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
        if top_level_arg_consumes_two_values(arg) {
            i += if arg.contains('=') { 2 } else { 3 };
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

fn top_level_arg_consumes_two_values(arg: &str) -> bool {
    let name = arg.split_once('=').map(|(name, _)| name).unwrap_or(arg);
    matches!(
        name,
        "--dsn-secret-config" | "--conninfo-secret-config" | "--password-secret-config"
    )
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
            | "--conninfo-secret-env"
            | "--host"
            | "--port"
            | "--user"
            | "--dbname"
            | "--password-secret"
            | "--password-secret-env"
            | "--ssh"
            | "--ssh-via"
            | "--ssh-option"
            | "--ssh-local-host"
            | "--ssh-local-port"
            | "--ssh-remote-socket"
            | "--ssh-sudo-user"
            | "--container"
            | "--container-driver"
            | "--container-runtime"
            | "--container-user"
            | "--container-namespace"
            | "--container-context"
            | "--container-compose-file"
            | "--container-compose-project"
            | "--container-pod-container"
            | "--output"
            | "--stdout-file"
            | "--stderr-file"
            | "--log"
    )
}

fn resolve_container_compose_files(cli_files: Vec<String>) -> Vec<String> {
    if !cli_files.is_empty() {
        return cli_files;
    }
    std::env::var("AFPSQL_CONTAINER_COMPOSE_FILE")
        .ok()
        .map(|value| {
            value
                .split(':')
                .filter(|part| !part.is_empty())
                .map(std::string::ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
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

fn parse_log_categories(entries: &[String]) -> LogFilters {
    cli_parse_log_filters(entries)
}

fn parse_csv_env(name: &str) -> Vec<String> {
    std::env::var(name)
        .ok()
        .into_iter()
        .flat_map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
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

fn with_connection_sources(mut args: Value, sources: &Value) -> Value {
    if let (Some(args), Some(sources)) = (args.as_object_mut(), sources.as_object())
        && !sources.is_empty()
    {
        args.insert(
            "connection_sources".to_string(),
            Value::Object(sources.clone()),
        );
    }
    args
}

fn connection_source_metadata<const N: usize>(
    sources: [(&str, bool, Option<&str>, Option<&SecretConfigRef>); N],
) -> Value {
    let mut metadata = serde_json::Map::new();
    for (slot, direct, env_name, config) in sources {
        let value = if let Some(reference) = config {
            Some(reference.safe_metadata())
        } else if let Some(env_name) = env_name {
            Some(json!({"kind": "env", "name": env_name}))
        } else if direct {
            Some(json!({"kind": "direct"}))
        } else {
            None
        };
        if let Some(value) = value {
            metadata.insert(slot.to_string(), value);
        }
    }
    Value::Object(metadata)
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
    config: Option<&SecretConfigRef>,
) -> Result<Option<String>, String> {
    let source_count = usize::from(direct.is_some())
        + usize::from(env_name.is_some())
        + usize::from(config.is_some());
    if source_count > 1 {
        return Err(format!(
            "{flag_name}, {flag_name}-env, and {flag_name}-config are mutually exclusive"
        ));
    }
    match (direct, env_name, config) {
        (Some(value), None, None) => Ok(Some(value)),
        (None, Some(name), None) => {
            if name.is_empty() {
                return Err(format!(
                    "{flag_name}-env requires a non-empty variable name"
                ));
            }
            std::env::var(name).map(Some).map_err(|_| {
                format!("{flag_name}-env references unset environment variable: {name}")
            })
        }
        (None, None, Some(reference)) => {
            resolve_config_secret(&format!("{flag_name}-config"), reference).map(Some)
        }
        (None, None, None) => Ok(None),
        _ => Err(format!(
            "{flag_name}, {flag_name}-env, and {flag_name}-config are mutually exclusive"
        )),
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
    // Strings are passed verbatim to PostgreSQL via the text bind path so
    // that values like "00123" or "1.0" preserve their original form. The
    // server coerces them based on the prepared statement's parameter type.
    Value::String(v.to_string())
}

fn wrap_explain_sql(user_sql: &str, analyze: bool) -> String {
    let body = user_sql.trim_end_matches([';', ' ', '\n', '\t', '\r']);
    if analyze {
        format!("explain (analyze true, format json, buffers true) {body}")
    } else {
        format!("explain (format json) {body}")
    }
}

fn optional_string_value(value: Option<String>) -> Value {
    value.map(Value::String).unwrap_or(Value::Null)
}

fn split_table_name(default_schema: String, name: String) -> (String, String) {
    match name.split_once('.') {
        Some((schema, table)) => (schema.to_string(), table.to_string()),
        None => (default_schema, name),
    }
}

fn split_optional_table(default_schema: String, table: Option<String>) -> (String, Option<String>) {
    match table {
        Some(name) => {
            let (schema, table_name) = split_table_name(default_schema, name);
            (schema, Some(table_name))
        }
        None => (default_schema, None),
    }
}

fn full_schema_snapshot_sql(relation_filter: &str, schema_only_filter: &str) -> String {
    format!(
        "with relation_filter as ( \
             select c.oid, c.relname, c.relkind, c.relpersistence, c.reltuples, c.relowner, \
                    n.nspname, pg_catalog.obj_description(c.oid, 'pg_class') as comment \
             from pg_catalog.pg_class c \
             join pg_catalog.pg_namespace n on n.oid = c.relnamespace \
             where n.nspname = $1 \
               and c.relkind in ('r', 'p', 'f', 'v', 'm', 'S') \
               and ({relation_filter}) \
         ), snapshot as ( \
             select 'extension'::text as kind, \
                    n.nspname::text as schema, \
                    null::text as relation, \
                    e.extname::text as name, \
                    'extension'::text as object_type, \
                    null::integer as position, \
                    null::text as definition, \
                    null::bigint as size_bytes, \
                    null::text as size, \
                    null::bigint as estimated_rows, \
                    pg_catalog.jsonb_build_object('version', e.extversion) as payload \
             from pg_catalog.pg_extension e \
             join pg_catalog.pg_namespace n on n.oid = e.extnamespace \
             where n.nspname = $1 and ({schema_only_filter}) \
             union all \
             select 'relation'::text as kind, \
                    rf.nspname::text as schema, \
                    rf.relname::text as relation, \
                    rf.relname::text as name, \
                    case rf.relkind \
                        when 'r' then 'table' \
                        when 'p' then 'partitioned table' \
                        when 'f' then 'foreign table' \
                        when 'v' then 'view' \
                        when 'm' then 'materialized view' \
                        else rf.relkind::text \
                    end as object_type, \
                    null::integer as position, \
                    case when rf.relkind in ('v', 'm') \
                         then pg_catalog.pg_get_viewdef(rf.oid, true) end as definition, \
                    case when rf.relkind in ('r', 'p', 'm') \
                         then pg_catalog.pg_total_relation_size(rf.oid) end as size_bytes, \
                    case when rf.relkind in ('r', 'p', 'm') \
                         then pg_catalog.pg_size_pretty(pg_catalog.pg_total_relation_size(rf.oid)) end as size, \
                    rf.reltuples::bigint as estimated_rows, \
                    pg_catalog.jsonb_build_object( \
                        'owner', pg_catalog.pg_get_userbyid(rf.relowner), \
                        'persistence', rf.relpersistence, \
                        'comment', rf.comment \
                    ) as payload \
             from relation_filter rf \
             where rf.relkind in ('r', 'p', 'f', 'v', 'm') \
             union all \
             select 'sequence'::text as kind, \
                    rf.nspname::text as schema, \
                    rf.relname::text as relation, \
                    rf.relname::text as name, \
                    'sequence'::text as object_type, \
                    null::integer as position, \
                    null::text as definition, \
                    pg_catalog.pg_relation_size(rf.oid) as size_bytes, \
                    pg_catalog.pg_size_pretty(pg_catalog.pg_relation_size(rf.oid)) as size, \
                    null::bigint as estimated_rows, \
                    pg_catalog.jsonb_build_object( \
                        'owner', pg_catalog.pg_get_userbyid(rf.relowner), \
                        'comment', rf.comment \
                    ) as payload \
             from relation_filter rf \
             where rf.relkind = 'S' \
             union all \
             select 'column'::text as kind, \
                    rf.nspname::text as schema, \
                    rf.relname::text as relation, \
                    a.attname::text as name, \
                    pg_catalog.format_type(a.atttypid, a.atttypmod)::text as object_type, \
                    a.attnum::integer as position, \
                    pg_catalog.pg_get_expr(ad.adbin, ad.adrelid)::text as definition, \
                    null::bigint as size_bytes, \
                    null::text as size, \
                    null::bigint as estimated_rows, \
                    pg_catalog.jsonb_build_object( \
                        'nullable', not a.attnotnull, \
                        'primary_key', coalesce(pk.is_primary, false), \
                        'identity', a.attidentity::text, \
                        'generated', a.attgenerated::text, \
                        'serial_sequence', pg_catalog.pg_get_serial_sequence( \
                            pg_catalog.format('%I.%I', rf.nspname, rf.relname), a.attname), \
                        'comment', pg_catalog.col_description(rf.oid, a.attnum) \
                    ) as payload \
             from pg_catalog.pg_attribute a \
             join relation_filter rf on rf.oid = a.attrelid \
             left join pg_catalog.pg_attrdef ad on ad.adrelid = a.attrelid and ad.adnum = a.attnum \
             left join lateral ( \
                 select true as is_primary \
                 from pg_catalog.pg_index i \
                 where i.indrelid = a.attrelid and i.indisprimary \
                   and a.attnum = any(i.indkey) \
             ) pk on true \
             where rf.relkind in ('r', 'p', 'f', 'v', 'm') \
               and a.attnum > 0 and not a.attisdropped \
             union all \
             select 'constraint'::text as kind, \
                    rf.nspname::text as schema, \
                    rf.relname::text as relation, \
                    con.conname::text as name, \
                    case con.contype \
                        when 'p' then 'primary key' \
                        when 'u' then 'unique' \
                        when 'f' then 'foreign key' \
                        when 'c' then 'check' \
                        when 'x' then 'exclusion' \
                        else con.contype::text \
                    end as object_type, \
                    null::integer as position, \
                    pg_catalog.pg_get_constraintdef(con.oid, true)::text as definition, \
                    null::bigint as size_bytes, \
                    null::text as size, \
                    null::bigint as estimated_rows, \
                    pg_catalog.jsonb_build_object( \
                        'type', con.contype::text, \
                        'deferrable', con.condeferrable, \
                        'deferred_by_default', con.condeferred, \
                        'validated', con.convalidated \
                    ) as payload \
             from pg_catalog.pg_constraint con \
             join relation_filter rf on rf.oid = con.conrelid \
             union all \
             select 'index'::text as kind, \
                    rf.nspname::text as schema, \
                    rf.relname::text as relation, \
                    ic.relname::text as name, \
                    am.amname::text as object_type, \
                    null::integer as position, \
                    pg_catalog.pg_get_indexdef(i.indexrelid)::text as definition, \
                    pg_catalog.pg_relation_size(i.indexrelid) as size_bytes, \
                    pg_catalog.pg_size_pretty(pg_catalog.pg_relation_size(i.indexrelid)) as size, \
                    null::bigint as estimated_rows, \
                    pg_catalog.jsonb_build_object( \
                        'unique', i.indisunique, \
                        'primary', i.indisprimary, \
                        'valid', i.indisvalid, \
                        'ready', i.indisready \
                    ) as payload \
             from pg_catalog.pg_index i \
             join pg_catalog.pg_class ic on ic.oid = i.indexrelid \
             join relation_filter rf on rf.oid = i.indrelid \
             join pg_catalog.pg_am am on am.oid = ic.relam \
             union all \
             select 'trigger'::text as kind, \
                    rf.nspname::text as schema, \
                    rf.relname::text as relation, \
                    tg.tgname::text as name, \
                    'trigger'::text as object_type, \
                    null::integer as position, \
                    pg_catalog.pg_get_triggerdef(tg.oid, true)::text as definition, \
                    null::bigint as size_bytes, \
                    null::text as size, \
                    null::bigint as estimated_rows, \
                    pg_catalog.jsonb_build_object( \
                        'enabled', tg.tgenabled::text, \
                        'function_schema', fn_ns.nspname, \
                        'function_name', fn.proname \
                    ) as payload \
             from pg_catalog.pg_trigger tg \
             join relation_filter rf on rf.oid = tg.tgrelid \
             join pg_catalog.pg_proc fn on fn.oid = tg.tgfoid \
             join pg_catalog.pg_namespace fn_ns on fn_ns.oid = fn.pronamespace \
             where not tg.tgisinternal \
             union all \
             select 'function'::text as kind, \
                    n.nspname::text as schema, \
                    null::text as relation, \
                    (p.proname || '(' || pg_catalog.pg_get_function_identity_arguments(p.oid) || ')')::text as name, \
                    'function'::text as object_type, \
                    null::integer as position, \
                    pg_catalog.pg_get_functiondef(p.oid)::text as definition, \
                    null::bigint as size_bytes, \
                    null::text as size, \
                    null::bigint as estimated_rows, \
                    pg_catalog.jsonb_build_object( \
                        'language', l.lanname, \
                        'result', pg_catalog.pg_get_function_result(p.oid), \
                        'identity_args', pg_catalog.pg_get_function_identity_arguments(p.oid) \
                    ) as payload \
             from pg_catalog.pg_proc p \
             join pg_catalog.pg_namespace n on n.oid = p.pronamespace \
             join pg_catalog.pg_language l on l.oid = p.prolang \
             where n.nspname = $1 \
               and p.prokind = 'f' \
               and ({schema_only_filter}) \
               and not exists ( \
                   select 1 \
                   from pg_catalog.pg_depend d \
                   where d.classid = 'pg_catalog.pg_proc'::regclass \
                     and d.objid = p.oid \
                     and d.deptype = 'e' \
               ) \
         ) \
         select * from snapshot \
         order by case kind \
                    when 'extension' then 0 \
                    when 'relation' then 1 \
                    when 'sequence' then 2 \
                    when 'column' then 3 \
                    when 'constraint' then 4 \
                    when 'index' then 5 \
                    when 'trigger' then 6 \
                    when 'function' then 7 \
                    else 99 end, \
                  schema, relation nulls first, position nulls last, name"
    )
}

fn build_schema_snapshot_sql(args: InspectSchemaArgs) -> (String, Vec<Value>) {
    (
        full_schema_snapshot_sql("$2::text is null or c.relname like $2", "$2::text is null"),
        vec![Value::String(args.schema), optional_string_value(args.like)],
    )
}

fn build_table_full_sql(schema: String, name: String) -> (String, Vec<Value>) {
    (
        full_schema_snapshot_sql("c.relname = $2", "false"),
        vec![Value::String(schema), Value::String(name)],
    )
}

fn build_inspect_indexes_sql(args: InspectIndexesArgs) -> (String, Vec<Value>) {
    let (schema, table) = split_optional_table(args.schema, args.table);
    let mut sql = String::from(
        "select n.nspname as schema, \
                tc.relname as table, \
                ic.relname as name, \
                am.amname as method, \
                i.indisunique as unique, \
                i.indisprimary as primary, \
                i.indisvalid as valid, \
                i.indisready as ready, \
                pg_catalog.pg_get_indexdef(i.indexrelid) as definition, \
                pg_catalog.pg_relation_size(i.indexrelid) as size_bytes, \
                pg_catalog.pg_size_pretty(pg_catalog.pg_relation_size(i.indexrelid)) as size",
    );
    if args.stats {
        sql.push_str(
            ", s.idx_scan as index_scan_count, \
             s.idx_tup_read as index_tuple_read_count, \
             s.idx_tup_fetch as index_tuple_fetch_count",
        );
    }
    sql.push_str(
        " from pg_catalog.pg_index i \
          join pg_catalog.pg_class ic on ic.oid = i.indexrelid \
          join pg_catalog.pg_class tc on tc.oid = i.indrelid \
          join pg_catalog.pg_namespace n on n.oid = tc.relnamespace \
          join pg_catalog.pg_am am on am.oid = ic.relam",
    );
    if args.stats {
        sql.push_str(" left join pg_catalog.pg_stat_user_indexes s on s.indexrelid = i.indexrelid");
    }
    sql.push_str(" where n.nspname = $1");

    let mut params = vec![Value::String(schema)];
    if let Some(table_name) = table {
        sql.push_str(" and tc.relname = $2");
        params.push(Value::String(table_name));
    }
    sql.push_str(" order by tc.relname, ic.relname");
    (sql, params)
}

fn build_inspect_sql(action: InspectAction) -> (String, Vec<Value>) {
    match action {
        InspectAction::Databases(args) => {
            let mut sql = String::from(
                "select d.datname as database, \
                        pg_catalog.pg_get_userbyid(d.datdba) as owner, \
                        pg_catalog.pg_encoding_to_char(d.encoding) as encoding, \
                        d.datcollate as collate, \
                        d.datctype as ctype, \
                        d.datistemplate as is_template, \
                        d.datallowconn as allow_connections, \
                        d.datconnlimit as connection_limit, \
                        case when has_database_privilege(d.datname, 'CONNECT') \
                             then pg_catalog.pg_database_size(d.oid) end as size_bytes, \
                        case when has_database_privilege(d.datname, 'CONNECT') \
                             then pg_catalog.pg_size_pretty(pg_catalog.pg_database_size(d.oid)) end as size, \
                        s.numbackends as active_connections \
                 from pg_catalog.pg_database d \
                 left join pg_catalog.pg_stat_database s on s.datid = d.oid",
            );
            if !args.all {
                sql.push_str(" where not d.datistemplate");
            }
            sql.push_str(" order by d.datname");
            (sql, vec![])
        }
        InspectAction::Database => (
            "with rels as ( \
                 select c.relkind \
                 from pg_catalog.pg_class c \
                 join pg_catalog.pg_namespace n on n.oid = c.relnamespace \
                 where n.nspname not in ('pg_catalog', 'information_schema') \
                   and n.nspname not like 'pg_toast%' \
                   and n.nspname not like 'pg_temp_%' \
             ) \
             select current_database() as database, \
                    ( select count(*) from pg_catalog.pg_namespace n \
                       where n.nspname not in ('pg_catalog', 'information_schema') \
                         and n.nspname not like 'pg_toast%' \
                         and n.nspname not like 'pg_temp_%' ) as schemas, \
                    count(*) filter (where relkind in ('r', 'p')) as tables, \
                    count(*) filter (where relkind = 'v') as views, \
                    count(*) filter (where relkind = 'm') as materialized_views, \
                    count(*) filter (where relkind = 'S') as sequences, \
                    pg_catalog.pg_database_size(current_database()) as size_bytes, \
                    pg_catalog.pg_size_pretty(pg_catalog.pg_database_size(current_database())) as size \
             from rels"
                .to_string(),
            vec![],
        ),
        InspectAction::Schemas => (
            "select n.nspname as schema, \
                    pg_catalog.pg_get_userbyid(n.nspowner) as owner, \
                    count(*) filter (where c.relkind in ('r', 'p')) as tables, \
                    count(*) filter (where c.relkind = 'v') as views, \
                    count(*) filter (where c.relkind = 'm') as materialized_views, \
                    count(*) filter (where c.relkind = 'S') as sequences, \
                    pg_catalog.pg_size_pretty(coalesce( \
                        sum(pg_catalog.pg_total_relation_size(c.oid)) \
                            filter (where c.relkind in ('r', 'p', 'm')), 0)) as size \
             from pg_catalog.pg_namespace n \
             left join pg_catalog.pg_class c on c.relnamespace = n.oid \
             where n.nspname not in ('pg_catalog', 'information_schema') \
               and n.nspname not like 'pg_toast%' \
               and n.nspname not like 'pg_temp_%' \
             group by n.nspname, n.nspowner \
             order by n.nspname"
                .to_string(),
            vec![],
        ),
        InspectAction::Schema(args) | InspectAction::Snapshot(args) => build_schema_snapshot_sql(args),
        InspectAction::Tables(args) => {
            let mut sql = String::from(
                "select n.nspname as schema, \
                        c.relname as name, \
                        case c.relkind when 'r' then 'table' \
                                       when 'p' then 'partitioned table' \
                                       when 'f' then 'foreign table' end as kind, \
                        pg_catalog.pg_get_userbyid(c.relowner) as owner, \
                        c.reltuples::bigint as estimated_rows, \
                        pg_catalog.pg_size_pretty(pg_catalog.pg_total_relation_size(c.oid)) as size, \
                        pg_catalog.pg_total_relation_size(c.oid) as size_bytes \
                 from pg_catalog.pg_class c \
                 join pg_catalog.pg_namespace n on n.oid = c.relnamespace \
                 where n.nspname = $1 and c.relkind in ('r', 'p', 'f')",
            );
            let mut params = vec![Value::String(args.schema)];
            if let Some(pattern) = args.like {
                sql.push_str(" and c.relname like $2");
                params.push(Value::String(pattern));
            }
            sql.push_str(" order by c.relname");
            (sql, params)
        }
        InspectAction::Views(args) => {
            let mut sql = String::from(
                "select n.nspname as schema, \
                        c.relname as name, \
                        case c.relkind when 'm' then true else false end as materialized, \
                        pg_catalog.pg_get_userbyid(c.relowner) as owner \
                 from pg_catalog.pg_class c \
                 join pg_catalog.pg_namespace n on n.oid = c.relnamespace \
                 where n.nspname = $1 and c.relkind in ('v', 'm')",
            );
            let mut params = vec![Value::String(args.schema)];
            if let Some(pattern) = args.like {
                sql.push_str(" and c.relname like $2");
                params.push(Value::String(pattern));
            }
            sql.push_str(" order by c.relname");
            (sql, params)
        }
        InspectAction::Indexes(args) => build_inspect_indexes_sql(args),
        InspectAction::Table(args) => {
            let (schema, name) = split_table_name("public".to_string(), args.name);
            if args.full {
                return build_table_full_sql(schema, name);
            }
            (
                "select a.attname as name, \
                        pg_catalog.format_type(a.atttypid, a.atttypmod) as type, \
                        not a.attnotnull as nullable, \
                        pg_catalog.pg_get_expr(ad.adbin, ad.adrelid) as default, \
                        a.attnum as position, \
                        coalesce(pk.is_primary, false) as primary_key, \
                        pg_catalog.col_description(c.oid, a.attnum) as comment \
                 from pg_catalog.pg_attribute a \
                 join pg_catalog.pg_class c on c.oid = a.attrelid \
                 join pg_catalog.pg_namespace n on n.oid = c.relnamespace \
                 left join pg_catalog.pg_attrdef ad \
                     on ad.adrelid = a.attrelid and ad.adnum = a.attnum \
                 left join lateral ( \
                     select true as is_primary \
                     from pg_catalog.pg_index i \
                     where i.indrelid = a.attrelid and i.indisprimary \
                       and a.attnum = any(i.indkey) \
                 ) pk on true \
                 where n.nspname = $1 and c.relname = $2 \
                   and a.attnum > 0 and not a.attisdropped \
                 order by a.attnum"
                    .to_string(),
                vec![Value::String(schema), Value::String(name)],
            )
        }
    }
}

#[cfg(test)]
#[path = "../tests/support/unit_cli.rs"]
mod tests;
