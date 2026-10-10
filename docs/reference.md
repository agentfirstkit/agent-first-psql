# Agent-First PSQL — Protocol Reference

Every stdin line is one JSON object tagged by a required `code`. Every emitted
line is an Agent-First Data envelope tagged by a top-level `kind`; see
[Output](#output) for the envelope shape.

- pipe mode: full protocol with `id` correlation
- CLI mode: same event schema, `id` may be omitted in display output
- a finite query splits by kind: `result` to stdout, `error`/`log`/`progress`
  to stderr, so a shell capture never mistakes a failure for data
- pipe mode and `--stream-rows` are ordered event streams: every event goes to
  stdout, because splitting a stream across two process streams would lose the
  ordering that makes it a stream
- `--output-to split|stdout|stderr` overrides the destination; `split` is
  rejected for a stream rather than silently reordering it

## Interface Boundary

This protocol is the only runtime interface.

- the CLI is a closed `cli-spec-v1` registry: an invocation runs only when it
  matches exactly one registered shape, there are no short flags, and a command
  path comes before its arguments (`afpsql --docs` renders the whole registry)
- `psql mode` is CLI argument translation only; runtime protocol is unchanged
- no legacy text interpolation
- no table/text output contract

Agent-facing reliability guarantees:

- recoverable runtime conditions are structured events, not ad-hoc prose
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
| `--dsn env:NAME`, `--conninfo env:NAME`, `--password env:NAME` | allowed on the ordinary entrypoint |
| `--dsn file:PATH#DOT_PATH`, `--conninfo file:PATH#DOT_PATH`, `--password file:PATH#DOT_PATH` | allowed on the ordinary entrypoint |
| explicit SSH options | only `ConnectionAttempts`, `ConnectTimeout`, `Port`, `ProxyJump`, `ServerAliveCountMax`, `ServerAliveInterval`, `TCPKeepAlive`; other settings belong in `~/.ssh/config` |
| custom container runtime | `invalid_request` on the ordinary entrypoint; administrator-locked profiles may select one |
| transaction control sent as query SQL | `invalid_request`; use typed pipe transaction requests |

Every readonly rejection is a structured error event with
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
visibility separately.

Output redaction covers connection, configuration and control metadata only. It
is not a guard on what a query returns: rows, column values, SQL text and the
parameters shown to a human approver are all emitted exactly as the database
gave them, including a column named `api_key_secret`. Redacting them would make
afpsql report something other than what is in the database, which is the one
thing this tool must never do — so a caller that must not see a value has to
avoid selecting it. Redaction of connection metadata also does not mean
connection secrets never leave the process.

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
runtimes. Explicit SSH options retain the same allowlist as the ordinary
entrypoint; put settings such as `IdentityFile`/`ProxyCommand` in `~/.ssh/config`.
A locked profile executable
rejects all connection/transport flags and all pipe `sessions` patches before
they can replace the profile.

A locked profile is also the whole connection: it reads none of the connection
environment variables that the ordinary readonly executable falls back to —
`AFPSQL_DSN_SECRET`, `AFPSQL_CONNINFO_SECRET`, `AFPSQL_HOST`/`PGHOST`,
`AFPSQL_PORT`/`PGPORT`, `AFPSQL_USER`/`PGUSER`, `AFPSQL_DBNAME`/`PGDATABASE`,
`AFPSQL_PASSWORD_SECRET`/`PGPASSWORD`, and `PGSSLMODE`. Otherwise a caller who
may only choose SQL could still move the endpoint, or downgrade its TLS, through
the environment. Put every connection field the profile needs — including the
password and any `sslmode` — in the profile file itself; use `dsn_secret` there
when you need `sslmode`. Hosts needing a single target should authorize
this distinct executable name, not infer a target by parsing shell arguments.

Locked profiles currently require Unix owner/mode checks. On other platforms,
use OS-native ACLs plus an equivalent wrapper policy; the generic readonly
executable continues to allow caller-selected targets as documented above.

## Connection secret sources

Each secret slot has three mutually exclusive explicit sources:

| Slot | Direct | Environment | Config file |
|---|---|---|---|
| DSN URI | `--dsn` | `--dsn env:NAME` | `--dsn file:FILE#DOT_PATH` |
| libpq conninfo | `--conninfo` | `--conninfo env:NAME` | `--conninfo file:FILE#DOT_PATH` |
| password | `--password` | `--password env:NAME` | `--password file:FILE#DOT_PATH` |

Config files may be JSON (`.json`), TOML (`.toml`), YAML (`.yaml`/`.yml`),
dotenv (`.env`, `.env.*`, `*.env`), or INI (`.ini`). A config source is one
typed argument containing both the file and dot path.

The grammar is AFDATA's, declared on each of the three arguments, so the
canonical registry refuses a malformed or unaccepted source while it resolves
argv — `cli_invalid_argument_value` at exit 2, before any file is opened — and
the psql-compatible translator, which parses its own argv, refuses the same set
through the same parser. Stream sources (`stdin`, `fd:N`, `prompt`) are
deliberately not accepted here: `afpsql` reads SQL on stdin, and blocking on a
terminal is a hang for an agent. The path must resolve to a non-empty
string; the value is returned verbatim — never trimmed, coerced from another
type, or URL-decoded, so percent-encode reserved characters in a DSN (`%40` for
`@`). A dot path resolving to a number or a boolean is refused rather than
coerced: a credential is text. Note that a double-quoted dotenv value or a TOML basic `"..."` string still
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

- `--param N=value` maps to this `params` array. Bare `null`, `true`, and
  `false` bind JSON primitives; prefix with `text:` to bind those spellings as
  literal strings (for example `--param 1=text:null`).
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
- `ssh_remote_socket`
- `ssh_sudo_user`
- `docker_name`
- `docker_user`
- `docker_context`
- `docker_runtime`
- `apple_name`
- `apple_user`
- `apple_runtime`
- `podman_name`
- `podman_user`
- `podman_runtime`
- `nerdctl_name`
- `nerdctl_user`
- `nerdctl_runtime`
- `compose_service`
- `compose_user`
- `compose_files`
- `compose_project`
- `compose_runtime`
- `kubectl_pod`
- `kubectl_container`
- `kubectl_namespace`
- `kubectl_context`
- `kubectl_runtime`

`session_info` is ordered through the named session's FIFO. While an explicit
transaction is open it reports configured defaults without probing PostgreSQL,
so identity fields may be null; this avoids adding savepoints or otherwise
mutating the caller's transaction.

Supported TLS settings supplied in `dsn_secret` or `conninfo_secret` are
honored. afpsql currently accepts `sslmode=disable/prefer/require`; unsupported
libpq TLS modes/options such as `verify-ca`, `verify-full`, `sslrootcert`,
`sslcert`, and `sslkey` fail with structured errors and hints.

SSH transport fields start an OpenSSH stdio bridge before
connecting. SSH accepts `dsn_secret`, `conninfo_secret`, or discrete connection
fields. afpsql parses DSN/conninfo locally, interprets its PostgreSQL endpoint
from the final SSH host, and preserves authentication, database, startup-option,
and supported TLS settings when connecting through the bridge. The source must
resolve to one PostgreSQL host and port; multi-host failover lists return a
structured non-retryable connection error.

Options that act on a socket the driver never opens are rejected by name rather
than accepted as no-ops: `connect_timeout`, `tcp_user_timeout`, `keepalives`,
`keepalives_idle`, `keepalives_interval`, `keepalives_retries`,
`target_session_attrs`, and `load_balance_hosts` fail with an error naming the
option that has to be dropped.

The bridge is carried over the `ssh` child process's stdin/stdout, so afpsql
opens no local listening port and no other process on the workstation can reach
the connection. In exchange, the host running the bridge needs `sh` plus one of
`python3`, `python`, or `perl` — any one is enough, and they are tried in that
order. A host with none of them fails with exit 127 and a message naming all
three. For `--ssh-via` chains the bridge runs on the final `--ssh` host, and
only that host needs an interpreter.

Container transport fields start a no-TTY exec bridge through one driver
(`apple`, `docker`, `podman`, `nerdctl`, `compose`, or `kubectl`) and run a
small stdio bridge inside the container. The `apple` family invokes the Apple
`container` CLI. The PostgreSQL host/port or Unix socket is interpreted inside
the container. Container transport can use `dsn_secret`, `conninfo_secret`, or
discrete connection fields and likewise requires one PostgreSQL endpoint.

The driver is not named separately: it is the family of fields used. Each field
is scoped to one driver by its own name, so a driver cannot be asked for an
option it does not have — there is no `kubectl_user` because `kubectl exec` has
no exec-as-user option, and no `podman_context` because Podman selects no
context. `kubectl_container` picks one container in a multi-container pod and is
emitted as `-c CTR` before `--`. `AFPSQL_CONTAINER_COMPOSE_FILE` may supply
colon-separated Compose files when no `compose_files` are configured.

Fields from two families cannot be combined: two families name two drivers, so
the session is rejected with an error naming both offending fields.

When a driver family is combined with `ssh`, afpsql uses SSH to run the
container exec command on the remote host, then bridges from inside the
container. In this combined mode, only `ssh` and `ssh_options` apply; SSH socket
and sudo bridge fields are for non-container SSH transport. The permission family
remains container (`container-read` / `container-write`).

CLI translation notes:

- agent-first mode uses direct agent-first flags (`--dsn`, `--host`, ...)
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

EOF and `close` share a five-second budget for draining all session workers and
closing database clients. Work still waiting at that deadline is cancelled
locally. The `close` acknowledgement does not confirm an unfinished write or
prove a transaction rolled back; verify an operation without its terminal
result before retrying it.

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
| `read_only` | no, `begin` only | defaults to `true`; set explicitly to `false` for a read-write transaction |
| `permission` | no, `begin` only | required when `read_only:false`; use the matching `write` / `ssh-write` / `container-write` permission |

The response is a `code:"result"` event with `command_tag` set to `"BEGIN"`,
`"COMMIT"`, or `"ROLLBACK"`. Failures (e.g. `begin` while already in a tx,
`commit` with no open tx, or PostgreSQL errors) surface as `error` or
`sql_error`.

Per-query failures inside an explicit transaction are wrapped in a savepoint
and rolled back individually, so the user's outer transaction is NOT
aborted by a single bad query — the agent can retry or move on without
losing prior progress. Send `rollback` to discard the whole transaction or
`commit` to persist the work done so far.

Every query inside a read-write explicit transaction must repeat the matching
write permission. A query that declares a read permission is rejected before
SQL execution. A read-only explicit transaction remains enforced by
PostgreSQL, including when a query supplies a write permission.

Tx control runs through the same session FIFO as `query`, so the order an
agent writes events to stdin is the order PostgreSQL sees them.

A `begin` that fails, or an explicit transaction whose connection is lost,
leaves the session in a failed transaction state rather than back in
autocommit: every `query` on that session is rejected with `invalid_request`
without reaching PostgreSQL, and another `begin` is rejected as already in an
explicit transaction. Send `rollback` to settle it (a `commit` settles it too,
reporting the transaction as rolled back); the session then accepts `begin`
and queries again. A caller that queues a transaction's statements ahead of
their responses should wait for the successful `begin` result before sending
the write statements.

Each session worker queue has 64 slots. A full queue rejects a new query or
control operation immediately with a retryable `invalid_request`, so stdin can
still accept cancellation and shutdown. A configuration patch reserves room
for every affected session's invalidation before changing any configuration;
if one queue cannot accept it, the entire patch is rejected. An accepted
`begin` keeps the endpoint and transport configuration used for its permission
check, even if a later patch changes that session before it executes.

## Output

Every output line is an Agent-First Data envelope: a top-level `kind` with the
event payload nested under the matching key, and `trace` as a top-level sibling.
The selected `--output-to` policy determines which process stream carries it.

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
full result of a read-only query, narrow it with `WHERE` or rerun with
`--stream-rows`. Never rerun a successful write just to recover truncated
RETURNING output. Choose `--stream-rows` before the first write when its full
result is needed; after truncation, use a separate `SELECT` with stable
business keys to inspect the current rows. That SELECT cannot necessarily
reconstruct old RETURNING values.

Bounded row results are produced through a CTE/JSON wrapper. PostgreSQL does
not guarantee that an outer read preserves ordering established only inside
that CTE, so callers must not treat the returned prefix as a contractual
ordered prefix—even when the submitted query contains `ORDER BY`. If exact
ordering is part of correctness, return an explicit ordinal/key and sort or
verify the rows in the caller.

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
- `cancelled`
- `internal_error`
- `transaction_lost_rolled_back`
- `commit_outcome_unknown`

`internal_error` means the failure is `afpsql`'s own, not the request's and not
PostgreSQL's; the hint asks the caller to retry and then restart the session.
Treat it as a bug report, not as something to work around.

An invocation rejected before anything ran carries the parser's own
classification instead, always on stderr and always exiting 2:
`cli_unknown_command`, `cli_unknown_argument`, `cli_unregistered_combination`,
`cli_missing_argument_value`, `cli_invalid_argument_value`,
`cli_duplicate_argument`, `cli_unexpected_positional`, `cli_invalid_utf8`.

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

## Panel delivery (`--mode`)

Every `afpsql ui` panel — `schema`, `table`, `indexes`, `connections`, `plan` —
takes an optional `--mode window|session`. `window` opens an isolated browser
window on this machine; `session` registers the panel only, for a person who
is not here to reach with `afui session serve`. Priority when `--mode` is
absent is `AFUI_DELIVERY` (also `window` or `session`), then `window` — AFUI's
own `UiDeliveryPlan::resolve` rule, applied here rather than reimplemented. An
`AFUI_DELIVERY` value neither command recognizes is rejected, listing the
legal words, rather than silently defaulting to a window nobody is there to
see.

`--mode` on a panel is a different axis from `afpsql --mode cli|pipe|psql`:
the two never appear on the same command line, since `ui` has no runtime mode
of its own, and each is scoped to its own command in the registry.

The kit's third delivery, `link` — a URL another device can open directly,
with no `afui` binary involved in reaching it — is not offered here, and does
not appear in a panel's `--help`. `link` binds every interface and advertises
the result on the network with no tunnel; for rows read out of a database
that is a wider, unencrypted exit than `--stdout-file`, which an
administrator-locked readonly profile already refuses (see above) for the
same reason it refuses every panel outright, regardless of `--mode`. `window`
never leaves this screen; `session` stays on loopback and is reached only
through afui's own registry, so neither carries that exposure.

The `ui_ready` progress event every panel emits carries AFUI's own delivery
facts beside the panel's: `mode`, naming the delivery with the same word
`--mode` and `AFUI_DELIVERY` accept, and `session_id`, the identity that panel
is listed under in `afui session list`. Which delivery reached a person
changes nothing about what `ui plan` accepts as an answer: only
`Completed(Approve)` — pressing the approve control — runs the statement, and
a closed window, a remotely-refused session, and a lapsed credential are all
still a refusal. The `ending` field on a `ui plan` result reports `approved`
or `refused` for a decision and AFUI's own `closed` or `expired` for every
other way a panel stops.

`ui connections` renders one opening snapshot before registration so a broken
database connection fails on the agent's stream. After that, periodic queries
are observation-driven: a visible page renews a short lease over the existing
AFUI runtime, and polling stops within 30 seconds after every page is gone or
hidden. The retained snapshot remains available, and polling resumes when a
page becomes visible again. These runtime calls do not count as AFUI attention
and do not extend a remote page's lifetime.

## Replacing a panel (`afui frontend`)

Every `afpsql ui` panel is a MiniJinja template rendered against a typed
document, and any of them can be replaced. AFUI owns where an override lives and
whether it is trusted; afpsql owns what the files mean.

| `ui_kind` | Panel |
| --- | --- |
| `schema_inspect` | `afpsql ui schema` |
| `table_inspect` | `afpsql ui table` |
| `index_inspect` | `afpsql ui indexes` |
| `connection_monitor` | `afpsql ui connections` |
| `plan_confirm` | `afpsql ui plan` |

Install one with `afui frontend init --provider-id afpsql --ui-kind <KIND>` and
turn it on with `afui frontend enable`. `ui_api_version` is `4`.

Files an override may supply, each independently — a file it does not supply
comes from afpsql, so replacing one page keeps the rest:

| Path | What it is |
| --- | --- |
| `templates/page.html.j2` | The panel body for this `ui_kind` |
| `templates/layout.html.j2` | The frame every page extends |
| `templates/table.html.j2` | The result table partial |
| `templates/relation.html.j2` | One relation's card, columns, constraints, indexes and triggers |
| `templates/decided.html.j2` | What `plan_confirm` shows after an answer |
| `templates/<path>.j2` | An additional partial, including nested paths, discovered at runtime and available by its full path |
| `style.css` | The stylesheet |
| `assets/**` | Stylesheets, images and fonts, served from the session origin |

A replacement `page.html.j2` or `decided.html.j2` that wants afpsql's frame
extends it by the same path a person sees in their own frontend directory:
`{% extends "templates/layout.html.j2" %}`, and likewise `{% include
"templates/table.html.j2" %}` for the result table or `{% include
"templates/relation.html.j2" %}` for whichever `document.relations` entry is in
scope. Including one you have not supplied gets afpsql's own.

Templates render against `document`, which carries everything the panel worked
out — already counted, sorted, classified and formatted. Reorder it, regroup it,
drop what you do not want; you cannot arrive at a different answer than the one
the agent read, because the panel and `afpsql inspect` run the same SQL.

The three inspection panels run one query that returns eight kinds of object
unioned into one set of generic columns — the shape `afpsql inspect` prints, and
the shape an agent wants. What their documents carry is that result already read
back apart: `document.relations` is one entry per relation with its `columns`,
`constraints`, `indexes` and `triggers` attached, its `facts` worded (`owner`,
`size`, a row estimate that says "never analyzed" rather than `-1`), and its
catalog codes translated into the tags beside each entry. Constraint rows that
only restate a column's own `not null` are not carried, because every column
already answers that. `document.table` is still the untouched result set, so a
page can show both.

Three things an override cannot do, and they are enforced rather than requested:

- **Ship JavaScript.** AFUI refuses a frontend file whose name says it is a
  script, and refuses a `<script>`, an `onclick=`, or a `javascript:` URL inside
  a template. The only script any panel loads is afpsql's own.
- **Decide what a control means.** `plan_confirm` templates *declare* a control
  with `data-afpsql-decision="approve"` or `"refuse"`; afpsql's runtime, spliced
  in at the layout's `<!-- afpsql:trusted-runtime -->` marker under a
  per-session nonce, is what binds that declaration to the route that runs or
  refuses the statement. A declaration afpsql does not recognise binds to
  nothing. A page that declares neither control does not open.
- **Replace the live protocol.** `connection_monitor` must declare the snapshot,
  EXPLAIN form/input/result, and live-status hooks its trusted runtime binds.
  Fresh snapshots are AFUI retained state and plain `EXPLAIN (FORMAT JSON)` is
  a typed AFUI call, both without reloading the page. AFUI owns correlation,
  reconnect, latest-state replacement, and caller-safe errors; the frontend
  may arrange the declarations however it likes, but cannot add an `ANALYZE`
  switch or ship a different runtime. The built-in frontend keeps only the
  current plan result; retaining a longer history is a frontend presentation
  choice, not durable database state. The terminal result reports
  `update_count`; it also retains the former `reload_count` name as a
  compatibility alias with the same value.
- **Turn escaping off.** `|safe`, a `filter safe` block and `autoescape` are
  refused, and every value a panel prints — including anything PostgreSQL
  returned — is escaped.

A frontend afpsql cannot load is an error naming safe mode
(`ui_frontend_incompatible`, `ui_frontend_unreadable`, `ui_frontend_unsafe`,
`ui_frontend_template`, `ui_frontend_incomplete`), never a quietly substituted
built-in page: no window opens and nothing runs. `AFUI_SAFE_MODE=1` ignores
every override. A workspace frontend that has not been enabled is skipped in
silence by design — the `ui_ready` progress event carries `ui_frontend_id` only
when an override is actually serving.

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
- `AFPSQL_SSH`
- `AFPSQL_SSH_VIA`
- `AFPSQL_SSH_REMOTE_SOCKET`
- `AFPSQL_SSH_SUDO_USER`
- `AFPSQL_CONTAINER_DOCKER_NAME`
- `AFPSQL_CONTAINER_DOCKER_USER`
- `AFPSQL_CONTAINER_DOCKER_CONTEXT`
- `AFPSQL_CONTAINER_DOCKER_RUNTIME`
- `AFPSQL_CONTAINER_APPLE_NAME`
- `AFPSQL_CONTAINER_APPLE_USER`
- `AFPSQL_CONTAINER_APPLE_RUNTIME`
- `AFPSQL_CONTAINER_PODMAN_NAME`
- `AFPSQL_CONTAINER_PODMAN_USER`
- `AFPSQL_CONTAINER_PODMAN_RUNTIME`
- `AFPSQL_CONTAINER_NERDCTL_NAME`
- `AFPSQL_CONTAINER_NERDCTL_USER`
- `AFPSQL_CONTAINER_NERDCTL_RUNTIME`
- `AFPSQL_CONTAINER_COMPOSE_SERVICE`
- `AFPSQL_CONTAINER_COMPOSE_USER`
- `AFPSQL_CONTAINER_COMPOSE_FILE` (colon-separated)
- `AFPSQL_CONTAINER_COMPOSE_PROJECT`
- `AFPSQL_CONTAINER_COMPOSE_RUNTIME`
- `AFPSQL_CONTAINER_KUBECTL_POD`
- `AFPSQL_CONTAINER_KUBECTL_CONTAINER`
- `AFPSQL_CONTAINER_KUBECTL_NAMESPACE`
- `AFPSQL_CONTAINER_KUBECTL_CONTEXT`
- `AFPSQL_CONTAINER_KUBECTL_RUNTIME`

Variables from two driver families are rejected the same way flags from two
families are: they name two drivers.

Administrator-locked readonly profiles ignore every connection, SSH, and
container environment fallback.

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
