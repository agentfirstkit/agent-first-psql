use std::io::Read;

use crate::limits::{MAX_PARAMS, MAX_SQL_BYTES};
use crate::secret_config::{SecretConfigRef, resolve_config_secret};
use crate::types::{ContainerConfig, Permission, QueryOptions, SessionConfig, SshConfig};
use agent_first_data::{
    ArgSpec, BuiltCliSpec, CliOutcome, CliSpec, CliSpecError, CliValue, Combination, CommandSpec,
    LogFilters, OutputFormat, OutputPlan, OutputSpec, OutputTo, ResolvedInvocation,
    build_afdata_cli, cli_parse_log_filters, cli_parse_output,
};
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
    "AFPSQL_SSH_VIA",
    "AFPSQL_SSH_REMOTE_SOCKET",
    "AFPSQL_SSH_SUDO_USER",
    "AFPSQL_CONTAINER_DOCKER_NAME",
    "AFPSQL_CONTAINER_DOCKER_USER",
    "AFPSQL_CONTAINER_DOCKER_CONTEXT",
    "AFPSQL_CONTAINER_DOCKER_RUNTIME",
    "AFPSQL_CONTAINER_PODMAN_NAME",
    "AFPSQL_CONTAINER_PODMAN_USER",
    "AFPSQL_CONTAINER_PODMAN_RUNTIME",
    "AFPSQL_CONTAINER_NERDCTL_NAME",
    "AFPSQL_CONTAINER_NERDCTL_USER",
    "AFPSQL_CONTAINER_NERDCTL_RUNTIME",
    "AFPSQL_CONTAINER_COMPOSE_SERVICE",
    "AFPSQL_CONTAINER_COMPOSE_USER",
    "AFPSQL_CONTAINER_COMPOSE_FILE",
    "AFPSQL_CONTAINER_COMPOSE_PROJECT",
    "AFPSQL_CONTAINER_COMPOSE_RUNTIME",
    "AFPSQL_CONTAINER_KUBECTL_POD",
    "AFPSQL_CONTAINER_KUBECTL_CONTAINER",
    "AFPSQL_CONTAINER_KUBECTL_NAMESPACE",
    "AFPSQL_CONTAINER_KUBECTL_CONTEXT",
    "AFPSQL_CONTAINER_KUBECTL_RUNTIME",
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkillAgentSelection {
    /// Manage every agent that supports the requested scope.
    All,
    /// Manage the Codex local skill under $CODEX_HOME/skills.
    Codex,
    /// Manage the Claude Code skill under ~/.claude/skills or .claude/skills.
    ClaudeCode,
    /// Manage the opencode skill under ~/.config/opencode/skills or .opencode/skills.
    Opencode,
    /// Manage the Hermes skill under $HERMES_HOME/skills or ~/.hermes/skills.
    Hermes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// Schema-discovery request, already narrowed to the shape that matched.
pub enum InspectAction {
    Databases(InspectDatabasesArgs),
    Database,
    Schemas,
    Schema(InspectSchemaArgs),
    Snapshot(InspectSchemaArgs),
    Tables(InspectTablesArgs),
    Views(InspectViewsArgs),
    Indexes(InspectIndexesArgs),
    Table(InspectTableArgs),
}

pub struct InspectDatabasesArgs {
    pub all: bool,
}

pub struct InspectTablesArgs {
    pub schema: String,
    pub like: Option<String>,
}

pub struct InspectSchemaArgs {
    pub schema: String,
    pub like: Option<String>,
}

pub struct InspectViewsArgs {
    pub schema: String,
    pub like: Option<String>,
}

pub struct InspectIndexesArgs {
    pub schema: String,
    pub table: Option<String>,
    pub stats: bool,
}

pub struct InspectTableArgs {
    pub name: String,
    pub full: bool,
}

/// One structural or argument-shaped rejection, decided before anything ran.
///
/// `code` is the classification an agent branches on: the parser's own
/// `cli_*` codes when the registry decided, and `invalid_request` when the
/// failure is about the world the arguments name (an unreadable file, an unset
/// variable) rather than the arguments themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub code: String,
    pub message: String,
    pub hint: Option<String>,
}

impl ParseError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
            hint: None,
        }
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// A failure about the named world, not about the argv shape.
    fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(crate::protocol::error_code::INVALID_REQUEST, message)
    }

    /// The parser's own classification for a value it could not accept. Reused
    /// rather than duplicated: the registry has no single-character, unsigned,
    /// or `N=value` type, so those checks land here and must not invent a
    /// second spelling for what is the same rejection.
    fn invalid_value(message: impl Into<String>) -> Self {
        Self::new("cli_invalid_argument_value", message)
    }
}

impl From<String> for ParseError {
    fn from(message: String) -> Self {
        Self::invalid_request(message)
    }
}

/// What `parse_args` resolved, plus the stream redirection it installed.
///
/// The guard belongs to the caller: dropping it restores the process fds, so
/// it has to outlive every event the selected mode is about to write.
pub struct Parsed {
    pub mode: Mode,
    pub redirect: Option<agent_first_data::stream_redirect::InstalledStreamRedirect>,
}

const PERMISSIONS: [&str; 6] = [
    "read",
    "write",
    "ssh-read",
    "ssh-write",
    "container-read",
    "container-write",
];

/// Every argument that can open a PostgreSQL session, plus `--log`.
///
/// A closed registry has no global arguments: an argument belongs to the
/// command that accepts it, which is what makes one `<command> --help` a
/// complete answer instead of a pointer to a parent. This is that one list,
/// applied to each command that can open a session.
const CONNECTION_IDS: [&str; 33] = [
    "dsn",
    "conninfo",
    "host",
    "port",
    "user",
    "dbname",
    "password",
    "ssh",
    "ssh_via",
    "ssh_option",
    "ssh_remote_socket",
    "ssh_sudo_user",
    "container_docker_name",
    "container_docker_user",
    "container_docker_context",
    "container_docker_runtime",
    "container_podman_name",
    "container_podman_user",
    "container_podman_runtime",
    "container_nerdctl_name",
    "container_nerdctl_user",
    "container_nerdctl_runtime",
    "container_compose_service",
    "container_compose_user",
    "container_compose_file",
    "container_compose_project",
    "container_compose_runtime",
    "container_kubectl_pod",
    "container_kubectl_container",
    "container_kubectl_namespace",
    "container_kubectl_context",
    "container_kubectl_runtime",
    "log",
];

/// A finite call: at most one terminal event, so results and diagnostics can
/// safely take different streams.
fn finite_output() -> OutputSpec {
    OutputSpec::protocol_finite(
        ["json", "yaml", "plain"],
        ["split", "stdout", "stderr"],
        "json",
        "split",
    )
    .file_sinks(["stdout", "stderr"])
}

/// An ordered event stream, which must stay on one stream: splitting it across
/// stdout and stderr loses the ordering that makes it a stream. `split` is
/// therefore absent from the contract rather than rejected once the process is
/// already running.
fn stream_output() -> OutputSpec {
    OutputSpec::protocol_stream(
        ["json", "yaml", "plain"],
        ["stdout", "stderr"],
        "json",
        "stdout",
    )
    .file_sinks(["stdout", "stderr"])
}

