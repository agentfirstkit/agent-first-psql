use crate::types::Output;
use agent_first_data::{
    OutputFormat, OutputOptions, OutputStyle, RedactionOptions, RedactionPolicy,
};
use serde_json::Value;

const LEGACY_SECRET_NAMES: &[&str] = &["PGPASSWORD"];

pub fn render_output(out: &Output, format: OutputFormat) -> String {
    let value = match serde_json::to_value(out) {
        Ok(v) => v,
        Err(_) => {
            let fallback = serde_json::json!({
                "code": "error",
                "error_code": "internal_error",
                "error": "output serialization failed",
                "retryable": false,
                "trace": {"duration_ms": 0}
            });
            return render_value(&fallback, format, &default_output_options());
        }
    };

    let options = output_options_for_output(out);
    render_value(&value, format, &options)
}

fn output_options_for_output(out: &Output) -> OutputOptions {
    let policy = match out {
        // Preserve SQL payload keys/values; redact trace-only metadata if needed.
        Output::Result { .. } | Output::ResultRows { .. } => {
            Some(RedactionPolicy::RedactionTraceOnly)
        }
        _ => None,
    };
    let style = match out {
        Output::Result { .. } | Output::ResultRows { .. } => OutputStyle::Raw,
        _ => OutputStyle::Readable,
    };
    OutputOptions {
        redaction: RedactionOptions {
            policy,
            secret_names: legacy_secret_names(),
        },
        style,
    }
}

fn default_output_options() -> OutputOptions {
    OutputOptions {
        redaction: RedactionOptions {
            policy: None,
            secret_names: legacy_secret_names(),
        },
        style: OutputStyle::Readable,
    }
}

fn legacy_secret_names() -> Vec<String> {
    LEGACY_SECRET_NAMES
        .iter()
        .map(|name| (*name).to_string())
        .collect()
}

fn render_value(value: &Value, format: OutputFormat, options: &OutputOptions) -> String {
    match format {
        OutputFormat::Json => agent_first_data::output_json_with_options(value, options),
        OutputFormat::Yaml => agent_first_data::output_yaml_with_options(value, options),
        OutputFormat::Plain => agent_first_data::output_plain_with_options(value, options),
    }
}
