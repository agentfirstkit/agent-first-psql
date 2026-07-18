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

For flag-level detail, run `afpsql --help` or `afpsql --help --recursive --output markdown`. This
skill covers behavior, decisions, and recovery only.

## Core Rules

- Treat stdout as the protocol: parse strict Agent-First Data envelopes by
  top-level `kind`. Business result codes stay at `result.code`; failures use
  `error.code`, `error.message`, and `error.retryable`.
- When only reads are needed, prefer `afpsql-readonly` as a narrow client guard.
  It hard-rejects PostgreSQL write permissions, read-write pipe transactions,
  transaction-control SQL, and psql translation. It still permits SQL/config
  files, arbitrary explicit secret-env names, SSH options, custom container
  runtimes, redirects, and skill management; it is not a host sandbox.
- For adversarial isolation, pair `afpsql-readonly` with a dedicated PostgreSQL
  reader role. A host wildcard still authorizes caller-selected database and
  SSH/container targets, network connections, every row that role can read,
  local file/environment reads, and process-spawning transport options. Approve
  a wildcard only when that full scope matches host policy. Use an
  administrator-locked profile when target and transport inputs must be fixed.
- Default to read-only. Native CLI and pipe mode require explicit write
  permissions: `write`, `ssh-write`, or `container-write`.
- Use `--ssh`, `--container`, or `--ssh + --container` as afpsql transports;
  keep afpsql local unless the user explicitly asks for server-side tools.
- For SSH jump hosts, keep using afpsql transport. If every hop is reachable
  from the local OpenSSH client, use `--ssh-option ProxyJump=bastion`. If a
  later hop is reachable only from an earlier host, repeat `--ssh-via` in chain
  order and put the final database host in `--ssh`; e.g.
  `--ssh-via ubuntu@jump1 --ssh-via ubuntu@jump2 --ssh ubuntu@db`.
- Use `$1..$N` placeholders plus `--param N=value` / JSON `params`; do not
  interpolate user data into SQL text. `--param` values pass to PostgreSQL
  as text — string forms like `"00123"` and `NUMERIC` precision survive.
- In shell commands, quote SQL containing `$1..$N` placeholders with single
  quotes, or use `--sql-file` / pipe mode JSON. Do not put such SQL in double
  quotes: shells expand `$1` and `$2` before `afpsql` sees the SQL, often into
  empty strings that cause PostgreSQL syntax errors.
- Use pipe mode and named sessions when transaction/session state, FIFO query
  ordering, cancellation, or streaming matters.
- In pipe mode, send `{"code":"session_info","session":"NAME"}` once before
  running queries to discover that session's `transport_kind`,
  `permission_default`, inline/batch limits, stream default, timeouts, and
  resolved `database`/`user`/`host`/`server_version`. This avoids probing
  limits or identity with failing queries.
- Keep PostgreSQL secret env names conventional (`PGPASSWORD`, `DATABASE_URL`);
  do not invent names such as `PGPASSWORD_SECRET`.
- When an application already stores a connection string or password in JSON,
  TOML, YAML, or dotenv, prefer `--dsn-secret-config FILE DOT_PATH`,
  `--conninfo-secret-config FILE DOT_PATH`, or
  `--password-secret-config FILE DOT_PATH`. Do not assemble Ruby/jq/yq command
  substitutions or shell out to another tool: afpsql reads the value once
  in-process through Agent-First Data's document layer, and config sources
  are mutually exclusive with direct/env flags for a slot.
- `afpsql-readonly` accepts config secret sources, but doing so reads the exact
  local file selected by the caller. Its guarantee remains database read-family
  permission, not absence of local file, process, or network side effects.
- In sandboxed agents, if a known-good local TCP read returns immediate
  `connect_failed`, rerun once with approval if available before changing SQL or
  connection details.

## Discovering Schema

Prefer `afpsql inspect` over hand-writing `information_schema` /
`pg_catalog` queries:

- `afpsql inspect databases` — databases on the server with size, encoding,
  collate/ctype, and connection facts (`--all` also lists template databases).
- `afpsql inspect database` — summary of the connected database: schema, table,
  view, materialized-view, and sequence counts plus total size.
- `afpsql inspect schemas` — user-visible schemas with object counts and size.
- `afpsql inspect schema [--schema X] [--like P]` — full metadata export for one
  schema: relations, columns, constraints, indexes, triggers, sequences,
  extensions, views/materialized views, and non-extension functions.
- `afpsql inspect snapshot [--schema X] [--like P]` — stable full-schema snapshot
  shape for downstream tooling or agent-side comparison.
- `afpsql inspect tables [--schema X] [--like P]` — tables in a schema with owner,
  estimated row count, and size.
