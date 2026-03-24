#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::print_stdout,
        clippy::print_stderr,
    )
)]

mod cli;
mod config;
mod conn;
mod db;
mod handler;
mod output_fmt;
mod types;
mod writer;

use std::io::Write;

use agent_first_data::{cli_output, OutputFormat};
use cli::Mode;
use config::sessions_to_invalidate;
use handler::App;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::AsyncBufReadExt;
use tokio::sync::mpsc;
use types::*;

const OUTPUT_CHANNEL_CAPACITY: usize = 4096;

#[tokio::main]
async fn main() {
    let mode = match cli::parse_args() {
        Ok(m) => m,
        Err(e) => {
            emit_cli_error(&e, None, OutputFormat::Json);
            std::process::exit(2);
        }
    };

    match mode {
        Mode::Cli(req) => run_cli(req).await,
        Mode::Pipe(init) => run_pipe(init).await,
    }
}

async fn run_cli(req: cli::CliRequest) {
    let cli::CliRequest {
        sql,
        params,
        options,
        session,
        output: output_format,
        log,
        startup_argv,
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
            &startup_argv,
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

async fn run_pipe(init: cli::PipeInit) {
    let cli::PipeInit {
        output,
        session,
        log,
        startup_argv,
        startup_args,
        startup_env,
        startup_requested,
    } = init;

    let mut config = RuntimeConfig::default();
    if has_session_override(&session) {
        config
            .sessions
            .insert(config.default_session.clone(), session.clone());
    }
    if !log.is_empty() {
        config.log = log.clone();
    }
    let startup_config = config.clone();

    if !log.is_empty() || startup_requested {
        let event = build_startup_log(
            None,
            &startup_config,
            &startup_argv,
            &startup_args,
            &startup_env,
        );
        emit_output(&event, output);
    }

    let (tx, rx) = mpsc::channel::<Output>(OUTPUT_CHANNEL_CAPACITY);
    tokio::spawn(writer::writer_task(rx, output));

    let app = Arc::new(App::new(config, tx));

    let stdin = tokio::io::stdin();
    let reader = tokio::io::BufReader::new(stdin);
    let mut lines = reader.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let input: Input = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                let _ = app
                    .writer
                    .send(Output::Error {
                        id: None,
                        error_code: "invalid_request".to_string(),
                        error: format!("parse error: {e}"),
                        hint: None,
                        retryable: false,
                        trace: Trace::only_duration(0),
                    })
                    .await;
                continue;
            }
        };

        match input {
            Input::Query {
                id,
                session,
                sql,
                params,
                options,
            } => {
                let key = id.clone();
                let mut reject_duplicate = false;
                {
                    let mut in_flight = app.in_flight.lock().await;
                    if let Some(existing) = in_flight.get(&key) {
                        if existing.is_finished() {
                            in_flight.remove(&key);
                        } else {
                            reject_duplicate = true;
                        }
                    }

                    if !reject_duplicate {
                        let app2 = app.clone();
                        app.requests_total.fetch_add(1, Ordering::Relaxed);
                        let handle = tokio::spawn(async move {
                            handler::execute_query(&app2, Some(id), session, sql, params, options)
                                .await;
                        });
                        in_flight.insert(key.clone(), handle);
                    }
                }

                if reject_duplicate {
                    let _ = app
                        .writer
                        .send(Output::Error {
                            id: Some(key),
                            error_code: "invalid_request".to_string(),
                            error: "duplicate in-flight query id".to_string(),
                            hint: None,
                            retryable: false,
                            trace: Trace::only_duration(0),
                        })
                        .await;
                }
            }
            Input::Config(patch) => {
                let sessions = sessions_to_invalidate(&patch);
                let cfg_snapshot = {
                    let mut cfg = app.config.write().await;
                    cfg.apply_update(patch);
                    cfg.clone()
                };
                app.executor.invalidate_sessions(&sessions).await;
                let _ = app.writer.send(Output::Config(cfg_snapshot)).await;
            }
            Input::Cancel { id } => {
                if let Some(handle) = app.in_flight.lock().await.remove(&id) {
                    if handle.is_finished() {
                        let _ = app
                            .writer
                            .send(Output::Error {
                                id: Some(id),
                                error_code: "invalid_request".to_string(),
                                error: "query already finished".to_string(),
                                hint: None,
                                retryable: false,
                                trace: Trace::only_duration(0),
                            })
                            .await;
                    } else {
                        handle.abort();
                        let _ = app
                            .writer
                            .send(Output::Error {
                                id: Some(id),
                                error_code: "cancelled".to_string(),
                                error: "query cancelled".to_string(),
                                hint: None,
                                retryable: false,
                                trace: Trace::only_duration(0),
                            })
                            .await;
                    }
                } else {
                    let _ = app
                        .writer
                        .send(Output::Error {
                            id: Some(id),
                            error_code: "invalid_request".to_string(),
                            error: "no in-flight query with this id".to_string(),
                            hint: None,
                            retryable: false,
                            trace: Trace::only_duration(0),
                        })
                        .await;
                }
            }
            Input::Ping => {
                let _ = app
                    .writer
                    .send(Output::Pong {
                        trace: PongTrace {
                            uptime_s: app.start_time.elapsed().as_secs(),
                            requests_total: app.requests_total.load(Ordering::Relaxed),
                            in_flight: app.in_flight.lock().await.len(),
                        },
                    })
                    .await;
            }
            Input::Close => break,
        }

        app.in_flight.lock().await.retain(|_, h| !h.is_finished());
    }

    let handles: Vec<tokio::task::JoinHandle<()>> =
        app.in_flight.lock().await.drain().map(|(_, h)| h).collect();
    let deadline = Instant::now() + std::time::Duration::from_secs(5);
    for handle in handles {
        let now = Instant::now();
        let remain = deadline.saturating_duration_since(now);
        if tokio::time::timeout(remain, handle).await.is_err() {
            // timeout waiting this task; move on
        }
    }

    let _ = app
        .writer
        .send(Output::Close {
            message: "shutdown".to_string(),
            trace: CloseTrace {
                uptime_s: app.start_time.elapsed().as_secs(),
                requests_total: app.requests_total.load(Ordering::Relaxed),
            },
        })
        .await;

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
}

