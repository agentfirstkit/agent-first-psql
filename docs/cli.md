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

## `afpsql`

Agent-First PostgreSQL client.

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
afpsql psql status
afpsql psql install
```

### Exit Codes

- `0`: query completed successfully
- `1`: SQL error or runtime error
- `2`: invalid CLI arguments

**Usage:** `afpsql [OPTIONS] [COMMAND]`

###### **Subcommands:**

* `psql` — Manage the local psql wrapper for afpsql --mode psql

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
* `--read-only` — Force the query to run in a read-only transaction
* `--dry-run` — Preview the query without executing it
* `--dsn-secret <DSN_SECRET>` — PostgreSQL DSN URI. Redacted in structured output
* `--dsn-secret-env <DSN_SECRET_ENV>` — Read PostgreSQL DSN URI from an environment variable
* `--conninfo-secret <CONNINFO_SECRET>` — libpq-style conninfo string. Redacted in structured output
* `--host <HOST>` — PostgreSQL host
* `--port <PORT>` — PostgreSQL port
* `--user <USER>` — PostgreSQL user name
* `--dbname <DBNAME>` — PostgreSQL database name
* `--password-secret <PASSWORD_SECRET>` — PostgreSQL password. Redacted in structured output
* `--password-secret-env <PASSWORD_SECRET_ENV>` — Read PostgreSQL password from an environment variable
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
