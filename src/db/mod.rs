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
pub use session::{cancel_query, new_cancel_slot, CancelSlot};

#[cfg(test)]
use crate::types::SessionConfig;
#[cfg(test)]
use params::{
    build_param_refs, build_params, parse_bool, parse_f64, parse_i16, parse_i32, parse_i64,
    parse_text, AnyNull,
};
#[cfg(test)]
use serde_json::Value;
#[cfg(test)]
use tokio_postgres::types::{ToSql, Type};

#[cfg(test)]
#[path = "../../tests/support/unit_db.rs"]
mod tests;
