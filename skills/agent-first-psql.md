---
name: agent-first-psql
description: "Use Agent-First PSQL for reliable agent/script PostgreSQL access: structured stdout events, explicit read/write permissions, stable pipe sessions, psql-compatible non-interactive translation, SSH transport, secret handling, and SQLSTATE-aware troubleshooting."
disable-model-invocation: true
allowed-tools: Bash, Read, Edit, Write, Glob, Grep
---

# Agent-First PSQL

Use this skill when an agent needs to query PostgreSQL, run non-interactive
PostgreSQL scripts, preserve database session state, or connect to PostgreSQL
that is reachable only from a remote server.

`afpsql` is for agent reliability, not for human terminal interaction and not as
a high-performance pooler. Prefer a predictable structured contract over parsing
`psql` tables or SSHing into a server to run human commands.

## Core Rules

- Prefer local `afpsql` for agent/script database work; do not SSH into a server
  just to run human-oriented `psql` unless the user explicitly asks.
- For routine SQL work, do not run `afpsql --help` as a preflight. Use the
  canonical command forms in this skill. Run `--help` only when the user asks,
  when troubleshooting an unknown flag, or when updating afpsql itself.
- The setup checklist is only for installation/preparation tasks. Do not rerun
  setup checks such as `afpsql --version` before every database query if
  `afpsql` is already known to be installed.
- Treat stdout as the protocol: parse `code:"result"`, `code:"sql_error"`,
  `code:"error"`, and other Agent-First Data events instead of human text.
- Default to read-only queries. Native CLI and pipe mode are read-only unless a
  write permission is explicitly requested.
- Ask or verify intent before writes. Use `--permission write` for direct writes
  and `--permission ssh-write` for writes through afpsql SSH transport.
- Do not add permission flags to psql-compatible mode. It preserves psql's
  writable default for script compatibility and does not expose afpsql SSH
  transport extensions.
- Use `$1..$N` placeholders and `--param N=value` / `params` for dynamic values.
  Do not interpolate user data into SQL text.
- Branch on `sql_error.sqlstate` when handling database failures.
- Use pipe mode/named sessions when PostgreSQL session state matters; queries in
  the same session are FIFO and should preserve backend session state until
  invalidation or shutdown.
- Keep secret names compatible with PostgreSQL conventions. Use `PGPASSWORD` as
  the env var name; do not invent `PGPASSWORD_SECRET`.
- In sandboxed coding agents, a direct local TCP database query may need
  command approval. If an otherwise-correct read-only `afpsql` query returns
  an immediate `connect_failed` and the same command can be rerun with approval,
  retry the same command once with approval before changing host, port, user, or
  SQL. Do not run `afpsql --help` to diagnose that case.

## Setup Checklist

When asked to prepare a machine for PostgreSQL agent work:

1. Install or verify `afpsql` on the machine where the agent runs:

```bash
afpsql --version || brew install agentfirstkit/tap/afpsql
```

If Homebrew is unavailable, use:

```bash
cargo install agent-first-psql
```

2. Install this skill if the agent supports local skills:

```bash
afpsql skill status
afpsql skill install
afpsql skill status
```

The default skill target installs personal skills for Codex and Claude Code.
For a Claude Code project-local skill, use:

```bash
afpsql skill install --agent claude-code --scope project
afpsql skill status --agent claude-code --scope project
```

3. If the user wants existing non-interactive scripts to call `psql`, install
   the managed wrapper and verify `active_in_path: true`:

```bash
afpsql psql install
afpsql psql status
```

Do not install afpsql on a database server only to query local PostgreSQL there;
prefer local afpsql with `--ssh user@server`.

## Basic Agent Usage

Read query, native flags:

```bash
afpsql --host 127.0.0.1 --port 5432 --user app --dbname appdb \
  --sql "select id, status from jobs where id = $1" \
  --param 1=123
```

Direct write after confirming intent:

```bash
afpsql --permission write \
  --host 127.0.0.1 --port 5432 --user app --dbname appdb \
  --sql "update jobs set checked_at = now() where id = $1" \
  --param 1=123
```

psql-compatible translation for scripts:

```bash
afpsql --mode psql -h 127.0.0.1 -p 5432 -U app -d appdb -c "select 1"
```

Long-running agent sessions should use pipe mode when ordering, cancellation,
streaming, or PostgreSQL session state matters. Do not describe this as a
performance pool; describe it as predictable session semantics for agents.

## Permission Model

Native CLI and pipe mode permissions:

| Transport | Default | Write permission |
|---|---|---|
| direct PostgreSQL connection | `read` | `write` |
| afpsql SSH transport | `ssh-read` | `ssh-write` |

`read` and `ssh-read` run in PostgreSQL read-only transactions. If a write SQL
fails with SQLSTATE `25006`, decide whether the user actually intended a write;
if yes, rerun with the correct explicit permission.

Permission mismatch errors are pre-execution `code:"error"` events with
`error_code:"invalid_request"` and a corrective `hint`:

- direct session + `ssh-read`/`ssh-write` -> use `read`/`write`
- SSH transport session + `read`/`write` -> use `ssh-read`/`ssh-write`

## Replace Non-Interactive psql

Install the managed wrapper when the user wants existing scripts to call `psql`
but receive afpsql structured output:

```bash
afpsql psql status
afpsql psql install
afpsql psql status
```

For a custom bin directory:

```bash
afpsql psql install --bin-dir ~/.local/bin
export PATH="$HOME/.local/bin:$PATH"
afpsql psql status --bin-dir ~/.local/bin
```

Confirm `active_in_path: true`. The wrapper is managed only when it contains the
afpsql marker, and must not overwrite unmanaged system `psql` binaries.

