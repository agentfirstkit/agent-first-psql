# Overview

`afpsql` is a PostgreSQL reliability contract for AI agents.

It is not trying to be an interactive `psql` clone, an ORM, a database UI, or a
high-throughput connection pooler. It gives agents a predictable way to run SQL:
structured events on stdout, explicit permission boundaries, stable session
state when requested, and machine-readable failures.

Supported platforms: macOS, Linux, Windows.

For agent behavior rules, see the [Agent Skill](../skills/agent-first-psql.md).
For exact fields, see the [Protocol Reference](reference.md).

## The contract: stdout events, SQLSTATE, and explicit write boundaries

Agents can depend on these semantics:

- **stdout is the protocol.** Every recoverable result or failure is emitted as a structured event on stdout.
- **SQL failures are data.** PostgreSQL errors are `sql_error` events with `SQLSTATE` and diagnostics.
- **Runtime failures are data.** Client, transport, validation, and protocol failures are `error` events with stable `error_code`, `retryable`, and `hint` fields.
- **Native writes are explicit.** Native CLI and pipe queries default to read-only PostgreSQL transactions.
- **SSH/container boundaries are first-class.** `--ssh`, `--container`, and `--ssh + --container` keep the agent local while crossing server, container, and remote-container boundaries.
- **Session ordering is deterministic.** Queries in the same pipe session run FIFO.
- **Named sessions preserve backend state.** A named pipe session is intended to reuse the same PostgreSQL backend session until config invalidation or process shutdown.
- **No SQL-text guessing.** `afpsql` uses PostgreSQL prepare/metadata results to decide result shape and parameter requirements.

Connection reuse exists to make session state reliable for agents. It is not a
promise of pooler-level throughput or workload balancing.

## Install it where the agent runs, not on every database server

Install `afpsql` on the machine where the agent runs. The database server or
container does not need afpsql installed; use SSH or container transport when
PostgreSQL is reachable only from that boundary.

```bash
brew install agentfirstkit/tap/afpsql   # macOS/Linux
scoop bucket add agentfirstkit https://github.com/agentfirstkit/scoop-bucket && scoop install afpsql  # Windows
cargo install agent-first-psql          # any platform
```

Install or load the Agent Skill so the agent keeps choosing structured database
access instead of human-oriented `psql`:

```bash
afpsql skill status
afpsql skill install
afpsql skill status
```

The default skill target installs personal skills for Codex and Claude Code.
Use `afpsql skill install --agent claude-code --scope project` when a Claude
Code skill should live in the current repository under `.claude/skills`.

Suggested agent instruction:

> Use local `afpsql` for non-interactive PostgreSQL work. Prefer read-only
> queries. Ask before writes and use explicit permission. Use `afpsql --ssh
> user@server` when PostgreSQL is only reachable from the server itself, and
> `afpsql --container CONTAINER` when PostgreSQL is only reachable from inside a
> container. For containers on a remote SSH host, combine `--ssh user@server`
> with `--container CONTAINER`. Do not SSH or run container exec commands just
> to run human `psql` unless I ask for that. Do not run `afpsql --help` as
> routine preflight before known query forms.

## Choose the mode by the reliability property you need

### Native CLI: one agent action

Use native CLI mode for a single query or command. Output is one structured event
or a structured error.

```bash
afpsql --dsn-secret-env DATABASE_URL \
  --sql "select id, status from jobs where id = $1" \
  --param 1=123
```

### Pipe: long agent session

Use pipe mode when an agent needs multiple ordered operations, cancellation,
streaming, or PostgreSQL session state such as temp tables or `set local`/GUC
behavior.

```bash
afpsql --mode pipe --dsn-secret-env DATABASE_URL
```

Each input line is one JSON object. Queries in the same session are queued FIFO.
Different sessions are isolated.

```json
{"code":"query","id":"q1","session":"work","sql":"select current_database() as db"}
```

Pipe mode also accepts `begin`/`commit`/`rollback` for explicit multi-statement
transactions. While a transaction is open, queries bypass the implicit
`BEGIN..COMMIT` wrap; per-query failures are isolated by a savepoint so a single
bad statement does not abort the whole tx. See `docs/reference.md` for the input
shape.

