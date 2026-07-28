<!-- Generated. Do not edit by hand. Regenerate: afpsql --help --recursive --output markdown -->

# afpsql CLI Reference

# afpsql - A PostgreSQL interface for AI agents: reliable, structured, explicit, and read-only by default.

`afpsql` gives agents a reliable PostgreSQL contract: structured
AFDATA events, first-class SSH/container transports, explicit write permissions,
stable pipe sessions, and machine-readable failures.

### Interface Policy

- default mode is canonical agent-first CLI
- `--mode psql` is argument translation only; runtime output stays JSONL
- `--output-to split` sends results to stdout and errors/logs/progress to stderr
- `--output-to stdout|stderr` selects one ordered event stream
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
- with `--ssh`, DSN/conninfo endpoints are interpreted from the final SSH host and secrets stay local
- SSH and container transports currently require one PostgreSQL endpoint, not a multi-host failover list
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
afpsql --ssh user@server --dsn-secret-config config.yaml database.url --sql "select 1"
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

```text
afpsql [OPTIONS] [COMMAND]
```

| Argument | Description |
| --- | --- |
| `--sql <SQL>` | Inline SQL string to execute |
| `--sql-file <SQL_FILE>` | Read SQL from a file |
| `--param <PARAM>...` | Positional bind parameter in `N=value` form. Repeat for additional parameters |
| `--stream-rows` | Stream large results as `result_rows` batches |
| `--batch-rows <BATCH_ROWS>` | Maximum rows per streamed batch |
| `--batch-bytes <BATCH_BYTES>` | Soft byte target per streamed batch |
| `--statement-timeout-ms <STATEMENT_TIMEOUT_MS>` | Per-query statement timeout in milliseconds |
| `--lock-timeout-ms <LOCK_TIMEOUT_MS>` | Per-query lock timeout in milliseconds |
| `--inline-max-rows <INLINE_MAX_ROWS>` | Maximum inline rows before returning `result_too_large` |
| `--inline-max-bytes <INLINE_MAX_BYTES>` | Maximum inline payload bytes before returning `result_too_large` |
| `--permission <PERMISSION>` | Query permission policy |
| `--dry-run` | Preview the query without executing it |
| `--explain` | Wrap the query in EXPLAIN (FORMAT JSON) and return the plan tree instead of executing the user's SQL |
| `--explain-analyze` | Run EXPLAIN ANALYZE with JSON and buffer metrics |
| `--dsn-secret <DSN_SECRET>` | PostgreSQL DSN URI. Redacted in structured output *(global)* |
| `--dsn-secret-env <DSN_SECRET_ENV>` | Read PostgreSQL DSN URI from an environment variable *(global)* |
| `--dsn-secret-config <FILE> <DOT_PATH>...` | Read PostgreSQL DSN URI from FILE at DOT_PATH *(global)* |
| `--conninfo-secret <CONNINFO_SECRET>` | libpq-style conninfo string. Redacted in structured output *(global)* |
| `--conninfo-secret-env <CONNINFO_SECRET_ENV>` | Read libpq-style conninfo string from an environment variable *(global)* |
| `--conninfo-secret-config <FILE> <DOT_PATH>...` | Read libpq-style conninfo from FILE at DOT_PATH *(global)* |
| `--host <HOST>` | PostgreSQL host *(global)* |
| `--port <PORT>` | PostgreSQL port *(global)* |
| `--user <USER>` | PostgreSQL user name *(global)* |
| `--dbname <DBNAME>` | PostgreSQL database name *(global)* |
| `--password-secret <PASSWORD_SECRET>` | PostgreSQL password. Redacted in structured output *(global)* |
| `--password-secret-env <PASSWORD_SECRET_ENV>` | Read PostgreSQL password from an environment variable *(global)* |
| `--password-secret-config <FILE> <DOT_PATH>...` | Read PostgreSQL password from FILE at DOT_PATH *(global)* |
| `--ssh <SSH>` | Open an SSH transport to USER@HOST before connecting to PostgreSQL *(global)* |
| `--ssh-via <SSH_VIA>...` | SSH hop to reach before the final --ssh destination. Repeat for multiple hops *(global)* |
| `--ssh-option <SSH_OPTIONS>...` | Additional OpenSSH -o option. Repeat for multiple options *(global)* |
| `--ssh-local-host <SSH_LOCAL_HOST>` | Local bind host for the SSH tunnel *(global)* |
| `--ssh-local-port <SSH_LOCAL_PORT>` | Local bind port for the SSH tunnel. Defaults to an ephemeral port *(global)* |
| `--ssh-remote-socket <SSH_REMOTE_SOCKET>` | Explicit remote PostgreSQL Unix socket path for SSH forwarding *(global)* |
| `--ssh-sudo-user <SSH_SUDO_USER>` | Remote OS user for sudo -n Unix-socket bridge mode; requires an explicit socket *(global)* |
| `--container <CONTAINER>` | Run a container exec stdio bridge in TARGET before connecting to PostgreSQL *(global)* |
| `--container-driver <CONTAINER_DRIVER>` | Container exec driver: docker, podman, nerdctl, compose, or kubectl *(global)* |
| `--container-runtime <CONTAINER_RUNTIME>` | Runtime command for the selected container driver. Defaults to the driver command *(global)* |
| `--container-user <CONTAINER_USER>` | OS user passed to drivers that support exec user selection *(global)* |
| `--container-namespace <CONTAINER_NAMESPACE>` | Kubernetes namespace for kubectl exec *(global)* |
| `--container-context <CONTAINER_CONTEXT>` | Docker or Kubernetes context for the selected driver *(global)* |
| `--container-compose-file <CONTAINER_COMPOSE_FILES>...` | Compose file passed before compose exec. Repeat for multiple files *(global)* |
| `--container-compose-project <CONTAINER_COMPOSE_PROJECT>` | Compose project name passed before compose exec *(global)* |
| `--container-pod-container <CONTAINER_POD_CONTAINER>` | Kubernetes container name for multi-container pods *(global)* |
| `--output <OUTPUT>` | Output format: json (default), yaml, or plain *(global, default: `json`)* |
| `--output-to <OUTPUT_TO>` | Output routing: split (default), stdout, or stderr *(global, default: `split`)* |
| `--stdout-file <PATH>` | Redirect stdout bytes to this file *(global)* |
| `--stderr-file <PATH>` | Redirect stderr bytes to this file *(global)* |
| `--log <LOG>...` | Diagnostic log filters (comma-separated) *(global)* |
| `--mode <MODE>` | Runtime mode: canonical cli, pipe, or `psql` translation mode *(default: `cli`)* |
| `--version` | Print version |
| `--help` | Print help. Add --recursive to expand every nested subcommand; add --output plain\|json\|yaml\|markdown to choose the format. |

