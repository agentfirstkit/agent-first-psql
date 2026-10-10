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
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;
use tokio::io::{AsyncBufRead, AsyncBufReadExt};
use tokio::sync::mpsc;

const PIPE_SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

pub(crate) enum PipeInput {
    WriterStopped,
    Line(std::io::Result<Option<Result<String, ()>>>),
}

pub(crate) async fn next_pipe_input<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    writer: &mut tokio::task::JoinHandle<Result<(), agent_first_data::CliEmitterError>>,
) -> PipeInput {
    tokio::select! {
        _ = writer => PipeInput::WriterStopped,
        read = read_limited_line(reader, MAX_PIPE_LINE_BYTES) => PipeInput::Line(read),
    }
}

pub async fn run(
    init: crate::cli::PipeInit,
    capability: crate::Capability,
    locked_readonly_profile: bool,
) {
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
        if emit_output(&event, output).is_err() {
            std::process::exit(4);
        }
    }

    let (tx, rx) = mpsc::channel::<Output>(OUTPUT_CHANNEL_CAPACITY);
    let mut writer = tokio::spawn(crate::writer::writer_task(rx, output));

    let app = Arc::new(App::new(config, tx, capability));
    app.locked_readonly_profile
        .store(locked_readonly_profile, Ordering::Relaxed);
    let runtime = Arc::new(PipeRuntime::new(app));

    let stdin = tokio::io::stdin();
    let reader = tokio::io::BufReader::new(stdin);
    let mut reader = reader;

    loop {
        let read = match next_pipe_input(&mut reader, &mut writer).await {
            PipeInput::WriterStopped => {
                shutdown_runtime(
                    &runtime,
                    tokio::time::Instant::now() + PIPE_SHUTDOWN_TIMEOUT,
                )
                .await;
                std::process::exit(4);
            }
            PipeInput::Line(read) => read,
        };
        let line = match read {
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

    shutdown_runtime(
        &runtime,
        tokio::time::Instant::now() + PIPE_SHUTDOWN_TIMEOUT,
    )
    .await;
    send_close_event(&runtime.app).await;
    drop(runtime);
    match writer.await {
        Ok(Ok(())) => {}
        Ok(Err(_)) | Err(_) => std::process::exit(4),
    }
}

struct PipeRuntime {
    app: Arc<App>,
    workers: tokio::sync::Mutex<HashMap<String, SessionWorker>>,
}

struct SessionWorker {
    tx: mpsc::Sender<SessionOp>,
    handle: tokio::task::JoinHandle<()>,
}

/// One operation queued for a session's worker. Tx control (`begin`/
/// `commit`/`rollback`) flows through the same FIFO as `query` so that
/// the agent's input order is the order PostgreSQL sees, even though
/// queries normally run on a separate task.
enum SessionOp {
    // Boxed: a queued query now carries the whole configuration it was
    // accepted under, which is much larger than a `begin` or a `commit`, and
    // every message in the channel would otherwise be sized for it.
    Query(Box<QueuedQuery>),
    Begin {
        id: Option<String>,
        session: String,
        read_only: bool,
        /// The endpoint and transport accepted with the permission check.
        session_cfg: Box<SessionConfig>,
    },
    Commit {
        id: Option<String>,
        session: String,
    },
    Rollback {
        id: Option<String>,
        session: String,
    },
    SessionInfo {
        id: Option<String>,
        session: String,
    },
    Invalidate {
        session: String,
    },
}

struct QueuedQuery {
    id: String,
    session: String,
    /// The configuration this query was accepted under, frozen at enqueue.
    accepted_config: crate::types::RuntimeConfig,
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
            if runtime.app.capability == crate::Capability::ReadOnly
                && runtime.app.locked_readonly_profile.load(Ordering::Relaxed)
                && patch.sessions.is_some()
            {
                send_protocol_error(
                    &runtime.app,
                    None,
                    error_code::INVALID_REQUEST,
                    "pipe session config cannot override an administrator-locked afpsql-readonly profile",
                    Some(crate::readonly_local_capability_hint()),
                    false,
                )
                .await;
                return false;
            }
            let sessions = sessions_to_invalidate(&patch);
            let mut cfg_snapshot = runtime.app.config.read().await.clone();
            cfg_snapshot.apply_update(patch);
            if runtime.app.capability == crate::Capability::ReadOnly
                && let Some(error) = cfg_snapshot
                    .sessions
                    .values()
                    .find_map(|session| crate::readonly_policy::validate_session(session).err())
            {
                send_protocol_error(
                    &runtime.app,
                    None,
                    error_code::INVALID_REQUEST,
                    &error,
                    Some(crate::readonly_local_capability_hint()),
                    false,
                )
                .await;
                return false;
            }
            dispatch_config_invalidation(runtime, sessions, cfg_snapshot).await;
        }
        Input::Cancel { id } => dispatch_cancel(&runtime.app, id).await,
        Input::SessionInfo { id, session } => {
            dispatch_session_info(runtime, id, session).await;
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
        Input::Begin {
            id,
            session,
            read_only,
            permission,
        } => dispatch_begin(runtime, id, session, read_only, permission).await,
        Input::Commit { id, session } => dispatch_commit(runtime, id, session).await,
        Input::Rollback { id, session } => dispatch_rollback(runtime, id, session).await,
        Input::Close => return true,
    }
    false
}

