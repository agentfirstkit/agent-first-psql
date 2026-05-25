# Agent-First PSQL

A PostgreSQL interface for AI agents: reliable, structured, explicit, and safe by default.

## The problem: a terminal transcript is not a database contract

Classic `psql` is excellent for a human at a terminal. It is not a stable
contract for an agent. It renders tables as text, mixes human interaction with
execution, and turns many failures into prose that an agent has to guess about.

`afpsql` gives agents a dependable PostgreSQL contract:

- **Structured results.** Rows, columns, command tags, logs, and errors are emitted as machine-readable Agent-First Data events.
- **Machine-readable failures.** PostgreSQL errors carry `SQLSTATE`; runtime/protocol failures use stable `error_code` values and actionable hints.
- **Safe write boundary.** Native CLI and pipe mode default to PostgreSQL read-only transactions; writes must opt in with explicit permission.
- **Predictable session state.** Pipe named sessions map to stable PostgreSQL backend sessions with FIFO execution, so temp tables, GUCs, and other session state behave predictably.
- **No SQL guesswork.** Runtime behavior is derived from PostgreSQL metadata, not SQL-text heuristics.
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

`--mode psql` deliberately keeps psql's writable default for script
compatibility and does not expose afpsql permission flags or afpsql SSH
transport extensions.

## Remote access: keep the agent local and tunnel PostgreSQL explicitly

Keep `afpsql` on the machine where the agent runs. If PostgreSQL only listens on
the server, use afpsql's SSH transport instead of installing afpsql on that
server or asking the agent to run human `psql` over SSH:

```bash
afpsql --ssh user@server --host 127.0.0.1 --port 5432 \
  --user app --dbname appdb \
  --password-secret-env PGPASSWORD \
  --sql "select now()"
```

For socket/peer-auth and sudo bridge cases, see the [Overview](docs/overview.md).

## Adopt it: make afpsql the agent's PostgreSQL interface

Agent-First PSQL is most useful when the agent treats it as the default way to
touch PostgreSQL, and treats human `psql` as the fallback for interactive admin
work. Paste this to your coding agent:

> Install Agent-First PSQL locally and install/load its Agent Skill. If `afpsql`
> is missing, use `brew install agentfirstkit/tap/afpsql` or `cargo install
> agent-first-psql`. Then read
> https://agentfirstkit.com/agent-first-psql/docs/overview and
> https://agentfirstkit.com/agent-first-psql/docs/agent-skill. Prefer native
> `afpsql` for ad-hoc database work, default to read-only queries, and ask before
> using `--permission write` or `--permission ssh-write`. If I ask to replace
> non-interactive `psql`, run `afpsql psql install` and verify
> `afpsql psql status` reports `active_in_path: true`.

Manual install, if you are doing it yourself:

```bash
brew install agentfirstkit/tap/afpsql   # macOS / Linux
cargo install agent-first-psql          # any platform
```

For agents with local skill support, install or load the
[Agent Skill](skills/agent-first-psql.md) so the permission and session rules
travel with the tool.

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

- [Overview](docs/overview.md) — agent usage guide: modes, permissions, sessions, and SSH
- [Protocol Reference](docs/reference.md) — exact runtime fields and event schema
- [Agent Skill](skills/agent-first-psql.md) — behavior rules for AI-assisted database access
- [CLI](docs/cli.md) — generated command and flag reference
- [Design](docs/design.md) — reliability-first architecture and non-goals

## License

MIT
