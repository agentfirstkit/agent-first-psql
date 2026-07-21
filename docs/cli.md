<!-- Generated. Do not edit by hand. Regenerate: afpsql --help --recursive --output markdown -->

# afpsql CLI Reference

# afpsql - A PostgreSQL interface for AI agents: reliable, structured, explicit, and read-only by default.

`afpsql` gives agents a reliable PostgreSQL contract: structured stdout
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

```text
Usage: afpsql [OPTIONS] [COMMAND]

Commands:
  psql     Manage the local psql wrapper for afpsql --mode psql
  skill    Manage Agent-First PSQL skills for Codex, Claude Code, opencode, and Hermes
  inspect  Schema discovery: inspect databases, schemas, tables, indexes, or snapshots
  help     Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help. Add --recursive to expand every nested subcommand; add --output json|yaml|markdown to render this help in another format.

  -V, --version
          Print version

Query:
      --sql <SQL>
          Inline SQL string to execute

      --sql-file <SQL_FILE>
          Read SQL from a file

      --param <PARAM>
          Positional bind parameter in `N=value` form. Repeat for additional parameters

      --stream-rows
          Stream large result sets as `result_rows` batches instead of a single inline result

      --batch-rows <BATCH_ROWS>
          Maximum rows per streamed batch

      --batch-bytes <BATCH_BYTES>
          Soft byte target per streamed batch

      --statement-timeout-ms <STATEMENT_TIMEOUT_MS>
          Per-query statement timeout in milliseconds

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Per-query lock timeout in milliseconds

      --inline-max-rows <INLINE_MAX_ROWS>
          Maximum inline rows before returning `result_too_large`

      --inline-max-bytes <INLINE_MAX_BYTES>
          Maximum inline payload bytes before returning `result_too_large`

      --permission <PERMISSION>
          Query permission. Defaults to read, ssh-read with --ssh, or container-read with --container

          [possible values: read, write, ssh-read, ssh-write, container-read, container-write]

      --dry-run
          Preview the query without executing it

      --explain
          Wrap the query in EXPLAIN (FORMAT JSON) and return the plan tree instead of executing the user's SQL

      --explain-analyze
          Wrap the query in EXPLAIN (ANALYZE, FORMAT JSON, BUFFERS). The underlying SQL actually runs; writes require the matching write permission

Connection:
      --dsn-secret <DSN_SECRET>
          PostgreSQL DSN URI. Redacted in structured output

      --dsn-secret-env <DSN_SECRET_ENV>
          Read PostgreSQL DSN URI from an environment variable

      --dsn-secret-config <FILE> <DOT_PATH>
          Read PostgreSQL DSN URI from FILE at DOT_PATH

      --conninfo-secret <CONNINFO_SECRET>
          libpq-style conninfo string. Redacted in structured output

      --conninfo-secret-env <CONNINFO_SECRET_ENV>
          Read libpq-style conninfo string from an environment variable

      --conninfo-secret-config <FILE> <DOT_PATH>
          Read libpq-style conninfo from FILE at DOT_PATH

      --host <HOST>
          PostgreSQL host

      --port <PORT>
          PostgreSQL port

      --user <USER>
          PostgreSQL user name

      --dbname <DBNAME>
          PostgreSQL database name

      --password-secret <PASSWORD_SECRET>
          PostgreSQL password. Redacted in structured output

      --password-secret-env <PASSWORD_SECRET_ENV>
          Read PostgreSQL password from an environment variable

      --password-secret-config <FILE> <DOT_PATH>
          Read PostgreSQL password from FILE at DOT_PATH

SSH Transport:
      --ssh <SSH>
          Open an SSH transport to USER@HOST before connecting to PostgreSQL

      --ssh-via <SSH_VIA>
          SSH hop to reach before the final --ssh destination. Repeat for multiple hops

      --ssh-option <SSH_OPTIONS>
          Additional OpenSSH -o option. Repeat for multiple options

      --ssh-local-host <SSH_LOCAL_HOST>
          Local bind host for the SSH tunnel

      --ssh-local-port <SSH_LOCAL_PORT>
          Local bind port for the SSH tunnel. Defaults to an ephemeral port

      --ssh-remote-socket <SSH_REMOTE_SOCKET>
          Explicit remote PostgreSQL Unix socket path for SSH forwarding

      --ssh-sudo-user <SSH_SUDO_USER>
          Remote OS user for sudo -n Unix-socket bridge mode; requires an explicit socket

Container Transport:
      --container <CONTAINER>
          Run a container exec stdio bridge in TARGET before connecting to PostgreSQL

      --container-driver <CONTAINER_DRIVER>
          Container exec driver: docker, podman, nerdctl, compose, or kubectl

      --container-runtime <CONTAINER_RUNTIME>
          Runtime command for the selected container driver. Defaults to the driver command

      --container-user <CONTAINER_USER>
          OS user passed to drivers that support exec user selection

      --container-namespace <CONTAINER_NAMESPACE>
          Kubernetes namespace for kubectl exec

      --container-context <CONTAINER_CONTEXT>
          Docker or Kubernetes context for the selected driver

      --container-compose-file <CONTAINER_COMPOSE_FILES>
          Compose file passed before compose exec. Repeat for multiple files

      --container-compose-project <CONTAINER_COMPOSE_PROJECT>
          Compose project name passed before compose exec

      --container-pod-container <CONTAINER_POD_CONTAINER>
          Kubernetes container name for multi-container pods

Runtime:
  -o, --output <OUTPUT>
          Output format: json (default), yaml, or plain

          [default: json]

      --stdout-file <PATH>
          Redirect stdout bytes to this file

      --stderr-file <PATH>
          Redirect stderr bytes to this file

      --log <LOG>
          Diagnostic log categories (comma-separated). Categories: startup, connect, query, transport, mode; or an exact event name like `query.error`; or `all` for everything

      --mode <MODE>
          Runtime mode: canonical cli, pipe, or `psql` translation mode

          [default: cli]
          [possible values: cli, pipe, psql]
```

