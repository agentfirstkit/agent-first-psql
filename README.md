# Agent-First PSQL

A PostgreSQL interface for AI agents: reliable, structured, explicit, and read-only by default.

## The problem: a terminal transcript is not a database contract

Classic `psql` is excellent for a human at a terminal. It is not a stable
contract for an agent. It renders tables as text, mixes human interaction with
execution, and turns many failures into prose that an agent has to guess about.

`afpsql` gives agents a dependable PostgreSQL contract:

- **Structured results.** Rows, columns, command tags, logs, and errors are emitted as machine-readable Agent-First Data events.
- **Machine-readable failures.** PostgreSQL execution errors carry `SQLSTATE`; connection-time PostgreSQL rejections preserve SQLSTATE diagnostics on `connect_failed`; runtime/protocol failures use stable `error_code` values and actionable hints.
- **Safe write boundary.** Native CLI and pipe mode default to PostgreSQL read-only transactions; writes must opt in with explicit permission.
- **Predictable session state.** Pipe named sessions map to stable PostgreSQL backend sessions with FIFO execution, so temp tables, GUCs, and other session state behave predictably.
- **No SQL guesswork.** Runtime behavior is derived from PostgreSQL metadata, not SQL-text heuristics.
- **First-class SSH/container boundaries.** Use `--ssh`, `--container`, or `--ssh + --container` to keep the agent local while crossing server, container, and remote-container boundaries with structured output and transport-specific permissions.
- **psql compatibility where it helps.** `--mode psql` translates common non-interactive psql flags for scripts, while preserving psql's writable default.

The goal is reliability for agents, not being a high-throughput pooler or an
interactive database UI. Reusing a backend session is part of the reliability
contract for session state; any latency benefit is secondary.

## Where to use it: read checks, safe writes, stateful sessions, and script bridges

Use native CLI mode for one agent action:

```bash
afpsql --host 127.0.0.1 --port 5432 --user app --dbname appdb \
  --sql "select id, status from jobs where id = $1" \
  --param 1=123
```

Use pipe mode when an agent needs a long-running conversation with the database,
especially when later statements depend on PostgreSQL session state:

```bash
afpsql --mode pipe --dsn-secret-env DATABASE_URL
```

Use psql-compatible mode only for non-interactive script compatibility:

```bash
afpsql --mode psql -h 127.0.0.1 -p 5432 -U app -d appdb -c "select 1"
```

Human terminal sessions, prompts, and psql meta-commands are intentionally out of
scope. Use the original PostgreSQL `psql` binary for those.

## Write safety: read by default, explicit by permission

Native `afpsql` and pipe mode are read-only by default:

- direct connection default: `read`
- afpsql SSH transport default: `ssh-read`
- afpsql container transport default: `container-read`

Writes are explicit:

```bash
afpsql --permission write \
  --sql "update jobs set checked_at = now() where id = $1" \
  --param 1=123
```

SSH transport has its own write permission so agents cannot silently turn a
remote/local boundary into a write path:

```bash
afpsql --permission ssh-write --ssh user@server --host 127.0.0.1 --port 5432 \
  --user app --dbname appdb \
  --password-secret-env PGPASSWORD \
  --sql "update jobs set checked_at = now() where id = $1" \
  --param 1=123
```

Container transport also has its own write permission:

```bash
afpsql --permission container-write --container pg-container \
  --dsn-secret-env DATABASE_URL \
  --sql "update jobs set checked_at = now() where id = $1" \
  --param 1=123
```

`--mode psql` deliberately keeps psql's writable default for script
compatibility and does not expose afpsql permission flags.

## First-class remote and container access: keep the agent local

Keep `afpsql` on the machine where the agent runs. SSH and container access are
core transports, not recipes for shelling into another environment to run human
`psql`. If PostgreSQL only listens on the server, use afpsql's SSH transport
instead of installing afpsql on that server or asking the agent to run human
`psql` over SSH:

