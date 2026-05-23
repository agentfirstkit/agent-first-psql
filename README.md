# Agent-First PSQL

A PostgreSQL tool for AI agents — SQL in, typed rows out, on a connection that stays open.

## The problem: psql was built for a person at a terminal

The standard PostgreSQL client, `psql`, was built for a person at a terminal. It prints results as tables drawn with text, and it opens a fresh connection every time you run it.

An agent reading those tables has to parse the layout to find a value. When a query fails, it gets an error sentence, not a code it can act on. And an agent running a whole session of queries pays the connection and handshake cost over and over.

## What it does: typed JSON rows on a persistent connection

Agent-First PSQL is the same database, addressed differently. SQL goes in; rows come back as structured JSON with real types. A database error comes back as a structured event carrying its exact `SQLSTATE` code — so the agent knows *which* error, not just *that* one happened. And the connection stays warm across the whole session.

- **Typed rows out.** Query results are structured JSON with proper types, not text tables.
- **Errors you can act on.** Every database error is a structured event with its `SQLSTATE` code; a failed query is data, not a crash.
- **Stays connected.** A long-lived pipe mode reuses one connection and handles many queries at once, including streaming large result sets.
- **Safe by default.** Parameters are bound positionally (`$1`, `$2`), never pasted into SQL text.
- **Knows `psql`.** A `psql`-compatible mode accepts familiar connection flags and translates them.

## Where to use it: queries, long sessions, and precise error handling

- **An agent querying a database** — it reads typed JSON rows directly, with no table-parsing.
- **A session of many queries** — pipe mode keeps one connection warm instead of reconnecting each time.
- **Handling failures precisely** — branch on `SQLSTATE`; a unique-violation is data, not a crash.
- **Dropping into existing scripts** — `psql`-compatible flags mean little has to change.

## Install

```bash
brew install agentfirstkit/tap/afpsql   # macOS / Linux
cargo install agent-first-psql          # any platform
```

## Docs

- [Overview](docs/overview.md) — the full guide: modes, parameters, and connection setup
- [CLI](docs/cli.md) — command and flag reference
- [Protocol Reference](docs/reference.md) — the complete field specification
- [Design](docs/design.md) — architecture and principles

## License

MIT