## afpsql psql - Manage the local psql wrapper for afpsql --mode psql

```text
Usage: psql <COMMAND>

Commands:
  status     Show whether the afpsql-managed psql wrapper is installed and active
  install    Install an afpsql-managed psql wrapper
  uninstall  Remove an afpsql-managed psql wrapper
  help       Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help
```

### afpsql psql status - Show whether the afpsql-managed psql wrapper is installed and active

```text
Usage: status [OPTIONS]

Options:
      --bin-dir <BIN_DIR>
          Directory that contains the psql wrapper. Defaults to the afpsql executable directory

  -h, --help
          Print help
```

### afpsql psql install - Install an afpsql-managed psql wrapper

```text
Usage: install [OPTIONS]

Options:
      --bin-dir <BIN_DIR>
          Directory that contains the psql wrapper. Defaults to the afpsql executable directory

  -h, --help
          Print help
```

### afpsql psql uninstall - Remove an afpsql-managed psql wrapper

```text
Usage: uninstall [OPTIONS]

Options:
      --bin-dir <BIN_DIR>
          Directory that contains the psql wrapper. Defaults to the afpsql executable directory

  -h, --help
          Print help
```

## afpsql skill - Manage Agent-First PSQL skills for Codex, Claude Code, opencode, and Hermes

```text
Usage: skill <COMMAND>

Commands:
  status     Show whether the Agent-First PSQL skill is installed, valid, and up to date
  install    Install the Agent-First PSQL skill
  uninstall  Remove an afpsql-managed Agent-First PSQL skill
  help       Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help
```

### afpsql skill status - Show whether the Agent-First PSQL skill is installed, valid, and up to date

```text
Usage: status [OPTIONS]

Options:
      --agent <AGENT>
          Agent to manage. Defaults to all personal skill targets

          Possible values:
          - all:         Manage every agent that supports the requested scope
          - codex:       Manage the Codex local skill under $CODEX_HOME/skills
          - claude-code: Manage the Claude Code skill under ~/.claude/skills or .claude/skills
          - opencode:    Manage the opencode skill under ~/.config/opencode/skills or .opencode/skills
          - hermes:      Manage the Hermes skill under $HERMES_HOME/skills or ~/.hermes/skills

          [default: all]

      --scope <SCOPE>
          Skill scope

          Possible values:
          - personal:  Install under the user-level skills directory
          - workspace: Install under the current workspace's skills directory

          [default: personal]

      --skills-dir <SKILLS_DIR>
          Directory that contains skill folders. Requires an explicit single --agent

  -h, --help
          Print help (see a summary with '-h')
```

### afpsql skill install - Install the Agent-First PSQL skill

```text
Usage: install [OPTIONS]

Options:
      --agent <AGENT>
          Agent to manage. Defaults to all personal skill targets

          Possible values:
          - all:         Manage every agent that supports the requested scope
          - codex:       Manage the Codex local skill under $CODEX_HOME/skills
          - claude-code: Manage the Claude Code skill under ~/.claude/skills or .claude/skills
          - opencode:    Manage the opencode skill under ~/.config/opencode/skills or .opencode/skills
          - hermes:      Manage the Hermes skill under $HERMES_HOME/skills or ~/.hermes/skills

          [default: all]

      --scope <SCOPE>
          Skill scope

          Possible values:
          - personal:  Install under the user-level skills directory
          - workspace: Install under the current workspace's skills directory

          [default: personal]

      --skills-dir <SKILLS_DIR>
          Directory that contains skill folders. Requires an explicit single --agent

      --force
          Overwrite or remove an unmanaged Agent-First PSQL skill at the target path

  -h, --help
          Print help (see a summary with '-h')
```

