use crate::types::Output;
use agent_first_data::OutputFormat;
use std::io::Write;
use tokio::sync::mpsc;

pub async fn writer_task(mut rx: mpsc::Receiver<Output>, format: OutputFormat) {
    while let Some(output) = rx.recv().await {
        let value = serde_json::to_value(output).unwrap_or(serde_json::Value::Null);
        let rendered = agent_first_data::cli_output(&value, format);

        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        let _ = out.write_all(rendered.as_bytes());
        if !rendered.ends_with('\n') {
            let _ = out.write_all(b"\n");
        }
        let _ = out.flush();
    }
}