```bash
afpsql --ssh user@server --host 127.0.0.1 --port 5432 \
  --user app --dbname appdb \
  --password-secret-env PGPASSWORD \
  --sql "select now()"
```

If the working manual command is `docker exec CONTAINER psql ...` or an
equivalent Podman, nerdctl, Compose, or Kubernetes exec, use container transport
instead of container-local `psql`. The container does not need afpsql or psql;
afpsql uses a no-TTY exec bridge through the selected driver:

```bash
afpsql --container pg-container \
  --dsn-secret 'postgresql://app:pw@127.0.0.1:5432/appdb' \
  --sql "select now()"
```

For container-local Unix sockets, pass the socket directory as `--host`:

```bash
afpsql --container pg-container \
  --host /var/run/postgresql --port 5432 \
  --user app --dbname appdb \
  --sql "select current_user"
```

For peer-authenticated sockets, add `--container-user` to run the bridge as the
matching container OS user.

For containers on a remote SSH host, combine afpsql's existing SSH transport
with container transport. Do not SSH in and then run a container-local `psql`;
local `afpsql` drives both boundaries. The container exec command runs on the
SSH host, and permissions stay in the container family:

```bash
afpsql --ssh root@server --container app-container \
  --container-driver docker \
  --host postgres --port 5432 \
  --user app --dbname appdb \
  --password-secret-env PGPASSWORD \
  --sql "select 1"
```

Use `--container-driver podman|nerdctl|compose|kubectl` when the target uses a
non-default exec driver. `--container-runtime` can override the executable path,
for example `--container-runtime docker-compose` with `--container-driver
compose` for Compose v1. Use named scope flags such as `--container-context`,
`--container-namespace`, `--container-compose-file`, and
`--container-compose-project` instead of raw driver option passthrough.

Use `host.docker.internal` only when the Docker environment provides it (Docker
Desktop, or Linux configured with `host-gateway`).

For socket/peer-auth and sudo bridge cases, see the [Overview](docs/overview.md).

## Adopt it: make afpsql the agent's PostgreSQL interface

Agent-First PSQL is most useful when the agent treats it as the default way to
touch PostgreSQL, and treats human `psql` as the fallback for interactive admin
work. Paste this to your coding agent:

> Ensure `afpsql` is installed locally (`brew install agentfirstkit/tap/afpsql`
> or `cargo install agent-first-psql` if missing). Then run
> `afpsql skill install` and verify `afpsql skill status`. After that, follow
> the installed Agent-First PSQL skill for PostgreSQL work. If I ask to replace
> non-interactive `psql`, run `afpsql psql install` and verify
> `afpsql psql status` reports `active_in_path: true`.

Manual install, if you are doing it yourself:

```bash
brew install agentfirstkit/tap/afpsql   # macOS / Linux
cargo install agent-first-psql          # any platform
```

For agents with local skill support, install the embedded
[Agent Skill](skills/agent-first-psql.md) from the binary:

```bash
afpsql skill status
afpsql skill install
afpsql skill status
```

To replace `psql` for non-interactive scripts:

```bash
afpsql psql status
afpsql psql install
afpsql psql status
```

When status reports `active_in_path: true`, ordinary script calls keep their
psql-shaped arguments and return structured afpsql events:

```bash
psql -h 127.0.0.1 -p 5432 -U app -d appdb -c "select 1 as n"
```

The wrapper is only for non-interactive `psql` calls. Human terminal sessions,
prompts, and meta-commands should use the original PostgreSQL `psql` binary.

## Docs

- [Overview](docs/overview.md) — agent usage guide: modes, permissions, sessions, SSH, and container
- [Protocol Reference](docs/reference.md) — exact runtime fields and event schema
- [Agent Skill](skills/agent-first-psql.md) — behavior rules for AI-assisted database access
- [CLI](docs/cli.md) — generated command and flag reference
- [Design](docs/design.md) — reliability-first architecture and non-goals

## License

MIT
