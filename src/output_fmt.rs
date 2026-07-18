use crate::types::Output;
use agent_first_data::{
    CliEmitter, CliEmitterError, OutputFormat, OutputOptions, PlainStyle, ProtocolViolation,
    RedactionPolicy, Redactor,
};
use serde_json::Value;
use std::io::Write;

const LEGACY_SECRET_NAMES: &[&str] = &["PGPASSWORD"];

#[cfg(test)]
pub fn render_output(out: &Output, format: OutputFormat) -> String {
    let mut bytes = Vec::new();
    if emit_output(&mut bytes, out, format).is_err() {
        return String::new();
    }
    String::from_utf8(bytes)
        .unwrap_or_default()
        .trim_end_matches('\n')
        .to_string()
}

pub fn emit_output<W: Write>(
    writer: W,
    out: &Output,
    format: OutputFormat,
) -> Result<(), CliEmitterError> {
    let mut value = serde_json::to_value(out).map_err(|error| {
        CliEmitterError::Validation(ProtocolViolation {
            rule: "output_serialization_failed",
            pointer: String::new(),
            message: format!("output serialization failed: {error}"),
        })
    })?;
    let trace = value
        .get("trace")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let code = value
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if let Value::Object(fields) = &mut value {
        fields.remove("trace");
    }
    let mut emitter = CliEmitter::with_options(writer, format, output_options_for_output(out))
        .with_strict_protocol();
    match code.as_str() {
        "log" => {
            let timestamp_epoch_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_millis() as u64)
                .unwrap_or_default();
            if let Value::Object(fields) = &mut value {
                fields.remove("code");
                fields.insert(
                    "timestamp_epoch_ms".to_string(),
                    Value::from(timestamp_epoch_ms),
                );
            }
            let event = agent_first_data::json_log(value).trace(trace).build();
            emitter.emit(event)
        }
        "result_start" | "result_rows" => {
            if let Value::Object(fields) = &mut value {
                let message = if code == "result_start" {
                    "query result stream started"
                } else {
                    "query result rows"
                };
                fields.insert("message".to_string(), Value::String(message.to_string()));
            }
            let event = agent_first_data::json_progress(value).trace(trace).build();
            emitter.emit(event)
        }
        "sql_error" | "error" => {
            let message = value
                .get("message")
                .or_else(|| value.get("error"))
                .and_then(Value::as_str)
                .unwrap_or("psql command failed")
                .to_string();
            let error_code = value
                .get("error_code")
                .and_then(Value::as_str)
                .unwrap_or(&code)
                .to_string();
            let hint = value
                .get("hint")
                .and_then(Value::as_str)
                .map(str::to_string);
            let retryable = value
                .get("retryable")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if let Value::Object(fields) = &mut value {
                fields.remove("code");
                fields.remove("error_code");
                fields.remove("error");
                fields.remove("message");
                fields.remove("hint");
                fields.remove("retryable");
            }
            let event = agent_first_data::json_error(&error_code, &message)
                .hint_if_some(hint.as_deref())
                .retryable_if(retryable)
                .fields(value)
                .trace(trace)
                .build()
                .map_err(CliEmitterError::Build)?;
            emitter.emit(event)
        }
        _ => {
            let event = agent_first_data::json_result(value).trace(trace).build();
            emitter.emit(event)
        }
    }
}

fn output_options_for_output(out: &Output) -> OutputOptions {
    let policy = match out {
        // Preserve SQL payload keys/values; redact trace-only metadata if needed.
        Output::Result { .. } | Output::ResultRows { .. } => RedactionPolicy::TraceOnly,
        _ => RedactionPolicy::All,
    };
    let style = match out {
        Output::Result { .. } | Output::ResultRows { .. } => PlainStyle::Raw,
        _ => PlainStyle::Readable,
    };
    let redaction = Redactor::new()
        .secret_names(LEGACY_SECRET_NAMES.iter().map(|s| s.to_string()))
        .policy(policy);
    OutputOptions { redaction, style }
}