| Command | Summary |
| --- | --- |
| `afpsql psql` | Manage the local psql wrapper for afpsql --mode psql |
| `afpsql skill` | Manage Agent-First PSQL skills for Codex, Claude Code, opencode, and Hermes |
| `afpsql inspect` | Schema discovery: inspect databases, schemas, tables, indexes, or snapshots |

## afpsql psql - Manage the local psql wrapper for afpsql --mode psql

```text
afpsql psql <COMMAND>
```

| Command | Summary |
| --- | --- |
| `afpsql psql status` | Show whether the afpsql-managed psql wrapper is installed and active |
| `afpsql psql install` | Install an afpsql-managed psql wrapper |
| `afpsql psql uninstall` | Remove an afpsql-managed psql wrapper |

### afpsql psql status - Show whether the afpsql-managed psql wrapper is installed and active

```text
afpsql psql status [OPTIONS]
```

| Argument | Description |
| --- | --- |
| `--bin-dir <BIN_DIR>` | Directory that contains the psql wrapper. Defaults to the afpsql executable directory |

### afpsql psql install - Install an afpsql-managed psql wrapper

```text
afpsql psql install [OPTIONS]
```

| Argument | Description |
| --- | --- |
| `--bin-dir <BIN_DIR>` | Directory that contains the psql wrapper. Defaults to the afpsql executable directory |

### afpsql psql uninstall - Remove an afpsql-managed psql wrapper

```text
afpsql psql uninstall [OPTIONS]
```

| Argument | Description |
| --- | --- |
| `--bin-dir <BIN_DIR>` | Directory that contains the psql wrapper. Defaults to the afpsql executable directory |

## afpsql skill - Manage Agent-First PSQL skills for Codex, Claude Code, opencode, and Hermes

```text
afpsql skill <COMMAND>
```

| Command | Summary |
| --- | --- |
| `afpsql skill status` | Show whether the Agent-First PSQL skill is installed, valid, and up to date |
| `afpsql skill install` | Install the Agent-First PSQL skill |
| `afpsql skill uninstall` | Remove an afpsql-managed Agent-First PSQL skill |

### afpsql skill status - Show whether the Agent-First PSQL skill is installed, valid, and up to date

```text
afpsql skill status [OPTIONS]
```

| Argument | Description |
| --- | --- |
| `--agent <AGENT>` | Agent to manage. Defaults to all personal skill targets *(default: `all`)* |
| `--scope <SCOPE>` | Skill scope *(default: `personal`)* |
| `--skills-dir <SKILLS_DIR>` | Directory that contains skill folders. Requires an explicit single --agent |

### afpsql skill install - Install the Agent-First PSQL skill

```text
afpsql skill install [OPTIONS]
```

