---
name: agent-first-psql
description: "Reliable agent/script access to PostgreSQL via structured stdout events, explicit read/write permissions, and SSH/container transports. Use instead of parsing human psql output or SSHing in to run psql."
disable-model-invocation: true
allowed-tools: Bash, Read
---

# Agent-First PSQL

Use this skill when an agent needs PostgreSQL access that is structured,
read-only by default, safe for scripts, or reachable only across SSH/container
boundaries. Prefer `afpsql` over parsing human `psql` tables, SSHing in to run
`psql`, or `docker exec`/`kubectl exec` with human output.

For flag-level detail, run `afpsql --help` or `afpsql --help-markdown`. This
skill covers behavior, decisions, and recovery only.

## Core Rules

- Treat stdout as the protocol: parse Agent-First Data events such as
  `code:"result"`, `code:"sql_error"`, and `code:"error"`.
- Default to read-only. Native CLI and pipe mode require explicit write
  permissions: `write`, `ssh-write`, or `container-write`.
- Use `--ssh`, `--container`, or `--ssh + --container` as afpsql transports;
  keep afpsql local unless the user explicitly asks for server-side tools.
- Use `$1..$N` placeholders plus `--param N=value` / JSON `params`; do not
  interpolate user data into SQL text.
- Branch on `sql_error.sqlstate` for database failures; do not scrape message
  text when SQLSTATE is present.
- Use pipe mode and named sessions when transaction/session state, FIFO query
  ordering, cancellation, or streaming matters.
- In pipe mode, send `{"code":"session_info","session":"NAME"}` once before
  running queries to discover that session's `transport_kind`, `permission_default`,
  inline/batch limits, stream default, and timeouts; this avoids probing limits
  with failing queries.
- Keep PostgreSQL secret env names conventional (`PGPASSWORD`, `DATABASE_URL`);
  do not invent names such as `PGPASSWORD_SECRET`.
- In sandboxed agents, if a known-good local TCP read returns immediate
  `connect_failed`, rerun once with approval if available before changing SQL or
  connection details.

## Setup Checklist

Only run setup when asked to prepare or repair the machine; do not run it before
every query.

```bash
afpsql --version || brew install agentfirstkit/tap/afpsql
cargo install agent-first-psql  # fallback when Homebrew is unavailable
```

Install/update local skills:

```bash
afpsql skill status
afpsql skill install
afpsql skill status
```

Install the managed non-interactive `psql` wrapper only when the user wants
existing scripts to invoke `psql` but receive structured afpsql output:

```bash
afpsql psql install
afpsql psql status
```

## Permission Model

| Transport | Default | Write permission |
|---|---|---|
| direct PostgreSQL | `read` | `write` |
| afpsql SSH transport | `ssh-read` | `ssh-write` |
| afpsql container transport | `container-read` | `container-write` |

`read`, `ssh-read`, and `container-read` run SQL in PostgreSQL read-only
transactions. If a write fails with SQLSTATE `25006`, confirm write intent and
rerun with the matching write permission only when appropriate. In psql mode,
do not add permission flags; it preserves psql's writable default for script
compatibility. Pass `--log mode` to surface a
`mode.permission_default_changed` event whenever psql mode bypasses the
native read-only default.

## Canonical Examples

Direct read with bind parameters:

```bash
afpsql --host 127.0.0.1 --port 5432 --user app --dbname appdb \
  --sql 'select id, status from jobs where id = $1' \
  --param 1=123
```

Remote PostgreSQL reachable only from a server:

```bash
afpsql --ssh user@server \
  --host 127.0.0.1 --port 5432 \
  --user app --dbname appdb --password-secret-env PGPASSWORD \
  --sql 'select now()'
```

Container-local PostgreSQL:

```bash
afpsql --container pg-container \
  --host 127.0.0.1 --port 5432 \
  --user app --dbname appdb --password-secret-env PGPASSWORD \
  --sql 'select 1'
```

Non-interactive psql-compatible script translation:

```bash
afpsql --mode psql -h 127.0.0.1 -p 5432 -U app -d appdb -c 'select 1'
```

For writes, `--ssh + --container` combos, Kubernetes pods, Compose drivers, or
other transport combinations, run `afpsql --help` to discover the current flag
set.

## Non-Obvious Behaviors

- SSH transport expects discrete connection fields; avoid `--dsn-secret` and
  `--conninfo-secret` with `--ssh`.
- SSH sudo bridge is a last-resort fallback for socket/peer setups. Prefer a
  password-authenticated database role or peer mapping when possible.
- Container transport runs a no-TTY stdio bridge. The target container needs
  `sh` plus one of `python3`, `python`, or `perl`, but does not need afpsql or
  `psql` installed.
- libpq `PG*` environment variables (`PGHOST`, `PGPORT`, `PGUSER`, `PGDATABASE`,
  `PGPASSWORD`, `PGSSLMODE`) silently fill connection fields not given via
  flags or secrets. Prefer explicit flags for agent runs, and pass `--log connect`
  to surface a `connect.libpq_env_fallback` event listing the variables in use.
- Enable `--log transport` to emit `transport.selected` once per new session,
  including a summary of the selected direct/SSH/container chain.

## Troubleshooting

- `invalid_request` permission mismatch: use `read/write` for direct sessions,
  `ssh-read/ssh-write` for SSH, and `container-read/container-write` for
  container transport.
- SQLSTATE `25006`: the SQL attempted a write in a read-only transaction;
  confirm intent and rerun with the matching write permission.
- `connect_failed` on container transport: the host/port are interpreted inside
  the container; verify the container/pod name, selected pod container,
  PostgreSQL listener, and whether a Unix socket is required.
- Bridge prerequisite errors: install `python3`, `python`, or `perl` in the
  target/sidecar, or connect through a host network path instead.
- SSH `connection refused`: check the remote host/port or Unix socket path, not
  the local workstation's PostgreSQL service.
- `password authentication failed`: TCP auth rules are in effect; use the correct
  secret or switch to a valid remote Unix-socket/peer pattern.
- `peer authentication failed`: the OS user does not match the database role;
  use a matching role, a `pg_ident` mapping, `--container-user`, or an explicit
  SSH sudo bridge only when needed.
- psql mode without `-c`, `-f`, or `-l`: use native afpsql or original human
  `psql` for interactive terminal sessions.
