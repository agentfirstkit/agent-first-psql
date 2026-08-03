pub mod error_code {
    pub const CANCELLED: &str = "cancelled";
    pub const CONNECT_FAILED: &str = "connect_failed";
    pub const INTERNAL_ERROR: &str = "internal_error";
    pub const INVALID_PARAMS: &str = "invalid_params";
    pub const INVALID_REQUEST: &str = "invalid_request";
}

pub mod log_event {
    pub const CONNECT_LIBPQ_ENV_FALLBACK: &str = "connect.libpq_env_fallback";
    pub const MODE_PERMISSION_DEFAULT_CHANGED: &str = "mode.permission_default_changed";
    pub const QUERY_ERROR: &str = "query.error";
    pub const QUERY_ROW_ENCODING_DEGRADED: &str = "query.row_encoding_degraded";
    pub const QUERY_RESULT: &str = "query.result";
    pub const QUERY_SQL_ERROR: &str = "query.sql_error";
    pub const STARTUP: &str = "startup";
    pub const TRANSPORT_SELECTED: &str = "transport.selected";
}

pub mod command_tag {
    pub const EXECUTE: &str = "EXECUTE";
    pub const SELECT: &str = "SELECT";
    pub const BEGIN: &str = "BEGIN";
    pub const COMMIT: &str = "COMMIT";
    pub const ROLLBACK: &str = "ROLLBACK";

    pub fn execute(affected: usize) -> String {
        format!("EXECUTE {affected}")
    }

    pub fn rows(row_count: usize) -> String {
        format!("ROWS {row_count}")
    }
}

pub fn sqlstate_retryable(sqlstate: &str) -> bool {
    matches!(sqlstate, "40001" | "40P01" | "57014")
}

#[cfg(test)]
mod tests {
    use super::sqlstate_retryable;

    #[test]
    fn retryable_sqlstates_cover_serialization_deadlock_and_cancel() {
        for sqlstate in ["40001", "40P01", "57014"] {
            assert!(sqlstate_retryable(sqlstate), "{sqlstate}");
        }
        for sqlstate in ["23505", "42501", "42601"] {
            assert!(!sqlstate_retryable(sqlstate), "{sqlstate}");
        }
    }
}

// Log-filter matching lives in afdata's `LogFilters::enabled`; afpsql threads
// the `LogFilters` type through its config instead of keeping a second matcher.
