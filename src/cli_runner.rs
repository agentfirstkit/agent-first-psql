use crate::handler;
use crate::handler::App;
use crate::limits::OUTPUT_CHANNEL_CAPACITY;
use crate::logutil::build_startup_log;
use crate::output_fmt;
use crate::types::{Output, RuntimeConfig, Trace};
use agent_first_data::OutputFormat;
use std::fs::File;
use std::io::Write;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::sync::mpsc;

pub async fn run(req: crate::cli::CliRequest) {
    let crate::cli::CliRequest {
        sql,
        params,
        options,
        session,
        output: output_format,
        output_file,
        log_file,
        log,
        startup_args,
        startup_env,
        startup_requested,
        dry_run,
    } = req;

    let mut sink = match CliOutputSink::new(output_format, output_file, log_file) {
        Ok(sink) => sink,
        Err(err) => {
            crate::emit::emit_cli_error(&err, None, OutputFormat::Json);
            std::process::exit(2);
        }
    };

    if dry_run {
        let dry = Output::DryRun {
            id: None,
            sql: sql.clone(),
            params: params.iter().map(|v| v.to_string()).collect(),
            session: Some("default".to_string()),
            trace: Trace::only_duration(0),
        };
        sink.emit(&dry);
        return;
    }

    let config = RuntimeConfig::default();
    let (tx, mut rx) = mpsc::channel::<Output>(OUTPUT_CHANNEL_CAPACITY);
    let app = Arc::new(App::new(config, tx));

    {
        let mut cfg = app.config.write().await;
        cfg.sessions.insert("default".to_string(), session.clone());
        if !log.is_empty() {
            cfg.log = log.clone();
        }
    }

    if startup_requested {
        let event = build_startup_log(Some("default"), &startup_args, &startup_env);
        sink.emit(&event);
    }

    app.requests_total.fetch_add(1, Ordering::Relaxed);
    handler::execute_query(
        &app,
        None,
        Some("default".to_string()),
        sql,
        params,
        options,
        None,
    )
    .await;

    drop(app);

    let mut had_error = false;
    while let Some(event) = rx.recv().await {
        if matches!(event, Output::Error { .. } | Output::SqlError { .. }) {
            had_error = true;
        }
        sink.emit(&event);
    }

    std::process::exit(if had_error { 1 } else { 0 });
}

struct CliOutputSink {
    format: OutputFormat,
    output_file: Option<File>,
    log_file: Option<File>,
}

impl CliOutputSink {
    fn new(
        format: OutputFormat,
        output_file: Option<String>,
        log_file: Option<String>,
    ) -> Result<Self, String> {
        Ok(Self {
            format,
            output_file: open_output_file(output_file.as_deref(), "-o/--output")?,
            log_file: open_output_file(log_file.as_deref(), "-L/--log-file")?,
        })
    }

    fn emit(&mut self, out: &Output) {
        let rendered = output_fmt::render_output(out, self.format);
        if let Some(file) = self.output_file.as_mut() {
            let _ = writeln!(file, "{rendered}");
        } else {
            let _ = writeln!(std::io::stdout(), "{rendered}");
        }
        if let Some(file) = self.log_file.as_mut() {
            let _ = writeln!(file, "{rendered}");
        }
    }
}

fn open_output_file(path: Option<&str>, flag: &str) -> Result<Option<File>, String> {
    let Some(path) = path else {
        return Ok(None);
    };
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .map(Some)
        .map_err(|e| format!("{flag} file open failed: {e}"))
}
