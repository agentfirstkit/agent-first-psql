use crate::types::Output;
use agent_first_data::OutputFormat;
use std::io::Write;
use tokio::sync::mpsc;

pub async fn writer_task(mut rx: mpsc::Receiver<Output>, format: OutputFormat) {
    while let Some(output) = rx.recv().await {
        let rendered = crate::output_fmt::render_output(&output, format);

        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        let _ = out.write_all(rendered.as_bytes());
        if !rendered.ends_with('\n') {
            let _ = out.write_all(b"\n");
        }
        let _ = out.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ColumnInfo, RuntimeConfig, SessionConfig, Trace};

    #[test]
    fn result_rows_json_keeps_payload_unredacted() {
        let out = Output::ResultRows {
            id: "q1".to_string(),
            rows: vec![serde_json::json!({"api_key_secret":"sk-live-1","n":1})],
            rows_batch_count: 1,
        };
        let rendered = crate::output_fmt::render_output(&out, OutputFormat::Json);
        assert!(rendered.contains("\"api_key_secret\":\"sk-live-1\""));
    }

    #[test]
    fn result_yaml_keeps_rows_structured_and_unredacted() {
        let out = Output::Result {
            id: None,
            session: None,
            command_tag: "SELECT 1".to_string(),
            columns: vec![ColumnInfo {
                name: "duration_ms".to_string(),
                type_name: "int4".to_string(),
            }],
            rows: vec![serde_json::json!({
                "api_key_secret": "sk-live-1",
                "duration_ms": 42
            })],
            row_count: 1,
            truncated: false,
            truncated_at_rows: None,
            truncated_at_bytes: None,
            trace: Trace::only_duration(7),
        };
        let rendered = crate::output_fmt::render_output(&out, OutputFormat::Yaml);
        assert!(rendered.contains("rows:\n  -"));
        assert!(rendered.contains("api_key_secret: \"sk-live-1\""));
        assert!(rendered.contains("duration_ms: 42"));
        assert!(!rendered.contains("rows: \"["));
        assert!(!rendered.contains("duration: \"42ms\""));
    }

    #[test]
    fn result_rows_yaml_keeps_batch_rows_structured() {
        let out = Output::ResultRows {
            id: "q1".to_string(),
            rows: vec![serde_json::json!({"n": 1})],
            rows_batch_count: 1,
        };
        let rendered = crate::output_fmt::render_output(&out, OutputFormat::Yaml);
        assert!(rendered.contains("rows:\n  -"));
        assert!(rendered.contains("n: 1"));
        assert!(!rendered.contains("rows: \"["));
    }

    #[test]
    fn config_json_remains_redacted() {
        let mut cfg = RuntimeConfig::default();
        cfg.sessions.insert(
            "default".to_string(),
            SessionConfig {
                dsn_secret: Some("postgresql://user:pass@host/db".to_string()),
                ..SessionConfig::default()
            },
        );
        let out = Output::Config(cfg);
        let rendered = crate::output_fmt::render_output(&out, OutputFormat::Json);
        assert!(rendered.contains("\"dsn_secret\":\"***\""));
        assert!(!rendered.contains("postgresql://user:pass@host/db"));
    }
}