fn has_session_override(session: &SessionConfig) -> bool {
    session.dsn_secret.is_some()
        || session.conninfo_secret.is_some()
        || session.host.is_some()
        || session.port.is_some()
        || session.user.is_some()
        || session.dbname.is_some()
        || session.password_secret.is_some()
}

fn build_startup_log(
    session: Option<&str>,
    config: &RuntimeConfig,
    argv: &[String],
    args: &serde_json::Value,
    env: &serde_json::Value,
) -> Output {
    Output::Log {
        event: "startup".to_string(),
        request_id: None,
        session: session.map(std::string::ToString::to_string),
        error_code: None,
        command_tag: None,
        version: Some(config::VERSION.to_string()),
        argv: Some(argv.to_vec()),
        config: Some(serde_json::to_value(config).unwrap_or(serde_json::Value::Null)),
        args: Some(args.clone()),
        env: Some(env.clone()),
        trace: Trace::only_duration(0),
    }
}

fn emit_cli_error(msg: &str, hint: Option<&str>, format: OutputFormat) {
    let value = agent_first_data::build_cli_error(msg, hint);
    let rendered = cli_output(&value, format);
    let _ = writeln!(std::io::stdout(), "{rendered}");
}

fn emit_output(out: &Output, format: OutputFormat) {
    let rendered = output_fmt::render_output(out, format);
    let _ = writeln!(std::io::stdout(), "{rendered}");
}

#[cfg(test)]
#[path = "../tests/support/unit_main.rs"]
mod tests;
