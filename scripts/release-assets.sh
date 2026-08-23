#!/bin/bash
# Verify that a release job is building its requested tag, then upload assets
# without ever replacing different bytes already attached to that release.

set -euo pipefail

usage() {
  echo "usage: release-assets.sh TAG [ASSET ...]" >&2
}

fail() {
  echo "release-assets.sh: $1" >&2
  exit 1
}

if [ "$#" -lt 1 ]; then
  usage
  exit 2
fi

TAG="$1"
shift

if [[ ! "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z][0-9A-Za-z.-]*)?(\+[0-9A-Za-z][0-9A-Za-z.-]*)?$ ]]; then
  fail "invalid release tag"
fi

HEAD_COMMIT="$(git rev-parse --verify 'HEAD^{commit}' 2>/dev/null)" || \
  fail "HEAD is not a Git commit"
TAG_COMMIT="$(git rev-parse --verify "${TAG}^{commit}" 2>/dev/null)" || \
  fail "release tag does not exist in this checkout: $TAG"
if [ "$HEAD_COMMIT" != "$TAG_COMMIT" ]; then
  fail "checkout HEAD $HEAD_COMMIT does not match $TAG commit $TAG_COMMIT"
fi

# With no assets this is the early workflow source gate. Keeping source
# verification and immutable upload in one helper means the upload repeats the
# check immediately before the only release mutation.
if [ "$#" -eq 0 ]; then
  exit 0
fi

GH_COMMAND="${GH_BIN:-gh}"
if ! command -v "$GH_COMMAND" >/dev/null 2>&1; then
  fail "GitHub CLI not found: $GH_COMMAND"
fi

ATTEMPTS="${RELEASE_ASSET_ATTEMPTS:-5}"
RETRY_BASE_S="${RELEASE_ASSET_RETRY_BASE_S:-10}"
case "$ATTEMPTS" in
  '' | *[!0-9]*) fail "RELEASE_ASSET_ATTEMPTS must be a positive integer" ;;
esac
case "$RETRY_BASE_S" in
  '' | *[!0-9]*) fail "RELEASE_ASSET_RETRY_BASE_S must be a non-negative integer" ;;
esac
if [ "$ATTEMPTS" -eq 0 ]; then
  fail "RELEASE_ASSET_ATTEMPTS must be a positive integer"
fi

sleep_before_retry() {
  local attempt="$1"
  if [ "$RETRY_BASE_S" -gt 0 ]; then
    sleep $((attempt * RETRY_BASE_S))
  fi
}

# Each remote asset as `name<TAB>state`. The state matters: GitHub records an
# asset whose upload never completed, and such an asset occupies the name while
# being undownloadable. Asking only for names cannot tell that apart from a
# finished upload.
query_asset_records() {
  local attempt=1 output
  while [ "$attempt" -le "$ATTEMPTS" ]; do
    if output=$("$GH_COMMAND" release view "$TAG" --json assets \
      --jq '.assets[] | "\(.name)\t\(.state)"'); then
      printf '%s\n' "$output"
      return 0
    fi
    echo "release asset query attempt $attempt failed" >&2
    if [ "$attempt" -lt "$ATTEMPTS" ]; then
      sleep_before_retry "$attempt"
    fi
    attempt=$((attempt + 1))
  done
  return 1
}

# Prints the remote state of `wanted`, or returns non-zero when the release has
# no asset by that name. A state GitHub did not report is read as `uploaded`:
# refusing to touch an asset that might be real is the safe way to be wrong.
remote_asset_state() {
  local records="$1"
  local wanted="$2"
  local remote_name remote_state
  while IFS=$'\t' read -r remote_name remote_state; do
    remote_name="${remote_name%$'\r'}"
    remote_state="${remote_state%$'\r'}"
    if [ -n "$remote_name" ] && [ "$remote_name" = "$wanted" ]; then
      case "$remote_state" in
        '' | null) printf 'uploaded\n' ;;
        *) printf '%s\n' "$remote_state" ;;
      esac
      return 0
    fi
  done <<<"$records"
  return 1
}

# Drop an asset whose upload never finished, so the name it holds can be used.
#
# This is the one deletion this script performs, and it is not a hole in the
# immutability rule: an asset in any state other than `uploaded` was never
# downloadable, so nothing can have pinned its bytes. Refusing it instead — the
# behavior before this existed — turns one lost upload response into a release
# that no retry can finish, which is what `--clobber` used to paper over by
# also being willing to replace real published bytes.
discard_incomplete_asset() {
  local name="$1"
  local state="$2"
  local attempt=1
  echo "release asset $name is in state '$state', not 'uploaded'; discarding it" >&2
  while [ "$attempt" -le "$ATTEMPTS" ]; do
    if "$GH_COMMAND" release delete-asset "$TAG" "$name" --yes; then
      return 0
    fi
    echo "release asset discard attempt $attempt failed: $name" >&2
    if [ "$attempt" -lt "$ATTEMPTS" ]; then
      sleep_before_retry "$attempt"
    fi
    attempt=$((attempt + 1))
  done
  return 1
}

download_remote_asset() {
  local name="$1"
  local destination="$2"
  local attempt=1
  while [ "$attempt" -le "$ATTEMPTS" ]; do
    if "$GH_COMMAND" release download "$TAG" --pattern "$name" --dir "$destination"; then
      [ -f "$destination/$name" ] && return 0
    fi
    echo "release asset download attempt $attempt failed: $name" >&2
    if [ "$attempt" -lt "$ATTEMPTS" ]; then
      sleep_before_retry "$attempt"
    fi
    attempt=$((attempt + 1))
  done
  return 1
}

