# afpsql MCP Server

`afpsql --mode mcp` starts an MCP server over stdio so AI tools can execute
structured PostgreSQL queries.

## Start

```bash
afpsql --mode mcp
```

## Claude Desktop setup

```json
{
  "mcpServers": {
    "afpsql": {
      "command": "afpsql",
      "args": ["--mode mcp"]
    }
  }
}
```

## Tools

### `psql_query`

Run one SQL query and return structured result JSON.

| Parameter | Type | Required | Description |
|---|---|---|---|
| `sql` | string | yes | SQL text |
| `params` | array | no | positional bind values for `$1..$N` |
| `session` | string | no | session id |
| `stream_rows` | boolean | no | stream large results |
| `batch_rows` | integer | no | rows per streamed batch |
| `statement_timeout_ms` | integer | no | per-query timeout |
| `lock_timeout_ms` | integer | no | per-query lock timeout |

Returns one of:

- `result`
- `result_start` + `result_rows` + `result_end`
- `sql_error`
- `error`

Binding policy:

- use `$1..$N` placeholders with `params`
- no text-template interpolation behavior

### `psql_config`

Get/update runtime config and connection defaults.

| Parameter | Type | Description |
|---|---|---|
| `default_session` | string | default session id |
| `sessions` | object | session connection map |
| `inline_max_rows` | integer | inline row cap |
| `inline_max_bytes` | integer | inline payload cap |
| `statement_timeout_ms` | integer | default statement timeout |
| `lock_timeout_ms` | integer | default lock timeout |

Session connection fields:

- `dsn_secret`
- `conninfo_secret`
- `host`
- `port`
- `user`
- `dbname`
- `password_secret`

## Notes

- This MCP interface is AFD-first and does not emulate `psql` behavior.
- `psql mode` is a CLI translation concern and does not affect MCP semantics.
- For full JSONL event streaming semantics, use `--mode pipe`.
