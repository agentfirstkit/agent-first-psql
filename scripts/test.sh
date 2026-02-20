#!/bin/bash
# Run tests for agent-first-psql
#
# Usage:
#   ./scripts/test.sh              # static checks + unit tests (no DB needed)
#   ./scripts/test.sh unit         # same as above
#   ./scripts/test.sh integration  # unit + integration tests (requires DATABASE_URL)

set -e
ROOTPATH="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODE="${1:-unit}"

echo "Testing agent-first-psql [$MODE]..."

# Static checks (always run)
(cd "$ROOTPATH" && cargo fmt --all --check)
(cd "$ROOTPATH" && cargo clippy -- -D warnings)

if [ "$MODE" = "unit" ] || [ "$MODE" = "integration" ]; then
    # Unit tests: no DB needed
    (cd "$ROOTPATH" && cargo test --bin afpsql)
fi

if [ "$MODE" = "integration" ]; then
    if [ -z "${DATABASE_URL:-}" ] && [ -z "${AFPSQL_TEST_DSN_SECRET:-}" ]; then
        echo "Error: integration tests require DATABASE_URL or AFPSQL_TEST_DSN_SECRET"
        exit 1
    fi
    # Build binary first (integration tests invoke it as a subprocess)
    (cd "$ROOTPATH" && cargo build)
    # Integration tests: all test binaries in tests/
    (cd "$ROOTPATH" && cargo test --tests)
fi

echo "All checks passed [$MODE]!"
