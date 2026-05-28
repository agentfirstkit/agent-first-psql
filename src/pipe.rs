use crate::config::sessions_to_invalidate;
use crate::conn::resolve_session_name;
use crate::emit::emit_output;
use crate::handler::{self, App, QueryPhase, QueryState};
use crate::limits::{
    MAX_ACTIVE_QUERIES, MAX_PARAMS, MAX_PIPE_LINE_BYTES, MAX_SQL_BYTES, OUTPUT_CHANNEL_CAPACITY,
};
use crate::logutil::build_startup_log;
use crate::protocol::error_code;
use crate::types::*;
use std::collections::HashMap;
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
    if startup_requested {
        let event = build_startup_log(None, &startup_args, &startup_env);
        emit_output(&event, output);
    }

    let (tx, rx) = mpsc::channel::<Output>(OUTPUT_CHANNEL_CAPACITY);
    tokio::spawn(crate::writer::writer_task(rx, output));

    let app = Arc::new(App::new(config, tx));
    let runtime = Arc::new(PipeRuntime::new(app));

    let stdin = tokio::io::stdin();
    let reader = tokio::io::BufReader::new(stdin);
    let mut reader = reader;

    loop {
        let line = match read_limited_line(&mut reader, MAX_PIPE_LINE_BYTES).await {
            Ok(Some(Ok(line))) => line,
            Ok(Some(Err(()))) => {
                send_protocol_error(
                    &runtime.app,
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
                    &runtime.app,
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
                    &runtime.app,
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

        if dispatch_input(&runtime, input).await {
            break;
        }
    }

    wait_for_workers_shutdown(&runtime).await;
    runtime.app.executor.shutdown().await;
    send_close_event(&runtime.app).await;

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
}

struct PipeRuntime {
    app: Arc<App>,
    workers: tokio::sync::Mutex<HashMap<String, SessionWorker>>,
}

struct SessionWorker {
    tx: mpsc::Sender<QueuedQuery>,
    handle: tokio::task::JoinHandle<()>,
}

struct QueuedQuery {
    id: String,
    session: String,
    sql: String,
    params: Vec<serde_json::Value>,
    options: QueryOptions,
    cancel_slot: crate::db::CancelSlot,
    state: Arc<QueryState>,
}

impl PipeRuntime {
    fn new(app: Arc<App>) -> Self {
        Self {
            app,
            workers: tokio::sync::Mutex::new(HashMap::new()),
        }
    }
}

async fn dispatch_input(runtime: &Arc<PipeRuntime>, input: Input) -> bool {
    match input {
        Input::Query {
            id,
            session,
            sql,
            params,
            options,
        } => dispatch_query(runtime, id, session, sql, params, options).await,
        Input::Config(patch) => {
            let sessions = sessions_to_invalidate(&patch);
            let cfg_snapshot = {
                let mut cfg = runtime.app.config.write().await;
                cfg.apply_update(patch);
                cfg.clone()
            };
            runtime.app.executor.invalidate_sessions(&sessions).await;
            let _ = runtime.app.writer.send(Output::Config(cfg_snapshot)).await;
        }
        Input::Cancel { id } => dispatch_cancel(&runtime.app, id).await,
        Input::SessionInfo { id, session } => {
            handler::handle_session_info(&runtime.app, id, session).await;
        }
        Input::Ping => {
            cleanup_finished_queries(&runtime.app).await;
            let _ = runtime
                .app
                .writer
                .send(Output::Pong {
                    trace: PongTrace {
                        uptime_s: runtime.app.start_time.elapsed().as_secs(),
                        requests_total: runtime.app.requests_total.load(Ordering::Relaxed),
                        in_flight: runtime.app.in_flight.lock().await.len(),
                    },
                })
                .await;
        }
        Input::Close => return true,
    }
    false
}

async fn dispatch_query(
    runtime: &Arc<PipeRuntime>,
    id: String,
    session: Option<String>,
    sql: String,
    params: Vec<serde_json::Value>,
    options: QueryOptions,
) {
    let app = &runtime.app;
    if let Some(error) = validate_query_request(&id, &sql, &params) {
        let _ = app.writer.send(error).await;
        return;
    }

    cleanup_finished_queries(app).await;

    let key = id.clone();
    let mut rejection: Option<Output> = None;
    let cancel_slot = crate::db::new_cancel_slot();
    let state = Arc::new(QueryState::queued());
    {
        let mut in_flight = app.in_flight.lock().await;
        if let Some(existing) = in_flight.get(&key) {
            if existing.state.is_finished() {
                let _ = in_flight.remove(&key);
            } else {
                rejection = Some(Output::Error {
                    id: Some(key.clone()),
                    error_code: error_code::INVALID_REQUEST.to_string(),
                    error: "duplicate active query id".to_string(),
                    hint: Some(
                        "pick a unique `id` per in-flight query, or cancel the prior one first"
                            .to_string(),
                    ),
                    retryable: false,
                    trace: Trace::only_duration(0),
                });
            }
        }

        if rejection.is_none() && in_flight.len() >= MAX_ACTIVE_QUERIES {
            rejection = Some(Output::Error {
                id: Some(key.clone()),
                error_code: error_code::INVALID_REQUEST.to_string(),
                error: "too many queued or running queries".to_string(),
                hint: Some(format!(
                    "maximum queued or running queries is {MAX_ACTIVE_QUERIES}"
                )),
                retryable: true,
                trace: Trace::only_duration(0),
            });
        }

        if rejection.is_none() {
            in_flight.insert(
                key.clone(),
                handler::InFlightQuery {
                    cancel_slot: cancel_slot.clone(),
                    state: state.clone(),
                },
            );
        }
    }

    if let Some(output) = rejection {
        let _ = app.writer.send(output).await;
        return;
    }

    let resolved_session = {
        let cfg = app.config.read().await;
        resolve_session_name(&cfg, session.as_deref())
    };
    let tx = get_session_worker(runtime, &resolved_session).await;
    app.requests_total.fetch_add(1, Ordering::Relaxed);
    let queued = QueuedQuery {
        id: key.clone(),
        session: resolved_session,
        sql,
        params,
        options,
        cancel_slot,
        state,
    };
    if tx.send(queued).await.is_err() {
        let _ = app.in_flight.lock().await.remove(&key);
        let _ = app
            .writer
            .send(Output::Error {
                id: Some(key),
                error_code: error_code::INVALID_REQUEST.to_string(),
                error: "session worker is unavailable".to_string(),
                hint: Some("retry the query; the session worker will be restarted".to_string()),
                retryable: true,
                trace: Trace::only_duration(0),
            })
            .await;
    }
}

async fn dispatch_cancel(app: &Arc<App>, id: String) {
    let query = {
        let in_flight = app.in_flight.lock().await;
        in_flight.get(&id).cloned()
    };

    if let Some(query) = query {
        if query.state.phase() == QueryPhase::Finished {
            let _ = app.in_flight.lock().await.remove(&id);
            let _ = app
                .writer
                .send(Output::Error {
                    id: Some(id),
                    error_code: error_code::INVALID_REQUEST.to_string(),
                    error: "query already finished".to_string(),
                    hint: Some(
                        "cancel raced completion; the prior result/error event holds the outcome"
                            .to_string(),
                    ),
                    retryable: false,
                    trace: Trace::only_duration(0),
                })
                .await;
        } else {
            query.state.set_phase(QueryPhase::Cancelled);
            let hint = match query.cancel_server_query().await {
                Ok(true) => Some("server-side cancel requested".to_string()),
                Ok(false) => {
                    Some("query cancelled before execution reached the database".to_string())
                }
                Err(e) => Some(e),
            };
            let _ = app.in_flight.lock().await.remove(&id);
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
                error: "no queued or running query with this id".to_string(),
                hint: Some(
                    "no matching in-flight query for this id; it may have already completed or never been submitted"
                        .to_string(),
                ),
                retryable: false,
                trace: Trace::only_duration(0),
            })
            .await;
    }
}

async fn get_session_worker(
    runtime: &Arc<PipeRuntime>,
    session: &str,
) -> mpsc::Sender<QueuedQuery> {
    let mut workers = runtime.workers.lock().await;
    if workers
        .get(session)
        .map(|worker| !worker.handle.is_finished())
        .unwrap_or(false)
    {
        if let Some(worker) = workers.get(session) {
            return worker.tx.clone();
        }
    }

    workers.remove(session);
    let (tx, rx) = mpsc::channel(MAX_ACTIVE_QUERIES);
    let app = runtime.app.clone();
    let handle = tokio::spawn(async move {
        session_worker_loop(app, rx).await;
    });
    workers.insert(
        session.to_string(),
        SessionWorker {
            tx: tx.clone(),
            handle,
        },
    );
    tx
}

async fn session_worker_loop(app: Arc<App>, mut rx: mpsc::Receiver<QueuedQuery>) {
    while let Some(query) = rx.recv().await {
        if query.cancel_slot.is_cancelled() || !query.state.try_start() {
            query.state.set_phase(QueryPhase::Cancelled);
            continue;
        }

        handler::execute_query(
            &app,
            Some(query.id),
            Some(query.session),
            query.sql,
            query.params,
            query.options,
            Some(query.cancel_slot.clone()),
        )
        .await;

        if query.cancel_slot.is_cancelled() || query.state.phase() == QueryPhase::Cancelled {
            query.state.set_phase(QueryPhase::Cancelled);
        } else {
            query.state.set_phase(QueryPhase::Finished);
        }
    }
}

async fn cleanup_finished_queries(app: &Arc<App>) {
    app.in_flight
        .lock()
        .await
        .retain(|_, query| !query.state.is_finished());
}

async fn wait_for_workers_shutdown(runtime: &Arc<PipeRuntime>) {
    let handles: Vec<tokio::task::JoinHandle<()>> = runtime
        .workers
        .lock()
        .await
        .drain()
        .map(|(_, worker)| worker.handle)
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
        || session.ssh.has_transport_fields()
        || session.container.has_transport_fields()
}
