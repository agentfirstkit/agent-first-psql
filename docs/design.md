# Agent-First PSQL — Design

## Purpose: a database contract agents can rely on

`afpsql` is a reliability layer between AI agents and PostgreSQL. It gives an
agent a stable operational contract: structured events, explicit permission
boundaries, predictable session state, and machine-readable failures.

It is not designed as a general-purpose high-performance pooler. Backend reuse is
used to make session semantics reliable for agents, not to promise throughput,
fair scheduling, or database-side load balancing.

## The problem: terminal clients make agents guess

Agents often reach PostgreSQL through terminal tooling built for humans. That
creates brittle automation:

1. Results are rendered as text tables instead of typed records.
2. Errors are prose instead of structured data with stable codes.
3. Writes can happen accidentally when a generated SQL statement is wrong.
4. Remote access often pushes agents toward SSHing into servers and running
   human `psql` commands.
5. Stateful multi-step work is unclear when every command opens an unrelated
   backend session.
6. Large results can flood stdout instead of using a bounded protocol.

`afpsql` addresses these with one Agent-First Data runtime protocol.

## Product boundary: one runtime, several entry points

`afpsql` has one runtime interface. All execution modes feed the same
agent-first core.

Modes:

- native CLI: one request, one structured response stream, exit
- pipe: long-lived JSONL runtime with named sessions and FIFO execution
- psql compatibility: argument translation for non-interactive scripts only

Non-goals:

- no interactive psql terminal behavior
- no psql meta-command compatibility (`\d`, `\x`, `\timing`, ...)
- no table/text output compatibility as the runtime contract
- no client-side SQL template interpolation
- no ORM/query-builder abstractions
- no database administration UI
- no high-performance pooler semantics
- no automatic remote/local host classification

## Core principles: reliability over throughput

1. Reliability over throughput.
2. Structured stdout events are the protocol; stderr is not a runtime channel.
3. Native/pipe writes require explicit permission.
4. PostgreSQL `SQLSTATE` is preserved for database errors.
5. Permission, validation, transport, and protocol errors include actionable hints.
6. Dynamic values use PostgreSQL parameters, never client text interpolation.
7. Session state is explicit: pipe named sessions are stable backend sessions.
8. Per-session query execution is FIFO.
9. Runtime behavior is based on PostgreSQL metadata, not SQL-text heuristics.
10. `psql mode` is compatibility translation only; it does not fork runtime semantics.

## Execution architecture: translate inputs, preserve one contract

High-level layering:

- CLI parser translates native flags or psql-compatible flags into canonical requests.
- Pipe reader validates JSONL protocol input and queues work by session.
- Handler resolves session config, permissions, timeouts, cancellation, and output routing.
- `DbExecutor` is the database adapter boundary.
- PostgreSQL execution uses `tokio-postgres` and transaction settings derived from resolved options.
- SSH transport is an implementation detail of session connection setup.

The user-facing model should stay simple: an agent sends SQL plus params and gets
structured events back.

## Permission model: writes cross an explicit boundary

Native CLI and pipe mode resolve a `permission` value for each query:

| Transport | Default | Write permission |
|---|---|---|
| direct PostgreSQL connection | `read` | `write` |
| afpsql SSH transport | `ssh-read` | `ssh-write` |

`read` and `ssh-read` start PostgreSQL read-only transactions. A write attempt in
that transaction fails as a `sql_error` with SQLSTATE `25006`.

Permission values are intentionally tied to transport:

- direct sessions accept only `read` or `write`
- afpsql SSH sessions accept only `ssh-read` or `ssh-write`

Mismatches fail before execution as `invalid_request` with a hint that tells the
agent which permission family to use.

`psql mode` keeps psql's writable default for script compatibility and does not
expose native permission flags.

## Session semantics: named sessions mean backend state

A pipe named session is intended to correspond to one PostgreSQL backend session
for as long as the session config remains valid and the process remains alive.
This lets agents rely on PostgreSQL session state, including temp tables and
session-level settings, within that named session.

Rules:

- queries in the same named session run FIFO
- different named sessions are isolated
- config changes invalidate affected sessions
- invalidation creates a new backend session on next use
- checked-out work must keep any required transport resources alive until it finishes

This is a reliability contract. It should not be documented as a pooler or a
performance feature.

## Protocol shape: every recoverable outcome is an event

Input commands:

- `query`
- `cancel`
- `config`
- `ping`
- `close`

Output events:

- `result`
- `result_start`
- `result_rows`
- `result_end`
- `sql_error`
- `error`
- `config`
- `pong`
- `close`
- `log`

Every recoverable runtime condition should be represented by one of these stdout
events. Startup argument parsing can still exit with code `2`, but it should use
structured CLI error output when possible.

## Parameter binding: values never become SQL text

When values are dynamic, clients should use `$N` placeholders and `params`.

```json
{"code":"query","id":"q1","sql":"select * from users where id = $1","params":[123]}
```

Validation rules:

1. Placeholder count must match `params` length, using prepared-statement metadata.
2. Invalid client-side shapes or local binding conversions return `error_code:"invalid_params"`.
3. PostgreSQL server conversion/execution failures remain `sql_error` events with the original SQLSTATE.

Unsupported by design:

- `:name` interpolation
- raw text expansion in SQL templates
- SQL text scanning to infer placeholders or statement kind

## Result handling: bounded inline, explicit streaming

Row/command behavior is decided from PostgreSQL statement metadata after prepare:

- statement has result columns -> row result path (`result` or streamed `result_*`)
- statement has no result columns -> command result path

Inline results are bounded by:

- `inline_max_rows`
- `inline_max_bytes`

When streaming is enabled:

1. emit `result_start` with column metadata
2. read PostgreSQL rows incrementally
3. emit repeated `result_rows` batches
4. emit `result_end` with totals in `trace`

If streaming is off and limits are exceeded, return
`error_code:"result_too_large"` and roll back the transaction.

## Error taxonomy: SQLSTATE or actionable runtime code

### `sql_error`

PostgreSQL execution failure. Include:

- `sqlstate`
- `message`
- optional `detail`, `hint`, `position`
- `trace`

Agents should branch on `sqlstate`, not parse message text.

### `error`

Client/protocol/runtime failure. Include:

- `error_code`
- `error`
- optional `hint`
- `retryable`
- `trace`

Permission mismatches, validation errors, connection setup failures, unsupported
TLS settings, and SSH transport validation should use this path.

## psql compatibility boundary: scripts yes, terminal semantics no

`psql mode` exists to let non-interactive scripts use familiar flags while
receiving structured afpsql events.

It may translate:

- `-c` / `--command`
- `-f` / `--file`
- `-l` / `--list`
- `-h`, `-p`, `-U`, `-d` and long aliases
- numeric `-v N=value` bind parameters
- selected non-interactive behavior flags that are safe to ignore or translate

It must reject or mark unsupported:

- interactive password prompts
- single-step/single-line interactive modes
- no command source
- meta-command workflows
- afpsql native permission flags
- afpsql SSH transport extension flags

## Implementation guardrails: protect the reliability contract

- Keep runtime errors structured and on stdout.
- Add tests for each permission boundary and hint.
- Preserve session/tunnel lifetimes with active work rather than only map entries.
- Avoid destructive changes to session state on config updates until active work is safe.
- Keep generated CLI docs in sync with `--help-markdown`.
- Preserve `clippy.toml` bans that prevent SQL keyword scanning and stderr protocol leaks.