| Argument | Description |
| --- | --- |
| `--agent <AGENT>` | Agent to manage. Defaults to all personal skill targets *(default: `all`)* |
| `--scope <SCOPE>` | Skill scope *(default: `personal`)* |
| `--skills-dir <SKILLS_DIR>` | Directory that contains skill folders. Requires an explicit single --agent |
| `--force` | Overwrite or remove an unmanaged Agent-First PSQL skill at the target path |

### afpsql skill uninstall - Remove an afpsql-managed Agent-First PSQL skill

```text
afpsql skill uninstall [OPTIONS]
```

| Argument | Description |
| --- | --- |
| `--agent <AGENT>` | Agent to manage. Defaults to all personal skill targets *(default: `all`)* |
| `--scope <SCOPE>` | Skill scope *(default: `personal`)* |
| `--skills-dir <SKILLS_DIR>` | Directory that contains skill folders. Requires an explicit single --agent |
| `--force` | Overwrite or remove an unmanaged Agent-First PSQL skill at the target path |

## afpsql inspect - Schema discovery: inspect databases, schemas, tables, indexes, or snapshots

```text
afpsql inspect <COMMAND>
```

| Command | Summary |
| --- | --- |
| `afpsql inspect databases` | List databases on the connected server with size, encoding, and connection facts |
| `afpsql inspect database` | Summarize the connected database: schema/table/view/sequence counts and size |
| `afpsql inspect schemas` | List user-visible schemas |
| `afpsql inspect schema` | Export full schema metadata for one schema |
| `afpsql inspect snapshot` | Export a stable full-schema snapshot for machine consumption |
| `afpsql inspect tables` | List tables in a schema with owner, estimated rows, and size |
| `afpsql inspect views` | List views (regular and materialized) in a schema with owner |
| `afpsql inspect indexes` | List indexes with definitions, size, validity, and optional usage stats |
| `afpsql inspect table` | Describe a table's columns: types, nullability, defaults, primary key, comments |

### afpsql inspect databases - List databases on the connected server with size, encoding, and connection facts

```text
afpsql inspect databases [OPTIONS]
```

| Argument | Description |
| --- | --- |
| `--all` | Include template databases (template0/template1) in the listing |

### afpsql inspect database - Summarize the connected database: schema/table/view/sequence counts and size

```text
afpsql inspect database
```

### afpsql inspect schemas - List user-visible schemas

```text
afpsql inspect schemas
```

### afpsql inspect schema - Export full schema metadata for one schema

```text
afpsql inspect schema [OPTIONS]
```

| Argument | Description |
| --- | --- |
| `--schema <SCHEMA>` | Schema to inspect. Defaults to `public` *(default: `public`)* |
| `--like <LIKE>` | Optional `LIKE` pattern matched against relation names (use `%` as wildcard) |

### afpsql inspect snapshot - Export a stable full-schema snapshot for machine consumption

```text
afpsql inspect snapshot [OPTIONS]
```

| Argument | Description |
| --- | --- |
| `--schema <SCHEMA>` | Schema to inspect. Defaults to `public` *(default: `public`)* |
| `--like <LIKE>` | Optional `LIKE` pattern matched against relation names (use `%` as wildcard) |

### afpsql inspect tables - List tables in a schema with owner, estimated rows, and size

```text
afpsql inspect tables [OPTIONS]
```

| Argument | Description |
| --- | --- |
| `--schema <SCHEMA>` | Schema to filter on. Defaults to `public` *(default: `public`)* |
| `--like <LIKE>` | Optional `LIKE` pattern matched against the table name (use `%` as wildcard) |

### afpsql inspect views - List views (regular and materialized) in a schema with owner

```text
afpsql inspect views [OPTIONS]
```

| Argument | Description |
| --- | --- |
| `--schema <SCHEMA>` | Schema to filter on. Defaults to `public` *(default: `public`)* |
| `--like <LIKE>` | Optional `LIKE` pattern matched against the view name (use `%` as wildcard) |

### afpsql inspect indexes - List indexes with definitions, size, validity, and optional usage stats

```text
afpsql inspect indexes [OPTIONS]
```

| Argument | Description |
| --- | --- |
| `--schema <SCHEMA>` | Schema to filter on. Defaults to `public` *(default: `public`)* |
| `--table <TABLE>` | Optional table name to filter on. Accepts `schema.table` to override --schema |
| `--stats` | Include PostgreSQL's built-in pg_stat_user_indexes usage counters |

### afpsql inspect table - Describe a table's columns: types, nullability, defaults, primary key, comments

```text
afpsql inspect table [OPTIONS] <NAME>
```

| Argument | Description |
| --- | --- |
| `NAME` | Table name. Accepts `schema.table`; defaults to `public.NAME` when unqualified *(required)* |
| `--full` | Include relation, constraints, indexes, triggers, and sequence/default metadata |
