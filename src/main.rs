#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::disallowed_methods,
    clippy::disallowed_macros
)]

mod cli;
mod config;
mod conn;
mod db;
mod handler;
#[cfg(feature = "mcp")]
mod mcp;
mod types;
mod writer;

use agent_first_data::OutputFormat;
use cli::Mode;
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
            emit_cli_error(&e, OutputFormat::Json);
            std::process::exit(2);
        }
    };

    match mode {
        Mode::Cli(req) => run_cli(req).await,
        Mode::Pipe(init) => run_pipe(init).await,
        #[cfg(feature = "mcp")]
        Mode::Mcp(init) => mcp::run_mcp(init.session, init.log).await,
    }
}

async fn run_cli(req: cli::CliRequest) {
    let config = RuntimeConfig::default();
    let (tx, mut rx) = mpsc::channel::<Output>(OUTPUT_CHANNEL_CAPACITY);
    let app = Arc::new(App::new(config, tx));

    let mut cfg = app.config.write().await;
    cfg.sessions
        .insert("default".to_string(), req.session.clone());
    if !req.log.is_empty() {
        cfg.log = req.log.clone();
    }
    drop(cfg);

    app.requests_total.fetch_add(1, Ordering::Relaxed);
    handler::execute_query(
        &app,
        None,
        Some("default".to_string()),
        req.sql,
        req.params,
        req.options,
    )
    .await;

    drop(app);

    let mut had_error = false;
    while let Some(output) = rx.recv().await {
        if matches!(output, Output::Error { .. } | Output::SqlError { .. }) {
            had_error = true;
        }
        emit_output(&output, req.output);
    }

    std::process::exit(if had_error { 1 } else { 0 });
}

async fn run_pipe(init: cli::PipeInit) {
    let mut config = RuntimeConfig::default();
    if has_session_override(&init.session) {
        config
            .sessions
            .insert(config.default_session.clone(), init.session.clone());
    }
    if !init.log.is_empty() {
        config.log = init.log.clone();
    }

    let (tx, rx) = mpsc::channel::<Output>(OUTPUT_CHANNEL_CAPACITY);
    tokio::spawn(writer::writer_task(rx, init.output));

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
                let app2 = app.clone();
                app.requests_total.fetch_add(1, Ordering::Relaxed);
                let key = id.clone();
                let handle = tokio::spawn(async move {
                    handler::execute_query(&app2, Some(id), session, sql, params, options).await;
                });
                app.in_flight.lock().await.insert(key, handle);
            }
            Input::Config(patch) => {
                let mut cfg = app.config.write().await;
                cfg.apply_update(patch);
                let _ = app.writer.send(Output::Config(cfg.clone())).await;
            }
            Input::Cancel { id } => {
                if let Some(handle) = app.in_flight.lock().await.remove(&id) {
                    handle.abort();
                    let _ = app
                        .writer
                        .send(Output::Error {
                            id: Some(id),
                            error_code: "cancelled".to_string(),
                            error: "query cancelled".to_string(),
                            retryable: false,
                            trace: Trace::only_duration(0),
                        })
                        .await;
                } else {
                    let _ = app
                        .writer
                        .send(Output::Error {
                            id: Some(id),
                            error_code: "invalid_request".to_string(),
                            error: "no in-flight query with this id".to_string(),
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

fn emit_cli_error(msg: &str, format: OutputFormat) {
    let value = agent_first_data::build_cli_error(msg);
    let rendered = agent_first_data::cli_output(&value, format);
    println!("{rendered}");
}

fn emit_output(out: &Output, format: OutputFormat) {
    let value = serde_json::to_value(out).unwrap_or(serde_json::Value::Null);
    let rendered = agent_first_data::cli_output(&value, format);
    println!("{rendered}");
}

#[cfg(test)]
#[path = "../tests/support/unit_main.rs"]
mod tests;