sha256_file() {
  local path="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$path" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$path" | awk '{print $1}'
  else
    return 1
  fi
}

validate_checksum_asset() {
  local checksum_path="$1"
  local archive_path="${checksum_path%.sha256}"
  local archive_name digest expected actual lines

  if [ "$archive_path" = "$checksum_path" ]; then
    return 0
  fi
  if [ ! -f "$archive_path" ] || [ -L "$archive_path" ]; then
    fail "checksum has no regular local archive: $(basename "$checksum_path")"
  fi
  archive_name="$(basename "$archive_path")"
  digest="$(sha256_file "$archive_path")" || \
    fail "no SHA-256 implementation is available"
  expected="$digest  $archive_name"
  actual="$(cat "$checksum_path")"
  lines="$(wc -l < "$checksum_path" | tr -d '[:space:]')"
  if [ "$lines" != 1 ] || [ "$actual" != "$expected" ]; then
    fail "checksum file does not exactly describe its local archive: $(basename "$checksum_path")"
  fi
}

# The Windows runner exports `RUNNER_TEMP` as a native path (`D:\a\_temp`),
# which this shell's `mktemp` cannot build a template from — and the whole job
# would fail before the first upload with nothing but "could not create staging
# directory" to go on. Separators are normalized, each candidate is tried in
# turn, and `mktemp`'s own default is the last resort.
make_work_dir() {
  local root candidate
  for root in "${RUNNER_TEMP:-}" "${TMPDIR:-}" /tmp; do
    [ -n "$root" ] || continue
    root="${root//\\//}"
    root="${root%/}"
    [ -d "$root" ] || continue
    if candidate="$(mktemp -d "$root/release-assets.XXXXXX" 2>/dev/null)"; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  candidate="$(mktemp -d 2>/dev/null)" || return 1
  printf '%s\n' "$candidate"
}

WORK_DIR="$(make_work_dir)" || fail "could not create release asset staging directory"
cleanup() {
  if [ -n "${WORK_DIR:-}" ] && [ -d "$WORK_DIR" ]; then
    rm -rf -- "$WORK_DIR"
  fi
}
trap cleanup EXIT

# Compare a finished remote asset with the local one. Identical bytes are an
# idempotent success; different bytes are refused, because something already
# published carries them.
reconcile_uploaded_asset() {
  local asset="$1"
  local name="$2"
  local download_dir="$3"
  local context="$4"

  mkdir -p "$download_dir"
  download_remote_asset "$name" "$download_dir" || \
    fail "could not download existing release asset$context: $name"
  if cmp -s "$asset" "$download_dir/$name"; then
    echo "release asset already matches$context: $name"
    return 0
  fi
  fail "release asset already exists with different bytes$context: $name"
}

ensure_asset() {
  local asset="$1"
  local name="$2"
  local index="$3"
  local attempt=1 records state

  while [ "$attempt" -le "$ATTEMPTS" ]; do
    records="$(query_asset_records)" || fail "could not inspect release $TAG"
    if state="$(remote_asset_state "$records" "$name")"; then
      if [ "$state" = uploaded ]; then
        reconcile_uploaded_asset "$asset" "$name" \
          "$WORK_DIR/download-${index}-${attempt}" ""
        return 0
      fi
      discard_incomplete_asset "$name" "$state" || \
        fail "could not discard the incomplete release asset: $name"
    fi

    if "$GH_COMMAND" release upload "$TAG" "$asset"; then
      echo "release asset uploaded: $name"
      return 0
    fi
    echo "release asset upload attempt $attempt failed: $name" >&2
    if [ "$attempt" -lt "$ATTEMPTS" ]; then
      sleep_before_retry "$attempt"
    fi
    attempt=$((attempt + 1))
  done

  # The upload response can be lost after GitHub accepted the bytes. One final
  # read turns that ambiguous transport failure into an idempotent success only
  # when the remote asset is finished and byte-for-byte identical.
  records="$(query_asset_records)" || fail "could not inspect release $TAG after upload failure"
  if state="$(remote_asset_state "$records" "$name")"; then
    if [ "$state" = uploaded ]; then
      reconcile_uploaded_asset "$asset" "$name" \
        "$WORK_DIR/download-${index}-final" " after a lost upload response"
      return 0
    fi
    fail "release asset $name is stuck in state '$state' after $ATTEMPTS attempts"
  fi
  fail "could not upload release asset after $ATTEMPTS attempts: $name"
}

SEEN_NAMES='|'
ASSET_INDEX=0
for ASSET in "$@"; do
  if [ ! -f "$ASSET" ] || [ -L "$ASSET" ]; then
    fail "asset is not a regular file"
  fi
  ASSET_NAME="$(basename "$ASSET")"
  if [ "$ASSET" != "$ASSET_NAME" ]; then
    fail "assets must be files in the checkout root: $ASSET_NAME"
  fi
  if [[ ! "$ASSET_NAME" =~ ^[0-9A-Za-z][0-9A-Za-z._+-]*$ ]]; then
    fail "asset name contains unsupported characters"
  fi
  case "$SEEN_NAMES" in
    *"|$ASSET_NAME|"*) fail "duplicate asset argument: $ASSET_NAME" ;;
  esac
  SEEN_NAMES="${SEEN_NAMES}${ASSET_NAME}|"
  validate_checksum_asset "$ASSET"
  ASSET_INDEX=$((ASSET_INDEX + 1))
  ensure_asset "$ASSET" "$ASSET_NAME" "$ASSET_INDEX"
done
