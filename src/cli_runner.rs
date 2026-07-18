use crate::db::ExecRequest;
use crate::handler;
use crate::handler::App;
use crate::limits::OUTPUT_CHANNEL_CAPACITY;
use crate::logutil::build_startup_log;
use crate::protocol::log_event;
use crate::types::{Output, QueryOptions, RuntimeConfig, Trace};
use agent_first_data::OutputFormat;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::sync::mpsc;

pub async fn run(
    req: crate::cli::CliRequest,
    capability: crate::Capability,
    locked_readonly_profile: bool,
) {
    let crate::cli::CliRequest {
        sql,
        params,
        options,
        session,
        output: output_format,
        log,
        startup_args,
        startup_env,
        startup_requested,
        dry_run,
        psql_mode,
    } = req;

    let stdout = std::io::stdout();
    let mut sink = CliOutputSink::new(stdout.lock(), output_format);

    let config = RuntimeConfig::default();
    let (tx, mut rx) = mpsc::channel::<Output>(OUTPUT_CHANNEL_CAPACITY);
    let app = Arc::new(App::new(config, tx, capability));
    app.locked_readonly_profile
        .store(locked_readonly_profile, Ordering::Relaxed);

    {
        let mut cfg = app.config.write().await;
        cfg.sessions.insert("default".to_string(), session.clone());
        if !log.is_empty() {
            cfg.log = log.clone();
        }
    }

    if dry_run {
        let cfg = app.config.read().await.clone();
        let session_cfg = cfg.sessions.get("default").cloned().unwrap_or_default();
        let resolved_opts = match cfg.resolve_options_for_session(&options, &session_cfg) {
            Ok(options) => options,
            Err(error) => {
                if crate::emit::emit_cli_error(&error, None, output_format).is_err() {
                    std::process::exit(4);
                }
                std::process::exit(2);
            }
        };
        if capability == crate::Capability::ReadOnly
            && let Err(error) = crate::readonly_policy::validate_sql(&sql)
        {
            if crate::emit::emit_cli_error(&error, Some(crate::readonly_hint()), output_format)
                .is_err()
            {
                std::process::exit(4);
            }
            std::process::exit(2);
        }
        if capability == crate::Capability::ReadOnly && !resolved_opts.read_only {
            if crate::emit::emit_cli_error(
                "write permission is unavailable in afpsql-readonly",
                Some(crate::readonly_hint()),
                output_format,
            )
            .is_err()
            {
                std::process::exit(4);
            }
            std::process::exit(2);
        }
        let start = std::time::Instant::now();
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
                if sink
                    .emit(&Output::DryRun {
                        id: None,
                        sql: sql.clone(),
                        params: params.iter().map(|v| v.to_string()).collect(),
                        session: Some("default".to_string()),
                        param_types: info.param_types,
                        columns: info.columns,
                        trace,
                    })
                    .is_err()
                {
                    std::process::exit(4);
                }
            }
            Err(err) => {
                had_error = true;
                handler::emit_exec_error(&app, None, "default", err, start).await;
            }
        }
        app.executor.shutdown().await;
        drop(app);
        while let Some(event) = rx.recv().await {
            if sink.emit(&event).is_err() {
                std::process::exit(4);
            }
        }
        std::process::exit(if had_error { 1 } else { 0 });
    }

    if startup_requested {
        let event = build_startup_log(Some("default"), &startup_args, &startup_env);
        if sink.emit(&event).is_err() {
            std::process::exit(4);
        }
    }

    if psql_mode
        && log.enabled(log_event::MODE_PERMISSION_DEFAULT_CHANGED)
        && sink.emit(&psql_mode_permission_event(&options)).is_err()
    {
        std::process::exit(4);
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
        if sink.emit(&event).is_err() {
            std::process::exit(4);
        }
    }

    std::process::exit(if had_error { 1 } else { 0 });
}

struct CliOutputSink<W: Write> {
    writer: W,
    format: OutputFormat,
}

impl<W: Write> CliOutputSink<W> {
    fn new(writer: W, format: OutputFormat) -> Self {
        Self { writer, format }
    }

    fn emit(&mut self, out: &Output) -> Result<(), agent_first_data::CliEmitterError> {
        crate::output_fmt::emit_output(&mut self.writer, out, self.format)
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
        assert!(
            agent_first_data::LogFilters::new(["mode"])
                .enabled(log_event::MODE_PERMISSION_DEFAULT_CHANGED)
        );
        assert!(
            !agent_first_data::LogFilters::default()
                .enabled(log_event::MODE_PERMISSION_DEFAULT_CHANGED)
        );
    }
}