### Schema discovery and plan inspection

Run `afpsql inspect <databases|schemas|tables|views|table NAME>` instead of
writing `information_schema` / `pg_catalog` queries by hand. Wrap any query in
`--explain` (`EXPLAIN (FORMAT JSON)`) or `--explain-analyze` (also runs the
statement) to receive the plan tree as a normal `code:"result"` row.

Pre-flight a query with `--dry-run`: afpsql opens a connection, runs `PREPARE`
inside a transaction that is rolled back, and emits a `dry_run` event carrying
the inferred `param_types` and output `columns` — no rows are scanned and no
side effects occur.

### psql compatibility: scripts only

Use `--mode psql` or the managed wrapper for non-interactive scripts that already
call `psql -c`, `psql -f`, or `psql -l`.

```bash
afpsql --mode psql -h 127.0.0.1 -p 5432 -U app -d appdb -c "select 1"
```

`psql` mode is only argument translation into the same structured runtime. It
preserves psql's writable default for script compatibility and intentionally does
not expose native afpsql permission flags. Prefer native afpsql mode for
transport-specific agent work.

Out of scope for psql mode:

- interactive terminals and prompts
- psql meta-commands such as `\d`, `\x`, `\timing`
- psql table/text output compatibility
- client-side variable interpolation

## Permission is the write boundary

Native CLI and pipe mode are read-only by default:

| Transport | Default | Write permission |
|---|---|---|
| direct PostgreSQL connection | `read` | `write` |
| afpsql SSH transport | `ssh-read` | `ssh-write` |
| afpsql container transport | `container-read` | `container-write` |

`read`, `ssh-read`, and `container-read` run the SQL inside a PostgreSQL read-only
transaction. Writes fail with SQLSTATE `25006` unless the agent explicitly
requests the right write permission.

Direct write:

```bash
afpsql --permission write \
  --sql "update jobs set checked_at = now() where id = $1" \
  --param 1=123
```

Pipe write:

```json
{"code":"query","id":"q1","sql":"update jobs set checked_at = now() where id = $1","params":[123],"options":{"permission":"write"}}
```

SSH write:

```bash
afpsql --permission ssh-write --ssh user@server \
  --host 127.0.0.1 --port 5432 \
  --user app --dbname appdb \
  --password-secret-env PGPASSWORD \
  --sql "update jobs set checked_at = now() where id = $1" \
  --param 1=123
```

Container write:

```bash
afpsql --permission container-write --container pg-container \
  --dsn-secret-env DATABASE_URL \
  --sql "update jobs set checked_at = now() where id = $1" \
  --param 1=123
```

Permission mismatches are rejected before execution with an `invalid_request`
error and a corrective `hint`. For example, `--permission write --ssh ...` tells
the agent to use `ssh-read` or `ssh-write`; `--permission ssh-write` without
SSH tells the agent to use `read` or `write`; container sessions similarly require
`container-read` or `container-write`.

## Parameters are data, not SQL text

Dynamic values should be bound with `$1..$N` placeholders and `params` / `--param`.
Do not concatenate values into SQL text.

```bash
afpsql --sql "select * from users where id = $1 and status = $2" \
  --param 1=123 \
  --param 2=active
```

Prepared-statement metadata validates parameter count and local binding shape.
Client-side parameter shape or local binding conversion failures return
`invalid_params`; PostgreSQL server conversion and execution failures remain
`sql_error` events with the original SQLSTATE.

Unsupported by design:

- `:name` interpolation
- raw text expansion in SQL templates
- SQL keyword scanning to decide runtime behavior

In psql compatibility mode, numeric `-v N=value` entries can be translated into
positional parameters. Non-numeric interpolation variables are not supported.

## Output is a protocol, not terminal formatting

Common output events:

- `result` — small row result, command result, or transaction control
  acknowledgement (`command_tag` of `BEGIN`/`COMMIT`/`ROLLBACK`). Carries
  `truncated:true` plus `truncated_at_rows`/`truncated_at_bytes` when the
  inline cap was hit.
