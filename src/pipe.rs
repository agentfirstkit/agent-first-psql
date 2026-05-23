use crate::config::sessions_to_invalidate;
use crate::emit::emit_output;
use crate::handler::{self, App};
use crate::limits::{
    MAX_IN_FLIGHT, MAX_PARAMS, MAX_PIPE_LINE_BYTES, MAX_SQL_BYTES, OUTPUT_CHANNEL_CAPACITY,
};
use crate::logutil::build_startup_log;
use crate::protocol::error_code;
use crate::types::*;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncBufRead, AsyncBufReadExt};
use tokio::sync::mpsc;

pub async fn run(init: crate::cli::PipeInit) {
    let crate::cli::PipeInit {
        output,
        session,
        log,
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
        let event = build_startup_log(None, &startup_config, &startup_args, &startup_env);
        emit_output(&event, output);
    }

    let (tx, rx) = mpsc::channel::<Output>(OUTPUT_CHANNEL_CAPACITY);
    tokio::spawn(crate::writer::writer_task(rx, output));

    let app = Arc::new(App::new(config, tx));

    let stdin = tokio::io::stdin();
    let reader = tokio::io::BufReader::new(stdin);
    let mut reader = reader;

    loop {
        let line = match read_limited_line(&mut reader, MAX_PIPE_LINE_BYTES).await {
            Ok(Some(Ok(line))) => line,
            Ok(Some(Err(()))) => {
                send_protocol_error(
                    &app,
                    None,
                    error_code::INVALID_REQUEST,
                    "input line exceeds maximum size",
                    Some("split large requests or reduce SQL/params payload size"),
                    false,
                )
                .await;
                continue;
            }
            Ok(None) => break,
            Err(e) => {
                send_protocol_error(
                    &app,
                    None,
                    error_code::INVALID_REQUEST,
                    &format!("read error: {e}"),
                    None,
                    false,
                )
                .await;
                break;
            }
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let input: Input = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                send_protocol_error(
                    &app,
                    None,
                    error_code::INVALID_REQUEST,
                    &format!("parse error: {e}"),
                    None,
                    false,
                )
                .await;
                continue;
            }
        };

        if dispatch_input(&app, input).await {
            break;
        }
        app.in_flight.lock().await.retain(|_, h| !h.is_finished());
    }

    wait_for_in_flight_shutdown(&app).await;
    send_close_event(&app).await;

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
}

async fn dispatch_input(app: &Arc<App>, input: Input) -> bool {
    match input {
        Input::Query {
            id,
            session,
            sql,
            params,
            options,
        } => dispatch_query(app, id, session, sql, params, options).await,
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
        Input::Cancel { id } => dispatch_cancel(app, id).await,
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
        Input::Close => return true,
    }
    false
}

async fn dispatch_query(
    app: &Arc<App>,
    id: String,
    session: Option<String>,
    sql: String,
    params: Vec<serde_json::Value>,
    options: QueryOptions,
) {
    if let Some(error) = validate_query_request(&id, &sql, &params) {
        let _ = app.writer.send(error).await;
        return;
    }

    let key = id.clone();
    let mut rejection: Option<Output> = None;
    {
        let mut in_flight = app.in_flight.lock().await;
        in_flight.retain(|_, h| !h.is_finished());
        if let Some(existing) = in_flight.get(&key) {
            if existing.is_finished() {
                in_flight.remove(&key);
            } else {
                rejection = Some(Output::Error {
                    id: Some(key.clone()),
                    error_code: error_code::INVALID_REQUEST.to_string(),
                    error: "duplicate in-flight query id".to_string(),
                    hint: None,
                    retryable: false,
                    trace: Trace::only_duration(0),
                });
            }
        }

        if rejection.is_none() && in_flight.len() >= MAX_IN_FLIGHT {
            rejection = Some(Output::Error {
                id: Some(key.clone()),
                error_code: error_code::INVALID_REQUEST.to_string(),
                error: "too many in-flight queries".to_string(),
                hint: Some(format!("maximum in-flight queries is {MAX_IN_FLIGHT}")),
                retryable: true,
                trace: Trace::only_duration(0),
            });
        }

        if rejection.is_none() {
            let app2 = app.clone();
            let cancel_slot = crate::db::new_cancel_slot();
            let task_cancel_slot = cancel_slot.clone();
            app.requests_total.fetch_add(1, Ordering::Relaxed);
            let handle = tokio::spawn(async move {
                handler::execute_query(
                    &app2,
                    Some(id),
                    session,
                    sql,
                    params,
                    options,
                    Some(task_cancel_slot),
                )
                .await;
            });
            in_flight.insert(
                key.clone(),
                handler::InFlightQuery {
                    handle,
                    cancel_slot,
                },
            );
        }
    }

    if let Some(output) = rejection {
        let _ = app.writer.send(output).await;
    }
}

