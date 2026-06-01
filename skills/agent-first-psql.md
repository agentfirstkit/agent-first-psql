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
  interpolate user data into SQL text. `--param` values pass to PostgreSQL
  as text — string forms like `"00123"` and `NUMERIC` precision survive.
- Use pipe mode and named sessions when transaction/session state, FIFO query
  ordering, cancellation, or streaming matters.
- In pipe mode, send `{"code":"session_info","session":"NAME"}` once before
  running queries to discover that session's `transport_kind`,
  `permission_default`, inline/batch limits, stream default, timeouts, and
  resolved `database`/`user`/`host`/`server_version`. This avoids probing
  limits or identity with failing queries.
- Keep PostgreSQL secret env names conventional (`PGPASSWORD`, `DATABASE_URL`);
  do not invent names such as `PGPASSWORD_SECRET`.
- In sandboxed agents, if a known-good local TCP read returns immediate
  `connect_failed`, rerun once with approval if available before changing SQL or
  connection details.

## Discovering Schema

Prefer `afpsql inspect` over hand-writing `information_schema` /
`pg_catalog` queries:

- `afpsql inspect databases` — non-template databases on the server.
- `afpsql inspect schemas` — user-visible schemas.
- `afpsql inspect tables [--schema X] [--like P]` — tables in a schema.
- `afpsql inspect views [--schema X] [--like P]` — views in a schema.
- `afpsql inspect table NAME` — column list, types, nullability, defaults
  (accepts `schema.table`; defaults to `public`).

For query plans, wrap with `--explain` (`EXPLAIN (FORMAT JSON)`) or
`--explain-analyze` (also runs the statement; writes still need write
permission). The plan JSON arrives in a normal `code:"result"` row.

## Validating Before Executing

`afpsql --dry-run --sql '...' --param 1=... [--param 2=...]` opens a
connection, runs `PREPARE` inside a transaction that is rolled back, and
emits a `code:"dry_run"` event with the inferred `param_types`, output
`columns`, and any prepare error. Use this to catch placeholder
mismatches, missing tables, and type confusion before letting a query
actually run.

## Branching on Failures

- `code:"sql_error"` — PostgreSQL rejected the SQL. Branch on `sqlstate`
  for typed handling (`25006` read-only tx, `42P01` missing relation,
  `23505` unique violation, etc.). Do not scrape `message` text when a
  SQLSTATE is present.
- `code:"error"` — non-SQL failure (connect, cancel, invalid request,
  config). Branch on `error_code` first: `connect_failed`, `cancelled`,
  `invalid_request`, `invalid_params`. Connect failures may also carry
  `sqlstate`/`message`/`detail` populated from the server-side rejection.
- Honor `retryable: true/false`. Only retry when `true`, and only after
  correcting whatever the hint pointed at. `retryable:false` means the
  same input will fail the same way.
- After a successful `cancel`, never resubmit the cancelled `id` — pick a
  fresh id. Cancellation is final.

## Results that Don't Fit Inline

If a `code:"result"` event carries `truncated:true`, the underlying
statement still ran in full, but the returned `rows` is only a prefix
(see `truncated_at_rows` / `truncated_at_bytes`). For `UPDATE ...
RETURNING` this means the writes happened; only the RETURNING projection
was capped. Either narrow the query (`WHERE` / `LIMIT`) or rerun with
`--stream-rows` to receive the full set in batches.

## Multi-Statement Atomicity (Pipe Mode)

Each `query` is its own transaction by default. For atomic multi-statement
work, open an explicit transaction:

```
{"code":"begin","id":"b","permission":"write"}
{"code":"query","id":"q1","sql":"insert into orders ...","options":{"permission":"write"}}
{"code":"query","id":"q2","sql":"update inventory ...","options":{"permission":"write"}}
{"code":"commit","id":"c"}
```

- Tx control flows through the same session FIFO as queries, so input
  order matches PostgreSQL's order.
- A failed query inside an explicit tx is wrapped in a savepoint and
  rolled back individually — the outer tx is NOT aborted, so the agent
  can retry or move on. Send `rollback` to discard everything since
  `begin`, or `commit` to persist what worked.
- `begin` with `read_only:true` opens `BEGIN READ ONLY` and needs no
  write permission. Read-write `begin` requires the matching write
  permission for the session's transport.

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

## Setup Checklist

Only run setup when asked to prepare or repair the machine; do not run it before
every query.

```bash
afpsql --version || brew install agentfirstkit/tap/afpsql
cargo install agent-first-psql  # fallback when Homebrew is unavailable
afpsql skill install            # personal Claude/Codex skill
afpsql psql install             # optional: psql-compatible wrapper
```

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
