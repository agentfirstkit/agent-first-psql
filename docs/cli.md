<!-- Generated. Do not edit by hand. -->

# afpsql CLI Reference

> Regenerate with `afpsql --help-markdown`.

# Command-Line Help for `afpsql`

This document contains the help content for the `afpsql` command-line program.

**Command Overview:**

* [`afpsql`↴](#afpsql)
* [`afpsql psql`↴](#afpsql-psql)
* [`afpsql psql status`↴](#afpsql-psql-status)
* [`afpsql psql install`↴](#afpsql-psql-install)
* [`afpsql psql uninstall`↴](#afpsql-psql-uninstall)
* [`afpsql skill`↴](#afpsql-skill)
* [`afpsql skill status`↴](#afpsql-skill-status)
* [`afpsql skill install`↴](#afpsql-skill-install)
* [`afpsql skill uninstall`↴](#afpsql-skill-uninstall)
* [`afpsql inspect`↴](#afpsql-inspect)
* [`afpsql inspect databases`↴](#afpsql-inspect-databases)
* [`afpsql inspect schemas`↴](#afpsql-inspect-schemas)
* [`afpsql inspect tables`↴](#afpsql-inspect-tables)
* [`afpsql inspect views`↴](#afpsql-inspect-views)
* [`afpsql inspect table`↴](#afpsql-inspect-table)

## `afpsql`

Agent-First PostgreSQL client.

`afpsql` gives agents a reliable PostgreSQL contract: structured stdout
events, first-class SSH/container transports, explicit write permissions,
stable pipe sessions, and machine-readable failures.

### Interface Policy

- default mode is canonical agent-first CLI
- `--mode psql` is argument translation only; runtime output stays JSONL
- stdout carries protocol events; stderr is not a protocol channel
- native CLI and pipe mode default to read-only transactions; writes require permission
- SSH/container transports keep afpsql local instead of running human `psql` across boundaries

### Query Sources and Parameters

- use `--sql` for inline SQL or `--sql-file` for a file
- use repeatable `--param N=value` for positional binds
- placeholder count is validated from prepared-statement metadata, not by SQL text scanning

### Connection Sources

- `--dsn-secret` for a PostgreSQL URI
- `--conninfo-secret` for libpq-style conninfo
- or discrete `--host`, `--port`, `--user`, `--dbname`, `--password-secret`
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
afpsql --sql "select * from users where id = $1" --param 1=123
afpsql --dsn-secret-env DATABASE_URL --sql "select 1"
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

**Usage:** `afpsql [OPTIONS] [COMMAND]`

###### **Subcommands:**

* `psql` — Manage the local psql wrapper for afpsql --mode psql
* `skill` — Manage Agent-First PSQL skills for Codex and Claude Code
* `inspect` — Schema discovery: list databases, schemas, tables, views, or describe a table

###### **Options:**

* `--sql <SQL>` — Inline SQL string to execute
* `--sql-file <SQL_FILE>` — Read SQL from a file
* `--param <PARAM>` — Positional bind parameter in `N=value` form. Repeat for additional parameters
* `--stream-rows` — Stream large result sets as `result_rows` batches instead of a single inline result
* `--batch-rows <BATCH_ROWS>` — Maximum rows per streamed batch
* `--batch-bytes <BATCH_BYTES>` — Soft byte target per streamed batch
* `--statement-timeout-ms <STATEMENT_TIMEOUT_MS>` — Per-query statement timeout in milliseconds
* `--lock-timeout-ms <LOCK_TIMEOUT_MS>` — Per-query lock timeout in milliseconds
* `--inline-max-rows <INLINE_MAX_ROWS>` — Maximum inline rows before returning `result_too_large`
* `--inline-max-bytes <INLINE_MAX_BYTES>` — Maximum inline payload bytes before returning `result_too_large`
* `--permission <PERMISSION>` — Query permission: read, write, ssh-read, ssh-write, container-read, or container-write. Defaults to read, ssh-read with --ssh, or container-read with --container
* `--dry-run` — Preview the query without executing it
* `--explain` — Wrap the query in EXPLAIN (FORMAT JSON) and return the plan tree instead of executing the user's SQL
* `--explain-analyze` — Wrap the query in EXPLAIN (ANALYZE, FORMAT JSON, BUFFERS). The underlying SQL actually runs; writes require the matching write permission
* `--dsn-secret <DSN_SECRET>` — PostgreSQL DSN URI. Redacted in structured output
* `--dsn-secret-env <DSN_SECRET_ENV>` — Read PostgreSQL DSN URI from an environment variable
* `--conninfo-secret <CONNINFO_SECRET>` — libpq-style conninfo string. Redacted in structured output
* `--host <HOST>` — PostgreSQL host
* `--port <PORT>` — PostgreSQL port
* `--user <USER>` — PostgreSQL user name
* `--dbname <DBNAME>` — PostgreSQL database name
* `--password-secret <PASSWORD_SECRET>` — PostgreSQL password. Redacted in structured output
* `--password-secret-env <PASSWORD_SECRET_ENV>` — Read PostgreSQL password from an environment variable
* `--ssh <SSH>` — Open an SSH transport to USER@HOST before connecting to PostgreSQL
* `--ssh-option <SSH_OPTIONS>` — Additional OpenSSH -o option. Repeat for multiple options
* `--ssh-local-host <SSH_LOCAL_HOST>` — Local bind host for the SSH tunnel
* `--ssh-local-port <SSH_LOCAL_PORT>` — Local bind port for the SSH tunnel. Defaults to an ephemeral port
* `--ssh-remote-socket <SSH_REMOTE_SOCKET>` — Explicit remote PostgreSQL Unix socket path for SSH forwarding
* `--ssh-sudo-user <SSH_SUDO_USER>` — Remote OS user for sudo -n Unix-socket bridge mode; requires an explicit socket
* `--container <CONTAINER>` — Run a container exec stdio bridge in TARGET before connecting to PostgreSQL
* `--container-driver <CONTAINER_DRIVER>` — Container exec driver: docker, podman, nerdctl, compose, or kubectl
* `--container-runtime <CONTAINER_RUNTIME>` — Runtime command for the selected container driver. Defaults to the driver command
* `--container-user <CONTAINER_USER>` — OS user passed to drivers that support exec user selection
* `--container-namespace <CONTAINER_NAMESPACE>` — Kubernetes namespace for kubectl exec
* `--container-context <CONTAINER_CONTEXT>` — Docker or Kubernetes context for the selected driver
* `--container-compose-file <CONTAINER_COMPOSE_FILES>` — Compose file passed before compose exec. Repeat for multiple files
* `--container-compose-project <CONTAINER_COMPOSE_PROJECT>` — Compose project name passed before compose exec
* `--container-pod-container <CONTAINER_POD_CONTAINER>` — Kubernetes container name for multi-container pods
* `--output <OUTPUT>` — Output format: json (default), yaml, or plain

  Default value: `json`
* `--log <LOG>` — Diagnostic log categories
* `--mode <MODE>` — Runtime mode: canonical cli, pipe, or `psql` translation mode

  Default value: `cli`

  Possible values: `cli`, `pipe`, `psql`




## `afpsql psql`

Manage the local psql wrapper for afpsql --mode psql

**Usage:** `afpsql psql <COMMAND>`

###### **Subcommands:**

* `status` — Show whether the afpsql-managed psql wrapper is installed and active
* `install` — Install an afpsql-managed psql wrapper
* `uninstall` — Remove an afpsql-managed psql wrapper



## `afpsql psql status`

Show whether the afpsql-managed psql wrapper is installed and active

**Usage:** `afpsql psql status [OPTIONS]`

###### **Options:**

* `--bin-dir <BIN_DIR>` — Directory that contains the psql wrapper. Defaults to the afpsql executable directory



## `afpsql psql install`

Install an afpsql-managed psql wrapper

**Usage:** `afpsql psql install [OPTIONS]`

###### **Options:**

* `--bin-dir <BIN_DIR>` — Directory that contains the psql wrapper. Defaults to the afpsql executable directory



## `afpsql psql uninstall`

Remove an afpsql-managed psql wrapper

**Usage:** `afpsql psql uninstall [OPTIONS]`

###### **Options:**

* `--bin-dir <BIN_DIR>` — Directory that contains the psql wrapper. Defaults to the afpsql executable directory



## `afpsql skill`

Manage Agent-First PSQL skills for Codex and Claude Code

**Usage:** `afpsql skill <COMMAND>`

###### **Subcommands:**

* `status` — Show whether the Agent-First PSQL skill is installed and valid
* `install` — Install the Agent-First PSQL skill
* `uninstall` — Remove an afpsql-managed Agent-First PSQL skill



## `afpsql skill status`

Show whether the Agent-First PSQL skill is installed and valid

**Usage:** `afpsql skill status [OPTIONS]`

###### **Options:**

* `--agent <AGENT>` — Agent to manage. Defaults to all personal skill targets

  Default value: `all`

  Possible values:
  - `all`:
    Manage both Codex and Claude Code personal skills
  - `codex`:
    Manage the Codex local skill under $CODEX_HOME/skills
  - `claude-code`:
    Manage the Claude Code skill under ~/.claude/skills or .claude/skills

* `--scope <SCOPE>` — Skill scope. Project scope is supported for Claude Code only

  Default value: `personal`

  Possible values:
  - `personal`:
    Install under the user-level skills directory
  - `project`:
    Install under the current project's skills directory

* `--skills-dir <SKILLS_DIR>` — Directory that contains skill folders. Requires an explicit single --agent



## `afpsql skill install`

Install the Agent-First PSQL skill

**Usage:** `afpsql skill install [OPTIONS]`

###### **Options:**

* `--agent <AGENT>` — Agent to manage. Defaults to all personal skill targets

  Default value: `all`

  Possible values:
  - `all`:
    Manage both Codex and Claude Code personal skills
  - `codex`:
    Manage the Codex local skill under $CODEX_HOME/skills
  - `claude-code`:
    Manage the Claude Code skill under ~/.claude/skills or .claude/skills

* `--scope <SCOPE>` — Skill scope. Project scope is supported for Claude Code only

  Default value: `personal`

  Possible values:
  - `personal`:
    Install under the user-level skills directory
  - `project`:
    Install under the current project's skills directory

* `--skills-dir <SKILLS_DIR>` — Directory that contains skill folders. Requires an explicit single --agent
* `--force` — Overwrite or remove an unmanaged Agent-First PSQL skill at the target path



## `afpsql skill uninstall`

Remove an afpsql-managed Agent-First PSQL skill

**Usage:** `afpsql skill uninstall [OPTIONS]`

###### **Options:**

* `--agent <AGENT>` — Agent to manage. Defaults to all personal skill targets

  Default value: `all`

  Possible values:
  - `all`:
    Manage both Codex and Claude Code personal skills
  - `codex`:
    Manage the Codex local skill under $CODEX_HOME/skills
  - `claude-code`:
    Manage the Claude Code skill under ~/.claude/skills or .claude/skills

* `--scope <SCOPE>` — Skill scope. Project scope is supported for Claude Code only

  Default value: `personal`

  Possible values:
  - `personal`:
    Install under the user-level skills directory
  - `project`:
    Install under the current project's skills directory

* `--skills-dir <SKILLS_DIR>` — Directory that contains skill folders. Requires an explicit single --agent
* `--force` — Overwrite or remove an unmanaged Agent-First PSQL skill at the target path



## `afpsql inspect`

Schema discovery: list databases, schemas, tables, views, or describe a table

**Usage:** `afpsql inspect <COMMAND>`

###### **Subcommands:**

* `databases` — List non-template databases on the connected server
* `schemas` — List user-visible schemas
* `tables` — List tables (and partitioned tables) in a schema, optionally filtered
* `views` — List views in a schema, optionally filtered
* `table` — Describe a single table's columns, types, nullability, and defaults



## `afpsql inspect databases`

List non-template databases on the connected server

**Usage:** `afpsql inspect databases`



## `afpsql inspect schemas`

List user-visible schemas

**Usage:** `afpsql inspect schemas`



## `afpsql inspect tables`

List tables (and partitioned tables) in a schema, optionally filtered

**Usage:** `afpsql inspect tables [OPTIONS]`

###### **Options:**

* `--schema <SCHEMA>` — Schema to filter on. Defaults to `public`

  Default value: `public`
* `--like <LIKE>` — Optional `LIKE` pattern matched against `table_name` (use `%` as wildcard)



## `afpsql inspect views`

List views in a schema, optionally filtered

**Usage:** `afpsql inspect views [OPTIONS]`

###### **Options:**

* `--schema <SCHEMA>` — Schema to filter on. Defaults to `public`

  Default value: `public`
* `--like <LIKE>` — Optional `LIKE` pattern matched against `table_name` (use `%` as wildcard)



## `afpsql inspect table`

Describe a single table's columns, types, nullability, and defaults

**Usage:** `afpsql inspect table <NAME>`

###### **Arguments:**

* `<NAME>` — Table name. Accepts `schema.table`; defaults to `public.NAME` when unqualified
