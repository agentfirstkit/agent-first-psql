# Agent-First PSQL — Protocol Reference

Every stdin/stdout line is one JSON object with required `code`.

- pipe mode: full protocol with `id` correlation
- CLI mode: same event schema, `id` may be omitted in display output
- protocol events are emitted on `stdout` only
- `stderr` is not part of the runtime protocol contract

## Interface Boundary

This protocol is the only runtime interface.

- `psql mode` is CLI argument translation only; runtime protocol is unchanged
- no legacy text interpolation
- no table/text output contract

## Input (stdin)

### `query`

Execute one SQL statement.

| Field | Required | Description |
|---|---|---|
| `code` | yes | `"query"` |
| `id` | yes | client correlation id |
| `session` | no | session id; default session if omitted |
| `sql` | yes | SQL text |
| `params` | no | positional bind values |
| `options` | no | query behavior |

`options` fields:

| Field | Default | Description |
|---|---|---|
| `stream_rows` | false | stream rows as `result_rows` events |
| `batch_rows` | 1000 | max rows per `result_rows` event |
| `batch_bytes` | 262144 | soft byte target per streamed batch |
| `statement_timeout_ms` | config default | per-query statement timeout |
| `lock_timeout_ms` | config default | per-query lock timeout |
| `read_only` | false | enforce read-only transaction for this query |
| `inline_max_rows` | config default | inline row cap for non-streaming |
| `inline_max_bytes` | config default | inline payload bytes cap for non-streaming |

### Parameter Binding Rules

1. Dynamic values should be passed via `params` with `$1..$N` placeholders.
2. Placeholder count must equal `params` length (validated from prepared-statement metadata, not SQL text scanning).
3. Count/type validation failures return `error_code: "invalid_params"`.

Driver-side type mapping (prepared statement parameter OIDs):

- `bool` -> JSON bool or `"true"/"false"`
- `int2/int4/int8` -> JSON integer or numeric string
- `float4/float8/numeric` -> JSON number or numeric string
- `json/jsonb` -> JSON object/array/scalar
- others -> text form (`string` preferred)

Unsupported:

- `:name` interpolation
- SQL string template expansion by client-side substitutions

CLI mapping notes:

- `--param N=value` maps to this `params` array
- in `psql mode`, numeric `-v N=value` may be translated to `params[N]`

### `config`

Partial runtime config update. Echoes full config afterward.

| Field | Required | Description |
|---|---|---|
| `code` | yes | `"config"` |
| `default_session` | no | default session name |
| `sessions` | no | session connection definitions |
| `inline_max_rows` | no | global inline row limit |
| `inline_max_bytes` | no | global inline payload bytes limit |
| `statement_timeout_ms` | no | global statement timeout |
| `lock_timeout_ms` | no | global lock timeout |
| `log` | no | enabled log categories |

Session connection shape supports:

- `dsn_secret`
- `conninfo_secret`
- `host`
- `port`
- `user`
- `dbname`
- `password_secret`

CLI translation notes:

- agent-first mode uses direct agent-first flags (`--dsn-secret`, `--host`, ...)
- `psql mode` may translate legacy flags (`-h`, `-p`, `-U`, `-d`, `-c`, `-f`)
  into these same canonical fields

### `cancel`

Cancel an in-flight query by id.

```json
{"code":"cancel","id":"q-123"}
```

### `ping`

Health check.

```json
{"code":"ping"}
```

### `close`

Graceful shutdown.

```json
{"code":"close"}
```

## Output (stdout)

### `result`

Small result returned inline.

| Field | Description |
|---|---|
| `code` | `"result"` |
| `id` | query id |
| `session` | session used |
| `command_tag` | Normalized command tag (`ROWS N` / `EXECUTE N`) |
| `columns` | column metadata array |
| `rows` | result rows |
| `row_count` | row count |
| `trace` | timing and counters |

### `result_start`

Start of streamed result.

| Field | Description |
|---|---|
| `code` | `"result_start"` |
| `id` | query id |
| `session` | session used |
| `columns` | column metadata |

### `result_rows`

One streamed row batch.

| Field | Description |
|---|---|
| `code` | `"result_rows"` |
| `id` | query id |
| `rows` | row objects for this batch |
| `rows_batch_count` | rows in batch |

### `result_end`

End of streamed result.

| Field | Description |
|---|---|
| `code` | `"result_end"` |
| `id` | query id |
| `session` | session used |
| `command_tag` | Normalized command tag (`ROWS N` / `EXECUTE N`) |
| `trace` | includes `duration_ms`, `row_count`, `payload_bytes` |

### `sql_error`

Database execution error.

| Field | Description |
|---|---|
| `code` | `"sql_error"` |
| `id` | query id |
| `session` | session used |
| `sqlstate` | SQLSTATE (`23505`, `42P01`, ...) |
| `message` | primary error message |
| `detail` | optional detail |
| `hint` | optional hint |
| `position` | optional SQL character position |
| `trace` | timing and counters |

### `error`

Client/runtime/protocol error.

| Field | Description |
|---|---|
| `code` | `"error"` |
| `id` | optional related query id |
| `error_code` | machine-readable code |
| `error` | human-readable detail |
| `retryable` | whether retry may succeed |
| `trace` | timing and counters |

Canonical `error_code` values:

- `invalid_request`
- `invalid_params`
- `connect_failed`
- `connect_timeout`
- `auth_failed`
- `result_too_large`
- `cancelled`

### Other output codes

| `code` | Meaning |
|---|---|
| `notice` | PostgreSQL NOTICE/WARNING |
| `config` | full runtime config echo |
| `pong` | ping response with counters |
| `close` | shutdown acknowledgement |
| `log` | optional runtime diagnostic event (enabled by `log` config/categories) |

`log` event fields:

- `event` (e.g. `query.result`, `query.error`, `query.sql_error`)
- `request_id` (optional)
- `session` (optional)
- `error_code` (optional)
- `command_tag` (optional)
- `trace`

`log` category matching (from `config.log` / `--log`):

- empty list disables `log` events
- `all` or `*` enables all categories
- exact match (`query.result`)
- group prefix match (`query` -> `query.*`)

## Environment Fallback

Optional runtime fallback variables:

- `AFPSQL_DSN_SECRET`
- `AFPSQL_CONNINFO_SECRET`
- `AFPSQL_HOST`
- `AFPSQL_PORT`
- `AFPSQL_USER`
- `AFPSQL_DBNAME`
- `AFPSQL_PASSWORD_SECRET`

Standard PostgreSQL environment fallback (lower precedence):

- `PGHOST`
- `PGPORT`
- `PGUSER`
- `PGDATABASE`

## Example: Small Result

Input:

```json
{"code":"query","id":"q1","sql":"select 1 as n"}
```

Output:

```json
{"code":"result","id":"q1","command_tag":"ROWS 1","columns":[{"name":"n","type":"int4"}],"rows":[{"n":1}],"row_count":1,"trace":{"duration_ms":2}}
```

## Example: Streamed Result

Input:

```json
{"code":"query","id":"q2","sql":"select * from big_table where id > $1","params":[100],"options":{"stream_rows":true,"batch_rows":1000}}
```

Output:

```json
{"code":"result_start","id":"q2","columns":[{"name":"id","type":"int8"},{"name":"name","type":"text"}]}
{"code":"result_rows","id":"q2","rows":[{"id":101,"name":"a"},{"id":102,"name":"b"}],"rows_batch_count":2}
{"code":"result_end","id":"q2","command_tag":"ROWS 200000","trace":{"duration_ms":443,"row_count":200000,"payload_bytes":34199211}}
```
