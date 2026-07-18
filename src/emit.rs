use crate::output_fmt;
use crate::types::Output;
use agent_first_data::OutputFormat;

pub fn emit_cli_error(
    msg: &str,
    hint: Option<&str>,
    format: OutputFormat,
) -> Result<(), agent_first_data::CliEmitterError> {
    let stdout = std::io::stdout();
    let mut emitter =
        agent_first_data::CliEmitter::new(stdout.lock(), format).with_strict_protocol();
    let event = agent_first_data::json_error(crate::protocol::error_code::INVALID_REQUEST, msg)
        .hint_if_some(hint)
        .build()
        .map_err(agent_first_data::CliEmitterError::Build)?;
    emitter.emit(event)
}

pub fn emit_output(
    out: &Output,
    format: OutputFormat,
) -> Result<(), agent_first_data::CliEmitterError> {
    let stdout = std::io::stdout();
    output_fmt::emit_output(stdout.lock(), out, format)
}
