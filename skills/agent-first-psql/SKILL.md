---
name: agent-first-psql
description: "Reliable agent/script access to PostgreSQL via structured AFDATA events, explicit read/write permissions, and SSH/container transports. Use instead of parsing human psql output or SSHing in to run psql."
allowed-tools: Bash, Read
---

# Agent-First PSQL

Use this skill when an agent needs PostgreSQL access that is structured,
read-only by default, safe for scripts, or reachable only across SSH/container
boundaries. Prefer `afpsql` over parsing human `psql` tables, SSHing in to run
`psql`, or `docker exec`/`kubectl exec` with human output.

For flag-level detail, run `afpsql --help`, or `afpsql <command> --help` for one
command. This skill covers behavior, decisions, and recovery only.

## Calling Convention

`afpsql` is compiled from a closed registry: an invocation runs only when it
matches exactly one registered shape.

- There are no short flags. `-c`, `-h`, `-V` are psql spellings and exist only
  behind `--mode psql`.
- A command path comes first, then its arguments: `afpsql inspect tables
  --dsn ...`, never the reverse. Nothing is global.
- One `--help` per command is the whole answer: every shape, complete with its
  optional arguments and closed value sets. There is no second level to ask for
  and no recursive mode. `afpsql --docs` renders the whole registry as Markdown,
  for reading rather than for calling.
- A value is never taken from a token that starts with `-`. SQL that looks like
  a flag is written `--sql=<value>`.
- Rejections name their own classification in `error.code` —
  `cli_unknown_argument`, `cli_unknown_command`, `cli_unregistered_combination`,
  `cli_missing_argument_value`, `cli_invalid_argument_value`,
  `cli_duplicate_argument`, `cli_unexpected_positional`, `cli_invalid_utf8` —
  and always exit 2 with stdout left empty. Branch on the code rather than
  parsing the message. `cli_unregistered_combination` means the arguments were
  individually known but not a registered mix: read the shapes in `--help`
  rather than dropping arguments at random.

## Core Rules

- Parse strict Agent-First Data envelopes by top-level `kind`. Business result
  codes stay at `result.code`; failures use `error.code`, `error.message`, and
  `error.retryable`.
- Read the stream the invocation actually uses. A finite query splits by kind,
  so capture the result from stdout and read diagnostics from stderr. `--mode
  pipe` and `--stream-rows` are ordered event streams and put every event on
  stdout, so read one stream and branch on `kind`. `--output-to` overrides the
  destination, but the streaming shapes have no `split` to select: it is not in
  their output contract.
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
- Use `--ssh`, one `--container-<driver>-*` flag family, or both together as
  afpsql transports; keep afpsql local unless the user explicitly asks for
  server-side tools.
- With `--ssh`, use `--dsn SOURCE` or `--conninfo SOURCE` directly when that is how the application
  stores its connection. afpsql parses the value locally in-process, uses its
  host/port as the PostgreSQL target visible from the final SSH host, and keeps
  the remaining authentication/TLS settings for the bridged connection. Never
  reveal or split a DSN in shell code.
- For SSH jump hosts, keep using afpsql transport. If every hop is reachable
  from the local OpenSSH client, use `--ssh-option ProxyJump=bastion`. If a
  later hop is reachable only from an earlier host, repeat `--ssh-via` in chain
  order and put the final database host in `--ssh`; e.g.
  `--ssh-via ubuntu@jump1 --ssh-via ubuntu@jump2 --ssh ubuntu@db`.
- Use `$1..$N` placeholders plus `--param N=value` / JSON `params`; do not
  interpolate user data into SQL text. `--param` values pass to PostgreSQL
  as text — string forms like `"00123"` and `NUMERIC` precision survive.
  Bare `null`, `true`, and `false` are primitives; use `text:null`,
  `text:true`, or `text:false` when the literal string is intended.
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
  TOML, YAML, or dotenv, prefer `--dsn file:FILE#DOT_PATH` and its
  `--conninfo file:FILE#DOT_PATH` / `--password file:FILE#DOT_PATH`
  siblings. The file and dot path form one typed argument. Do not assemble Ruby/jq/yq
  command substitutions or shell out to another tool: afpsql reads the value once
  in-process through Agent-First Data's document layer.
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
- `afpsql inspect connections [--all]` — one row per server backend with state,
  wait event, ages, and the `max_connections` the count is read against.
  `--all` adds the backends PostgreSQL runs for itself, which that limit does
  not govern.

## Showing Something to a Person

`afpsql ui schema`, `afpsql ui table`, `afpsql ui indexes`, and
`afpsql ui connections` open the same data a person can read in a window
instead of returning it. Reach for one only when a *person* asked to look, or
when you have already read the data and they need to see its shape to answer
you. Never use `ui` to read data yourself: the result carries no rows, only that
the window closed.

These are watch sessions, so the call blocks until the person closes the window.
Treat that closure as "they are done looking", never as approval of anything.
A window that cannot open is an environment problem — report it and fall back to
the matching `inspect` command rather than retrying.

`ui connections` is the one panel meant to outlive your attention: it reloads
itself, so open it, report that it is open, and go back to work rather than
waiting on it. Run it in the background when you have anything else to do, and
leave the interval alone unless the person asked — it is a repeated query
against a server other people are using.

## Asking a Person to Approve a Statement

`afpsql ui plan --sql '...' [--param N=V] [--permission write]` shows one
statement to a person and runs it only if they approve. Use it when a write is
consequential enough that a person should see it first, not as a substitute for
knowing what your own statement does.

