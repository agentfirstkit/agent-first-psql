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

# A finite query splits by kind, so these checks must read the stream the event
# actually lands on. Capturing only stdout would make every assertion below pass
# vacuously against an empty string — including the secret-leak check.
OUT=""
ERR=""
STATUS=0
run_readonly() {
  local out_file err_file
  out_file="$(mktemp)"
  err_file="$(mktemp)"
  set +e
  "$READONLY" "$@" >"$out_file" 2>"$err_file"
  STATUS=$?
  set -e
  OUT="$(cat "$out_file")"
  ERR="$(cat "$err_file")"
  rm -f "$out_file" "$err_file"
}

expect_invalid_request() {
  local label="$1"
  shift
  run_readonly "$@"
  if [ "$STATUS" -eq 0 ] || ! grep -q '"code":"invalid_request"' <<<"$ERR"; then
    echo "$label was not rejected before execution: stdout=$OUT stderr=$ERR" >&2
    exit 1
  fi
  if [ -n "$(tr -d '[:space:]' <<<"$OUT")" ]; then
    echo "$label wrote a rejection to the result stream: $OUT" >&2
    exit 1
  fi
}

expect_runtime_failure() {
  local label="$1"
  shift
  run_readonly "$@"
  if [ "$STATUS" -eq 0 ] || grep -q 'unavailable in afpsql-readonly' <<<"$ERR"; then
    echo "$label did not reach ordinary readonly runtime semantics: stderr=$ERR" >&2
    exit 1
  fi
}

"$READONLY" --version
expect_invalid_request "write permission" --permission write --sql "select 1"
"$READONLY" skill status >/dev/null
expect_invalid_request "ProxyCommand" --ssh invalid --ssh-option "ProxyCommand=false" --sql "select 1"
expect_invalid_request "custom container runtime" --container-docker-name invalid --container-docker-runtime false --sql "select 1"

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT
sql_path="$tmp_dir/query.sql"
printf '%s\n' 'select 1' >"$sql_path"

# Stream redirection (`--stdout-file`/`--stderr-file`) is a Unix-only capability;
# on Windows afpsql rejects it outright ("stream redirection is only supported on
# Unix platforms"), so the redirect smoke checks below only apply on Unix.
case "$(uname -s 2>/dev/null || echo unknown)" in
MINGW* | MSYS* | CYGWIN* | *NT*)
  echo "skipping Unix-only stream-redirect checks on this platform"
  ;;
*)
  # The runtime error is a diagnostic, so it is `--stderr-file` that must
  # receive it under the default split routing.
  redirect_path="$tmp_dir/output"
  printf '%s' 'preserve-me' >"$redirect_path"
  set +e
  "$READONLY" --stderr-file "$redirect_path" --sql "select 1" >/dev/null
  redirect_status=$?
  set -e
  if [ "$redirect_status" -eq 0 ] || ! grep -q 'connect_failed' "$redirect_path"; then
    echo "readonly redirect did not receive the runtime error" >&2
    exit 1
  fi

  # Collapsing the stream onto stdout must route the same error into
  # `--stdout-file` instead.
  collapsed_path="$tmp_dir/collapsed"
  set +e
  "$READONLY" --output-to stdout --stdout-file "$collapsed_path" --sql "select 1"
  collapsed_status=$?
  set -e
  if [ "$collapsed_status" -eq 0 ] || ! grep -q 'connect_failed' "$collapsed_path"; then
    echo "readonly --output-to stdout did not reach the redirected result stream" >&2
    exit 1
  fi

  # A file sink is installed from the resolved output plan, so an invocation the
  # parser refuses never reaches the filesystem.
  rejected_path="$tmp_dir/rejected"
  set +e
  "$READONLY" --sql "--stdout-file=$rejected_path" >/dev/null 2>&1
  rejected_status=$?
  set -e
  if [ "$rejected_status" -eq 0 ] || [ -e "$rejected_path" ]; then
    echo "a rejected invocation created a file sink" >&2
    exit 1
  fi
  ;;
esac

expect_runtime_failure "local SQL file" --sql-file "$sql_path"

config_path="$tmp_dir/config.env"
canary="AFPSQL_RELEASE_SMOKE_SECRET_CANARY"
printf '%s\n' "DATABASE_URL=postgresql://user:$canary@127.0.0.1:1/db" >"$config_path"
run_readonly --dsn file:"$config_path"#DATABASE_URL \
  --sql 'select 1'
if [ "$STATUS" -eq 0 ]; then
  echo "readonly config secret smoke unexpectedly succeeded" >&2
  exit 1
fi
# The secret must not appear on either stream, not merely on the one the
# failure happened to avoid.
if grep -q "$canary" <<<"$OUT" || grep -q "$canary" <<<"$ERR"; then
  echo "readonly config secret leaked: stdout=$OUT stderr=$ERR" >&2
  exit 1
fi
expect_invalid_request "config-backed write permission" \
  --dsn file:"$config_path"#DATABASE_URL \
  --permission write --sql "select 1"

# A structural rejection names its own rule, so the smoke also proves the parser
# and the readonly capability refusals stay two distinguishable failures.
run_readonly -c "select 1"
if [ "$STATUS" -ne 2 ] || ! grep -q '"code":"cli_unknown_argument"' <<<"$ERR"; then
  echo "psql short flag was not rejected as an unknown argument: stderr=$ERR" >&2
  exit 1
fi

echo "afpsql-readonly release smoke passed"
