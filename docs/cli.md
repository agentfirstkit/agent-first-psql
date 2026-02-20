# Agent-First PSQL — CLI Manual

Practical usage for `afpsql`.

## Interface Policy

`afpsql` has two CLI entry modes with one execution core:

1. AFD mode (default)
2. `psql mode` (`--mode psql`) as argument translation only

Both modes execute through the same AFD runtime protocol and produce the same
structured output events.

`afpsql` CLI parsing/output follows shared AFD helpers from `agent-first-data`
(`cli_parse_output`, `cli_parse_log_filters`, `cli_output`, `build_cli_error`).

Protocol stream contract:

- `stdout` carries all structured protocol events (`result` / `error` / `log` / ...)
- `stderr` is not a protocol channel and should not be parsed by agents

## Run One Query

```bash
afpsql --sql "select now() as now_rfc3339"
```

Default output is structured JSON:

```json
{"code":"result","command_tag":"ROWS 1","columns":[{"name":"now_rfc3339","type":"timestamptz"}],"rows":[{"now_rfc3339":"2026-02-19T12:34:56Z"}],"row_count":1,"trace":{"duration_ms":3}}
```

## Query Sources

SQL string:

```bash
afpsql --sql "select * from users limit 10"
```

SQL file:

```bash
afpsql --sql-file ./query.sql
```

## Safe Parameters

Use placeholders with positional param flags:

```bash
afpsql \
  --sql "select * from users where id = $1 and status = $2" \
  --param 1=123 \
  --param 2=active
```

Rules:

- placeholder count must match parameter count (validated against prepared-statement metadata, not SQL text scanning)
- malformed params return `invalid_params`
- values are type-checked against server-prepared parameter OIDs
- text-template substitution is not supported

Single canonical form: `--param N=VALUE` (repeatable).

## Connection Flags (AFD)

URI DSN:

```bash
afpsql --dsn-secret "postgresql://app:secret@127.0.0.1:5432/appdb?sslmode=prefer" --sql "select 1"
```

Conninfo:

```bash
afpsql --conninfo-secret "host=127.0.0.1 port=5432 dbname=appdb user=app sslmode=prefer" --sql "select 1"
```

Discrete fields:

```bash
afpsql \
  --host 127.0.0.1 \
  --port 5432 \
  --user app \
  --dbname appdb \
  --password-secret 'secret' \
  --sql "select 1"
```

Optional environment fallback (AFD names):

- `AFPSQL_DSN_SECRET`
- `AFPSQL_CONNINFO_SECRET`
- `AFPSQL_HOST`
- `AFPSQL_PORT`
- `AFPSQL_USER`
- `AFPSQL_DBNAME`
- `AFPSQL_PASSWORD_SECRET`

## `psql` Mode (Translation Only)

Enable with `--mode psql`.

Purpose:

- parse legacy-style CLI connection/query arguments
- translate to canonical AFD request/config fields

Still not supported:

- table/text output compatibility
- meta-commands
- text interpolation

Supported translated inputs:

- query: `-c`, `-f`
- connection: `-h`, `-p`, `-U`, `-d`, DSN/conninfo equivalents
- numeric `-v` bindings -> `params` positions

Example:

```bash
afpsql --mode psql -h 127.0.0.1 -p 5432 -U app -d appdb \
  -c "select * from users where id = $1 and status = $2" \
  -v 1=123 -v 2=active
```

Compatibility boundary:

- compatible: accepted CLI flags and positional bind translation
- incompatible by design: output format, `psql` meta-commands, interpolation semantics

## Large Result Sets

For large `select *`, enable streaming:

```bash
afpsql --sql "select * from big_table" --stream-rows --batch-rows 1000
```

Output sequence:

1. `result_start`
2. repeated `result_rows`
3. `result_end`

If streaming is disabled and result exceeds inline limits, `afpsql` returns:

```json
{"code":"error","error_code":"result_too_large","retryable":false,...}
```

## Pipe Mode

Long-lived JSONL session:

```bash
afpsql --mode pipe <<'EOF'
{"code":"query","id":"q1","sql":"select 1 as n"}
{"code":"query","id":"q2","sql":"select * from big_table where id > $1","params":[100],"options":{"stream_rows":true,"batch_rows":1000}}
{"code":"close"}
EOF
```

## Output Formats

```bash
afpsql --sql "select 1 as n" --output json
afpsql --sql "select 1 as n" --output yaml
afpsql --sql "select 1 as n" --output plain
```

## Diagnostic Log Events

Structured diagnostics are optional and disabled by default.

Enable by category:

```bash
afpsql --dsn-secret "$DATABASE_URL" --log query.result --sql "select 1 as n"
```

Enable multiple categories:

```bash
afpsql --mode pipe --log query.result,query.error
```

Category matching:

- exact event (`query.result`)
- group prefix (`query` matches `query.result`, `query.error`, `query.sql_error`)
- wildcard (`all` or `*`)

## Exit Codes

| Code | Meaning |
|---|---|
| `0` | Query completed (`result` or `result_*`) |
| `1` | `sql_error` or runtime `error` |
| `2` | Invalid CLI arguments |
