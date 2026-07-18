use crate::types::Output;
use agent_first_data::OutputFormat;
use tokio::sync::mpsc;

pub async fn writer_task(
    mut rx: mpsc::Receiver<Output>,
    format: OutputFormat,
) -> Result<(), agent_first_data::CliEmitterError> {
    while let Some(output) = rx.recv().await {
        let stdout = std::io::stdout();
        crate::output_fmt::emit_output(stdout.lock(), &output, format)?;
    }
    Ok(())
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
        assert!(rendered.contains("rows:"));
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
        assert!(rendered.contains("rows:"));
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
                conninfo_secret: Some("host=db password=conninfo-canary".to_string()),
                password_secret: Some("password-canary".to_string()),
                ..SessionConfig::default()
            },
        );
        let out = Output::Config(cfg);
        let rendered = crate::output_fmt::render_output(&out, OutputFormat::Json);
        assert!(rendered.contains("\"dsn_secret\":\"***\""));
        assert!(!rendered.contains("postgresql://user:pass@host/db"));
        assert!(rendered.contains("\"conninfo_secret\":\"***\""));
        assert!(rendered.contains("\"password_secret\":\"***\""));
        assert!(!rendered.contains("conninfo-canary"));
        assert!(!rendered.contains("password-canary"));
    }

    #[test]
    fn config_yaml_and_plain_remain_redacted() {
        let mut cfg = RuntimeConfig::default();
        cfg.sessions.insert(
            "default".to_string(),
            SessionConfig {
                dsn_secret: Some("dsn-canary".to_string()),
                conninfo_secret: Some("conninfo-canary".to_string()),
                password_secret: Some("password-canary".to_string()),
                ..SessionConfig::default()
            },
        );
        for format in [OutputFormat::Yaml, OutputFormat::Plain] {
            let rendered = crate::output_fmt::render_output(&Output::Config(cfg.clone()), format);
            assert!(rendered.contains("***"));
            for canary in ["dsn-canary", "conninfo-canary", "password-canary"] {
                assert!(!rendered.contains(canary), "{format:?} leaked {canary}");
            }
        }
    }
}