/// The whole canonical CLI: one registry that is the single source for argv
/// parsing, typed values, which argument mixes are legal, each mix's output
/// contract, `--help`, and `docs/cli.md`.
pub fn build_cli(bin_name: &str) -> Result<BuiltCliSpec, CliSpecError> {
    let mut spec =
        CliSpec::new(bin_name, env!("CARGO_PKG_VERSION"))
            .about(env!("CARGO_PKG_DESCRIPTION"))
            .display_name(env!("DISPLAY_NAME"))
            .lifecycle_output(finite_output())
            .command(root_command())
            .command(CommandSpec::new(["inspect"]).about(
                "Schema discovery: databases, schemas, tables, views, indexes, or a full snapshot.",
            ))
            .command(inspect_databases_command())
            .command(inspect_simple_command(
                "database",
                "Summarize the connected database: schema/table/view/sequence counts and size.",
                "inspect_database",
            ))
            .command(inspect_simple_command(
                "schemas",
                "List user-visible schemas with owner, object counts, and size.",
                "inspect_schemas",
            ))
            .command(inspect_like_command(
                "schema",
                "Export full schema metadata for one schema.",
                "inspect_schema",
                "Optional `LIKE` pattern matched against relation names (`%` is the wildcard)",
            ))
            .command(inspect_like_command(
                "snapshot",
                "Export a stable full-schema snapshot for machine consumption.",
                "inspect_snapshot",
                "Optional `LIKE` pattern matched against relation names (`%` is the wildcard)",
            ))
            .command(inspect_like_command(
                "tables",
                "List tables in a schema with owner, estimated rows, and size.",
                "inspect_tables",
                "Optional `LIKE` pattern matched against the table name (`%` is the wildcard)",
            ))
            .command(inspect_like_command(
                "views",
                "List views (regular and materialized) in a schema with owner.",
                "inspect_views",
                "Optional `LIKE` pattern matched against the view name (`%` is the wildcard)",
            ))
            .command(inspect_indexes_command())
            .command(inspect_table_command())
            .command(
                CommandSpec::new(["psql"])
                    .about("Manage the local psql wrapper that forwards to `--mode psql`."),
            )
            .command(psql_admin_command(
                "status",
                "Show whether the afpsql-managed psql wrapper is installed and active.",
            ))
            .command(psql_admin_command(
                "install",
                "Install an afpsql-managed psql wrapper.",
            ))
            .command(psql_admin_command(
                "uninstall",
                "Remove an afpsql-managed psql wrapper.",
            ))
            .command(CommandSpec::new(["skill"]).about(
                "Manage Agent-First PSQL skills for Codex, Claude Code, opencode, and Hermes.",
            ))
            .command(skill_command(
                "status",
                "Show whether the Agent-First PSQL skill is installed, valid, and up to date.",
                false,
            ))
            .command(skill_command(
                "install",
                "Install the Agent-First PSQL skill.",
                true,
            ))
            .command(skill_command(
                "uninstall",
                "Remove an afpsql-managed Agent-First PSQL skill.",
                true,
            ));
    // Absent from a source tarball with no reachable .git, and the version
    // payload omits it rather than reporting the literal "unknown".
    if let Some(build) = Some(env!("GIT_SHA")).filter(|sha| *sha != "unknown") {
        spec = spec.build_id(build);
    }
    build_afdata_cli(spec)
}

/// Arguments that any connecting command accepts, declared once.
fn with_connection_args(command: CommandSpec) -> CommandSpec {
    command
        .arg(ArgSpec::option("--dsn", "SOURCE").about(
            "PostgreSQL DSN source: literal value, env:NAME, file:PATH#DOT_PATH, or \
                 literal:VALUE for a literal starting with a source prefix",
        ))
        .arg(ArgSpec::option("--conninfo", "SOURCE").about(
            "libpq conninfo source: literal value, env:NAME, file:PATH#DOT_PATH, or \
                 literal:VALUE for a literal starting with a source prefix",
        ))
        .arg(ArgSpec::option("--host", "HOST").about("PostgreSQL host"))
        .arg(ArgSpec::option_i64("--port", "PORT").about("PostgreSQL port"))
        .arg(ArgSpec::option("--user", "USER").about("PostgreSQL user name"))
        .arg(ArgSpec::option("--dbname", "DBNAME").about("PostgreSQL database name"))
        .arg(ArgSpec::option("--password", "SOURCE").about(
            "PostgreSQL password source: literal value, env:NAME, file:PATH#DOT_PATH, or \
                 literal:VALUE for a literal starting with a source prefix",
        ))
        .arg(
            ArgSpec::option("--ssh", "USER@HOST")
                .about("Open an SSH transport to USER@HOST before connecting to PostgreSQL"),
        )
        .arg(
            ArgSpec::option("--ssh-via", "USER@HOST")
                .repeatable()
                .about("SSH hop to reach before the final --ssh destination; repeat for more hops"),
        )
        .arg(
            ArgSpec::option("--ssh-option", "OPTION")
                .repeatable()
                .about("Additional OpenSSH -o option; repeat for more options"),
        )
        .arg(
            ArgSpec::option("--ssh-remote-socket", "PATH")
                .about("Explicit remote PostgreSQL Unix socket path for SSH forwarding"),
        )
        .arg(
            ArgSpec::option("--ssh-sudo-user", "USER").about(
                "Remote OS user for the sudo -n Unix-socket bridge; needs an explicit socket",
            ),
        )
        .arg(
            ArgSpec::option("--container-docker-name", "NAME")
                .about("Run a docker exec stdio bridge in this container before connecting"),
        )
        .arg(
            ArgSpec::option("--container-docker-user", "USER")
                .about("Container OS user to run the docker exec bridge as"),
        )
        .arg(
            ArgSpec::option("--container-docker-context", "CONTEXT")
                .about("Docker context to run the exec against"),
        )
        .arg(
            ArgSpec::option("--container-docker-runtime", "COMMAND")
                .about("Docker runtime command; defaults to docker"),
        )
        .arg(
            ArgSpec::option("--container-podman-name", "NAME")
                .about("Run a podman exec stdio bridge in this container before connecting"),
        )
        .arg(
            ArgSpec::option("--container-podman-user", "USER")
                .about("Container OS user to run the podman exec bridge as"),
        )
        .arg(
            ArgSpec::option("--container-podman-runtime", "COMMAND")
                .about("Podman runtime command; defaults to podman"),
        )
        .arg(
            ArgSpec::option("--container-nerdctl-name", "NAME")
                .about("Run a nerdctl exec stdio bridge in this container before connecting"),
        )
        .arg(
            ArgSpec::option("--container-nerdctl-user", "USER")
                .about("Container OS user to run the nerdctl exec bridge as"),
        )
        .arg(
            ArgSpec::option("--container-nerdctl-runtime", "COMMAND")
                .about("Nerdctl runtime command; defaults to nerdctl"),
        )
        .arg(
            ArgSpec::option("--container-compose-service", "NAME")
                .about("Run a compose exec stdio bridge in this service before connecting"),
        )
        .arg(
            ArgSpec::option("--container-compose-user", "USER")
                .about("Container OS user to run the compose exec bridge as"),
        )
        .arg(
            ArgSpec::option("--container-compose-file", "FILE")
                .repeatable()
                .about("Compose file passed before compose exec; repeat for more files"),
        )
        .arg(
            ArgSpec::option("--container-compose-project", "NAME")
                .about("Compose project name passed before compose exec"),
        )
        .arg(
            ArgSpec::option("--container-compose-runtime", "COMMAND")
                .about("Compose runtime command; defaults to docker, use docker-compose for v1"),
        )
        .arg(
            ArgSpec::option("--container-kubectl-pod", "NAME")
                .about("Run a kubectl exec stdio bridge in this pod before connecting"),
        )
        .arg(
            ArgSpec::option("--container-kubectl-container", "NAME")
                .about("Container within a multi-container pod to exec into"),
        )
        .arg(
            ArgSpec::option("--container-kubectl-namespace", "NAMESPACE")
                .about("Kubernetes namespace to run the exec in"),
        )
        .arg(
            ArgSpec::option("--container-kubectl-context", "CONTEXT")
                .about("Kubernetes context to run the exec against"),
        )
        .arg(
            ArgSpec::option("--container-kubectl-runtime", "COMMAND")
                .about("Kubectl runtime command; defaults to kubectl"),
        )
        .arg(ArgSpec::option("--log", "FILTER").repeatable().about(
            "Diagnostic log filter: startup, connect, query, transport, mode, an exact \
                     event such as query.error, or all. Comma-separated or repeated",
        ))
}