async fn dispatch_cancel(app: &Arc<App>, id: String) {
    if let Some(query) = app.in_flight.lock().await.remove(&id) {
        if query.is_finished() {
            let _ = app
                .writer
                .send(Output::Error {
                    id: Some(id),
                    error_code: error_code::INVALID_REQUEST.to_string(),
                    error: "query already finished".to_string(),
                    hint: None,
                    retryable: false,
                    trace: Trace::only_duration(0),
                })
                .await;
        } else {
            let hint = match query.cancel_server_query().await {
                Ok(true) => Some("server-side cancel requested".to_string()),
                Ok(false) => {
                    Some("query cancelled before database connection was ready".to_string())
                }
                Err(e) => Some(e),
            };
            query.abort();
            let _ = app
                .writer
                .send(Output::Error {
                    id: Some(id),
                    error_code: error_code::CANCELLED.to_string(),
                    error: "query cancelled".to_string(),
                    hint,
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
                error_code: error_code::INVALID_REQUEST.to_string(),
                error: "no in-flight query with this id".to_string(),
                hint: None,
                retryable: false,
                trace: Trace::only_duration(0),
            })
            .await;
    }
}

async fn wait_for_in_flight_shutdown(app: &Arc<App>) {
    let handles: Vec<tokio::task::JoinHandle<()>> = app
        .in_flight
        .lock()
        .await
        .drain()
        .map(|(_, q)| q.into_handle())
        .collect();
    let deadline = Instant::now() + std::time::Duration::from_secs(5);
    for handle in handles {
        let now = Instant::now();
        let remain = deadline.saturating_duration_since(now);
        if tokio::time::timeout(remain, handle).await.is_err() {
            // timeout waiting this task; move on
        }
    }
}

async fn send_close_event(app: &Arc<App>) {
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
}

pub(crate) async fn read_limited_line<R>(
    reader: &mut R,
    max_bytes: usize,
) -> std::io::Result<Option<Result<String, ()>>>
where
    R: AsyncBufRead + Unpin,
{
    let mut out = Vec::new();
    let mut too_long = false;

    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if out.is_empty() && !too_long {
                return Ok(None);
            }
            break;
        }

        let take = available
            .iter()
            .position(|b| *b == b'\n')
            .map_or(available.len(), |pos| pos + 1);

        if !too_long {
            if out.len() + take <= max_bytes {
                out.extend_from_slice(&available[..take]);
            } else {
                too_long = true;
            }
        }

        let ended = available.get(take.saturating_sub(1)) == Some(&b'\n');
        reader.consume(take);
        if ended {
            break;
        }
    }

    if too_long {
        return Ok(Some(Err(())));
    }

    Ok(Some(Ok(String::from_utf8_lossy(&out).to_string())))
}

pub(crate) fn validate_query_request(
    id: &str,
    sql: &str,
    params: &[serde_json::Value],
) -> Option<Output> {
    if sql.len() > MAX_SQL_BYTES {
        return Some(Output::Error {
            id: Some(id.to_string()),
            error_code: error_code::INVALID_REQUEST.to_string(),
            error: "sql exceeds maximum size".to_string(),
            hint: Some(format!("maximum SQL size is {MAX_SQL_BYTES} bytes")),
            retryable: false,
            trace: Trace::only_duration(0),
        });
    }
    if params.len() > MAX_PARAMS {
        return Some(Output::Error {
            id: Some(id.to_string()),
            error_code: error_code::INVALID_REQUEST.to_string(),
            error: "too many params".to_string(),
            hint: Some(format!("maximum params is {MAX_PARAMS}")),
            retryable: false,
            trace: Trace::only_duration(0),
        });
    }
    None
}

async fn send_protocol_error(
    app: &Arc<App>,
    id: Option<String>,
    error_code: &str,
    error: &str,
    hint: Option<&str>,
    retryable: bool,
) {
    let _ = app
        .writer
        .send(Output::Error {
            id,
            error_code: error_code.to_string(),
            error: error.to_string(),
            hint: hint.map(std::string::ToString::to_string),
            retryable,
            trace: Trace::only_duration(0),
        })
        .await;
}

pub(crate) fn has_session_override(session: &SessionConfig) -> bool {
    session.dsn_secret.is_some()
        || session.conninfo_secret.is_some()
        || session.host.is_some()
        || session.port.is_some()
        || session.user.is_some()
        || session.dbname.is_some()
        || session.password_secret.is_some()
}
