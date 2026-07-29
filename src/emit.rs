use crate::output_fmt;
use crate::types::Output;
use agent_first_data::{OutputFormat, OutputTo};
use serde_json::Value;
use std::io::Write;
use std::sync::OnceLock;

static OUTPUT_TO: OnceLock<OutputTo> = OnceLock::new();

/// Resolve the process-wide event destination from raw arguments.
///
/// The destination follows the invocation's *consumption mode*. A plain query
/// emits at most one terminal event and exits, so it is a finite one-shot
/// command and splits by kind. `--mode pipe` and `--stream-rows` instead emit an
/// interleaved multi-event stream whose consumer reads it in order, so they are
/// event streams: every event stays on one stream, because splitting a stream
/// across stdout and stderr loses the ordering that makes it a stream.
pub fn install_output_to_from_raw(raw_args: &[String]) -> Result<(), String> {
    let mut requested: Option<OutputTo> = None;
    let mut event_stream = false;
    // Walk arguments the same way `is_psql_mode_requested` does, so this scan
    // and clap agree on which token is a flag and which is a flag's value.
    // `--sql` and `--sql-file` take hyphen values, so without the value table
    // `--sql --stream-rows` (a SQL comment) would read as a streaming flag.
    let mut top_level = true;
    let mut i = 1usize;
    while i < raw_args.len() {
        let arg = raw_args[i].as_str();
        if arg == "--" {
            break;
        }

        // `--output-to` is global, so it still applies after a subcommand.
        if let Some(value) = arg.strip_prefix("--output-to=") {
            requested = Some(OutputTo::parse(value)?);
            i += 1;
            continue;
        }
        if arg == "--output-to" {
            let value = raw_args.get(i + 1).ok_or_else(|| {
                "--output-to requires a value: expected split, stdout, or stderr".to_string()
            })?;
            requested = Some(OutputTo::parse(value)?);
            i += 2;
            continue;
        }

        if !top_level {
            i += 1;
            continue;
        }

        // `--stream-rows` and `--mode` are top-level only. Counting them after a
        // subcommand would pick a destination for an invocation clap is about to
        // reject, sending the usage error to the wrong stream.
        if arg == "--stream-rows" || arg == "--mode=pipe" {
            event_stream = true;
            i += 1;
            continue;
        }
        if arg == "--mode" {
            if raw_args.get(i + 1).is_some_and(|value| value == "pipe") {
                event_stream = true;
            }
            i += 2;
            continue;
        }
        if crate::cli::top_level_arg_consumes_two_values(arg) {
            i += if arg.contains('=') { 2 } else { 3 };
            continue;
        }
        if crate::cli::top_level_arg_consumes_value(arg) {
            i += if arg.contains('=') { 1 } else { 2 };
            continue;
        }
        if arg.starts_with('-') {
            i += 1;
            continue;
        }
        top_level = false;
        i += 1;
    }

    let output_to = match (requested, event_stream) {
        (Some(OutputTo::Split), true) => {
            return Err(
                "--output-to split cannot be combined with --mode pipe or --stream-rows: an ordered event stream must stay on one stream. Use --output-to stdout (the streaming default) or --output-to stderr"
                    .to_string(),
            );
        }
        (Some(explicit), _) => explicit,
        (None, true) => OutputTo::Stdout,
        (None, false) => OutputTo::Split,
    };
    let _ = OUTPUT_TO.set(output_to);
    Ok(())
}

pub fn output_to() -> OutputTo {
    OUTPUT_TO.get().copied().unwrap_or(OutputTo::Split)
}

pub fn emit_cli_error(
    msg: &str,
    hint: Option<&str>,
    format: OutputFormat,
) -> Result<(), agent_first_data::CliEmitterError> {
    let mut emitter =
        agent_first_data::CliEmitter::from_output_to(output_to(), format).with_strict_protocol();
    let event = agent_first_data::json_error(crate::protocol::error_code::INVALID_REQUEST, msg)
        .hint_if_some(hint)
        .build()
        .map_err(agent_first_data::CliEmitterError::Build)?;
    emitter.emit(event)
}

pub fn emit_value(
    value: Value,
    format: OutputFormat,
) -> Result<(), agent_first_data::CliEmitterError> {
    let mut emitter =
        agent_first_data::CliEmitter::from_output_to(output_to(), format).with_strict_protocol();
    emitter.emit_validated_value(value)
}

pub fn emit_output(
    out: &Output,
    format: OutputFormat,
) -> Result<(), agent_first_data::CliEmitterError> {
    output_fmt::emit_process_output(out, format, output_to())
}

#[allow(clippy::disallowed_methods)]
pub fn write_result_text(text: &str) -> std::io::Result<()> {
    let mut writer: Box<dyn Write> = match output_to() {
        OutputTo::Stderr => Box::new(std::io::stderr()),
        OutputTo::Split | OutputTo::Stdout => Box::new(std::io::stdout()),
    };
    writer.write_all(text.as_bytes())?;
    writer.flush()
}
