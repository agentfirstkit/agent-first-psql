use crate::db::ExecRequest;
use crate::handler;
use crate::handler::App;
use crate::limits::OUTPUT_CHANNEL_CAPACITY;
use crate::logutil::build_startup_log;
use crate::output_fmt;
use crate::protocol::{log_enabled, log_event};
use crate::types::{Output, QueryOptions, RuntimeConfig, Trace};
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
        psql_mode,
    } = req;

    let mut sink = match CliOutputSink::new(output_format, output_file, log_file) {
        Ok(sink) => sink,
        Err(err) => {
            crate::emit::emit_cli_error(&err, None, OutputFormat::Json);
            std::process::exit(2);
        }
    };

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

    if dry_run {
        let start = std::time::Instant::now();
        let cfg = app.config.read().await.clone();
        let session_cfg = cfg.sessions.get("default").cloned().unwrap_or_default();
        let resolved_opts = cfg
            .resolve_options_for_session(&options, &session_cfg)
            .unwrap_or_else(|_| cfg.resolve_options(&options));
        let outcome = app
            .executor
            .prepare_only(ExecRequest {
                session_name: "default",
                session_cfg: &session_cfg,
                sql: &sql,
                params: &params,
                opts: &resolved_opts,
                cancel_slot: None,
                transport_log: None,
            })
            .await;
        let mut had_error = false;
        match outcome {
            Ok(info) => {
                let trace = Trace::only_duration(start.elapsed().as_millis() as u64);
                sink.emit(&Output::DryRun {
                    id: None,
                    sql: sql.clone(),
                    params: params.iter().map(|v| v.to_string()).collect(),
                    session: Some("default".to_string()),
                    param_types: info.param_types,
                    columns: info.columns,
                    trace,
                });
            }
            Err(err) => {
                had_error = true;
                handler::emit_exec_error(&app, None, "default", err, start).await;
            }
        }
        app.executor.shutdown().await;
        drop(app);
        while let Some(event) = rx.recv().await {
            sink.emit(&event);
        }
        std::process::exit(if had_error { 1 } else { 0 });
    }

    if startup_requested {
        let event = build_startup_log(Some("default"), &startup_args, &startup_env);
        sink.emit(&event);
    }

    if psql_mode && log_enabled(&log, log_event::MODE_PERMISSION_DEFAULT_CHANGED) {
        sink.emit(&psql_mode_permission_event(&options));
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

    app.executor.shutdown().await;
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

fn psql_mode_permission_event(options: &QueryOptions) -> Output {
    let permission = options.permission.map(|p| p.as_str()).unwrap_or("write");
    let mut config = serde_json::Map::new();
    config.insert("mode".to_string(), serde_json::Value::from("psql"));
    config.insert(
        "permission".to_string(),
        serde_json::Value::from(permission),
    );
    config.insert(
        "note".to_string(),
        serde_json::Value::from(
            "psql mode inherits psql's writable default; native mode defaults to read",
        ),
    );
    Output::Log {
        event: log_event::MODE_PERMISSION_DEFAULT_CHANGED.to_string(),
        request_id: None,
        session: Some("default".to_string()),
        error_code: None,
        command_tag: None,
        version: None,
        config: Some(serde_json::Value::Object(config)),
        args: None,
        env: None,
        chain: None,
        trace: Trace::only_duration(0),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Permission;

    #[test]
    fn psql_mode_event_reports_resolved_permission_and_filter_prefix() {
        let opts = QueryOptions {
            permission: Some(Permission::ContainerWrite),
            ..Default::default()
        };
        let emitted = psql_mode_permission_event(&opts);
        assert!(matches!(emitted, Output::Log { .. }));
        let Output::Log { event, config, .. } = emitted else {
            return;
        };
        assert_eq!(event, "mode.permission_default_changed");
        let cfg = config.unwrap_or_default();
        assert_eq!(cfg.get("mode").and_then(|v| v.as_str()), Some("psql"));
        assert_eq!(
            cfg.get("permission").and_then(|v| v.as_str()),
            Some("container-write")
        );
        assert!(cfg.get("note").is_some());
        assert!(log_enabled(
            &["mode".to_string()],
            log_event::MODE_PERMISSION_DEFAULT_CHANGED
        ));
        assert!(!log_enabled(
            &[],
            log_event::MODE_PERMISSION_DEFAULT_CHANGED
        ));
    }
}
