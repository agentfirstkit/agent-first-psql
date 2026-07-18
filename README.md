# Agent-First PSQL

A PostgreSQL interface for AI agents: reliable, structured, explicit, and read-only by default.

> **Ask your agent:** "How many orders shipped late last month?"

## The problem: a terminal transcript is not a database contract

Classic `psql` is excellent for a human at a terminal. It is not a stable
contract for an agent. It renders tables as text, mixes human interaction with
execution, and turns many failures into prose that an agent has to guess about.

`afpsql` gives agents a dependable PostgreSQL contract:

- **Structured results.** Rows, columns, command tags, logs, and errors are emitted as machine-readable Agent-First Data events.
- **Machine-readable failures.** PostgreSQL execution errors carry `SQLSTATE`; connection-time PostgreSQL rejections preserve SQLSTATE diagnostics on `error.code="connect_failed"`; runtime/protocol failures use stable `error.code` values and actionable hints.
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
  --sql 'select id, status from jobs where id = $1' \
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

## Read connection secrets from application config

Each connection secret can come directly from a JSON, TOML, YAML, or dotenv
file without shell command substitution or a subprocess: afpsql reads the
value in-process through Agent-First Data's document layer.

```bash
afpsql --dsn-secret-config config.yaml database.url --sql 'select 1'
afpsql --dsn-secret-config .env DATABASE_URL --sql 'select 1'
afpsql --conninfo-secret-config .env PG_CONNINFO --sql 'select 1'
afpsql --host localhost --user app --dbname app \
  --password-secret-config .env PGPASSWORD --sql 'select 1'
```

The syntax is always two separate values: `FILE DOT_PATH`; the `=FILE` form is
not accepted. Within one secret slot, direct, `*-secret-env`, and
`*-secret-config` sources are mutually exclusive. A config value must exist,
be a non-empty string, and is read once during process startup. Pipe mode keeps
that resolved in-memory value across reconnects; it does not watch the file or
accept dynamic config-file references in pipe requests.

Resolved secrets never enter argv, a temporary environment variable, startup
logs, errors, or config responses. Runtime config output represents a configured
`dsn_secret`, `conninfo_secret`, or `password_secret` as `***`. Startup logging
may include only the source kind, file path, and dot-path.

## Write safety: read by default, explicit by permission

Native `afpsql` and pipe mode are read-only by default:

- direct connection default: `read`
- afpsql SSH transport default: `ssh-read`
- afpsql container transport default: `container-read`

Writes are explicit:

```bash
afpsql --permission write \
  --sql 'update jobs set checked_at = now() where id = $1' \
  --param 1=123
```

SSH transport has its own write permission so agents cannot silently turn a
remote/local boundary into a write path:

```bash
afpsql --permission ssh-write --ssh user@server --host 127.0.0.1 --port 5432 \
  --user app --dbname appdb \
  --password-secret-env PGPASSWORD \
  --sql 'update jobs set checked_at = now() where id = $1' \
  --param 1=123
```

Container transport also has its own write permission:

```bash
afpsql --permission container-write --container pg-container \
  --dsn-secret-env DATABASE_URL \
  --sql 'update jobs set checked_at = now() where id = $1' \
  --param 1=123
```

`--mode psql` deliberately keeps psql's writable default for script
compatibility and does not expose afpsql permission flags.

## Narrow client guard for read access

Use `afpsql-readonly` as a client-side guard when an agent needs database reads.
It hard-rejects write permissions, read-write pipe transactions, transaction
control SQL, and psql translation mode while continuing to support the same
SQL files, secret env/config sources, SSH options, container runtimes, stream
redirection, and skill management as `afpsql`. Its name promises no PostgreSQL
write permission; it is not a general host capability sandbox.

This executable is not the database authorization boundary. In adversarial
deployments, use a dedicated PostgreSQL reader role with only the required
`CONNECT`, schema `USAGE`, relation `SELECT`, and audited function `EXECUTE`
privileges. A host rule such as `Bash(afpsql-readonly:*)` still authorizes
caller-selected local files and environment variables, arbitrary database and
SSH/container targets, process-spawning transport options, network connections,
and all data the reader role may see; evaluate that scope against the host's
policy before whitelisting it.

For a host rule restricted to one administrator-defined endpoint, install a
profile executable name such as `afpsql-readonly-production` (a symlink or copy
of `afpsql-readonly`) and a matching
`/etc/afpsql/readonly-profiles/production.json`. The JSON has the same flat
connection/SSH/container fields as a pipe session. It must be root-owned and
not group/world writable. This locked executable rejects every CLI connection
or transport override and every pipe session patch; the agent may supply only
query/result-shaping inputs. Whitelist the profile executable itself rather
than parsing flags in a shell-prefix rule.

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

For two-hop SSH, keep `afpsql` in charge of the transport and pass the jump host
through OpenSSH options instead of creating an external temporary port forward:

```bash
afpsql --ssh user@db-server \
  --ssh-option ProxyJump=bastion \
  --host 127.0.0.1 --port 5432 \
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

For socket/peer-auth and sudo bridge cases, use `--ssh-remote-socket` with
`--ssh-sudo-user` (see the [CLI reference](docs/cli.md)).

## Adopt it: hand afpsql to your agent

The quickest way to find out whether afpsql fits your setup is to let your agent
read it and tell you. Paste this to your agent:

> Read what Agent-First PSQL is at https://agentfirstkit.com/agent-first-psql,
> then tell me in plain terms what it would do for me and whether it fits what
> I'm working on. If it's a fit, install it — the prebuilt package for the quick
> path, or build from source after a quick security review of the repo if you'd
> rather read what you run — then run `afpsql skill install` so you follow its
> behavior rules.

If it's a fit, install it — a prebuilt package, or from source if you want to
read it first:

```bash
# prebuilt binary
brew install agentfirstkit/tap/afpsql   # macOS / Linux
scoop bucket add agentfirstkit https://github.com/agentfirstkit/scoop-bucket && scoop install afpsql   # Windows

# or build from source after reviewing the repo
git clone https://github.com/agentfirstkit/agent-first-psql
cargo install --path agent-first-psql
```

Then install the embedded [Agent Skill](skills/agent-first-psql/SKILL.md) so the agent
follows afpsql's behavior rules. `skill install` targets Codex, Claude Code,
opencode, and Hermes; `skill status` reports whether each install is present,
valid, and current:

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

- [CLI](docs/cli.md) — generated command, flag, and concept reference
- [Protocol Reference](docs/reference.md) — exact runtime fields and event schema
- [Agent Skill](skills/agent-first-psql/SKILL.md) — behavior rules for AI-assisted database access
- [Design](docs/design.md) — reliability-first architecture and non-goals

## License

MIT
