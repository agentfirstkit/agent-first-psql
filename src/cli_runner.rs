use crate::emit::emit_output;
use crate::handler;
use crate::handler::App;
use crate::limits::OUTPUT_CHANNEL_CAPACITY;
use crate::logutil::build_startup_log;
use crate::types::{Output, RuntimeConfig, Trace};
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
        log,
        startup_args,
        startup_env,
        startup_requested,
        dry_run,
    } = req;

    if dry_run {
        let dry = Output::DryRun {
            id: None,
            sql: sql.clone(),
            params: params.iter().map(|v| v.to_string()).collect(),
            session: Some("default".to_string()),
            trace: Trace::only_duration(0),
        };
        emit_output(&dry, output_format);
        return;
    }

    let config = RuntimeConfig::default();
    let (tx, mut rx) = mpsc::channel::<Output>(OUTPUT_CHANNEL_CAPACITY);
    let app = Arc::new(App::new(config, tx));

    let mut cfg = app.config.write().await;
    cfg.sessions.insert("default".to_string(), session.clone());
    if !log.is_empty() {
        cfg.log = log.clone();
    }
    let startup_config = cfg.clone();
    drop(cfg);

    if !log.is_empty() || startup_requested {
        let event = build_startup_log(
            Some("default"),
            &startup_config,
            &startup_args,
            &startup_env,
        );
        emit_output(&event, output_format);
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
        emit_output(&event, output_format);
    }

    std::process::exit(if had_error { 1 } else { 0 });
}