- `result_start` / `result_rows` / `result_end` — streamed row result
- `sql_error` — PostgreSQL error with `sqlstate`
- `error` — validation, connection, permission, protocol, or transport error
- `dry_run` — preview response for `--dry-run`; carries `param_types` and
  `columns` inferred from a rolled-back PREPARE
- `config`, `pong`, `close`, `log` — runtime protocol events

Connection-stage PostgreSQL rejections use `code:"error"` with
`error_code:"connect_failed"` and, when PostgreSQL provides them, `sqlstate`,
`message`, `detail`, and an actionable `hint`. Agents can tell a missing role
from a password failure, missing database, server startup state, or connection
capacity problem without parsing terminal prose.

Large result handling is explicit. By default, small results are returned inline.
Use streaming when the agent expects a large result set:

```bash
afpsql --sql "select * from big_table" --stream-rows --batch-rows 1000
```

If streaming is off and inline limits are exceeded, `afpsql` soft-truncates
the result: the agent receives a `code:"result"` event with the first N rows
and `truncated:true` (plus the cap that fired). The underlying SQL still
executed in full; only the projection returned to the agent is capped.
Either narrow the query with `WHERE`/`LIMIT` or switch to `--stream-rows`
to see everything.

## SSH transport makes the remote boundary explicit

Use `--ssh` when PostgreSQL is reachable from the server but not directly from
the agent machine. `afpsql` stays local, starts OpenSSH, connects through the
forwarded path, and tears down the transport with the process/session.

Remote TCP PostgreSQL is the preferred path when the server can run a normal
password-authenticated local connection:

```bash
export PGPASSWORD='...'
afpsql --ssh user@server \
  --host 127.0.0.1 --port 5432 \
  --user app --dbname appdb \
  --password-secret-env PGPASSWORD \
  --sql "select now()"
```

Remote Unix socket without sudo, useful when the SSH login user can peer-auth to
PostgreSQL:

```bash
afpsql --ssh user@server \
  --host /var/run/postgresql \
  --user user --dbname appdb \
  --sql "select current_user"
```

If the only working manual command is `sudo -u postgres psql`, prefer changing
PostgreSQL roles/authentication over using sudo from an agent. When necessary,
afpsql has an explicit, non-interactive sudo bridge:

```bash
afpsql --ssh user@server \
  --ssh-sudo-user postgres \
  --ssh-remote-socket /path/to/.s.PGSQL.5432 \
  --user postgres --dbname postgres \
  --sql "select current_user"
```

This bridge uses `sudo -n` and fails instead of prompting. It requires an exact
socket path or a socket directory in `--host`/`PGHOST`; afpsql does not guess
socket locations.

Supported SSH options:

- `--ssh user@server` / `AFPSQL_SSH=user@server`
- `--ssh-option ProxyJump=bastion` (repeatable OpenSSH `-o` options)
- `--ssh-local-host 127.0.0.1` / `AFPSQL_SSH_LOCAL_HOST`
- `--ssh-local-port 15432` / `AFPSQL_SSH_LOCAL_PORT` (defaults to an ephemeral port)
- `--ssh-remote-socket /path/to/.s.PGSQL.5432` / `AFPSQL_SSH_REMOTE_SOCKET`
- `--ssh-sudo-user postgres` / `AFPSQL_SSH_SUDO_USER`

SSH transport expects discrete connection fields. Prefer
`--host/--port/--user/--dbname/--password-secret-env` over `--dsn-secret` or
`--conninfo-secret` when `--ssh` is active.

## Container transport reaches container-local PostgreSQL

Use `--container TARGET` when PostgreSQL listens only from inside a container.
This replaces `docker exec CONTAINER psql ...` and similar Podman, nerdctl,
Compose, or Kubernetes exec calls for agent work. `afpsql` stays local, starts a
no-TTY exec bridge through the selected driver, and runs a small stdio bridge in
the container; the container does not need afpsql or psql.

Container-local TCP:

```bash
afpsql --container pg-container \
  --dsn-secret 'postgresql://app:pw@127.0.0.1:5432/appdb' \
  --sql "select now()"
```

Container-local Unix socket:

```bash
afpsql --container pg-container \
  --host /var/run/postgresql --port 5432 \
  --user app --dbname appdb \
  --sql "select current_user"
```