async fn dispatch_begin(
    runtime: &Arc<PipeRuntime>,
    id: Option<String>,
    session: Option<String>,
    read_only: bool,
    permission: Option<crate::types::Permission>,
) {
    let app = &runtime.app;
    if app.capability == crate::Capability::ReadOnly
        && (!read_only || permission.is_some_and(|value| !value.is_read_only()))
    {
        send_protocol_error(
            app,
            id,
            error_code::INVALID_REQUEST,
            "read-write transactions are unavailable in afpsql-readonly",
            Some(crate::readonly_hint()),
            false,
        )
        .await;
        return;
    }
    let cfg = app.config.read().await.clone();
    let resolved_session = crate::conn::resolve_session_name(&cfg, session.as_deref());
    let Some(session_cfg) = cfg.sessions.get(&resolved_session).cloned() else {
        let _ = app
            .writer
            .send(Output::Error {
                id,
                error_code: error_code::INVALID_REQUEST.to_string(),
                error: format!("unknown session: {resolved_session}"),
                sqlstate: None,
                message: None,
                detail: None,
                hint: Some(
                    "list active sessions with a `config` request, or omit `session` to use the default"
                        .to_string(),
                ),
                retryable: false,
                trace: Trace::only_duration(0),
            })
            .await;
        return;
    };
    if app.capability == crate::Capability::ReadOnly
        && let Err(error) = crate::readonly_policy::validate_session_with_trust(
            &session_cfg,
            app.locked_readonly_profile.load(Ordering::Relaxed),
        )
    {
        send_protocol_error(
            app,
            id,
            error_code::INVALID_REQUEST,
            &error,
            Some(crate::readonly_local_capability_hint()),
            false,
        )
        .await;
        return;
    }

    // A read-write begin needs a write permission matching the session's
    // transport. Read-only begin always passes (it's strictly less than the
    // session default).
    if !read_only {
        let probe = QueryOptions {
            permission,
            ..Default::default()
        };
        if let Err(message) = cfg.resolve_write_options_for_session(&probe, &session_cfg) {
            let _ = app
                .writer
                .send(Output::Error {
                    id,
                    error_code: error_code::INVALID_REQUEST.to_string(),
                    error: message,
                    sqlstate: None,
                    message: None,
                    detail: None,
                    hint: Some(
                        "pass `permission` matching this session's transport (write / ssh-write / container-write), or set read_only:true"
                            .to_string(),
                    ),
                    retryable: false,
                    trace: Trace::only_duration(0),
                })
                .await;
            return;
        }
    }

    let tx = get_session_worker(runtime, &resolved_session).await;
    if let Err(error) = tx.try_send(SessionOp::Begin {
        id: id.clone(),
        session: resolved_session,
        read_only,
        session_cfg: Box::new(session_cfg),
    }) {
        send_worker_queue_error(app, id, error).await;
    }
}

async fn dispatch_commit(runtime: &Arc<PipeRuntime>, id: Option<String>, session: Option<String>) {
    enqueue_tx_finish(runtime, id, session, true).await;
}

async fn dispatch_rollback(
    runtime: &Arc<PipeRuntime>,
    id: Option<String>,
    session: Option<String>,
) {
    enqueue_tx_finish(runtime, id, session, false).await;
}