/// Arguments every SQL query shape shares, whatever its source or lifecycle.
fn query_shared_optional() -> Vec<&'static str> {
    vec![
        "param",
        "permission",
        "statement_timeout_ms",
        "lock_timeout_ms",
        "explain",
    ]
}

fn query_optional(extra: &[&'static str]) -> Vec<&'static str> {
    let mut ids = query_shared_optional();
    ids.extend_from_slice(extra);
    ids.extend_from_slice(&CONNECTION_IDS);
    ids
}

fn root_command() -> CommandSpec {
    let command = CommandSpec::root()
        .about("Run one SQL action per process, or open a long-lived pipe session.")
        .arg(ArgSpec::option("--sql", "SQL").about("Inline SQL to execute"))
        .arg(
            ArgSpec::option("--sql-file", "PATH")
                .about("File to read SQL from; `-` reads it from stdin"),
        )
        .arg(ArgSpec::option("--param", "N=VALUE").repeatable().about(
            "Positional bind parameter in N=value form; repeat for more parameters. \
                     Bare null/true/false bind as JSON null/booleans; prefix with `text:` to \
                     bind any value as a literal string",
        ))
        .arg(
            ArgSpec::flag("--stream-rows")
                .about("Stream the result as ordered result_rows batches instead of one payload"),
        )
        .arg(ArgSpec::option_i64("--batch-rows", "N").about("Maximum rows per streamed batch"))
        .arg(ArgSpec::option_i64("--batch-bytes", "N").about("Soft byte target per streamed batch"))
        .arg(
            ArgSpec::option_i64("--statement-timeout-ms", "MS")
                .about("Per-query statement timeout in milliseconds"),
        )
        .arg(
            ArgSpec::option_i64("--lock-timeout-ms", "MS")
                .about("Per-query lock timeout in milliseconds"),
        )
        .arg(
            ArgSpec::option_i64("--inline-max-rows", "N")
                .about("Maximum inline rows before returning a truncated result"),
        )
        .arg(
            ArgSpec::option_i64("--inline-max-bytes", "N")
                .about("Maximum inline payload bytes before returning a truncated result"),
        )
        .arg(
            ArgSpec::option_enum("--permission", PERMISSIONS)
                .value_name("PERMISSION")
                .about(
                    "Query permission policy; defaults to read, ssh-read with --ssh, or \
                     container-read with a --container-<driver>-* flag",
                ),
        )
        .arg(
            ArgSpec::flag("--dry-run")
                .about("Prepare the query and report its shape without running it"),
        )
        .arg(
            ArgSpec::option_enum("--explain", ["plan", "analyze"])
                .value_name("EXPLAIN")
                .about(
                    "Return the plan instead of the rows: `plan` wraps the SQL in EXPLAIN \
                     (FORMAT JSON); `analyze` runs it and buffers metrics",
                ),
        )
        .arg(
            ArgSpec::option_enum("--mode", ["cli", "pipe", "psql"])
                .value_name("MODE")
                .default("cli")
                .about(
                    "Runtime mode: one SQL action, a long-lived JSONL session, or psql \
                     argument translation",
                ),
        );

    with_connection_args(command)
        .combination(
            Combination::new("query-inline")
                .action("query")
                .about("Run inline --sql and return one bounded result")
                .fixed("mode", "cli")
                .required(["sql"])
                .optional(query_optional(&[
                    "dry_run",
                    "inline_max_rows",
                    "inline_max_bytes",
                ]))
                .output(finite_output()),
        )
        .combination(
            Combination::new("query-file")
                .action("query")
                .about("Run SQL read from --sql-file and return one bounded result")
                .fixed("mode", "cli")
                .required(["sql_file"])
                .optional(query_optional(&[
                    "dry_run",
                    "inline_max_rows",
                    "inline_max_bytes",
                ]))
                .output(finite_output()),
        )
        .combination(
            Combination::new("query-inline-stream")
                .action("query")
                .about("Stream inline --sql as ordered row batches on one stream")
                .fixed("mode", "cli")
                .required(["sql", "stream_rows"])
                .optional(query_optional(&["batch_rows", "batch_bytes"]))
                .output(stream_output()),
        )
        .combination(
            Combination::new("query-file-stream")
                .action("query")
                .about("Stream SQL read from --sql-file as ordered row batches on one stream")
                .fixed("mode", "cli")
                .required(["sql_file", "stream_rows"])
                .optional(query_optional(&["batch_rows", "batch_bytes"]))
                .output(stream_output()),
        )
        .combination(
            Combination::new("pipe")
                .action("pipe")
                .about("Open a long-lived JSONL session that reads requests from stdin")
                .fixed("mode", "pipe")
                .optional(CONNECTION_IDS)
                .output(stream_output()),
        )
        .combination(
            Combination::new("psql-translation")
                .action("psql_mode")
                .about(
                    "Translate a psql command line: every remaining argument is psql's own \
                     (-c, -f, -l, -h, -p, -U, -d, -v, DBNAME USERNAME), parsed by the \
                     compatibility layer rather than by this registry",
                )
                .fixed("mode", "psql")
                .output(finite_output()),
        )
}

fn inspect_command(name: &'static str, about: &'static str) -> CommandSpec {
    with_connection_args(CommandSpec::new(["inspect", name]).about(about))
}

fn inspect_combination(id: &'static str, extra: &[&'static str]) -> Combination {
    let mut optional: Vec<&str> = extra.to_vec();
    optional.extend_from_slice(&CONNECTION_IDS);
    Combination::new(id)
        .action(id)
        .optional(optional)
        .output(finite_output())
}

fn inspect_simple_command(
    name: &'static str,
    about: &'static str,
    id: &'static str,
) -> CommandSpec {
    inspect_command(name, about).combination(inspect_combination(id, &[]))
}

fn inspect_databases_command() -> CommandSpec {
    inspect_command(
        "databases",
        "List databases on the connected server with size, encoding, and connection facts.",
    )
    .arg(ArgSpec::flag("--all").about("Include template databases (template0/template1)"))
    .combination(inspect_combination("inspect_databases", &["all"]))
}

fn inspect_like_command(
    name: &'static str,
    about: &'static str,
    id: &'static str,
    like_about: &'static str,
) -> CommandSpec {
    inspect_command(name, about)
        .arg(
            ArgSpec::option("--schema", "SCHEMA")
                .default("public")
                .about("Schema to inspect"),
        )
        .arg(ArgSpec::option("--like", "PATTERN").about(like_about))
        .combination(inspect_combination(id, &["schema", "like"]))
}

fn inspect_indexes_command() -> CommandSpec {
    inspect_command(
        "indexes",
        "List indexes with definitions, size, validity, and optional usage stats.",
    )
    .arg(
        ArgSpec::option("--schema", "SCHEMA")
            .default("public")
            .about("Schema to filter on"),
    )
    .arg(
        ArgSpec::option("--table", "TABLE")
            .about("Table to filter on; `schema.table` overrides --schema"),
    )
    .arg(
        ArgSpec::flag("--stats")
            .about("Include PostgreSQL's built-in pg_stat_user_indexes usage counters"),
    )
    .combination(inspect_combination(
        "inspect_indexes",
        &["schema", "table", "stats"],
    ))
}

fn inspect_table_command() -> CommandSpec {
    // Declared before the connection arguments so the usage line reads the way
    // the call is written: the table name comes first.
    let command = CommandSpec::new(["inspect", "table"])
        .about("Describe a table's columns: types, nullability, defaults, primary key, comments.")
        .arg(
            ArgSpec::positional("name", 0, "NAME")
                .about("Table name; `schema.table` overrides the default `public` schema"),
        )
        .arg(
            ArgSpec::flag("--full")
                .about("Also return constraints, indexes, triggers, and sequence/default metadata"),
        );
    with_connection_args(command).combination({
        let mut optional: Vec<&str> = vec!["full"];
        optional.extend_from_slice(&CONNECTION_IDS);
        Combination::new("inspect_table")
            .action("inspect_table")
            .required(["name"])
            .optional(optional)
            .output(finite_output())
    })
}

fn psql_admin_command(verb: &'static str, about: &'static str) -> CommandSpec {
    CommandSpec::new(["psql", verb])
        .about(about)
        .arg(ArgSpec::option("--bin-dir", "DIR").about(
            "Directory that holds the psql wrapper; defaults to the afpsql executable directory",
        ))
        .combination(
            Combination::new(format!("psql-{verb}"))
                .action(format!("psql_{verb}"))
                .optional(["bin_dir"])
                .output(finite_output()),
        )
}

/// One skill verb, as two shapes.
///
/// `--skills-dir` names a single directory, so it is meaningless when the verb
/// fans out across every agent. Registering that as two shapes rather than one
/// shape plus a runtime check means the illegal mix is rejected by the parser,
/// and both legal mixes are visible in one `--help`.
fn skill_command(verb: &'static str, about: &'static str, force: bool) -> CommandSpec {
    let mut command = CommandSpec::new(["skill", verb])
        .about(about)
        .arg(
            ArgSpec::option_enum(
                "--agent",
                ["all", "codex", "claude-code", "opencode", "hermes"],
            )
            .value_name("AGENT")
            .default("all")
            .about("Agent to manage"),
        )
        .arg(
            ArgSpec::option_enum("--scope", ["personal", "workspace"])
                .value_name("SCOPE")
                .default("personal")
                .about("Skill scope"),
        )
        .arg(ArgSpec::option("--skills-dir", "DIR").about("Directory that contains skill folders"));

    let mut every: Vec<&str> = vec!["scope"];
    let mut named: Vec<&str> = vec!["scope", "skills_dir"];
    if force {
        command =
            command.arg(ArgSpec::flag("--force").about(
                "Overwrite or remove an unmanaged Agent-First PSQL skill at the target path",
            ));
        every.push("force");
        named.push("force");
    }

    command
        .combination(
            Combination::new(format!("skill-{verb}-every-agent"))
                .action(format!("skill_{verb}"))
                .about("Target every agent that supports the scope")
                .fixed("agent", "all")
                .optional(every)
                .output(finite_output()),
        )
        .combination(
            Combination::new(format!("skill-{verb}-one-agent"))
                .action(format!("skill_{verb}"))
                .about("Target one named agent; only this shape accepts --skills-dir")
                .fixed_one_of("agent", ["codex", "claude-code", "opencode", "hermes"])
                .optional(named)
                .output(finite_output()),
        )
}

type ActionResult = Result<Mode, ParseError>;
type ActionHandler = fn(&ResolvedInvocation) -> ActionResult;

fn actions() -> Vec<(&'static str, ActionHandler)> {
    vec![
        ("query", run_query as ActionHandler),
        ("pipe", run_pipe),
        ("psql_mode", run_psql_mode),
        ("inspect_databases", run_inspect_databases),
        ("inspect_database", run_inspect_database),
        ("inspect_schemas", run_inspect_schemas),
        ("inspect_schema", run_inspect_schema),
        ("inspect_snapshot", run_inspect_snapshot),
        ("inspect_tables", run_inspect_tables),
        ("inspect_views", run_inspect_views),
        ("inspect_indexes", run_inspect_indexes),
        ("inspect_table", run_inspect_table),
        ("psql_status", run_psql_status),
        ("psql_install", run_psql_install),
        ("psql_uninstall", run_psql_uninstall),
        ("skill_status", run_skill_status),
        ("skill_install", run_skill_install),
        ("skill_uninstall", run_skill_uninstall),
    ]
}

/// Resolve argv into one mode, or answer `--help`/`--version`/`--docs`.
pub fn parse_args(bin_name: &str) -> Result<Parsed, ParseError> {
    let cli = build_cli(bin_name)
        .map_err(|error| ParseError::new("cli_spec_invalid", error.to_string()))?;
    let raw: Vec<String> = std::env::args().collect();
    // The one invocation this registry cannot parse: psql's option grammar has
    // clustered shorts and positional dbname/username, so it is translated
    // rather than registered. The registered `psql-translation` shape and this
    // branch agree on what `--mode psql` alone means.
    if is_psql_mode_requested(&raw) {
        let (mode, routing) = parse_psql_mode_full(&raw)?;
        let redirect = install_redirect(routing.stdout_file, routing.stderr_file)?;
        crate::emit::set_output_to(routing.output_to);
        return Ok(Parsed { mode, redirect });
    }

    let app = cli
        .bind_actions(actions())
        .map_err(|error| ParseError::new("cli_actions_invalid", error.to_string()))?;
    let outcome = app
        .resolve_from(std::env::args_os())
        .map_err(|error| ParseError {
            code: error.rule.code().to_string(),
            message: error.message.clone(),
            hint: Some(error.hint.clone()),
        })?;

    match outcome {
        CliOutcome::Run(invocation) => {
            let redirect = redirect_for(invocation.output_plan())?;
            crate::emit::set_output_to(destination_of(invocation.output_plan()));
            let mode = app.execute(&invocation)?;
            Ok(Parsed { mode, redirect })
        }
        // `--docs` renders the whole registry as Markdown, so it carries no
        // format of its own and never becomes a protocol event.
        CliOutcome::Docs(docs) => {
            let _redirect = redirect_for(docs.output_plan())?;
            crate::emit::set_output_to(OutputTo::Stdout);
            let rendered = agent_first_data::render_cli_reference(&cli).replace(
                "| 2 | The invocation was rejected before anything ran. `error.code` is one of the `cli_*` codes below. |",
                "| 2 | The invocation was rejected before anything ran. `error.code` is one of the `cli_*` codes below. |\n| 4 | A terminal event could not be written; the requested outcome is unknown to the caller. |",
            );
            let _ = crate::emit::write_result_text(&rendered);
            std::process::exit(0);
        }
        CliOutcome::Help(help) => {
            let _redirect = redirect_for(help.output_plan())?;
            let format = format_of_plan(help.output_plan())?;
            crate::emit::set_output_to(destination_of(help.output_plan()));
            if format == OutputFormat::Plain {
                let _ = crate::emit::write_result_text(&help.plain());
            } else {
                let _ = crate::emit::emit_event(agent_first_data::cli_help_event(&help), format);
            }
            std::process::exit(0);
        }
        CliOutcome::Version(version) => {
            let _redirect = redirect_for(version.output_plan())?;
            let format = format_of_plan(version.output_plan())?;
            crate::emit::set_output_to(destination_of(version.output_plan()));
            let _ = crate::emit::emit_event(agent_first_data::cli_version_event(&version), format);
            std::process::exit(0);
        }
    }
}

fn redirect_for(
    plan: &OutputPlan,
) -> Result<Option<agent_first_data::stream_redirect::InstalledStreamRedirect>, ParseError> {
    install_redirect(
        plan.stdout_file().map(std::path::Path::to_path_buf),
        plan.stderr_file().map(std::path::Path::to_path_buf),
    )
}

fn install_redirect(
    stdout_file: Option<std::path::PathBuf>,
    stderr_file: Option<std::path::PathBuf>,
) -> Result<Option<agent_first_data::stream_redirect::InstalledStreamRedirect>, ParseError> {
    let config =
        agent_first_data::stream_redirect::StreamRedirectConfig::new(stdout_file, stderr_file)
            .map_err(|error| ParseError::invalid_request(error.to_string()))?;
    config
        .as_ref()
        .map(agent_first_data::stream_redirect::install)
        .transpose()
        .map_err(|error| ParseError::invalid_request(error.to_string()))
}

fn destination_of(plan: &OutputPlan) -> OutputTo {
    plan.destination()
        .and_then(|destination| OutputTo::parse(destination).ok())
        .unwrap_or(OutputTo::Split)
}

fn format_of_plan(plan: &OutputPlan) -> Result<OutputFormat, ParseError> {
    match plan.format() {
        None => Ok(OutputFormat::Json),
        Some(format) => cli_parse_output(format).map_err(ParseError::invalid_value),
    }
}

fn format_of(invocation: &ResolvedInvocation) -> Result<OutputFormat, ParseError> {
    format_of_plan(invocation.output_plan())
}

fn optional_string(invocation: &ResolvedInvocation, id: &str) -> Option<String> {
    invocation
        .optional(id)
        .and_then(CliValue::as_str)
        .map(str::to_string)
}

fn required_string(invocation: &ResolvedInvocation, id: &str) -> String {
    invocation
        .required(id)
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn flag(invocation: &ResolvedInvocation, id: &str) -> bool {
    invocation
        .optional(id)
        .and_then(CliValue::as_bool)
        .unwrap_or(false)
}

fn repeated_strings(invocation: &ResolvedInvocation, id: &str) -> Vec<String> {
    invocation
        .repeated(id)
        .iter()
        .filter_map(CliValue::as_str)
        .map(str::to_string)
        .collect()
}

/// A count the registry can only type as `i64`, because it has no unsigned
/// type. The check reports the parser's own classification.
fn count_of(
    invocation: &ResolvedInvocation,
    id: &str,
    flag_name: &str,
) -> Result<Option<usize>, ParseError> {
    match invocation.optional(id).and_then(CliValue::as_i64) {
        None => Ok(None),
        Some(value) => usize::try_from(value).map(Some).map_err(|_| {
            ParseError::invalid_value(format!("{flag_name} must not be negative"))
                .hint(format!("pass zero or a positive count to {flag_name}"))
        }),
    }
}

fn millis_of(
    invocation: &ResolvedInvocation,
    id: &str,
    flag_name: &str,
) -> Result<Option<u64>, ParseError> {
    match invocation.optional(id).and_then(CliValue::as_i64) {
        None => Ok(None),
        Some(value) => u64::try_from(value).map(Some).map_err(|_| {
            ParseError::invalid_value(format!("{flag_name} must not be negative"))
                .hint(format!("pass zero or a positive duration to {flag_name}"))
        }),
    }
}

fn port_of(
    invocation: &ResolvedInvocation,
    id: &str,
    flag_name: &str,
) -> Result<Option<u16>, ParseError> {
    match invocation.optional(id).and_then(CliValue::as_i64) {
        None => Ok(None),
        Some(value) => u16::try_from(value).map(Some).map_err(|_| {
            ParseError::invalid_value(format!("{flag_name} must be a port between 0 and 65535"))
                .hint(format!("pass a TCP port in 0..=65535 to {flag_name}"))
        }),
    }
}

/// The `--log` filters, accepting both repetition and comma-separated lists.
fn log_entries(invocation: &ResolvedInvocation) -> Vec<String> {
    split_log_entries(&repeated_strings(invocation, "log"))
}

fn split_log_entries(values: &[String]) -> Vec<String> {
    values
        .iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(std::string::ToString::to_string)
        .collect()
}

/// Whether the caller asked for the startup log line.
fn startup_requested(entries: &[String]) -> bool {
    entries.iter().any(|entry| {
        matches!(
            entry.trim().to_ascii_lowercase().as_str(),
            "startup" | "all" | "*"
        )
    })
}

/// One resolved connection, plus the source metadata the startup log reports.
struct Connection {
    session: SessionConfig,
    sources: Value,
}

enum TypedSecretSource {
    Literal(String),
    Env(String),
    File(SecretConfigRef),
}

impl TypedSecretSource {
    fn parse(flag: &str, raw: Option<String>) -> Result<Option<Self>, ParseError> {
        let Some(raw) = raw else {
            return Ok(None);
        };
        // Escape hatch for a literal secret that itself starts with a source
        // prefix; without it such a value would be unrepresentable.
        if let Some(value) = raw.strip_prefix("literal:") {
            return Ok(Some(Self::Literal(value.to_string())));
        }
        if let Some(name) = raw.strip_prefix("env:") {
            if name.is_empty() {
                return Err(ParseError::invalid_value(format!(
                    "{flag} env source requires a variable name"
                )));
            }
            return Ok(Some(Self::Env(name.to_string())));
        }
        if let Some(file_source) = raw.strip_prefix("file:") {
            let Some((file, path)) = file_source.rsplit_once('#') else {
                return Err(ParseError::invalid_value(format!(
                    "{flag} file source must be file:PATH#DOT_PATH"
                )));
            };
            if file.is_empty() || path.is_empty() {
                return Err(ParseError::invalid_value(format!(
                    "{flag} file source requires both PATH and DOT_PATH"
                )));
            }
            return Ok(Some(Self::File(SecretConfigRef {
                file: std::path::PathBuf::from(file),
                path: path.to_string(),
            })));
        }
        Ok(Some(Self::Literal(raw)))
    }

    fn resolve(&self, flag: &str) -> Result<String, ParseError> {
        match self {
            Self::Literal(value) => Ok(value.clone()),
            Self::Env(name) => std::env::var(name).map_err(|_| {
                ParseError::invalid_value(format!(
                    "{flag} references an unset environment variable"
                ))
            }),
            Self::File(reference) => {
                resolve_config_secret(flag, reference).map_err(ParseError::invalid_value)
            }
        }
    }

    fn metadata(&self) -> Value {
        match self {
            Self::Literal(_) => json!({"kind": "direct"}),
            Self::Env(name) => json!({"kind": "env", "name": name}),
            Self::File(reference) => reference.safe_metadata(),
        }
    }
}

fn connection_from(invocation: &ResolvedInvocation) -> Result<Connection, ParseError> {
    let dsn = TypedSecretSource::parse("--dsn", optional_string(invocation, "dsn"))?;
    let conninfo = TypedSecretSource::parse("--conninfo", optional_string(invocation, "conninfo"))?;
    let password = TypedSecretSource::parse("--password", optional_string(invocation, "password"))?;
    let mut source_fields = serde_json::Map::new();
    for (name, source) in [
        ("dsn", dsn.as_ref()),
        ("conninfo", conninfo.as_ref()),
        ("password", password.as_ref()),
    ] {
        if let Some(source) = source {
            source_fields.insert(name.to_string(), source.metadata());
        }
    }
    let sources = Value::Object(source_fields);

    let session = SessionConfig {
        // Caller-supplied, so the ordinary environment fallbacks still apply;
        // only a locked administrator profile pins the endpoint.
        profile_pinned: false,
        dsn_secret: dsn
            .as_ref()
            .map(|source| source.resolve("--dsn"))
            .transpose()?,
        conninfo_secret: conninfo
            .as_ref()
            .map(|source| source.resolve("--conninfo"))
            .transpose()?,
        host: optional_string(invocation, "host"),
        port: port_of(invocation, "port", "--port")?,
        user: optional_string(invocation, "user"),
        dbname: optional_string(invocation, "dbname"),
        password_secret: password
            .as_ref()
            .map(|source| source.resolve("--password"))
            .transpose()?,
        ssh: SshConfig {
            destination: optional_string(invocation, "ssh")
                .or_else(|| crate::runtime_env::nonempty("AFPSQL_SSH")),
            via: non_empty_or_env(repeated_strings(invocation, "ssh_via"), "AFPSQL_SSH_VIA"),
            options: repeated_strings(invocation, "ssh_option"),
            local_host: None,
            local_port: None,
            remote_socket: optional_string(invocation, "ssh_remote_socket")
                .or_else(|| crate::runtime_env::nonempty("AFPSQL_SSH_REMOTE_SOCKET")),
            sudo_user: optional_string(invocation, "ssh_sudo_user")
                .or_else(|| crate::runtime_env::nonempty("AFPSQL_SSH_SUDO_USER")),
        },
        container: cli_container_config(ContainerConfig {
            docker_name: optional_string(invocation, "container_docker_name"),
            docker_user: optional_string(invocation, "container_docker_user"),
            docker_context: optional_string(invocation, "container_docker_context"),
            docker_runtime: optional_string(invocation, "container_docker_runtime"),
            podman_name: optional_string(invocation, "container_podman_name"),
            podman_user: optional_string(invocation, "container_podman_user"),
            podman_runtime: optional_string(invocation, "container_podman_runtime"),
            nerdctl_name: optional_string(invocation, "container_nerdctl_name"),
            nerdctl_user: optional_string(invocation, "container_nerdctl_user"),
            nerdctl_runtime: optional_string(invocation, "container_nerdctl_runtime"),
            compose_service: optional_string(invocation, "container_compose_service"),
            compose_user: optional_string(invocation, "container_compose_user"),
            compose_files: repeated_strings(invocation, "container_compose_file"),
            compose_project: optional_string(invocation, "container_compose_project"),
            compose_runtime: optional_string(invocation, "container_compose_runtime"),
            kubectl_pod: optional_string(invocation, "container_kubectl_pod"),
            kubectl_container: optional_string(invocation, "container_kubectl_container"),
            kubectl_namespace: optional_string(invocation, "container_kubectl_namespace"),
            kubectl_context: optional_string(invocation, "container_kubectl_context"),
            kubectl_runtime: optional_string(invocation, "container_kubectl_runtime"),
        }),
    };
    // Which driver runs is inferred from the flag family used, so mixing two
    // families names no driver. Rejected here rather than at connect time: it
    // is a fact about the arguments, not about the world they point at.
    session
        .container
        .selected_driver()
        .map_err(ParseError::invalid_value)?;
    Ok(Connection { session, sources })
}

/// Fill unset container fields from the environment.
///
/// CLI mode resolves the environment while parsing so `startup` reports the
/// session it will actually connect with; a pinned readonly profile replaces
/// this whole session later and reads no environment at all.
fn cli_container_config(container: ContainerConfig) -> ContainerConfig {
    crate::container_transport::container_config_with_env(&container, false)
}

fn non_empty_or_env(values: Vec<String>, key: &str) -> Vec<String> {
    if values.is_empty() {
        parse_csv_env(key)
    } else {
        values
    }
}

fn run_query(invocation: &ResolvedInvocation) -> ActionResult {
    let output = format_of(invocation)?;
    let entries = log_entries(invocation);
    let connection = connection_from(invocation)?;
    let sql_file = optional_string(invocation, "sql_file");
    let user_sql = load_sql(optional_string(invocation, "sql"), sql_file.clone())?;
    let params =
        parse_params(&repeated_strings(invocation, "param")).map_err(ParseError::invalid_value)?;
    let sql = match optional_string(invocation, "explain").as_deref() {
        Some("analyze") => wrap_explain_sql(&user_sql, true),
        Some(_) => wrap_explain_sql(&user_sql, false),
        None => user_sql,
    };
    let startup_args = with_connection_sources(
        startup_args("cli", Some(&sql), sql_file.as_deref(), params.len()),
        &connection.sources,
    );

    Ok(Mode::Cli(CliRequest {
        sql,
        params,
        options: QueryOptions {
            stream_rows: flag(invocation, "stream_rows"),
            batch_rows: count_of(invocation, "batch_rows", "--batch-rows")?,
            batch_bytes: count_of(invocation, "batch_bytes", "--batch-bytes")?,
            statement_timeout_ms: millis_of(
                invocation,
                "statement_timeout_ms",
                "--statement-timeout-ms",
            )?,
            lock_timeout_ms: millis_of(invocation, "lock_timeout_ms", "--lock-timeout-ms")?,
            permission: permission_of(invocation),
            inline_max_rows: count_of(invocation, "inline_max_rows", "--inline-max-rows")?,
            inline_max_bytes: count_of(invocation, "inline_max_bytes", "--inline-max-bytes")?,
        },
        session: connection.session,
        output,
        log: parse_log_categories(&entries),
        startup_args,
        startup_env: startup_env_snapshot(),
        startup_requested: startup_requested(&entries),
        dry_run: flag(invocation, "dry_run"),
        psql_mode: false,
    }))
}

fn permission_of(invocation: &ResolvedInvocation) -> Option<Permission> {
    optional_string(invocation, "permission")
        .as_deref()
        .and_then(|value| value.parse().ok())
}

fn run_pipe(invocation: &ResolvedInvocation) -> ActionResult {
    let output = format_of(invocation)?;
    let entries = log_entries(invocation);
    let connection = connection_from(invocation)?;
    Ok(Mode::Pipe(PipeInit {
        output,
        session: connection.session,
        log: parse_log_categories(&entries),
        startup_args: with_connection_sources(
            startup_args("pipe", None, None, 0),
            &connection.sources,
        ),
        startup_env: startup_env_snapshot(),
        startup_requested: startup_requested(&entries),
    }))
}

/// Both routes into psql translation end here.
///
/// `parse_args` short-circuits to the translator whenever argv asks for psql
/// mode, because psql's option grammar is not expressible in this registry. The
/// registered shape binds to this handler, which runs that same translator on
/// that same argv — so the registry's account of `--mode psql` and what the
/// process actually does cannot drift apart.
fn run_psql_mode(_invocation: &ResolvedInvocation) -> ActionResult {
    let raw: Vec<String> = std::env::args().collect();
    Ok(parse_psql_mode_full(&raw).map(|(mode, _)| mode)?)
}

fn run_inspect(invocation: &ResolvedInvocation, action: InspectAction) -> ActionResult {
    let output = format_of(invocation)?;
    let entries = log_entries(invocation);
    let connection = connection_from(invocation)?;
    let (sql, params) = build_inspect_sql(action);
    let startup_args = with_connection_sources(
        startup_args("cli", Some(&sql), None, params.len()),
        &connection.sources,
    );
    Ok(Mode::Cli(CliRequest {
        sql,
        params,
        options: QueryOptions::default(),
        session: connection.session,
        output,
        log: parse_log_categories(&entries),
        startup_args,
        startup_env: startup_env_snapshot(),
        startup_requested: startup_requested(&entries),
        dry_run: false,
        psql_mode: false,
    }))
}

fn schema_of(invocation: &ResolvedInvocation) -> String {
    optional_string(invocation, "schema").unwrap_or_else(|| "public".to_string())
}

fn run_inspect_databases(invocation: &ResolvedInvocation) -> ActionResult {
    run_inspect(
        invocation,
        InspectAction::Databases(InspectDatabasesArgs {
            all: flag(invocation, "all"),
        }),
    )
}

fn run_inspect_database(invocation: &ResolvedInvocation) -> ActionResult {
    run_inspect(invocation, InspectAction::Database)
}

fn run_inspect_schemas(invocation: &ResolvedInvocation) -> ActionResult {
    run_inspect(invocation, InspectAction::Schemas)
}

fn run_inspect_schema(invocation: &ResolvedInvocation) -> ActionResult {
    run_inspect(
        invocation,
        InspectAction::Schema(InspectSchemaArgs {
            schema: schema_of(invocation),
            like: optional_string(invocation, "like"),
        }),
    )
}

fn run_inspect_snapshot(invocation: &ResolvedInvocation) -> ActionResult {
    run_inspect(
        invocation,
        InspectAction::Snapshot(InspectSchemaArgs {
            schema: schema_of(invocation),
            like: optional_string(invocation, "like"),
        }),
    )
}

fn run_inspect_tables(invocation: &ResolvedInvocation) -> ActionResult {
    run_inspect(
        invocation,
        InspectAction::Tables(InspectTablesArgs {
            schema: schema_of(invocation),
            like: optional_string(invocation, "like"),
        }),
    )
}

fn run_inspect_views(invocation: &ResolvedInvocation) -> ActionResult {
    run_inspect(
        invocation,
        InspectAction::Views(InspectViewsArgs {
            schema: schema_of(invocation),
            like: optional_string(invocation, "like"),
        }),
    )
}

fn run_inspect_indexes(invocation: &ResolvedInvocation) -> ActionResult {
    run_inspect(
        invocation,
        InspectAction::Indexes(InspectIndexesArgs {
            schema: schema_of(invocation),
            table: optional_string(invocation, "table"),
            stats: flag(invocation, "stats"),
        }),
    )
}

fn run_inspect_table(invocation: &ResolvedInvocation) -> ActionResult {
    run_inspect(
        invocation,
        InspectAction::Table(InspectTableArgs {
            name: required_string(invocation, "name"),
            full: flag(invocation, "full"),
        }),
    )
}

fn psql_admin_request(
    invocation: &ResolvedInvocation,
    action: fn(Option<String>) -> PsqlAdminAction,
) -> ActionResult {
    Ok(Mode::PsqlAdmin(PsqlAdminRequest {
        action: action(optional_string(invocation, "bin_dir")),
        output: format_of(invocation)?,
    }))
}

fn run_psql_status(invocation: &ResolvedInvocation) -> ActionResult {
    psql_admin_request(invocation, |bin_dir| PsqlAdminAction::Status { bin_dir })
}

fn run_psql_install(invocation: &ResolvedInvocation) -> ActionResult {
    psql_admin_request(invocation, |bin_dir| PsqlAdminAction::Install { bin_dir })
}

fn run_psql_uninstall(invocation: &ResolvedInvocation) -> ActionResult {
    psql_admin_request(invocation, |bin_dir| PsqlAdminAction::Uninstall { bin_dir })
}

fn skill_request(
    invocation: &ResolvedInvocation,
    action: fn(SkillAdminOptions) -> SkillAdminAction,
) -> ActionResult {
    let options = SkillAdminOptions {
        agent: match optional_string(invocation, "agent").as_deref() {
            Some("codex") => SkillAgentSelection::Codex,
            Some("claude-code") => SkillAgentSelection::ClaudeCode,
            Some("opencode") => SkillAgentSelection::Opencode,
            Some("hermes") => SkillAgentSelection::Hermes,
            _ => SkillAgentSelection::All,
        },
        scope: match optional_string(invocation, "scope").as_deref() {
            Some("workspace") => SkillScope::Workspace,
            _ => SkillScope::Personal,
        },
        skills_dir: optional_string(invocation, "skills_dir"),
        force: flag(invocation, "force"),
    };
    Ok(Mode::SkillAdmin(SkillAdminRequest {
        action: action(options),
        output: format_of(invocation)?,
    }))
}

fn run_skill_status(invocation: &ResolvedInvocation) -> ActionResult {
    skill_request(invocation, SkillAdminAction::Status)
}

fn run_skill_install(invocation: &ResolvedInvocation) -> ActionResult {
    skill_request(invocation, SkillAdminAction::Install)
}

fn run_skill_uninstall(invocation: &ResolvedInvocation) -> ActionResult {
    skill_request(invocation, SkillAdminAction::Uninstall)
}

/// Where a translated psql invocation writes, decided by the translator rather
/// than by the registry's output contract.
struct PsqlRouting {
    output_to: OutputTo,
    stdout_file: Option<std::path::PathBuf>,
    stderr_file: Option<std::path::PathBuf>,
}

fn parse_psql_mode_full(raw: &[String]) -> Result<(Mode, PsqlRouting), String> {
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

    let routing = PsqlRouting {
        output_to: state.output_to.unwrap_or(OutputTo::Split),
        stdout_file: state.stdout_file.clone().map(std::path::PathBuf::from),
        stderr_file: state.stderr_file.clone().map(std::path::PathBuf::from),
    };
    let startup_requested = startup_requested(&state.log_entries);

    if let Some(reason) = state.interactive_reason {
        return Ok((
            Mode::PsqlUnsupported(PsqlUnsupportedRequest { reason }),
            routing,
        ));
    }

    apply_psql_positionals(&mut state)?;
    if state.list_databases {
        state.sql = Some(psql_list_databases_sql());
        state.sql_file = None;
    }
    if state.sql.is_none() && state.sql_file.is_none() {
        return Ok((
            Mode::PsqlUnsupported(PsqlUnsupportedRequest {
                reason: "no -c/--command, -f/--file, or -l/--list was provided".to_string(),
            }),
            routing,
        ));
    }

    let dsn = TypedSecretSource::parse("--dsn", state.dsn_secret).map_err(|error| error.message)?;
    let conninfo = TypedSecretSource::parse("--conninfo", state.conninfo_secret)
        .map_err(|error| error.message)?;
    let password = TypedSecretSource::parse("--password", state.password_secret)
        .map_err(|error| error.message)?;
    let mut source_fields = serde_json::Map::new();
    for (name, source) in [
        ("dsn", dsn.as_ref()),
        ("conninfo", conninfo.as_ref()),
        ("password", password.as_ref()),
    ] {
        if let Some(source) = source {
            source_fields.insert(name.to_string(), source.metadata());
        }
    }
    let connection_sources = Value::Object(source_fields);
    let dsn_secret = dsn
        .as_ref()
        .map(|source| source.resolve("--dsn").map_err(|error| error.message))
        .transpose()?;
    let conninfo_secret = conninfo
        .as_ref()
        .map(|source| source.resolve("--conninfo").map_err(|error| error.message))
        .transpose()?;
    let password_secret = password
        .as_ref()
        .map(|source| source.resolve("--password").map_err(|error| error.message))
        .transpose()?;
    let session = SessionConfig {
        // Caller-supplied, so the ordinary environment fallbacks still apply;
        // only a locked administrator profile pins the endpoint.
        profile_pinned: false,
        dsn_secret,
        conninfo_secret,
        host: state.host,
        port: state.port,
        user: state.user,
        dbname: state.dbname,
        password_secret,
        ssh: SshConfig::default(),
        container: cli_container_config(state.container),
    };
    session.container.selected_driver()?;

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
    Ok((
        Mode::Cli(CliRequest {
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
        }),
        routing,
    ))
}

struct PsqlModeState {
    sql: Option<String>,
    sql_file: Option<String>,
    host: Option<String>,
    port: Option<u16>,
    user: Option<String>,
    dbname: Option<String>,
    dsn_secret: Option<String>,
    conninfo_secret: Option<String>,
    password_secret: Option<String>,
    container: ContainerConfig,
    params_kv: Vec<String>,
    output: OutputFormat,
    output_to: Option<OutputTo>,
    stdout_file: Option<String>,
    stderr_file: Option<String>,
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
            conninfo_secret: None,
            password_secret: None,
            container: ContainerConfig::default(),
            params_kv: vec![],
            output: OutputFormat::Json,
            output_to: None,
            stdout_file: None,
            stderr_file: None,
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
    if let Some(source) = arg.strip_prefix("--password=") {
        if source.is_empty() {
            return Err("--password=SOURCE requires a non-empty source".to_string());
        }
        state.password_secret = Some(source.to_string());
        *i += 1;
        return Ok(());
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
        "--dsn" => {
            state.dsn_secret = Some(take_long_arg_value(raw, i, "--dsn")?);
            Ok(())
        }
        "--conninfo" => {
            state.conninfo_secret = Some(take_long_arg_value(raw, i, "--conninfo")?);
            Ok(())
        }
        "--container-docker-name" => {
            state.container.docker_name =
                Some(take_long_arg_value(raw, i, "--container-docker-name")?);
            Ok(())
        }
        "--container-docker-user" => {
            state.container.docker_user =
                Some(take_long_arg_value(raw, i, "--container-docker-user")?);
            Ok(())
        }
        "--container-docker-context" => {
            state.container.docker_context =
                Some(take_long_arg_value(raw, i, "--container-docker-context")?);
            Ok(())
        }
        "--container-docker-runtime" => {
            state.container.docker_runtime =
                Some(take_long_arg_value(raw, i, "--container-docker-runtime")?);
            Ok(())
        }
        "--container-podman-name" => {
            state.container.podman_name =
                Some(take_long_arg_value(raw, i, "--container-podman-name")?);
            Ok(())
        }
        "--container-podman-user" => {
            state.container.podman_user =
                Some(take_long_arg_value(raw, i, "--container-podman-user")?);
            Ok(())
        }
        "--container-podman-runtime" => {
            state.container.podman_runtime =
                Some(take_long_arg_value(raw, i, "--container-podman-runtime")?);
            Ok(())
        }
        "--container-nerdctl-name" => {
            state.container.nerdctl_name =
                Some(take_long_arg_value(raw, i, "--container-nerdctl-name")?);
            Ok(())
        }
        "--container-nerdctl-user" => {
            state.container.nerdctl_user =
                Some(take_long_arg_value(raw, i, "--container-nerdctl-user")?);
            Ok(())
        }
        "--container-nerdctl-runtime" => {
            state.container.nerdctl_runtime =
                Some(take_long_arg_value(raw, i, "--container-nerdctl-runtime")?);
            Ok(())
        }
        "--container-compose-service" => {
            state.container.compose_service =
                Some(take_long_arg_value(raw, i, "--container-compose-service")?);
            Ok(())
        }
        "--container-compose-user" => {
            state.container.compose_user =
                Some(take_long_arg_value(raw, i, "--container-compose-user")?);
            Ok(())
        }
        "--container-compose-file" => {
            state.container.compose_files.push(take_long_arg_value(
                raw,
                i,
                "--container-compose-file",
            )?);
            Ok(())
        }
        "--container-compose-project" => {
            state.container.compose_project =
                Some(take_long_arg_value(raw, i, "--container-compose-project")?);
            Ok(())
        }
        "--container-compose-runtime" => {
            state.container.compose_runtime =
                Some(take_long_arg_value(raw, i, "--container-compose-runtime")?);
            Ok(())
        }
        "--container-kubectl-pod" => {
            state.container.kubectl_pod =
                Some(take_long_arg_value(raw, i, "--container-kubectl-pod")?);
            Ok(())
        }
        "--container-kubectl-container" => {
            state.container.kubectl_container = Some(take_long_arg_value(
                raw,
                i,
                "--container-kubectl-container",
            )?);
            Ok(())
        }
        "--container-kubectl-namespace" => {
            state.container.kubectl_namespace = Some(take_long_arg_value(
                raw,
                i,
                "--container-kubectl-namespace",
            )?);
            Ok(())
        }
        "--container-kubectl-context" => {
            state.container.kubectl_context =
                Some(take_long_arg_value(raw, i, "--container-kubectl-context")?);
            Ok(())
        }
        "--container-kubectl-runtime" => {
            state.container.kubectl_runtime =
                Some(take_long_arg_value(raw, i, "--container-kubectl-runtime")?);
            Ok(())
        }
        "--stdout-file" => {
            state.stdout_file = Some(take_long_arg_value(raw, i, "--stdout-file")?);
            Ok(())
        }
        "--stderr-file" => {
            state.stderr_file = Some(take_long_arg_value(raw, i, "--stderr-file")?);
            Ok(())
        }
        "--output-to" => {
            let value = take_long_arg_value(raw, i, "--output-to")?;
            state.output_to = Some(OutputTo::parse(&value)?);
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
    let _ = crate::emit::write_result_text(&format!(
        "psql (afpsql wrapper) {}\n",
        env!("CARGO_PKG_VERSION")
    ));
}

fn emit_psql_mode_help() {
    let _ = crate::emit::write_result_text(&format!(
        "psql (afpsql wrapper) {}\n\
Usage:\n  psql [OPTION]... [DBNAME [USERNAME]]\n\n\
Supported non-interactive forms:\n  -c, --command=SQL\n  -f, --file=FILE\n  -l, --list\n  -h/-p/-U/-d and --host/--port/--username/--dbname\n  -v N=value, --set N=value for positional bind parameters\n\n\
Output:\n  --stdout-file=FILE redirects stdout bytes to FILE\n  --stderr-file=FILE redirects stderr bytes to FILE\n  --output-to=split|stdout|stderr selects AFDATA event routing\n\n\
Human-interactive psql modes and psql meta-commands are not supported by this wrapper.",
        env!("CARGO_PKG_VERSION")
    ));
}

/// Whether this argv asks for psql translation rather than the registry.
///
/// The scan skips each option's value using the registry's own knowledge of
/// which long arguments take one, so this decision and the parser can never
/// disagree about which token is a flag — `--sql --mode` names a value, not a
/// mode. It stops at the first subcommand token, because `--mode` is a
/// root-only argument.
fn is_psql_mode_requested(raw: &[String]) -> bool {
    let takes_value = root_value_arguments();
    let mut i = 1usize;
    while i < raw.len() {
        let arg = raw[i].as_str();
        if arg == "--" {
            break;
        }
        if arg == "--mode" {
            return raw.get(i + 1).is_some_and(|value| value == "psql");
        }
        if arg == "--mode=psql" {
            return true;
        }
        if arg.starts_with("--") {
            let name = long_name(arg);
            i += if arg.contains('=') || !takes_value.contains(name) {
                1
            } else {
                2
            };
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

/// Every root-level long argument that consumes a following token.
///
/// Derived from the registry plus the four output arguments AFDATA injects
/// into it, so adding an argument cannot leave this scanner behind.
fn root_value_arguments() -> std::collections::BTreeSet<String> {
    let mut names: std::collections::BTreeSet<String> = root_command()
        .arguments
        .iter()
        .filter(|argument| argument.value_type != agent_first_data::ArgValueType::Flag)
        .filter_map(|argument| match &argument.syntax {
            agent_first_data::ArgSyntax::Long { name } => Some(name.clone()),
            agent_first_data::ArgSyntax::Positional { .. } => None,
        })
        .collect();
    for injected in ["--output", "--output-to", "--stdout-file", "--stderr-file"] {
        names.insert(injected.to_string());
    }
    names
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
    if let Some(text) = v.strip_prefix("text:") {
        return Value::String(text.to_string());
    }
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
    let body = crate::db::trim_trailing_statement_terminators(user_sql);
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