- Only an approval runs anything. A closed window, a refusal, and an expired
  credential are all the same answer, and the terminal event says
  `result.code:"ui_plan_refused"` with `executed:false`. Never re-run the
  statement yourself after a refusal, and never read "the window closed" as
  consent.
- On approval the statement runs through the ordinary execution path, so the
  events that follow are the ordinary ones: a `kind:"result"` result, or a
  `sql_error`. Branch on those exactly as you would for `afpsql --sql`.
- The statement is fixed when you invoke the command. Changing a `--sql-file`
  after the window opens changes nothing, and there is no way to amend what the
  person is looking at — refuse and ask again with a new statement instead.
- `afpsql-readonly` refuses a write here as it does everywhere; the window does
  not open at all. Do not reach for `ui plan` to get around a readonly
  capability.

For query plans, add `--explain plan` (`EXPLAIN (FORMAT JSON)`) or
`--explain analyze` (also runs the statement; writes still need write
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
  `invalid_request`, `invalid_params`, `internal_error`. Connect failures may
  also carry `sqlstate`/`message`/`detail` populated from the server-side
  rejection. `internal_error` is afpsql's own fault rather than the request's:
  retry once, restart the session if it repeats, and report it rather than
  working around it.
- Honor `retryable: true/false`. Only retry when `true`, and only after
  correcting whatever the hint pointed at. `retryable:false` means the
  same input will fail the same way.
- After a successful `cancel`, never resubmit the cancelled `id` — pick a
  fresh id. Cancellation is final.

## Row Encoding Fidelity

Rows are normally encoded by PostgreSQL itself, so `numeric`, `timestamptz`,
`uuid`, `interval` and friends keep their exact server representation. A few
statements cannot be encoded that way — utility statements such as `EXPLAIN`
and `SHOW`, and any SQL whose text prevents the wrapper from being built — and
those fall back to a narrower client-side decoder that only handles booleans,
integers, floats, JSON, bytea, and text-like types.

The fallback is announced by the `query.row_encoding_degraded` log event; ask
for it with `--log query.row_encoding_degraded` whenever exact value fidelity
matters. A statement whose columns the narrow decoder cannot represent fails
loudly instead of returning an approximation, so a `kind:"result"` is always
trustworthy — the log only tells you which decoder produced it.

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
  write permission; `read_only` defaults to `true`. Read-write `begin`
  requires explicit `read_only:false` and the matching write permission for
  the session's transport. Every query in that transaction must repeat the
  matching write permission.

## Non-Obvious Behaviors

- SSH and container transports accept DSN, conninfo, or discrete connection
  fields. Their PostgreSQL host/port or Unix socket is interpreted inside the
  final transport boundary. A DSN/conninfo used with either transport must
  resolve to one PostgreSQL endpoint; choose one host explicitly when an
  application DSN contains a failover host list.
- Every `--ssh` connection runs a stdio bridge on the remote host, so that host
  needs `sh` plus any one of `python3`, `python`, or `perl` — not all three.
  There is no local listening port, so nothing else on the workstation can
  reach the database through afpsql's connection. A host missing all three
  interpreters fails with exit 127 and a message naming them.
- `--ssh-via` is repeatable and means "local SSHs to this hop, that hop SSHs to
  the next hop, and the final `--ssh` host runs the PostgreSQL bridge." The
  PostgreSQL `--host/--port` are interpreted on the final host, so
  `--host localhost --port 5432` means final-host localhost, not workstation
  localhost. The bridge runs on that final host.
- `--ssh-option` is OpenSSH `-o` passthrough and is repeatable; use it for
  bastion/jump-host setups such as `ProxyJump=bastion` when local OpenSSH can
  authenticate to the final host through the jump. Use `--ssh-via` instead
  when hop-to-hop credentials live on the intermediate hosts.
- SSH sudo bridge is a last-resort fallback for socket/peer setups. Prefer a
  password-authenticated database role or peer mapping when possible.
- Container transport runs a no-TTY stdio bridge. The target container needs
  `sh` plus one of `python3`, `python`, or `perl`, but does not need afpsql or
  `psql` installed.
- The container driver is inferred from the flag family used, never named
  separately, and two families cannot be combined. Each family carries only the
  options its driver actually has, so an unavailable option has no flag rather
  than a runtime rejection: `kubectl exec` cannot exec as a user, and no kubectl
  flag asks it to.
- Connecting to a containerized PostgreSQL without a known password: prefer peer
  auth over the container's Unix socket with the family's user flag plus
  `--host /var/run/postgresql`. That exec user must match the database role
  (commonly `postgres`). TCP (`--host 127.0.0.1`) requires a password, and the
  kubectl family cannot take this path at all.
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
  output remains structured and SSH stderr is captured in the error event.
- SSH `connection refused`: check the remote host/port or Unix socket path, not
  the local workstation's PostgreSQL service.
- A `single PostgreSQL host and port` error means the DSN/conninfo contains a
  failover list that one SSH/container bridge cannot target; select one host or
  use discrete connection fields for that run.
- `password authentication failed`: TCP auth rules are in effect; use the correct
  secret or switch to a valid remote Unix-socket/peer pattern.
- `peer authentication failed`: the OS user does not match the database role;
  use a matching role, a `pg_ident` mapping, the container family's user flag,
  or an explicit SSH sudo bridge only when needed.
- psql mode without `-c`, `-f`, or `-l`: use native afpsql or original human
  `psql` for interactive terminal sessions.
- `cli_unregistered_combination` on a query: the most common causes are two SQL
  sources (`--sql` with `--sql-file`), a buffering argument on a streaming shape
  (`--dry-run` or `--inline-max-*` with `--stream-rows`), a batching argument
  without it, or two sources for one secret slot.