async fn enqueue_tx_finish(
    runtime: &Arc<PipeRuntime>,
    id: Option<String>,
    session: Option<String>,
    commit: bool,
) {
    let app = &runtime.app;
    let cfg = app.config.read().await.clone();
    let resolved_session = crate::conn::resolve_session_name(&cfg, session.as_deref());
    if !cfg.sessions.contains_key(&resolved_session) {
        let _ = app
            .writer
            .send(Output::Error {
                id,
                error_code: error_code::INVALID_REQUEST.to_string(),
                error: format!("unknown session: {resolved_session}"),
                sqlstate: None,
                message: None,
                detail: None,
                hint: Some(
                    "list active sessions with a `config` request, or omit `session` to use the default"
                        .to_string(),
                ),
                retryable: false,
                trace: Trace::only_duration(0),
            })
            .await;
        return;
    }
    let tx = get_session_worker(runtime, &resolved_session).await;
    let op = if commit {
        SessionOp::Commit {
            id: id.clone(),
            session: resolved_session,
        }
    } else {
        SessionOp::Rollback {
            id: id.clone(),
            session: resolved_session,
        }
    };
    if let Err(error) = tx.try_send(op) {
        send_worker_queue_error(app, id, error).await;
    }
}

/// Transaction control errors go through the query path's mapper.
///
/// They used to have their own, and the two disagreed about the same
/// `ExecError`: a failed connect was `connect_failed` for a query and
/// `invalid_request` for a `begin`, and an internal error was `retryable:
/// false` for a query and `retryable: true` for a `commit` — which is exactly
/// the bit an agent reads before deciding whether to send a write again. One
/// mapper is the point: an agent branches on what happened, not on which verb
/// it used.
async fn emit_tx_error(
    app: &Arc<App>,
    id: Option<String>,
    resolved_session: &str,
    err: crate::db::ExecError,
    start: Instant,
) {
    handler::emit_exec_error(app, id, resolved_session, err, start).await;
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
                    sqlstate: None,
                    message: None,
                    detail: None,
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
                sqlstate: None,
                message: None,
                detail: None,
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

    // One read, and the query carries it. Resolving the name now and the
    // endpoint later is what let a `config` line overtake a queued query.
    let accepted_config = app.config.read().await.clone();
    let resolved_session = resolve_session_name(&accepted_config, session.as_deref());
    let tx = get_session_worker(runtime, &resolved_session).await;
    app.requests_total.fetch_add(1, Ordering::Relaxed);
    let queued = QueuedQuery {
        id: key.clone(),
        session: resolved_session,
        accepted_config,
        sql,
        params,
        options,
        cancel_slot,
        state,
    };
    if let Err(error) = tx.try_send(SessionOp::Query(Box::new(queued))) {
        let _ = app.in_flight.lock().await.remove(&key);
        send_worker_queue_error(app, Some(key), error).await;
    }
}

/// Ask the database to stop a query. Not a claim about how it ended.
///
/// This used to take the query's terminal event slot before the CancelRequest
/// was even sent, emit `cancelled`, and drop the worker's real outcome on the
/// floor. PostgreSQL's protocol says the opposite is true: a CancelRequest gets
/// no response, may arrive after the backend has finished the statement, and
/// the client has to keep waiting on the original query to learn whether it
/// succeeded or ended with `57014`. A committed write reported as "cancelled"
/// is the worst answer this program can give — the agent then does the work
/// again, or reports to a person that nothing happened.
///
/// So there are two shapes now. A query still queued has not reached the
/// database at all, and cancelling it *is* its outcome: that one is claimed
/// atomically and gets a terminal `cancelled`. A running query gets an
/// acknowledgement, and its own terminal event still arrives when the worker
/// learns what actually happened.
async fn dispatch_cancel(app: &Arc<App>, id: String) {
    let query = {
        let in_flight = app.in_flight.lock().await;
        in_flight.get(&id).cloned()
    };

    let Some(query) = query else {
        let _ = app
            .writer
            .send(Output::Error {
                id: Some(id),
                error_code: error_code::INVALID_REQUEST.to_string(),
                error: "no queued or running query with this id".to_string(),
                sqlstate: None,
                message: None,
                detail: None,
                hint: Some(
                    "no matching in-flight query for this id; it may have already completed or never been submitted"
                        .to_string(),
                ),
                retryable: false,
                trace: Trace::only_duration(0),
            })
            .await;
        return;
    };

    // Never started: the cancel is the whole story, and this is the only path
    // that may speak for the query's outcome.
    if query.state.try_cancel_queued() {
        query.cancel_slot.request_cancel();
        let terminal = query.cancel_slot.claim_terminal_emit();
        let _ = app.in_flight.lock().await.remove(&id);
        if terminal {
            let _ = app
                .writer
                .send(Output::Error {
                    id: Some(id),
                    error_code: error_code::CANCELLED.to_string(),
                    error: "query cancelled".to_string(),
                    sqlstate: None,
                    message: None,
                    detail: None,
                    hint: Some(
                        "cancelled before execution reached the database; nothing was sent"
                            .to_string(),
                    ),
                    retryable: false,
                    trace: Trace::only_duration(0),
                })
                .await;
        }
        return;
    }

    if query.state.is_finished() {
        let _ = app.in_flight.lock().await.remove(&id);
        let _ = app
            .writer
            .send(Output::Error {
                id: Some(id),
                error_code: error_code::INVALID_REQUEST.to_string(),
                error: "query already finished".to_string(),
                sqlstate: None,
                message: None,
                detail: None,
                hint: Some(
                    "cancel raced completion; the prior result/error event holds the outcome"
                        .to_string(),
                ),
                retryable: false,
                trace: Trace::only_duration(0),
            })
            .await;
        return;
    }

    // Running. Ask, then say only that it was asked. The entry stays in flight
    // and the id stays taken until the worker emits the real outcome.
    let sent = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        query.cancel_server_query(),
    )
    .await
    .unwrap_or_else(|_| Err("server-side cancel timed out after 5 seconds".to_owned()));
    let hint = match sent {
        Ok(true) => "cancel sent to the server; wait for this id's result, sql_error \
                     (`57014` means it was stopped), or error"
            .to_string(),
        Ok(false) => {
            "no server-side cancel could be sent; wait for this id's terminal event".to_string()
        }
        Err(reason) => format!("{reason}; wait for this id's terminal event"),
    };
    let _ = app
        .writer
        .send(Output::CancelRequested {
            id,
            session: None,
            hint,
            trace: Trace::only_duration(0),
        })
        .await;
}