- `afpsql inspect views [--schema X] [--like P]` — views (regular and materialized)
  in a schema with owner.
- `afpsql inspect indexes [--schema X] [--table T] [--stats]` — indexes with
  definitions, size, validity flags, and optional PostgreSQL built-in
  `pg_stat_user_indexes` counters. `--stats` does not require an extension, but
  counters follow PostgreSQL stats reset/window semantics.
- `afpsql inspect table NAME` — column list with precise types, nullability,
  defaults, primary-key flag, and comments (accepts `schema.table`; defaults to
  `public`).
- `afpsql inspect table NAME --full` — table-focused metadata export including
  relation, columns, constraints, indexes, triggers, and sequence/default
  relationships.

For query plans, wrap with `--explain` (`EXPLAIN (FORMAT JSON)`) or
`--explain-analyze` (also runs the statement; writes still need write
permission). The plan JSON arrives in a normal `kind:"result"` event under
`result.rows`.

## Validating Before Executing

`afpsql --dry-run --sql '...' --param 1=... [--param 2=...]` opens a
connection, runs `PREPARE` inside a transaction that is rolled back, and
emits a `kind:"result"` event whose `result.code` is `dry_run`, with the inferred `param_types`, output
`columns`, and any prepare error. Use this to catch placeholder
mismatches, missing tables, and type confusion before letting a query
actually run.

## Branching on Failures

- `kind:"error"` with `error.code:"sql_error"` — PostgreSQL rejected the SQL. Branch on `error.sqlstate`
  for typed handling (`25006` read-only tx, `42P01` missing relation,
  `23505` unique violation, etc.). Do not scrape `message` text when a
  SQLSTATE is present.
- Other `kind:"error"` events are non-SQL failures (connect, cancel, invalid request,
  config). Branch on `error.code` first: `connect_failed`, `cancelled`,
  `invalid_request`, `invalid_params`. Connect failures may also carry
  `sqlstate`/`message`/`detail` populated from the server-side rejection.
- Honor `retryable: true/false`. Only retry when `true`, and only after
  correcting whatever the hint pointed at. `retryable:false` means the
  same input will fail the same way.
- After a successful `cancel`, never resubmit the cancelled `id` — pick a
  fresh id. Cancellation is final.

## Results that Don't Fit Inline

If a `kind:"result"` event carries `result.truncated:true`, the underlying
statement still ran in full, but `result.rows` is only a prefix
(see `result.truncated_at_rows` / `result.truncated_at_bytes`). For `UPDATE ...
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
- `--ssh-via` is repeatable and means "local SSHs to this hop, that hop SSHs to
  the next hop, and the final `--ssh` host runs the PostgreSQL bridge." The
  PostgreSQL `--host/--port` are interpreted on the final host, so
  `--host localhost --port 5432` means final-host localhost, not workstation
  localhost. The final host needs `python3`, `python`, or `perl` for the bridge.
- `--ssh-option` is OpenSSH `-o` passthrough and is repeatable; use it for
  bastion/jump-host setups such as `ProxyJump=bastion` when local OpenSSH can
  authenticate to the final host through the jump. Use `--ssh-via` instead
  when hop-to-hop credentials live on the intermediate hosts.
- SSH sudo bridge is a last-resort fallback for socket/peer setups. Prefer a
  password-authenticated database role or peer mapping when possible.
- Container transport runs a no-TTY stdio bridge. The target container needs
  `sh` plus one of `python3`, `python`, or `perl`, but does not need afpsql or
  `psql` installed.
- Connecting to a containerized PostgreSQL without a known password: prefer peer
  auth over the container's Unix socket with
  `--container-user <db-os-user> --host /var/run/postgresql`. The `--container-user`
  must match the database role (commonly `postgres`). TCP (`--host 127.0.0.1`)
  requires a password.
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
- Multi-hop SSH with hop-local credentials: repeat `--ssh-via` in order, for
  example `--ssh-via ubuntu@me_automanage --ssh ubuntu@zhiya --host localhost`.
  Do not replace this with nested manual `ssh ... psql`; keep afpsql local so
  stdout remains structured and SSH stderr is captured in the error event.
- SSH `connection refused`: check the remote host/port or Unix socket path, not
  the local workstation's PostgreSQL service.
- `password authentication failed`: TCP auth rules are in effect; use the correct
  secret or switch to a valid remote Unix-socket/peer pattern.
- `peer authentication failed`: the OS user does not match the database role;
  use a matching role, a `pg_ident` mapping, `--container-user`, or an explicit
  SSH sudo bridge only when needed.
- psql mode without `-c`, `-f`, or `-l`: use native afpsql or original human
  `psql` for interactive terminal sessions.
