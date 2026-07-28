use crate::output_fmt;
use crate::types::Output;
use agent_first_data::{OutputFormat, OutputTo};
use serde_json::Value;
use std::io::Write;
use std::sync::OnceLock;

static OUTPUT_TO: OnceLock<OutputTo> = OnceLock::new();

pub fn install_output_to_from_raw(raw_args: &[String]) -> Result<(), String> {
    let mut output_to = OutputTo::Split;
    let mut args = raw_args.iter().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        }
        if let Some(value) = arg.strip_prefix("--output-to=") {
            output_to = OutputTo::parse(value)?;
        } else if arg == "--output-to" {
            let value = args.next().ok_or_else(|| {
                "--output-to requires a value: expected split, stdout, or stderr".to_string()
            })?;
            output_to = OutputTo::parse(value)?;
        }
    }
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