async fn get_session_worker(runtime: &Arc<PipeRuntime>, session: &str) -> mpsc::Sender<SessionOp> {
    let mut workers = runtime.workers.lock().await;
    if let Some(worker) = workers.get(session)
        && !worker.handle.is_finished()
    {
        return worker.tx.clone();
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

async fn session_worker_loop(app: Arc<App>, mut rx: mpsc::Receiver<SessionOp>) {
    while let Some(op) = rx.recv().await {
        match op {
            SessionOp::Query(query) => {
                // The CAS is the whole handover: if the cancel dispatcher took
                // this query while it was queued, it owns the outcome and
                // already emitted it. Checking `is_cancelled` here as well —
                // which this used to do — would also skip a query the
                // dispatcher decided to let run, leaving an id with no
                // terminal event at all.
                if !query.state.try_start() {
                    continue;
                }

                handler::execute_query(
                    &app,
                    Some(query.id),
                    Some(query.session),
                    query.sql,
                    query.params,
                    query.options,
                    Some(handler::Queued {
                        cancel_slot: query.cancel_slot.clone(),
                        accepted_config: query.accepted_config,
                    }),
                )
                .await;

                // Finished means the terminal event went out, whatever it said.
                // A cancel that landed mid-flight does not change that: what
                // the database did is what `execute_query` just reported.
                // Left in the map, finished. `cleanup_finished_queries` reaps
                // it later; until then a cancel that lost the race is told the
                // query already finished rather than that it never existed, and
                // the id stays taken.
                query.state.set_phase(QueryPhase::Finished);
            }
            SessionOp::Begin {
                id,
                session,
                read_only,
                session_cfg,
            } => exec_tx_begin_on_worker(&app, id, session, read_only, &session_cfg).await,
            SessionOp::Commit { id, session } => {
                exec_tx_finish_on_worker(&app, id, session, true).await
            }
            SessionOp::Rollback { id, session } => {
                exec_tx_finish_on_worker(&app, id, session, false).await
            }
            SessionOp::SessionInfo { id, session } => {
                handler::handle_session_info(&app, id, Some(session)).await;
            }
            SessionOp::Invalidate { session } => {
                app.executor.invalidate_sessions(&[session]).await;
            }
        }
    }
}

async fn dispatch_session_info(
    runtime: &Arc<PipeRuntime>,
    id: Option<String>,
    requested_session: Option<String>,
) {
    let cfg = runtime.app.config.read().await;
    let session = crate::conn::resolve_session_name(&cfg, requested_session.as_deref());
    drop(cfg);
    let worker = get_session_worker(runtime, &session).await;
    if let Err(error) = worker.try_send(SessionOp::SessionInfo {
        id: id.clone(),
        session,
    }) {
        send_worker_queue_error(&runtime.app, id, error).await;
    }
}

async fn dispatch_config_invalidation(
    runtime: &Arc<PipeRuntime>,
    sessions: Vec<String>,
    cfg_snapshot: RuntimeConfig,
) {
    let mut reservations = Vec::with_capacity(sessions.len());
    for session in sessions {
        let worker = get_session_worker(runtime, &session).await;
        match worker.try_reserve_owned() {
            Ok(permit) => reservations.push((session, permit)),
            Err(error) => {
                // No invalidation has been sent and no configuration installed.
                // Dropping every reservation restores each queue's capacity.
                drop(reservations);
                send_worker_queue_error(&runtime.app, None, error).await;
                return;
            }
        }
    }
    *runtime.app.config.write().await = cfg_snapshot.clone();
    for (session, permit) in reservations {
        let _sender = permit.send(SessionOp::Invalidate { session });
    }
    let _ = runtime.app.writer.send(Output::Config(cfg_snapshot)).await;
}

async fn exec_tx_begin_on_worker(
    app: &Arc<App>,
    id: Option<String>,
    session: String,
    read_only: bool,
    session_cfg: &SessionConfig,
) {
    let start = Instant::now();
    match app
        .executor
        .tx_begin(&session, session_cfg, read_only)
        .await
    {
        Ok(()) => {
            let _ = app
                .writer
                .send(Output::Result {
                    id,
                    session: Some(session),
                    command_tag: crate::protocol::command_tag::BEGIN.to_string(),
                    columns: vec![],
                    rows: vec![],
                    row_count: 0,
                    truncated: false,
                    truncated_at_rows: None,
                    truncated_at_bytes: None,
                    trace: Trace::only_duration(start.elapsed().as_millis() as u64),
                })
                .await;
        }
        Err(err) => emit_tx_error(app, id, &session, err, start).await,
    }
}

async fn exec_tx_finish_on_worker(
    app: &Arc<App>,
    id: Option<String>,
    session: String,
    commit: bool,
) {
    let cfg = app.config.read().await.clone();
    let Some(session_cfg) = cfg.sessions.get(&session).cloned() else {
        let _ = app
            .writer
            .send(Output::Error {
                id,
                error_code: error_code::INVALID_REQUEST.to_string(),
                error: format!("unknown session: {session}"),
                sqlstate: None,
                message: None,
                detail: None,
                hint: Some(
                    "list active sessions with a `config` request, or omit `session` to use the default"
                        .to_string(),
                ),
                retryable: false,
                trace: Trace::only_duration(0),
            })
            .await;
        return;
    };
    let start = Instant::now();
    let result = if commit {
        app.executor.tx_commit(&session, &session_cfg).await
    } else {
        app.executor.tx_rollback(&session, &session_cfg).await
    };
    match result {
        Ok(()) => {
            let _ = app
                .writer
                .send(Output::Result {
                    id,
                    session: Some(session),
                    command_tag: if commit {
                        crate::protocol::command_tag::COMMIT.to_string()
                    } else {
                        crate::protocol::command_tag::ROLLBACK.to_string()
                    },
                    columns: vec![],
                    rows: vec![],
                    row_count: 0,
                    truncated: false,
                    truncated_at_rows: None,
                    truncated_at_bytes: None,
                    trace: Trace::only_duration(start.elapsed().as_millis() as u64),
                })
                .await;
        }
        Err(err) => emit_tx_error(app, id, &session, err, start).await,
    }
}

async fn cleanup_finished_queries(app: &Arc<App>) {
    app.in_flight
        .lock()
        .await
        .retain(|_, query| !query.state.is_finished());
}

/// One deadline covers worker draining and the executor's client locks.
async fn shutdown_runtime(runtime: &Arc<PipeRuntime>, deadline: tokio::time::Instant) {
    let handles: Vec<tokio::task::JoinHandle<()>> = runtime
        .workers
        .lock()
        .await
        .drain()
        .map(|(_, worker)| worker.handle)
        .collect();
    for mut handle in handles {
        if tokio::time::timeout_at(deadline, &mut handle)
            .await
            .is_err()
        {
            handle.abort();
            let _joined = handle.await;
        }
    }
    // Cancelling shutdown drops the executor's owned session entries. A local
    // deadline cannot establish how an unfinished write settled on the server.
    let _finished = tokio::time::timeout_at(deadline, runtime.app.executor.shutdown()).await;
}

async fn send_worker_queue_error<T>(
    app: &Arc<App>,
    id: Option<String>,
    error: mpsc::error::TrySendError<T>,
) {
    let (message, hint) = match error {
        mpsc::error::TrySendError::Full(_) => (
            "session worker queue is full",
            "retry after queued operations complete",
        ),
        mpsc::error::TrySendError::Closed(_) => (
            "session worker is unavailable",
            "retry; the session worker will be restarted",
        ),
    };
    send_protocol_error(
        app,
        id,
        error_code::INVALID_REQUEST,
        message,
        Some(hint),
        true,
    )
    .await;
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
            sqlstate: None,
            message: None,
            detail: None,
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
            sqlstate: None,
            message: None,
            detail: None,
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
            sqlstate: None,
            message: None,
            detail: None,
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::db::{DbExecutor, DryRunOutcome, ExecError, ExecOutcome, ExecRequest};
    use std::sync::atomic::AtomicBool;
    use tokio::sync::{Mutex, Notify};

    #[tokio::test]
    async fn an_unresponsive_cancel_returns_before_the_next_close() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });
        let executor = Arc::new(BarrierExecutor::default());
        let (runtime, mut outputs) = fixture(executor, config_with_host("old"));
        let slot = crate::db::new_cancel_slot();
        slot.set_fallback_context_for_test(crate::types::SessionConfig {
            dsn_secret: Some(format!("postgresql://test@{address}/test?sslmode=disable")),
            ..Default::default()
        })
        .await;
        let state = Arc::new(handler::QueryState::queued());
        assert!(state.try_start());
        runtime.app.in_flight.lock().await.insert(
            "q1".to_owned(),
            handler::InFlightQuery {
                cancel_slot: slot,
                state,
            },
        );
        let started = std::time::Instant::now();
        tokio::time::timeout(
            std::time::Duration::from_secs(7),
            dispatch_cancel(&runtime.app, "q1".to_owned()),
        )
        .await
        .unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(7));
        let event = outputs.recv().await.unwrap();
        assert!(
            matches!(event, Output::CancelRequested { hint, .. } if hint.contains("timed out"))
        );
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            shutdown_runtime(
                &runtime,
                tokio::time::Instant::now() + std::time::Duration::from_millis(100),
            ),
        )
        .await
        .unwrap();
        peer.abort();
        let _ = peer.await;
    }

    struct MarkDropped<'a>(&'a AtomicBool);

    impl Drop for MarkDropped<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[derive(Default)]
    struct BarrierExecutor {
        started: Notify,
        release: Notify,
        events: Mutex<Vec<String>>,
        query_dropped: AtomicBool,
        shutdown_started: AtomicBool,
        shutdown_dropped: AtomicBool,
        block_shutdown: bool,
    }

    #[async_trait::async_trait]
    impl DbExecutor for BarrierExecutor {
        async fn execute(&self, req: ExecRequest<'_>) -> Result<ExecOutcome, ExecError> {
            self.events.lock().await.push(format!(
                "query:{}",
                req.session_cfg.host.as_deref().unwrap_or("unset")
            ));
            if req.sql == "select hold" {
                let _dropped = MarkDropped(&self.query_dropped);
                self.started.notify_one();
                self.release.notified().await;
            }
            Ok(ExecOutcome::Command { affected: 0 })
        }

        async fn prepare_only(&self, _req: ExecRequest<'_>) -> Result<DryRunOutcome, ExecError> {
            Err(ExecError::Internal(
                "unexpected dry run in fixture".to_string(),
            ))
        }

        async fn tx_begin(
            &self,
            _session_name: &str,
            session_cfg: &SessionConfig,
            read_only: bool,
        ) -> Result<(), ExecError> {
            self.events.lock().await.push(format!(
                "begin:{}:{read_only}",
                session_cfg.host.as_deref().unwrap_or("unset")
            ));
            Ok(())
        }

        async fn invalidate_sessions(&self, sessions: &[String]) {
            for session in sessions {
                self.events
                    .lock()
                    .await
                    .push(format!("invalidate:{session}"));
            }
        }

        async fn shutdown(&self) {
            self.shutdown_started.store(true, Ordering::SeqCst);
            let _dropped = MarkDropped(&self.shutdown_dropped);
            if self.block_shutdown {
                std::future::pending::<()>().await;
            }
        }
    }

    fn fixture(
        executor: Arc<BarrierExecutor>,
        config: RuntimeConfig,
    ) -> (Arc<PipeRuntime>, mpsc::Receiver<Output>) {
        let (writer, outputs) = mpsc::channel(128);
        let mut app = App::new(config, writer, crate::Capability::ReadWrite);
        app.executor = executor;
        (Arc::new(PipeRuntime::new(Arc::new(app))), outputs)
    }

    fn config_with_host(host: &str) -> RuntimeConfig {
        let mut config = RuntimeConfig::default();
        config.sessions.insert(
            config.default_session.clone(),
            SessionConfig {
                host: Some(host.to_string()),
                ..SessionConfig::default()
            },
        );
        config
    }

    fn input(value: serde_json::Value) -> Input {
        serde_json::from_value(value).expect("fixture input")
    }

    fn assert_queue_error(output: Output, expected_id: Option<&str>, expected_message: &str) {
        match output {
            Output::Error {
                id,
                error_code,
                error,
                retryable,
                ..
            } => {
                assert_eq!(id.as_deref(), expected_id);
                assert_eq!(error_code, crate::protocol::error_code::INVALID_REQUEST);
                assert_eq!(error, expected_message);
                assert!(retryable);
            }
            _ => panic!("expected a retryable queue error"),
        }
    }

    async fn paused_worker(
        runtime: &Arc<PipeRuntime>,
        session: &str,
        capacity: usize,
    ) -> mpsc::Receiver<SessionOp> {
        let (tx, rx) = mpsc::channel(capacity);
        runtime.workers.lock().await.insert(
            session.to_string(),
            SessionWorker {
                tx,
                handle: tokio::spawn(std::future::pending()),
            },
        );
        rx
    }

    #[tokio::test]
    async fn a_full_worker_queue_rejects_every_operation_without_blocking_cancel() {
        let executor = Arc::new(BarrierExecutor::default());
        let (runtime, mut outputs) = fixture(executor.clone(), config_with_host("old"));
        dispatch_input(
            &runtime,
            input(serde_json::json!({"code":"query","id":"hold","sql":"select hold"})),
        )
        .await;
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            executor.started.notified(),
        )
        .await
        .expect("worker reached the barrier");
        while outputs.try_recv().is_ok() {}

        for index in 0..MAX_ACTIVE_QUERIES {
            dispatch_input(
                &runtime,
                input(serde_json::json!({"code":"session_info","id":format!("queued-{index}")})),
            )
            .await;
        }
        for (id, request) in [
            (
                "begin",
                serde_json::json!({"code":"begin","id":"begin","read_only":true}),
            ),
            ("commit", serde_json::json!({"code":"commit","id":"commit"})),
            (
                "rollback",
                serde_json::json!({"code":"rollback","id":"rollback"}),
            ),
            (
                "info",
                serde_json::json!({"code":"session_info","id":"info"}),
            ),
            (
                "query",
                serde_json::json!({"code":"query","id":"query","sql":"select 1"}),
            ),
        ] {
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                dispatch_input(&runtime, input(request)),
            )
            .await
            .expect("a full queue must not suspend the input dispatcher");
            assert_queue_error(
                outputs.recv().await.expect("queue error"),
                Some(id),
                "session worker queue is full",
            );
        }
        assert!(!runtime.app.in_flight.lock().await.contains_key("query"));
        assert!(runtime.app.in_flight.lock().await.contains_key("hold"));
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            dispatch_input(
                &runtime,
                input(serde_json::json!({"code":"cancel","id":"hold"})),
            ),
        )
        .await
        .expect("cancel remains dispatchable with a full session queue");
        assert!(
            matches!(outputs.recv().await, Some(Output::CancelRequested { id, .. }) if id == "hold")
        );

        shutdown_runtime(
            &runtime,
            tokio::time::Instant::now() + std::time::Duration::from_millis(20),
        )
        .await;
        assert!(executor.query_dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn config_reservations_are_all_or_nothing_for_full_and_closed_queues() {
        for closed in [false, true] {
            let executor = Arc::new(BarrierExecutor::default());
            let mut config = RuntimeConfig::default();
            for session in ["a", "b"] {
                config.sessions.insert(
                    session.to_string(),
                    SessionConfig {
                        host: Some("old".to_string()),
                        ..SessionConfig::default()
                    },
                );
            }
            let (runtime, mut outputs) = fixture(executor, config);
            let mut first_rx = paused_worker(&runtime, "a", 2).await;
            let mut second_rx = Some(paused_worker(&runtime, "b", 1).await);
            let first_tx = get_session_worker(&runtime, "a").await;
            let second_tx = get_session_worker(&runtime, "b").await;
            if closed {
                drop(second_rx.take());
            } else {
                assert!(
                    second_tx
                        .try_send(SessionOp::SessionInfo {
                            id: None,
                            session: "b".to_string()
                        })
                        .is_ok()
                );
            }
            let before =
                serde_json::to_value(&*runtime.app.config.read().await).expect("old config");
            let first_capacity = first_tx.capacity();
            dispatch_input(
                &runtime,
                input(serde_json::json!({
                    "code":"config",
                    "sessions":{"a":{"host":"new"},"b":{"host":"new"}}
                })),
            )
            .await;
            assert_queue_error(
                outputs.recv().await.expect("config rejection"),
                None,
                if closed {
                    "session worker is unavailable"
                } else {
                    "session worker queue is full"
                },
            );
            assert_eq!(
                serde_json::to_value(&*runtime.app.config.read().await).expect("unchanged config"),
                before
            );
            assert_eq!(
                first_tx.capacity(),
                first_capacity,
                "failed reservations must restore capacity"
            );
            assert!(matches!(
                first_rx.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ));
            assert!(
                matches!(outputs.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
                "a rejected patch must not emit a Config result"
            );
            if let Some(second_rx) = second_rx.as_mut() {
                assert!(matches!(
                    second_rx.try_recv(),
                    Ok(SessionOp::SessionInfo { .. })
                ));
                assert!(matches!(
                    second_rx.try_recv(),
                    Err(mpsc::error::TryRecvError::Empty)
                ));
            }
            shutdown_runtime(
                &runtime,
                tokio::time::Instant::now() + std::time::Duration::from_millis(20),
            )
            .await;
        }
    }

    #[tokio::test]
    async fn begin_keeps_its_accepted_config_before_fifo_invalidation() {
        let executor = Arc::new(BarrierExecutor::default());
        let config = config_with_host("old");
        let session = config.default_session.clone();
        let (runtime, mut outputs) = fixture(executor.clone(), config);
        dispatch_input(
            &runtime,
            input(serde_json::json!({"code":"query","id":"hold","sql":"select hold"})),
        )
        .await;
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            executor.started.notified(),
        )
        .await
        .expect("worker barrier");
        dispatch_input(
            &runtime,
            input(serde_json::json!({"code":"begin","id":"begin","read_only":true})),
        )
        .await;
        dispatch_input(
            &runtime,
            input(
                serde_json::json!({"code":"config","sessions":{(session.clone()):{"host":"new"}}}),
            ),
        )
        .await;
        assert_eq!(
            runtime.app.config.read().await.sessions[&session]
                .host
                .as_deref(),
            Some("new")
        );
        dispatch_input(
            &runtime,
            input(serde_json::json!({"code":"query","id":"after","sql":"select 1"})),
        )
        .await;
        executor.release.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                match outputs.recv().await.expect("worker result") {
                    Output::Result { id: Some(id), .. } if id == "after" => break,
                    Output::Error { error, .. } => panic!("unexpected worker error: {error}"),
                    _ => {}
                }
            }
        })
        .await
        .expect("the new query finishes after invalidation");
        assert_eq!(
            *executor.events.lock().await,
            vec![
                "query:old".to_string(),
                "begin:old:true".to_string(),
                format!("invalidate:{session}"),
                "query:new".to_string(),
            ]
        );
        shutdown_runtime(
            &runtime,
            tokio::time::Instant::now() + std::time::Duration::from_secs(1),
        )
        .await;
    }

    #[tokio::test]
    async fn shutdown_uses_one_deadline_and_reaps_an_aborted_worker() {
        let executor = Arc::new(BarrierExecutor {
            block_shutdown: true,
            ..BarrierExecutor::default()
        });
        let (runtime, _outputs) = fixture(executor.clone(), config_with_host("old"));
        dispatch_input(
            &runtime,
            input(serde_json::json!({"code":"query","id":"hold","sql":"select hold"})),
        )
        .await;
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            executor.started.notified(),
        )
        .await
        .expect("worker barrier");
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            shutdown_runtime(
                &runtime,
                tokio::time::Instant::now() + std::time::Duration::from_millis(20),
            ),
        )
        .await
        .expect("executor shutdown shares the worker's deadline");
        assert!(
            executor.query_dropped.load(Ordering::SeqCst),
            "the worker future must be dropped before shutdown returns"
        );
        assert!(runtime.workers.lock().await.is_empty());
    }

    #[tokio::test]
    async fn shutdown_deadline_also_cancels_an_executor_wait_without_workers() {
        let executor = Arc::new(BarrierExecutor {
            block_shutdown: true,
            ..BarrierExecutor::default()
        });
        let (runtime, _outputs) = fixture(executor.clone(), config_with_host("old"));
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            shutdown_runtime(
                &runtime,
                tokio::time::Instant::now() + std::time::Duration::from_millis(20),
            ),
        )
        .await
        .expect("a client-lock wait cannot bypass the shutdown deadline");
        assert!(executor.shutdown_started.load(Ordering::SeqCst));
        assert!(executor.shutdown_dropped.load(Ordering::SeqCst));
    }
}
