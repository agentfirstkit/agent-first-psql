mod errors;
mod executor;
mod params;
mod rows;
mod session;

pub(crate) use errors::ConnectError;
pub use errors::ExecError;
#[cfg(test)]
pub(crate) use executor::DryRunOutcome;
pub use executor::{
    DbExecutor, ExecOutcome, ExecRequest, PostgresExecutor, RowSink, StreamOutcome,
    TransportLogContext,
};
pub use session::{CancelSlot, cancel_query, new_cancel_slot};

/// Strip trailing statement terminators so the statement can be embedded in a
/// larger one (a `to_jsonb` CTE, an `EXPLAIN` prefix). Repeated semicolons and
/// any interleaved whitespace are removed; a trailing comment is deliberately
/// left alone, because deciding whether `--` opens a comment or sits inside a
/// string literal would require parsing SQL.
pub fn trim_trailing_statement_terminators(sql: &str) -> &str {
    sql.trim_end_matches(|c: char| c == ';' || c.is_whitespace())
}

#[cfg(test)]
use crate::types::SessionConfig;
#[cfg(test)]
use params::{
    AnyNull, build_param_refs, build_params, parse_bool, parse_f64, parse_i16, parse_i32,
    parse_i64, parse_text,
};
#[cfg(test)]
use serde_json::Value;
#[cfg(test)]
use tokio_postgres::types::{ToSql, Type};

#[cfg(test)]
#[path = "../../tests/support/unit_db.rs"]
mod tests;
