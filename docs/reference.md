# Agent-First PSQL — Protocol Reference

Every stdin line is one JSON object tagged by a required `code`. Every stdout
line is an Agent-First Data envelope tagged by a top-level `kind`; see
[Output (stdout)](#output-stdout) for the envelope shape.

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

## `afpsql-readonly` capability boundary

`afpsql-readonly` is a separately installed database-write guard. It shares the
read/query, inspect, file/config, and typed transport implementation with
`afpsql`. It is not a replacement for PostgreSQL role authorization and is not
a general local-process sandbox.

| Path | readonly behavior |
|---|---|
| direct / SSH / container read permission | allowed |
| `write`, `ssh-write`, `container-write` | `invalid_request` |
| pipe `begin` read-only | allowed |
| pipe read-write `begin` or write query | `invalid_request` |
| `--mode psql`, `psql status/install/uninstall` | `invalid_request`; use `afpsql psql ...` |
| `skill status/install/uninstall` | allowed on the ordinary entrypoint |
| `--stdout-file`, `--stderr-file`, local `--sql-file` | allowed on the ordinary entrypoint |
| arbitrary explicit `--*-secret-env NAME` | allowed on the ordinary entrypoint |
| `--*-secret-config FILE DOT_PATH` | allowed on the ordinary entrypoint |
| SSH options and custom container runtime | allowed on the ordinary entrypoint |
| transaction control sent as query SQL | `invalid_request`; use typed pipe transaction requests |

Every readonly rejection is a structured stdout error with
`error.code: "invalid_request"` and a hint directing write work to `afpsql`.
The client transaction is a safety belt, not an adversarial SQL sandbox. A
whitelist for the ordinary executable grants caller-selected local file and env
reads, process-spawning transport options, network access to caller-selected
direct/SSH/container targets, and every row visible to the database role. For a
server-enforced baseline, provision a non-owner login role:

```sql
CREATE ROLE app_reader LOGIN
  NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
GRANT CONNECT ON DATABASE app TO app_reader;
GRANT USAGE ON SCHEMA app TO app_reader;
GRANT SELECT ON ALL TABLES IN SCHEMA app TO app_reader;
ALTER DEFAULT PRIVILEGES FOR ROLE app_owner IN SCHEMA app
  GRANT SELECT ON TABLES TO app_reader;
```

Keep the role out of writer/owner memberships and audit `PUBLIC`, function
`EXECUTE`, `SECURITY DEFINER`, extensions, predefined roles, sequences, and RLS
visibility separately. Output redaction prevents display of secret fields; it
does not mean connection secrets never leave the process.

PostgreSQL READ ONLY rejects temporary-object creation, sequence `nextval`, and
writes performed inside SECURITY DEFINER functions. PostgreSQL still permits
operations including `NOTIFY` and transaction-level advisory locks; constrain
those through role/function audit, timeouts, concurrency limits, and lock/resource
governance. Readonly rejects transaction-control SQL before execution because
PostgreSQL can accept nested `BEGIN`/`COMMIT`; pipe clients must use the typed
`begin`, `commit`, and `rollback` requests so afpsql owns transaction state.

### Administrator-locked readonly profiles

On Unix, an executable basename `afpsql-readonly-NAME` selects exactly
`/etc/afpsql/readonly-profiles/NAME.json`. `NAME` is limited to ASCII letters,
digits, `-`, and `_`; the profile must be a regular root-owned file, no larger
than 64 KiB, and not writable by group or others. For example:

```json
{"host":"db.internal","port":5432,"user":"app_reader","dbname":"app","password_secret":"..."}
```

The file uses the flat session fields documented for pipe config. Because this
is administrator-controlled configuration, it may contain custom container
runtimes or SSH options such as `IdentityFile`/`ProxyCommand`. A locked profile executable
rejects all connection/transport flags and all pipe `sessions` patches before
they can replace the profile. Hosts needing a single target should authorize
this distinct executable name, not infer a target by parsing shell arguments.

Locked profiles currently require Unix owner/mode checks. On other platforms,
use OS-native ACLs plus an equivalent wrapper policy; the generic readonly
executable continues to allow caller-selected targets as documented above.

## Connection secret sources

Each secret slot has three mutually exclusive explicit sources:

| Slot | Direct | Environment | Config file |
|---|---|---|---|
| DSN URI | `--dsn-secret` | `--dsn-secret-env NAME` | `--dsn-secret-config FILE DOT_PATH` |
| libpq conninfo | `--conninfo-secret` | `--conninfo-secret-env NAME` | `--conninfo-secret-config FILE DOT_PATH` |
| password | `--password-secret` | `--password-secret-env NAME` | `--password-secret-config FILE DOT_PATH` |

Config files may be JSON (`.json`), TOML (`.toml`), YAML (`.yaml`/`.yml`), or
dotenv (`.env`, `.env.*`, `*.env`). The config argument requires two
space-separated values; `--flag=FILE PATH` is rejected consistently by both
canonical and psql-compatible parsers. The path must resolve to a non-empty
string; the value is returned verbatim — never trimmed, coerced from another
type, or URL-decoded, so percent-encode reserved characters in a DSN (`%40` for
`@`). Note that a double-quoted dotenv value or a TOML basic `"..."` string still
undergoes that format's own escape processing (`\t`→tab, `\\`→`\`); use an
unquoted or single-quoted `'...'` value for raw bytes.

The file is read once during startup, before database or transport work. An
explicit config source overrides `AFPSQL_*` and libpq `PG*` fallbacks for that
slot. Pipe reconnects reuse the resolved value and do not observe file changes.
Pipe config requests cannot submit file/path references in protocol v1.

`Output::Config` serializes configured `dsn_secret`, `conninfo_secret`, and
`password_secret` as `***` for JSON, YAML, and plain output regardless of whether
the original source was direct, env, or config. Startup logs may report safe
source metadata (kind and, for config, file/dot-path), never the resolved value.

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
3. Client-side count/shape/local binding conversion failures return `error.code: "invalid_params"`.
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
- `ssh_via`
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

Unknown session names return `kind:"error"` with `error.code:"invalid_request"`
and a hint pointing to `config`.

### `begin` / `commit` / `rollback`

Pipe-mode explicit transactions. Without these, every `query` is wrapped in
its own implicit `BEGIN..COMMIT`, so multi-statement atomicity requires
jamming everything into one SQL string. After `begin`, subsequent `query`
events on the same session run inside the open transaction until a matching
`commit` or `rollback`.

```json
{"code":"begin","id":"b1","session":"default","read_only":false,"permission":"write"}
{"code":"commit","id":"c1","session":"default"}
{"code":"rollback","id":"rb1","session":"default"}
```

| Field | Required | Description |
|---|---|---|
| `code` | yes | `"begin"`, `"commit"`, or `"rollback"` |
| `id` | no | client correlation id, echoed on the response |
| `session` | no | session id; default session if omitted |
| `read_only` | no, `begin` only | when `true`, send `BEGIN READ ONLY` |
| `permission` | no, `begin` only | required when `read_only:false` on a session whose default permission is read; `write` / `ssh-write` / `container-write` |

The response is a `code:"result"` event with `command_tag` set to `"BEGIN"`,
`"COMMIT"`, or `"ROLLBACK"`. Failures (e.g. `begin` while already in a tx,
`commit` with no open tx, or PostgreSQL errors) surface as `error` or
`sql_error`.

Per-query failures inside an explicit transaction are wrapped in a savepoint
and rolled back individually, so the user's outer transaction is NOT
aborted by a single bad query — the agent can retry or move on without
losing prior progress. Send `rollback` to discard the whole transaction or
`commit` to persist the work done so far.

Tx control runs through the same session FIFO as `query`, so the order an
agent writes events to stdin is the order PostgreSQL sees them.

## Output (stdout)

Every stdout line is an Agent-First Data envelope: a top-level `kind` with the
event payload nested under the matching key, and `trace` as a top-level sibling.

```json
{"kind": "result", "result": { ...payload... }, "trace": { ...timing... }}
```

`kind` is one of `result`, `progress`, `error`, or `log`. The business event
name is the payload's own `code` field (except `log`, which drops `code` and adds
`timestamp_epoch_ms`). The per-event tables below list **payload** fields — the
object nested under the envelope key — so `code`/`id`/`columns`/… arrive as
`result.code`/`result.id`/… on the wire. Each table also lists `trace` for
reference, but it is the top-level sibling shown above, not nested in the payload.

