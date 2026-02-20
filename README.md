# Agent-First PSQL

Persistent PostgreSQL client for AI agents — SQL-native JSONL in, JSONL out.

Supported platforms: macOS, Linux, Windows.

`psql` is optimized for humans in a terminal. Agents need stable machine-readable
structures, connection reuse, and streaming for large result sets.

`afpsql` is a long-lived process with SQL-native protocol events:

- `query` input command
- `result` (small result) output event
- `result_start` / `result_rows` / `result_end` (streaming)
- `sql_error` (database error with SQLSTATE)
- `error` (transport/client/protocol error)

## Modes

- CLI mode: one SQL, one structured result, exit
- Pipe mode: JSONL stdin/stdout session with connection reuse and concurrent query handling
- MCP mode: tool interface for AI assistants

## Contract

`afpsql` has one Agent-First Data runtime protocol and two CLI entry styles:

1. agent-first CLI mode (default)
2. `psql mode` (CLI translation layer only)

`psql mode` scope:

- translates legacy-style CLI arguments into canonical agent-first request/config fields
- does not change runtime protocol or output format
- runtime protocol output goes to `stdout` only (JSON/YAML/Plain rendering of the
  same structured events)
- `stderr` is not a protocol channel

Out of scope in `psql mode`:

- `psql` table/text output compatibility
- `psql` meta-command compatibility (`\d`, `\x`, `\timing`, ...)
- client-side SQL text interpolation

## Secure Parameters (Default)

`JSON` transport does not imply SQL safety. `afpsql` uses positional parameter binding:

```json
{"code":"query","id":"q1","sql":"select * from users where id = $1 and status = $2","params":[123,"active"]}
```

Canonical protocol shape:

- SQL with `$1..$N`
- `params` JSON array

CLI binding syntax:

- `--param 1=... --param 2=...` (single canonical CLI form)

In `psql mode`, translation may also accept numeric `-v` entries and map them to
agent-first `params` by position.

Not supported:

- string interpolation modes like `:name`
- textual SQL template expansion

## Connection Inputs

Canonical agent-first connection fields:

- `dsn_secret` (PostgreSQL URI)
- `conninfo_secret` (key/value conninfo)
- discrete fields (`host`, `port`, `user`, `dbname`, `password_secret`)

`psql mode` accepts legacy CLI connection flags and translates them to the same
agent-first fields.

Environment fallback (lowest precedence) also reads standard PostgreSQL variables:

- `PGHOST`
- `PGPORT`
- `PGUSER`
- `PGDATABASE`

Unix socket example using system `PGHOST` and only passing dbname in the command:

```bash
export PGHOST=/var/run/postgresql
export PGUSER=roger
afpsql --dbname appdb --sql "select current_database()"
```

See docs for details:

- [CLI Manual](docs/cli.md)
- [Protocol Reference](docs/reference.md)
- [Design](docs/design.md)
- [MCP Reference](docs/mcp.md)

## License

MIT