The wrapper is for non-interactive usage: `psql -c`, `psql -f`, `psql -l`,
connection flags, positional DB names, PostgreSQL URIs, and conninfo strings. If
an invocation needs a terminal prompt or `psql` meta-command behavior, return or
explain the structured error with a `hint` to use the original `psql`.

In psql mode, `-o/--output` means psql-compatible output file routing, and
`-L/--log-file` tees the same structured event stream to a file. Use
`--output-format json|yaml|plain` to choose the afpsql rendering format.

## Remote Server With Local-Only PostgreSQL

If a server does not expose PostgreSQL publicly, keep `afpsql` on the local
machine and use afpsql's built-in SSH transport. The server does not need afpsql
installed.

Prefer remote TCP with a normal database password when available:

```bash
afpsql --ssh user@server \
  --host 127.0.0.1 --port 5432 \
  --user dbuser --dbname appdb \
  --password-secret-env PGPASSWORD \
  --sql "select now()"
```

SSH reads default to `ssh-read`; SSH writes must use `--permission ssh-write`:

```bash
afpsql --permission ssh-write --ssh user@server \
  --host 127.0.0.1 --port 5432 \
  --user dbuser --dbname appdb \
  --password-secret-env PGPASSWORD \
  --sql "update jobs set checked_at = now() where id = $1" \
  --param 1=123
```

The managed `psql` wrapper does not expose afpsql SSH transport extensions. Use
native afpsql or create an SSH tunnel yourself first.

When the server only allows Unix-socket/peer authentication and the SSH login
user maps to the database role:

```bash
afpsql --ssh user@server \
  --host /var/run/postgresql \
  --user user --dbname appdb \
  --sql "select current_user"
```

If the only working manual command is `sudo -u postgres psql`, avoid sudo bridge
mode when possible. Prefer creating a password-authenticated database role for
remote TCP, or a role/`pg_ident` mapping that lets the SSH login user peer-auth
through the remote socket. Use original server-side `psql` for human admin
sessions.

As an advanced fallback, afpsql can run a sudo bridge:

```bash
afpsql --ssh user@server \
  --ssh-sudo-user postgres \
  --ssh-remote-socket /path/to/.s.PGSQL.5432 \
  --user postgres --dbname postgres \
  --sql "select current_user"
```

This uses `sudo -n` and a small Python stdio bridge on the server. It does not
guess socket paths; require an explicit `--ssh-remote-socket`, or set
`--host`/`PGHOST` to the remote socket directory. It fails instead of prompting
if sudo needs a password.

Find the remote socket path with:

```bash
ssh user@server 'sudo -n -u postgres psql -Atqc "show unix_socket_directories; show port"'
```

SSH transport currently expects discrete connection fields; avoid `--dsn-secret`
or `--conninfo-secret` with `--ssh`.

## Secrets

Prefer environment variables for secrets:

```bash
export PGPASSWORD='...'
afpsql --host 127.0.0.1 --port 15432 --user app --dbname appdb \
  --password-secret-env PGPASSWORD \
  --sql "select 1"
```

Also supported:

```bash
afpsql --dsn-secret-env DATABASE_URL --sql "select 1"
afpsql --conninfo-secret "host=127.0.0.1 port=15432 user=app dbname=appdb" --sql "select 1"
```

Never rename PostgreSQL compatibility inputs to fake secret names like
`PGPASSWORD_SECRET`. Keep the compatibility name and rely on AFDATA redaction
options when the name must appear in structured output.

## Troubleshooting

- `sql_error` with SQLSTATE `25006`: a write was attempted in a read-only transaction; confirm intent and rerun with `write` or `ssh-write` only if appropriate.
- `invalid_request` mentioning permission mismatch: use direct permissions (`read`/`write`) for direct sessions and SSH permissions (`ssh-read`/`ssh-write`) for afpsql SSH transport.
- immediate `connect_failed` from local `127.0.0.1`/`localhost` in a sandboxed agent: if the command shape is known good and approval is available, rerun the same read-only command with approval once before treating it as a PostgreSQL configuration problem.
- `connection refused` on `127.0.0.1:15432`: the SSH tunnel is not running, the local port is wrong, or the SSH tunnel failed.
- `password authentication failed`: the forwarded connection uses TCP auth rules; try the remote Unix socket forwarding pattern if bare server-side `psql` works only through socket/peer auth.
- `peer authentication failed`: the SSH login user does not match the requested database role, PostgreSQL lacks a peer mapping, or sudo bridge mode is needed.
- missing sudo bridge socket: pass `--ssh-remote-socket /path/to/.s.PGSQL.PORT`, or set `--host`/`PGHOST` to the remote socket directory; afpsql does not guess.
- `sudo` failure in bridge mode: configure NOPASSWD sudo for the bridge user, use a database role matching the SSH user, or use original server-side `psql` for human admin work.
- No command source in psql mode: use `-c`, `-f`, or `-l`; otherwise use original `psql` for a human terminal session.

## Implementation Checklist

When editing afpsql itself:

1. Preserve the reliability contract before optimizing internals.
2. Use Agent-First Data helpers for protocol builders, CLI errors, output formats, log filters, and redaction.
3. Emit recoverable runtime errors as structured stdout events with `hint`, not as stderr text.
4. Keep native/pipe write permissions explicit and tested.
5. Support psql-compatible non-interactive flags at the argument layer without adding native-only behavior to psql mode.
6. Reject unsupported interactive behavior with structured data rather than silently falling through to human `psql` behavior.
7. Cover psql flag behavior with tests, including accepted aliases, unsupported interactive flags, `-o/--output`, `-L/--log-file`, and secret redaction.