| Event (`payload.code`) | Envelope `kind` | Payload key |
|---|---|---|
| `result`, `result_end`, `dry_run`, `session_info`, `config`, `pong`, `close` | `result` | `result` |
| `result_start`, `result_rows` | `progress` | `progress` |
| `sql_error`, `error` | `error` | `error` |
| `log` | `log` | `log` |

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
| `row_count` | row count actually emitted (the prefix size when `truncated`) |
| `truncated` | optional; `true` when `rows` is a prefix of the full result |
| `truncated_at_rows` | optional; inline row cap that fired |
| `truncated_at_bytes` | optional; inline byte cap that fired |
| `trace` | timing and counters |

When `truncated: true`, the underlying SQL still executed in full. For
`UPDATE ... RETURNING`, this means the writes happened and the RETURNING
projection delivered to the agent is the first N rows. To collect the
full result, narrow the query with `WHERE` or switch to `--stream-rows`.

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

### `dry_run`

Emitted instead of executing the SQL when `--dry-run` is passed. The server
prepares the statement inside a transaction that is rolled back, so this also
validates table/column existence and placeholder counts without side effects.

| Field | Description |
|---|---|
| `code` | `"dry_run"` |
| `id` | optional client correlation id |
| `sql` | the SQL that would have been executed |
| `params` | the params that would have been bound, in JSON-encoded form |
| `session` | session that would have been used |
| `param_types` | inferred PostgreSQL types for `$1`, `$2`, ... in placeholder order |
| `columns` | output column metadata (empty for non-SELECT statements) |
| `trace` | timing and counters |

