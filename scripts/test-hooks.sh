#!/bin/bash

cmn_test_usage() {
  echo "Usage: $(cmn_script_invocation) [static|unit|integration|all]" >&2
}

cmn_test_run_static() {
  echo "[static] fmt/clippy"
  cmn_test_default_static
}

cmn_test_run_unit() {
  echo "[unit] Rust tests"
  cmn_cargo test --bin afpsql
}

cmn_test_run_integration() {
  if [ -z "${DATABASE_URL:-}" ] && [ -z "${AFPSQL_TEST_DSN_SECRET:-}" ]; then
    echo "Error: integration tests require DATABASE_URL or AFPSQL_TEST_DSN_SECRET" >&2
    exit 1
  fi

  echo "[integration] Rust integration tests"
  cmn_test_run_unit
  cmn_cargo build
  cmn_cargo test --tests
}

cmn_test_run_all() {
  cmn_test_run_static
  cmn_test_run_unit
}
