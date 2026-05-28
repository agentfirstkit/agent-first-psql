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

Agent-facing reliability guarantees:

- recoverable runtime conditions are stdout events, not stderr prose
- native CLI and pipe writes require explicit permission
- pipe named sessions execute FIFO and are intended to preserve PostgreSQL
  backend session state until invalidation or shutdown
- database failures preserve PostgreSQL `SQLSTATE`
- validation and permission failures include actionable hints when possible

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
| `permission` | native/pipe transport default | `read`, `write`, `ssh-read`, `ssh-write`, `container-read`, or `container-write` |
| `inline_max_rows` | config default | inline row cap for non-streaming |
| `inline_max_bytes` | config default | inline payload bytes cap for non-streaming |

In native CLI and pipe mode, permission defaults to `read` for direct sessions,
`ssh-read` for sessions using afpsql SSH transport, and `container-read` for
sessions using afpsql container transport. Read permissions run in PostgreSQL
read-only transactions. Direct writes require `write`; SSH writes require
`ssh-write`; container writes require `container-write`.

### Parameter Binding Rules

1. Dynamic values should be passed via `params` with `$1..$N` placeholders.
2. Placeholder count must equal `params` length (validated from prepared-statement metadata, not SQL text scanning).
3. Client-side count/shape/local binding conversion failures return `error_code: "invalid_params"`.
4. PostgreSQL server conversion/execution failures return `code: "sql_error"` with the original SQLSTATE.

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
- `ssh`
- `ssh_options`
- `ssh_local_host`
- `ssh_local_port`
- `ssh_remote_socket`
- `ssh_sudo_user`
- `container`
- `container_driver`
- `container_runtime`
- `container_user`
- `container_namespace`
- `container_context`
- `container_compose_files`
- `container_compose_project`
- `container_pod_container`

Supported TLS settings supplied in `dsn_secret` or `conninfo_secret` are
honored. afpsql currently accepts `sslmode=disable/prefer/require`; unsupported
libpq TLS modes/options such as `verify-ca`, `verify-full`, `sslrootcert`,
`sslcert`, and `sslkey` fail with structured errors and hints.

SSH transport fields start a local OpenSSH tunnel or Unix-socket bridge before
connecting. They currently expect discrete connection fields rather than
`dsn_secret` or `conninfo_secret`.

Container transport fields start a no-TTY exec bridge through the selected
driver (`docker`, `podman`, `nerdctl`, `compose`, or `kubectl`) and run a small
stdio bridge inside the container. The PostgreSQL host/port or Unix socket is
interpreted inside the container. Container transport can use `dsn_secret`,
`conninfo_secret`, or discrete connection fields.

Container driver scope is configured with named fields, not raw argv
passthrough: `container_context` applies to Docker and kubectl,
`container_namespace` applies to kubectl, and `container_compose_files` /
`container_compose_project` apply to Compose. `container_pod_container` applies
to kubectl multi-container pods and is emitted as `-c CTR` before `--`.
`AFPSQL_CONTAINER_COMPOSE_FILE` may supply colon-separated Compose files when no
`container_compose_files` are configured.

When both `ssh` and `container` are set, afpsql uses SSH to run the container
exec command on the remote host, then bridges from inside the container. In this
combined mode, only `ssh` and `ssh_options` apply; SSH tunnel and sudo bridge
fields are for non-container SSH transport. The permission family remains
container (`container-read` / `container-write`).

CLI translation notes:

- agent-first mode uses direct agent-first flags (`--dsn-secret`, `--host`, ...)
- `psql mode` may translate legacy flags (`-h`, `-p`, `-U`, `-d`, `-c`, `-f`)
  into these same canonical fields
- `psql mode` does not expose afpsql permission flags and preserves psql's
  writable default for script compatibility; use native afpsql for agent-safe
  permissions and transport-specific agent behavior

### `cancel`

Cancel a queued or running query by id.

```json
{"code":"cancel","id":"q-123"}
```

When the database connection is already executing the query, `afpsql` sends a
PostgreSQL server-side cancel request. When the query is still queued, `afpsql`
removes it before execution. Cancellation is still race-prone: a query may
finish normally before the cancel request is processed.

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

### `session_info`

