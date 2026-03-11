use crate::types::Output;
use agent_first_data::{cli_output, OutputFormat, RedactionPolicy};
use serde_json::Value;

pub fn render_output(out: &Output, format: OutputFormat) -> String {
    let mut value = match serde_json::to_value(out) {
        Ok(v) => v,
        Err(_) => {
            let fallback = serde_json::json!({
                "code": "error",
                "error_code": "internal_error",
                "error": "output serialization failed",
                "retryable": false,
                "trace": {"duration_ms": 0}
            });
            return cli_output(&fallback, format);
        }
    };

    if matches!(format, OutputFormat::Json) {
        match json_redaction_policy_for_output(out) {
            Some(policy) => agent_first_data::output_json_with(&value, policy),
            None => cli_output(&value, OutputFormat::Json),
        }
    } else {
        protect_result_rows(&mut value, out);
        cli_output(&value, format)
    }
}

fn json_redaction_policy_for_output(out: &Output) -> Option<RedactionPolicy> {
    match out {
        // Preserve SQL payload keys/values; redact trace-only metadata if needed.
        Output::Result { .. } | Output::ResultRows { .. } => {
            Some(RedactionPolicy::RedactionTraceOnly)
        }
        _ => None,
    }
}

fn protect_result_rows(value: &mut Value, out: &Output) {
    if !matches!(out, Output::Result { .. } | Output::ResultRows { .. }) {
        return;
    }
    if let Some(obj) = value.as_object_mut() {
        if let Some(rows) = obj.get("rows").cloned() {
            if !rows.is_null() && !rows.is_string() {
                if let Ok(rows_json) = serde_json::to_string(&rows) {
                    obj.insert("rows".to_string(), Value::String(rows_json));
                }
            }
        }
    }
}
