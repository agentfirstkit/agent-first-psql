use crate::output_fmt;
use crate::types::Output;
use agent_first_data::{OutputFormat, OutputTo};
use serde_json::Value;
use std::sync::OnceLock;

static OUTPUT_TO: OnceLock<OutputTo> = OnceLock::new();

/// Record the process-wide event destination the resolved invocation chose.
///
/// The destination is part of each combination's output contract, not a
/// separate scan of argv: a finite query emits at most one terminal event and
/// splits by kind, while `--mode pipe` and `--stream-rows` are ordered event
/// streams whose contract has no `split` to select. Anything emitted before an
/// invocation resolves — a rejected argv, a readonly capability refusal — falls
/// back to the split default, which puts errors on stderr.
pub fn set_output_to(value: OutputTo) {
    let _ = OUTPUT_TO.set(value);
}

pub fn output_to() -> OutputTo {
    OUTPUT_TO.get().copied().unwrap_or(OutputTo::Split)
}

pub fn emit_cli_error(
    msg: &str,
    hint: Option<&str>,
    format: OutputFormat,
) -> Result<(), agent_first_data::CliEmitterError> {
    emit_coded_error(
        crate::protocol::error_code::INVALID_REQUEST,
        msg,
        hint,
        format,
    )
}

/// Emit one error event under a caller-chosen classification.
///
/// Structural rejections carry the parser's own `cli_*` code, so an agent
/// branches on `error.code` for those exactly as it does for domain failures.
pub fn emit_coded_error(
    code: &str,
    msg: &str,
    hint: Option<&str>,
    format: OutputFormat,
) -> Result<(), agent_first_data::CliEmitterError> {
    let mut emitter =
        agent_first_data::CliEmitter::from_output_to(output_to(), format).with_strict_protocol();
    let event = agent_first_data::json_error(code, msg)
        .hint_if_some(hint)
        .build()
        .map_err(agent_first_data::CliEmitterError::Build)?;
    emitter.emit(event)
}

pub fn emit_event(
    event: agent_first_data::Event,
    format: OutputFormat,
) -> Result<(), agent_first_data::CliEmitterError> {
    let mut emitter =
        agent_first_data::CliEmitter::from_output_to(output_to(), format).with_strict_protocol();
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

// AFDATA injects the raw outcomes this writes (`--docs`, plain help), so it owns
// the routing and the rule that a closed reader is success rather than failure.
pub fn write_result_text(text: &str) -> std::io::Result<()> {
    agent_first_data::write_raw(text, output_to())
}