Pipe-mode introspection request. Returns the named session's resolved transport,
permission default, and runtime limits so an agent can discover what it is
connected to without probing via failing queries.

| Field | Required | Description |
|---|---|---|
| `code` | yes | `"session_info"` |
| `id` | no | client correlation id |
| `session` | no | session id; default session if omitted |

Unknown session names return `code:"error"` with `error_code:"invalid_request"`
and a hint pointing to `config`.

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
| `sqlstate` | optional SQLSTATE when PostgreSQL rejects connection setup |
| `message` | optional PostgreSQL primary message for connection setup failures |
| `detail` | optional PostgreSQL detail for connection setup failures |
| `hint` | optional remediation hint |
| `retryable` | whether retry may succeed |
| `trace` | timing and counters |

Canonical `error_code` values:

- `invalid_request`
- `invalid_params`
- `connect_failed`
- `result_too_large`
- `cancelled`

For connection setup failures, `code` remains `"error"` and `error_code` remains
`"connect_failed"`. If PostgreSQL returns a server diagnostic during startup
(for example password auth failure, missing role/database, too many connections,
or cannot-connect-now), `afpsql` also includes `sqlstate` plus PostgreSQL
diagnostic fields and a SQLSTATE-specific `hint`.

### `session_info`

Response to a `session_info` request.

| Field | Description |
|---|---|
| `code` | `"session_info"` |
| `id` | optional client correlation id |
| `session` | resolved session name |
| `transport_kind` | `"direct"`, `"ssh"`, or `"container"` |
| `permission_default` | transport-default permission (`"read"`, `"ssh-read"`, or `"container-read"`) |
| `stream_rows_default` | session's default `stream_rows` value |
| `batch_rows` | resolved `batch_rows` default |
| `batch_bytes` | resolved `batch_bytes` default |
| `inline_max_rows` | resolved inline row cap |
| `inline_max_bytes` | resolved inline payload byte cap |
| `statement_timeout_ms` | resolved statement timeout |
| `lock_timeout_ms` | resolved lock timeout |
| `trace` | timing and counters |

### Other output codes

| `code` | Meaning |
|---|---|
| `config` | full runtime config echo |
| `pong` | ping response with counters |
| `close` | shutdown acknowledgement |
| `log` | optional runtime diagnostic event (enabled by `log` config/categories) |

`log` event fields:

- `event` (e.g. `query.result`, `query.error`, `query.sql_error`,
  `transport.selected`, `mode.permission_default_changed`,
  `connect.libpq_env_fallback`)
- `request_id` (optional)
- `session` (optional)
- `error_code` (optional)
- `command_tag` (optional)
- `chain` (optional transport summary)
- `trace`

Startup `log` events include `version`, parsed/summarized `args`, and selected
environment fallback presence metadata (`key` plus `present`). They intentionally
omit raw `argv`, raw environment values, and config snapshots. Bind values are
summarized as `param_count`, not logged as plaintext.

`log` category matching (from `config.log` / `--log`):

- empty list disables `log` events
- `all` or `*` enables all categories
- exact match (`query.result`)
- group prefix match (`query` -> `query.*`)

`transport.selected` is emitted once when a new session connection is opened
and the `transport` log category (or `all` / `*`) is enabled. Its `chain`
summarizes the selected boundary, for example
`ssh:user@server -> docker exec pg -> tcp 127.0.0.1:5432`.

`mode.permission_default_changed` is emitted under the `mode` log category
whenever `--mode psql` bypasses the native read-only default, so agents can see
when psql-compat translation has dropped the write boundary.

`connect.libpq_env_fallback` is emitted under the `connect` log category when
libpq `PG*` environment variables (`PGHOST`, `PGPORT`, `PGUSER`, `PGDATABASE`,
`PGPASSWORD`, `PGSSLMODE`) fill connection fields that were not provided via
flags or secrets, listing which variables were used.

## Runtime Safety Limits

Pipe mode applies hard protocol limits before executing a request:

- max JSONL line: 1 MiB
- max SQL text: 1 MiB
- max params per query: 65,535
- max queued/running query ids: 64

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
- `PGPASSWORD`
- `PGSSLMODE` (`disable`, `prefer`, or `require`)

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
