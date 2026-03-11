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
    use crate::types::{RuntimeConfig, SessionConfig};

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
