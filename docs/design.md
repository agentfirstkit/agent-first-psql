# Agent-First PSQL — Design

## Problem

AI agents call SQL through shell tooling, but classic terminal-first clients are not
ideal for automated workflows:

1. Output is human-formatted, not protocol-stable.
2. Process-per-query overhead wastes latency and connection setup.
3. Large `SELECT *` workloads need predictable streaming.
4. Text interpolation patterns are easy to misuse and unsafe by default.

`afpsql` is an agent runtime for PostgreSQL: structured protocol, persistent
connections, safe parameter binding, and AFD naming everywhere.

## Product Boundary

`afpsql` has one runtime interface (AFD protocol). `psql mode` is only a CLI
argument translation layer.

Non-goals:

- no runtime protocol fork for `psql mode`
- no table/text output compatibility
- no `psql` meta-command compatibility (`\\d`, `\\x`, `\\timing`, ...)
- no text-template variable substitution semantics

Architecture is two CLI frontends -> one canonical AFD execution core.

Execution layering:

- `handler` is protocol orchestration only
- `DbExecutor` is the DB adapter boundary
- default adapter uses `tokio-postgres` + `deadpool-postgres`

## Core Principles

1. SQL-native protocol events.
2. AFD naming conventions for fields and flags.
3. Parameter binding for dynamic values.
4. Structured errors with machine-readable codes.
5. Large-result streaming as first-class behavior.
6. `psql mode` performs argument translation only.
7. production SQL execution path has no test failpoint semantics.
8. No SQL-text heuristics for runtime behavior.
9. Protocol events use stdout only; stderr is not a protocol channel.

## Protocol Shape

Input commands:

- `query`
- `cancel`
- `config`
- `ping`
- `close`

Output events:

- `result`
- `result_start`
- `result_rows`
- `result_end`
- `sql_error`
- `error`
- `notice`
- `config`
- `pong`
- `close`
- `log`

## Parameter Binding (Required for Dynamic Values)

When values are dynamic, clients should use `$N` placeholders and `params`.

```json
{"code":"query","id":"q1","sql":"select * from users where id = $1","params":[123]}
```

Validation rules:

1. Placeholder count must match `params` length (validated from prepared-statement
   metadata, not by scanning SQL text).
2. Invalid shape returns `error_code: "invalid_params"`.
3. Server-type conversion failures return `error_code: "invalid_params"`.

Unsupported by design:

- `:name`-style interpolation
- raw text expansion in SQL templates

### CLI Binding Forms

AFD CLI uses one canonical binding form:

- `--param 1=value --param 2=value` (repeatable)

CLI parsing translates this form into canonical protocol `params` array.

`psql mode` translation may accept numeric `-v` bindings:

```bash
afpsql --mode psql -c "select * from t where id = $1" -v 1=42
```

Translation rule: only numeric variable names are allowed; non-numeric names
are rejected because interpolation is unsupported.

## Modes

### CLI mode

Single query execution and structured output.

Two parsers are available:

1. AFD parser (default)
2. `psql` parser (`--mode psql`) -> translated into AFD request

### Pipe mode (`--mode pipe`)

Long-lived JSONL session on stdin/stdout:

- persistent process
- reusable DB sessions/pools
- concurrent in-flight queries
- id-based correlation

### MCP mode (`--mode mcp`)

Exposes structured SQL tools to MCP clients.

## Connection Model (AFD)

Connection may be supplied by:

1. `dsn_secret`
2. `conninfo_secret`
3. discrete fields: `host`, `port`, `user`, `dbname`, `password_secret`

Optional environment fallback uses AFD names:

- `AFPSQL_DSN_SECRET`
- `AFPSQL_CONNINFO_SECRET`
- `AFPSQL_HOST`
- `AFPSQL_PORT`
- `AFPSQL_USER`
- `AFPSQL_DBNAME`
- `AFPSQL_PASSWORD_SECRET`

Resolution precedence:

1. request/session explicit fields
2. translated CLI flags (AFD or `psql mode`)
3. environment fallback
4. built-in defaults

## Large Result Strategy

### Runtime decision rule

Row/command behavior is decided from PostgreSQL statement metadata after prepare:

- statement has result columns -> row result path (`result` or `result_*`)
- statement has no result columns -> command path

`afpsql` must not parse SQL text (keyword scanning, placeholder text scans, etc.)
to decide execution/output behavior.

Enforcement:

- Clippy `disallowed_methods` is enabled at crate level.
- Clippy `disallowed_macros` is enabled at crate level.
- `clippy.toml` bans:
  - `str::split_whitespace` (prevent SQL keyword scanning in runtime decisions)
  - `std::eprintln` (prevent protocol diagnostics from leaking to stderr)

### Inline path

Return one `result` if payload is below both limits:

- `inline_max_rows`
- `inline_max_bytes`

### Streaming path

When `stream_rows=true`:

1. emit `result_start` with column metadata
2. emit repeated `result_rows` batches
3. emit `result_end` with totals in `trace`

Batch controls:

- `batch_rows`
- `batch_bytes`

If streaming is off and limits are exceeded, return:

- `error_code: "result_too_large"`

## Error Taxonomy

### `sql_error`

PostgreSQL execution failure. Include `sqlstate` and SQL diagnostics.

### `error`

Client/protocol/runtime failures. Always include:

- `error_code`
- `error`
- `retryable`
- `trace`

Runtime diagnostics:

- each query can emit structured `code: "log"` diagnostics to stdout when enabled:
  - `query.result`
  - `query.error`
  - `query.sql_error`

## AFD Rules

- AFD field suffix semantics (`duration_ms`, `payload_bytes`, `_secret`, ...)
- long-form self-describing CLI flags
- CLI/output dispatch via `agent_first_data::cli_parse_output` + `cli_output`
- CLI parse errors via `agent_first_data::build_cli_error` (structured `code:"error"`)
- redaction on `_secret` fields in config/log output

## Exit Codes (CLI)

- `0`: success (`result` or `result_*`)
- `1`: `sql_error` or `error`
- `2`: invalid CLI arguments

## MVP Scope

1. `query/config/cancel/ping/close`
2. `result` + `result_*` streaming
3. connection resolution model above
4. parameter binding (`params`)
5. MCP tools (`psql_query`, `psql_config`)

Future:

- prepared statement caching
- transaction workflow commands (`begin`/`commit`/`rollback`)
- `COPY` streaming
- `LISTEN/NOTIFY` bridge