### afpsql skill uninstall - Remove an afpsql-managed Agent-First PSQL skill

```text
Usage: uninstall [OPTIONS]

Options:
      --agent <AGENT>
          Agent to manage. Defaults to all personal skill targets

          Possible values:
          - all:         Manage every agent that supports the requested scope
          - codex:       Manage the Codex local skill under $CODEX_HOME/skills
          - claude-code: Manage the Claude Code skill under ~/.claude/skills or .claude/skills
          - opencode:    Manage the opencode skill under ~/.config/opencode/skills or .opencode/skills
          - hermes:      Manage the Hermes skill under $HERMES_HOME/skills or ~/.hermes/skills

          [default: all]

      --scope <SCOPE>
          Skill scope

          Possible values:
          - personal:  Install under the user-level skills directory
          - workspace: Install under the current workspace's skills directory

          [default: personal]

      --skills-dir <SKILLS_DIR>
          Directory that contains skill folders. Requires an explicit single --agent

      --force
          Overwrite or remove an unmanaged Agent-First PSQL skill at the target path

  -h, --help
          Print help (see a summary with '-h')
```

## afpsql inspect - Schema discovery: inspect databases, schemas, tables, indexes, or snapshots

```text
Usage: inspect <COMMAND>

Commands:
  databases  List databases on the connected server with size, encoding, and connection facts
  database   Summarize the connected database: schema/table/view/sequence counts and size
  schemas    List user-visible schemas
  schema     Export full schema metadata for one schema
  snapshot   Export a stable full-schema snapshot for machine consumption
  tables     List tables in a schema with owner, estimated rows, and size
  views      List views (regular and materialized) in a schema with owner
  indexes    List indexes with definitions, size, validity, and optional usage stats
  table      Describe a table's columns: types, nullability, defaults, primary key, comments
  help       Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help
```

### afpsql inspect databases - List databases on the connected server with size, encoding, and connection facts

```text
Usage: databases [OPTIONS]

Options:
      --all
          Include template databases (template0/template1) in the listing

  -h, --help
          Print help
```

### afpsql inspect database - Summarize the connected database: schema/table/view/sequence counts and size

```text
Usage: database

Options:
  -h, --help
          Print help
```

### afpsql inspect schemas - List user-visible schemas

```text
Usage: schemas

Options:
  -h, --help
          Print help
```

### afpsql inspect schema - Export full schema metadata for one schema

```text
Usage: schema [OPTIONS]

Options:
      --schema <SCHEMA>
          Schema to inspect. Defaults to `public`

          [default: public]

      --like <LIKE>
          Optional `LIKE` pattern matched against relation names (use `%` as wildcard)

  -h, --help
          Print help
```

### afpsql inspect snapshot - Export a stable full-schema snapshot for machine consumption

```text
Usage: snapshot [OPTIONS]

Options:
      --schema <SCHEMA>
          Schema to inspect. Defaults to `public`

          [default: public]

      --like <LIKE>
          Optional `LIKE` pattern matched against relation names (use `%` as wildcard)

  -h, --help
          Print help
```

### afpsql inspect tables - List tables in a schema with owner, estimated rows, and size

```text
Usage: tables [OPTIONS]

Options:
      --schema <SCHEMA>
          Schema to filter on. Defaults to `public`

          [default: public]

      --like <LIKE>
          Optional `LIKE` pattern matched against the table name (use `%` as wildcard)

  -h, --help
          Print help
```

### afpsql inspect views - List views (regular and materialized) in a schema with owner

```text
Usage: views [OPTIONS]

Options:
      --schema <SCHEMA>
          Schema to filter on. Defaults to `public`

          [default: public]

      --like <LIKE>
          Optional `LIKE` pattern matched against the view name (use `%` as wildcard)

  -h, --help
          Print help
```

### afpsql inspect indexes - List indexes with definitions, size, validity, and optional usage stats

```text
Usage: indexes [OPTIONS]

Options:
      --schema <SCHEMA>
          Schema to filter on. Defaults to `public`

          [default: public]

      --table <TABLE>
          Optional table name to filter on. Accepts `schema.table` to override --schema

      --stats
          Include PostgreSQL's built-in pg_stat_user_indexes usage counters

  -h, --help
          Print help
```

### afpsql inspect table - Describe a table's columns: types, nullability, defaults, primary key, comments

```text
Usage: table [OPTIONS] <NAME>

Arguments:
  <NAME>
          Table name. Accepts `schema.table`; defaults to `public.NAME` when unqualified

Options:
      --full
          Include relation, constraints, indexes, triggers, and sequence/default metadata

  -h, --help
          Print help
```
AFDATA: 0.22.0