If the socket uses peer auth, add `--container-user OSUSER` so the container OS user
matches the requested PostgreSQL role.

Supported container options:

- `--container TARGET` / `AFPSQL_CONTAINER`
- `--container-driver docker|podman|nerdctl|compose|kubectl` / `AFPSQL_CONTAINER_DRIVER`
- `--container-runtime docker` / `AFPSQL_CONTAINER_RUNTIME`
- `--container-user postgres` / `AFPSQL_CONTAINER_USER`
- `--container-context CTX` / `AFPSQL_CONTAINER_CONTEXT` for Docker or kubectl
- `--container-namespace NS` / `AFPSQL_CONTAINER_NAMESPACE` for kubectl
- repeatable `--container-compose-file PATH` for Compose, or
  colon-separated `AFPSQL_CONTAINER_COMPOSE_FILE`
- `--container-compose-project NAME` / `AFPSQL_CONTAINER_COMPOSE_PROJECT` for Compose
- `--container-pod-container CTR` / `AFPSQL_CONTAINER_POD_CONTAINER` for
  multi-container Kubernetes pods

Use `--container-runtime docker-compose` with `--container-driver compose` for
Compose v1. The default Compose driver form is Docker Compose v2
(`docker compose exec`).

For Kubernetes pods with more than one container, select the pod as
`--container POD` and the inner container with `--container-pod-container CTR`;
afpsql emits `kubectl exec POD -c CTR -i -- ...`.

Container bridge prerequisites: the target container must provide `sh` plus one
of `python3`, `python`, or `perl`. For distroless or scratch containers, attach
a small sidecar with a supported interpreter or connect through PostgreSQL's host
network path instead.

The PostgreSQL host/port or socket path is interpreted inside the container.
When `--container` is used alone, the container exec command runs locally.

For containers on a remote SSH host, combine `--ssh` and `--container`. This is
one local afpsql transport chain, not "SSH in, then run psql." It runs the
container exec command on the SSH host, then bridges from inside the container:

```bash
afpsql --ssh root@server --container app-container \
  --container-driver docker \
  --host postgres --port 5432 \
  --user app --dbname appdb \
  --password-secret-env PGPASSWORD \
  --sql "select 1"
```

Use `host.docker.internal` only when the remote Docker environment provides it
(Docker Desktop, or Linux configured with `host-gateway`).

The permission family is still container: reads default to `container-read`, and
writes require `--permission container-write`.

## Connection inputs keep secrets out of shell history

Canonical connection fields:

- `dsn_secret` — PostgreSQL URI
- `conninfo_secret` — libpq-style key/value conninfo
- discrete fields — `host`, `port`, `user`, `dbname`, `password_secret`

CLI secret values can be read from environment variables so they do not appear in
shell history:

```bash
afpsql --dsn-secret-env DATABASE_URL --sql "select 1"
afpsql --password-secret-env PGPASSWORD --host localhost --sql "select 1"
```

Environment fallback also reads standard PostgreSQL variables:

- `PGHOST`
- `PGPORT`
- `PGUSER`
- `PGDATABASE`
- `PGPASSWORD`
- `PGSSLMODE`

## Managed psql wrapper is script compatibility, not a new runtime

Install the wrapper only when existing non-interactive scripts should call
`psql` and receive structured afpsql output:

```bash
afpsql psql status
afpsql psql install
afpsql psql status
```

Use `--bin-dir` for a custom location:

```bash
afpsql psql install --bin-dir ~/.local/bin
export PATH="$HOME/.local/bin:$PATH"
afpsql psql status --bin-dir ~/.local/bin
```

Check `active_in_path: true`. The wrapper is managed only when it contains the
afpsql marker; unmanaged system `psql` binaries are not overwritten.

## Non-goals: not psql, not an ORM, not a pooler

`afpsql` deliberately does not provide:

- interactive psql terminal behavior
- psql meta-command compatibility
- table/text output compatibility as a runtime contract
- ORM/query-builder abstractions
- database admin UI behavior
- high-performance pooler semantics
- automatic remote/local host classification

## License

MIT
