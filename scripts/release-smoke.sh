#!/usr/bin/env bash
set -euo pipefail

BIN_DIR="${1:-target/release}"
READONLY="$BIN_DIR/afpsql-readonly"
if [ ! -x "$READONLY" ] && [ -x "${READONLY}.exe" ]; then
  READONLY="${READONLY}.exe"
fi
if [ ! -x "$READONLY" ]; then
  echo "missing afpsql-readonly binary under $BIN_DIR" >&2
  exit 1
fi

expect_invalid_request() {
  local label="$1"
  shift
  local output status
  set +e
  output="$($READONLY "$@")"
  status=$?
  set -e
  if [ "$status" -eq 0 ] || ! grep -q '"code":"invalid_request"' <<<"$output"; then
    echo "$label was not rejected before execution: $output" >&2
    exit 1
  fi
}

expect_runtime_failure() {
  local label="$1"
  shift
  local output status
  set +e
  output="$($READONLY "$@")"
  status=$?
  set -e
  if [ "$status" -eq 0 ] || grep -q 'unavailable in afpsql-readonly' <<<"$output"; then
    echo "$label did not reach ordinary readonly runtime semantics: $output" >&2
    exit 1
  fi
}

"$READONLY" --version
expect_invalid_request "write permission" --permission write --sql "select 1"
"$READONLY" skill status >/dev/null
expect_runtime_failure "ProxyCommand" --ssh invalid --ssh-option "ProxyCommand=false" --sql "select 1"
expect_runtime_failure "custom container runtime" --container invalid --container-runtime false --sql "select 1"

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT
sql_path="$tmp_dir/query.sql"
printf '%s\n' 'select 1' >"$sql_path"

# Stream redirection (`--stdout-file`) is a Unix-only capability; on Windows
# afpsql rejects it outright ("stream redirection is only supported on Unix
# platforms"), so the redirect smoke checks below only apply on Unix.
case "$(uname -s 2>/dev/null || echo unknown)" in
MINGW* | MSYS* | CYGWIN* | *NT*)
  echo "skipping Unix-only stream-redirect checks on this platform"
  ;;
*)
  redirect_path="$tmp_dir/output"
  printf '%s' 'preserve-me' >"$redirect_path"
  set +e
  "$READONLY" --stdout-file "$redirect_path" --sql "select 1"
  redirect_status=$?
  set -e
  if [ "$redirect_status" -eq 0 ] || ! grep -q 'connect_failed' "$redirect_path"; then
    echo "readonly redirect did not receive the runtime error" >&2
    exit 1
  fi

  # A redirect flag positioned where a value-skipping arg walk would treat it as
  # the SQL value is still installed by the independent stream-redirect scanner.
  # Ordinary readonly deliberately grants this same host capability as afpsql.
  smuggled_path="$tmp_dir/smuggled"
  set +e
  "$READONLY" --sql "--stdout-file=$smuggled_path"
  smuggled_status=$?
  set -e
  if [ "$smuggled_status" -eq 0 ] || [ ! -e "$smuggled_path" ]; then
    echo "ordinary readonly redirect scanner behavior changed" >&2
    exit 1
  fi
  ;;
esac

expect_runtime_failure "local SQL file" --sql-file "$sql_path"

config_path="$tmp_dir/config.env"
canary="AFPSQL_RELEASE_SMOKE_SECRET_CANARY"
printf '%s\n' "DATABASE_URL=postgresql://user:$canary@127.0.0.1:1/db" >"$config_path"
set +e
config_output="$($READONLY --dsn-secret-config "$config_path" DATABASE_URL --sql 'select 1')"
config_status=$?
set -e
if [ "$config_status" -eq 0 ] || grep -q "$canary" <<<"$config_output"; then
  echo "readonly config secret smoke failed or leaked secret: $config_output" >&2
  exit 1
fi
expect_invalid_request "config-backed write permission" \
  --dsn-secret-config "$config_path" DATABASE_URL \
  --permission write --sql "select 1"

echo "afpsql-readonly release smoke passed"