If preparation fails, `afpsql` emits `sql_error` (PostgreSQL diagnostic) or
`error` (placeholder-count mismatch / connect failure) with the same shape as
a normal query, and exits non-zero.

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
| `code` | machine-readable code |
| `message` | human-readable detail |
| `sqlstate` | optional SQLSTATE when PostgreSQL rejects connection setup |
| `detail` | optional PostgreSQL detail for connection setup failures |
| `hint` | optional remediation hint |
| `retryable` | whether retry may succeed |
| `trace` | timing and counters (top-level sibling) |

Canonical `error.code` values:

- `invalid_request`
- `invalid_params`
- `connect_failed`
- `result_too_large`
- `cancelled`

For connection setup failures, `kind` remains `"error"` and `error.code` remains
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
| `database` | optional PostgreSQL database name (from probe or config) |
| `user` | optional PostgreSQL role (from probe or config) |
| `host` | optional server host (from probe or config) |
| `port` | optional server port (from probe or config) |
| `server_version` | optional PostgreSQL server version (from probe) |
| `trace` | timing and counters |

If the probe SELECT succeeds during `session_info`, `database`/`user`/`host`/
`port`/`server_version` reflect what the PostgreSQL server itself reports.
If the probe fails (typically because connection setup itself fails), the
fields fall back to the resolved session config and `server_version` is omitted.
Probe failures do not cause `session_info` to error.

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
- `error.code` (optional)
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
{"kind":"result","result":{"code":"result","id":"q1","command_tag":"ROWS 1","columns":[{"name":"n","type":"int4"}],"rows":[{"n":1}],"row_count":1},"trace":{"duration_ms":2}}
```

## Example: Streamed Result

Input:

```json
{"code":"query","id":"q2","sql":"select * from big_table where id > $1","params":[100],"options":{"stream_rows":true,"batch_rows":1000}}
```

Output:

```json
{"kind":"progress","progress":{"code":"result_start","id":"q2","columns":[{"name":"id","type":"int8"},{"name":"name","type":"text"}],"message":"query result stream started"},"trace":{}}
{"kind":"progress","progress":{"code":"result_rows","id":"q2","rows":[{"id":101,"name":"a"},{"id":102,"name":"b"}],"rows_batch_count":2,"message":"query result rows"},"trace":{}}
{"kind":"result","result":{"code":"result_end","id":"q2","command_tag":"ROWS 200000"},"trace":{"duration_ms":443,"row_count":200000,"payload_bytes":34199211}}
```
