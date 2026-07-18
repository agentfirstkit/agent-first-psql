#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr
)]

pub mod cli;
pub mod cli_runner;
pub mod config;
pub mod conn;
pub mod container_transport;
pub mod db;
pub mod emit;
pub mod handler;
pub mod limits;
pub mod logutil;
pub mod output_fmt;
pub mod pipe;
pub mod protocol;
pub mod psql_admin;
pub mod readonly_policy;
pub mod secret_config;
pub mod skill_admin;
pub mod ssh_transport;
pub mod types;
pub mod writer;

use agent_first_data::OutputFormat;
use std::io::Write as _;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capability {
    ReadWrite,
    ReadOnly,
}

impl Capability {
    pub fn permits(self, permission: types::Permission) -> bool {
        self == Self::ReadWrite || permission.is_read_only()
    }
}

pub async fn run(capability: Capability, bin_name: &str) {
    let mut locked_profile = None;
    if capability == Capability::ReadOnly {
        let raw_args = std::env::args().collect::<Vec<_>>();
        let profile_name = match raw_args
            .first()
            .map(String::as_str)
            .map(readonly_policy::locked_profile_name)
            .transpose()
        {
            Ok(name) => name.flatten(),
            Err(error) => reject_readonly(&error, readonly_local_capability_hint()),
        };
        if let Err(error) =
            readonly_policy::validate_raw_args_for_profile(&raw_args, profile_name.is_some())
        {
            reject_readonly(&error, readonly_local_capability_hint());
        }
        if let Some(name) = profile_name {
            locked_profile = match readonly_policy::load_locked_profile(&name) {
                Ok(profile) => Some(profile),
                Err(error) => reject_readonly(&error, readonly_local_capability_hint()),
            };
        }
    }
    let _stream_redirect = install_stream_redirect_or_exit();
    let mode = match cli::parse_args(bin_name) {
        Ok(mode) => mode,
        Err(error) => {
            if emit::emit_cli_error(&error, None, OutputFormat::Json).is_err() {
                std::process::exit(4);
            }
            std::process::exit(2);
        }
    };

    match mode {
        cli::Mode::Cli(request) if capability == Capability::ReadOnly && request.psql_mode => {
            reject_readonly(
                "psql mode is unavailable in afpsql-readonly",
                "use `afpsql` for psql compatibility mode; it intentionally has writable semantics",
            );
        }
        cli::Mode::Cli(mut request) => {
            let has_locked_profile = locked_profile.is_some();
            if let Some(profile) = locked_profile.clone() {
                request.session = profile;
            }
            if capability == Capability::ReadOnly
                && let Err(error) = readonly_policy::validate_session_with_trust(
                    &request.session,
                    has_locked_profile,
                )
            {
                reject_readonly(&error, readonly_local_capability_hint());
            }
            cli_runner::run(request, capability, has_locked_profile).await
        }
        cli::Mode::Pipe(mut init) => {
            let has_locked_profile = locked_profile.is_some();
            if let Some(profile) = locked_profile {
                init.session = profile;
            }
            if capability == Capability::ReadOnly
                && let Err(error) =
                    readonly_policy::validate_session_with_trust(&init.session, has_locked_profile)
            {
                reject_readonly(&error, readonly_local_capability_hint());
            }
            pipe::run(init, capability, has_locked_profile).await
        }
        cli::Mode::PsqlAdmin(_) if capability == Capability::ReadOnly => {
            reject_readonly(
                "the psql wrapper is a writable interface",
                "use `afpsql psql status`, `afpsql psql install`, or `afpsql psql uninstall`",
            );
        }
        cli::Mode::PsqlAdmin(request) => std::process::exit(psql_admin::run(request)),
        cli::Mode::SkillAdmin(_)
            if capability == Capability::ReadOnly && locked_profile.is_some() =>
        {
            reject_readonly(
                "skill management is unavailable through an administrator-locked afpsql-readonly profile",
                "use the ordinary afpsql-readonly or afpsql entrypoint for skill management",
            );
        }
        cli::Mode::SkillAdmin(request) => std::process::exit(skill_admin::run(request)),
        cli::Mode::PsqlUnsupported(_) if capability == Capability::ReadOnly => {
            reject_readonly(
                "psql mode is unavailable in afpsql-readonly",
                "use `afpsql` for psql compatibility mode; it intentionally has writable semantics",
            );
        }
        cli::Mode::PsqlUnsupported(request) => {
            if emit::emit_cli_error(&format!("unsupported psql mode: {}", request.reason), Some("run the original psql binary directly, for example /path/to/postgresql/bin/psql, or put that PostgreSQL bin directory before the afpsql wrapper in PATH"), OutputFormat::Json).is_err() {
                std::process::exit(4);
            }
            std::process::exit(2);
        }
    }
}

pub fn readonly_hint() -> &'static str {
    "write operations require `afpsql`; use afpsql-readonly only for database reads"
}

pub fn readonly_local_capability_hint() -> &'static str {
    "afpsql-readonly restricts PostgreSQL writes; an administrator-locked profile may additionally restrict host capabilities"
}

fn reject_readonly(error: &str, hint: &str) -> ! {
    if emit::emit_cli_error(error, Some(hint), OutputFormat::Json).is_err() {
        std::process::exit(4);
    }
    std::process::exit(2);
}

fn install_stream_redirect_or_exit()
-> Option<agent_first_data::stream_redirect::InstalledStreamRedirect> {
    match agent_first_data::stream_redirect::install_from_raw_args(std::env::args()) {
        Ok(redirect) => redirect,
        Err(error) => {
            let value = agent_first_data::build_cli_error(&error.to_string(), None);
            let rendered = agent_first_data::render(
                value.as_value(),
                OutputFormat::Json,
                &agent_first_data::OutputOptions::default(),
            );
            let _ = writeln!(std::io::stdout(), "{rendered}");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
#[path = "../tests/support/env.rs"]
mod test_env;

#[cfg(test)]
#[path = "../tests/support/unit_main.rs"]
mod main_tests {
    use crate::limits::{MAX_PARAMS, MAX_SQL_BYTES};
    use crate::logutil::build_startup_log;
    use crate::pipe::{has_session_override, read_limited_line, validate_query_request};
    use crate::types::{ContainerConfig, Output, SessionConfig};
    include!("../tests/support/unit_main.rs");
}
