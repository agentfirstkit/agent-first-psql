#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::print_stdout,
        clippy::print_stderr,
    )
)]

mod cli;
mod cli_runner;
mod config;
mod conn;
mod container_transport;
mod db;
mod emit;
mod handler;
mod limits;
mod logutil;
mod output_fmt;
mod pipe;
mod protocol;
mod psql_admin;
mod skill_admin;
mod ssh_transport;
mod types;
mod writer;

use agent_first_data::OutputFormat;
use cli::Mode;

#[tokio::main]
async fn main() {
    let mode = match cli::parse_args() {
        Ok(m) => m,
        Err(e) => {
            emit::emit_cli_error(&e, None, OutputFormat::Json);
            std::process::exit(2);
        }
    };

    match mode {
        Mode::Cli(req) => cli_runner::run(req).await,
        Mode::Pipe(init) => pipe::run(init).await,
        Mode::PsqlAdmin(req) => std::process::exit(psql_admin::run(req)),
        Mode::SkillAdmin(req) => std::process::exit(skill_admin::run(req)),
        Mode::PsqlUnsupported(req) => {
            emit::emit_cli_error(
                &format!("unsupported psql mode: {}", req.reason),
                Some("run the original psql binary directly, for example /path/to/postgresql/bin/psql, or put that PostgreSQL bin directory before the afpsql wrapper in PATH"),
                OutputFormat::Json,
            );
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
use limits::{MAX_PARAMS, MAX_SQL_BYTES};
#[cfg(test)]
use logutil::build_startup_log;
#[cfg(test)]
use pipe::{has_session_override, read_limited_line, validate_query_request};
#[cfg(test)]
use types::*;

#[cfg(test)]
#[path = "../tests/support/unit_main.rs"]
mod tests;
