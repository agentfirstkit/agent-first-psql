use crate::output_fmt;
use crate::types::Output;
use agent_first_data::{cli_output, OutputFormat};
use std::io::Write;

pub fn emit_cli_error(msg: &str, hint: Option<&str>, format: OutputFormat) {
    let value = agent_first_data::build_cli_error(msg, hint);
    let rendered = cli_output(&value, format);
    let _ = writeln!(std::io::stdout(), "{rendered}");
}

pub fn emit_output(out: &Output, format: OutputFormat) {
    let rendered = output_fmt::render_output(out, format);
    let _ = writeln!(std::io::stdout(), "{rendered}");
}
