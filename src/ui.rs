//! Windows onto PostgreSQL, in AFUI's two shapes.
//!
//! `ui schema`, `ui table`, `ui indexes` and `ui connections` are *watch*
//! sessions: afpsql opens a window onto something a person reads, nothing on
//! the page submits, and the session ends when they close it. Their value type
//! is `()` and `Outcome::Completed` never occurs. `ui connections` differs from
//! the other three in one way only — it reloads itself, because it is a view of
//! something that moves.
//!
//! `ui plan` is a *decide* session. It shows one statement and returns the
//! person's answer as a typed value, and only one answer runs it: closing the
//! window, letting a credential lapse, and pressing refuse are all refusals.
//! Absence of an answer is never consent — see [`approved`], the single place
//! that judgement is made.
//!
//! The data is not a second source of truth. Every view panel runs the exact SQL
//! that the matching `afpsql inspect` subcommand runs, and an approved statement
//! runs through `handler::execute_query` exactly as `afpsql --sql` would — the
//! same connection, transport, permission resolution, and readonly policy.
//!
//! None of these pages is written in Rust. Each is a MiniJinja template
//! rendered against the typed document its section below builds, and a person
//! may replace any of those templates — see [`frontend`]. The documents are
//! therefore a contract rather than an implementation detail: what a panel
//! computes is afpsql's, what a panel looks like is not.

use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use agent_first_data::OutputFormat;
use agent_first_ui::{Outcome, UiCspNonce, UiSecurityPolicy, UiSession, UiWindowConfig};
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderValue, header};
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{Mutex, mpsc};

mod frontend;

use frontend::{FrontendFailure, PanelFrontend, PanelShape};

use crate::cli::{
    InspectAction, InspectConnectionsArgs, InspectIndexesArgs, InspectSchemaArgs, InspectTableArgs,
    UiPanel, UiPlan, UiView,
};
use crate::db::{ExecError, ExecRequest};
use crate::handler::{self, App};
use crate::limits::OUTPUT_CHANNEL_CAPACITY;
use crate::logutil::build_startup_log;
use crate::protocol::error_code;
use crate::types::{
    ColumnInfo, Output, QueryOptions, ResolvedOptions, RuntimeConfig, SessionConfig,
};

/// A watch panel loads a stylesheet, images and fonts from this session and
/// nothing else. `script-src` is absent, so `default-src 'none'` governs it:
/// these pages have no behaviour, and neither afpsql nor a frontend can give
/// them any.
///
/// `img-src` and `font-src` name `'self'` rather than `'none'` because a
/// frontend may ship assets, and they are served from this session's own origin
/// by `UiFrontend::assets_router`. Same origin, same credential, no network.
const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; style-src 'self'; img-src 'self'; font-src 'self'; \
     connect-src 'none'; form-action 'none'; base-uri 'none'; frame-ancestors 'none'";

/// The decide panel differs in exactly two directives, and both are what make
/// its answer trustworthy rather than what weaken it.
///
/// `script-src 'nonce-…'` admits precisely one script — afpsql's decision
/// runtime, spliced in at the layout marker with this session's nonce. A
/// frontend cannot supply a script file (AFUI refuses one by name) or hide one
/// in a template (refused by content), and could not run it if it did: it does
/// not know the nonce. `form-action 'self'` is where the answer goes.
fn plan_content_security_policy(nonce: &UiCspNonce) -> String {
    format!(
        "default-src 'none'; style-src 'self'; img-src 'self'; font-src 'self'; \
         script-src 'nonce-{}'; connect-src 'none'; form-action 'self'; base-uri 'none'; \
         frame-ancestors 'none'",
        nonce.as_str()
    )
}

const PLAN_UI_KIND: &str = "plan_confirm";

/// How many backends the "longest running" table lists before it stops.
///
/// A monitor is read at a glance; the full activity table below it holds every
/// row, so nothing is hidden by this cut.
const LONGEST_ROWS: usize = 10;

/// The leading keywords afpsql calls a write when it describes a statement.
///
/// Presentation only, and deliberately over-inclusive: `copy … to stdout` reads
/// but lands here, which warns about a read rather than staying quiet about a
/// write. Nothing branches on it — what a statement is *allowed* to do is the
/// transaction mode beside it, which PostgreSQL enforces.
const WRITE_KEYWORDS: [&str; 20] = [
    "insert", "update", "delete", "merge", "truncate", "copy", "create", "alter", "drop", "grant",
    "revoke", "comment", "refresh", "reindex", "cluster", "vacuum", "analyze", "call", "do",
    "lock",
];

pub async fn run(
    request: crate::cli::UiRequest,
    capability: crate::Capability,
    locked_readonly_profile: bool,
) {
    let crate::cli::UiRequest {
        panel,
        session,
        output,
        log,
        startup_args,
        startup_env,
        startup_requested,
    } = request;

    // Opening a window spawns a browser and writes a profile directory. That is
    // a host capability, and an administrator-locked profile restricts those
    // for the same reason it refuses `--stdout-file`. It holds for every panel,
    // including the one that would otherwise only ever read.
    if locked_readonly_profile {
        fail(
            error_code::INVALID_REQUEST,
            "panels are unavailable through an administrator-locked profile",
            Some(crate::readonly_local_capability_hint()),
            output,
        );
    }

    let config = RuntimeConfig::default();
    let (tx, rx) = mpsc::channel::<Output>(OUTPUT_CHANNEL_CAPACITY);
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

    if startup_requested {
        let event = build_startup_log(Some("default"), &startup_args, &startup_env);
        if crate::emit::emit_output(&event, output).is_err() {
            std::process::exit(4);
        }
    }

    match panel {
        UiPanel::View(view) => view_panel(app, rx, view, &session, output).await,
        UiPanel::Plan(plan) => plan_panel(app, rx, plan, session, output).await,
    }
}

// ═══════════════════════════════════════════
// Shared plumbing
// ═══════════════════════════════════════════

/// What one run of a panel's query produced.
#[derive(Default)]
struct QueryRun {
    table: Option<(Vec<ColumnInfo>, Vec<Value>)>,
    /// The first failure's message, when the query failed.
    error: Option<String>,
    /// Everything that was neither the result nor a failure, in order. Who it
    /// belongs to depends on the caller: the first run reports to the agent,
    /// and a refresh reports to the window.
    other: Vec<Output>,
}

impl QueryRun {
    fn absorb(&mut self, event: Output) {
        match event {
            Output::Result { columns, rows, .. } => self.table = Some((columns, rows)),
            Output::Error { ref error, .. } => {
                self.error.get_or_insert_with(|| error.clone());
                self.other.push(event);
            }
            Output::SqlError {
                ref sqlstate,
                ref message,
                ..
            } => {
                let text = format!("{sqlstate}: {message}");
                self.error.get_or_insert(text);
                self.other.push(event);
            }
            other => self.other.push(other),
        }
    }

    /// Send everything but the result to the agent, and stop the process if the
    /// query failed.
    fn report_to_agent(&self, output: OutputFormat) {
        for event in &self.other {
            if crate::emit::emit_output(event, output).is_err() {
                std::process::exit(4);
            }
        }
        if self.error.is_some() {
            std::process::exit(1);
        }
    }
}

/// Run one panel query and collect everything it emitted.
///
/// Draining with `try_recv` is what makes this reusable by a panel that keeps
/// running: `execute_query` awaits each send, so by the time it returns, every
/// event it produced is already in the channel and nothing is left to wait for.
/// A receive loop would instead wait for the channel to close, which only
/// happens once the app is dropped — the end of the process, not the end of one
/// refresh.
async fn run_query_once(
    app: &Arc<App>,
    sql: &str,
    params: &[Value],
    rx: &mut mpsc::Receiver<Output>,
) -> QueryRun {
    app.requests_total.fetch_add(1, Ordering::Relaxed);
    handler::execute_query(
        app,
        None,
        Some("default".to_string()),
        sql.to_string(),
        params.to_vec(),
        QueryOptions::default(),
        None,
    )
    .await;
    let mut run = QueryRun::default();
    while let Ok(event) = rx.try_recv() {
        run.absorb(event);
    }
    run
}

fn security_policy(policy: &str) -> UiSecurityPolicy {
    match HeaderValue::from_str(policy) {
        Ok(value) => UiSecurityPolicy::isolated().with_content_security_policy(value),
        Err(_) => UiSecurityPolicy::isolated(),
    }
}

fn window_hint(error: &agent_first_ui::Error) -> Option<&'static str> {
    match error {
        agent_first_ui::Error::WindowBinaryNotFound => Some(
            "install a Chromium-family browser, or read the same data with the matching \
             `afpsql inspect` command",
        ),
        _ => None,
    }
}

fn fail(code: &str, message: &str, hint: Option<&str>, output: OutputFormat) -> ! {
    if crate::emit::emit_coded_error(code, message, hint, output).is_err() {
        std::process::exit(4);
    }
    std::process::exit(1);
}

fn emit_event_or_exit(event: agent_first_data::Event, output: OutputFormat) {
    if crate::emit::emit_event(event, output).is_err() {
        std::process::exit(4);
    }
}

/// The routes every panel serves besides its own page.
///
/// The stylesheet is read once, when the panel starts, rather than per request:
/// a frontend's bytes are fixed for the life of a window — editing it revokes
/// its trust anyway — and reading it here is what turns an unreadable
/// stylesheet into a failure before a window opens rather than into a page with
/// no styling.
fn shared_panel_routes(frontend: &PanelFrontend, stylesheet: Vec<u8>) -> Router {
    let stylesheet = Arc::new(stylesheet);
    Router::new()
        .route(
            "/style.css",
            get(move || {
                let stylesheet = Arc::clone(&stylesheet);
                async move {
                    (
                        [(
                            header::CONTENT_TYPE,
                            HeaderValue::from_static("text/css; charset=utf-8"),
                        )],
                        stylesheet.to_vec(),
                    )
                        .into_response()
                }
            }),
        )
        .nest("/assets", frontend.assets_router())
}

/// A frontend that will not load ends the command. It is never a quietly
/// substituted afpsql page, because that is indistinguishable from the
/// override having worked.
fn fail_frontend(failure: FrontendFailure, output: OutputFormat) -> ! {
    fail(failure.code, &failure.message, failure.hint, output)
}

/// What a panel over this session is a view *of*, in a person's terms.
///
/// Read from the connection afpsql resolved, not from the flags it was given,
/// because most sessions name their endpoint inside a DSN or a conninfo string
/// and would otherwise all be called the same thing. That resolution is
/// `tokio_postgres`' own parser — the endpoint is never picked apart here — and
/// only the host, port and database are taken from it. The password in the same
/// string is never asked for: this text is a window title and a line in
/// `afui session list`.
fn connection_subject(session: &SessionConfig) -> String {
    let Ok(config) = crate::conn::resolve_pg_config(session) else {
        return "the connected server".to_string();
    };
    let mut subject = match config.get_hosts().first() {
        Some(tokio_postgres::config::Host::Tcp(host)) => host.clone(),
        // `Host::Unix` is compiled out where Unix sockets do not exist, so the
        // arm has to be absent on Windows rather than merely unreachable —
        // every other match on this enum in the crate is gated the same way.
        #[cfg(unix)]
        Some(tokio_postgres::config::Host::Unix(path)) => path.display().to_string(),
        None => String::new(),
    };
    if let (false, Some(port)) = (subject.is_empty(), config.get_ports().first()) {
        let _ = write!(subject, ":{port}");
    }
    if let Some(dbname) = config.get_dbname() {
        if subject.is_empty() {
            subject = dbname.to_string();
        } else {
            let _ = write!(subject, "/{dbname}");
        }
    }
    if subject.is_empty() {
        "the connected server".to_string()
    } else {
        subject
    }
}

// ═══════════════════════════════════════════
// View panels
// ═══════════════════════════════════════════

async fn view_panel(
    app: Arc<App>,
    rx: mpsc::Receiver<Output>,
    view: UiView,
    session: &SessionConfig,
    output: OutputFormat,
) -> ! {
    let (sql, params) = crate::cli::inspect_sql_for(view.inspect_action());
    // The subject is what tells two panels of the same kind apart in
    // `afui session list` — "orders" from "invoices", one server from another.
    let subject = match view.subject() {
        subject if subject.is_empty() => connection_subject(session),
        subject => subject,
    };
    // Before the first query, so a frontend afpsql cannot load costs a person
    // an error rather than a connection, a window, and then an error.
    let shape = match view.refresh_seconds() {
        Some(_) => PanelShape::Monitor,
        None => PanelShape::Inspect,
    };
    let frontend = match PanelFrontend::resolve(view.ui_kind(), shape) {
        Ok(frontend) => Arc::new(frontend),
        Err(failure) => fail_frontend(failure, output),
    };
    match view.refresh_seconds() {
        Some(seconds) => {
            live_panel(
                app, rx, view, sql, params, subject, seconds, frontend, output,
            )
            .await
        }
        None => static_panel(app, rx, view, sql, params, subject, frontend, output).await,
    }
}

/// A panel over something that does not change while a person reads it.
///
/// It queries once, releases the connection, and serves the same page for as
/// long as the window is open.
#[allow(clippy::too_many_arguments)]
async fn static_panel(
    app: Arc<App>,
    mut rx: mpsc::Receiver<Output>,
    view: UiView,
    sql: String,
    params: Vec<Value>,
    subject: String,
    frontend: Arc<PanelFrontend>,
    output: OutputFormat,
) -> ! {
    let mut run = run_query_once(&app, &sql, &params, &mut rx).await;
    app.executor.shutdown().await;
    drop(app);
    // Anything the shutdown itself produced still belongs to the agent.
    while let Some(event) = rx.recv().await {
        run.absorb(event);
    }

    // Report before opening anything: a connection or permission failure must
    // reach the agent as an error event, not as an empty window.
    run.report_to_agent(output);
    let Some((columns, rows)) = run.table else {
        fail(
            error_code::INVALID_REQUEST,
            "the inspection query returned no result set",
            Some("check the connection target and that the object exists"),
            output,
        );
    };

    let stylesheet = match frontend.stylesheet() {
        Ok(bytes) => bytes,
        Err(failure) => fail_frontend(failure, output),
    };
    let document = inspect_document(&view, &subject, &columns, &rows);
    let page = match frontend.render_page(&document, None) {
        Ok(page) => page,
        Err(failure) => fail_frontend(failure, output),
    };
    let router = Router::new()
        .route("/", get(move || async move { Html(page) }))
        .merge(shared_panel_routes(&frontend, stylesheet));

    let session = match UiSession::<()>::new("afpsql", view.ui_kind()) {
        Ok(session) => session
            .with_subject(subject.clone())
            .with_security_policy(security_policy(CONTENT_SECURITY_POLICY)),
        Err(error) => fail("ui_session_invalid", &error.to_string(), None, output),
    };

    // The window is about to appear, and `window()` does not return until the
    // person closes it, so the readiness event goes out first.
    emit_event_or_exit(
        agent_first_data::json_progress(serde_json::json!({
            "phase": "ui_ready",
            "message": "The inspection panel is open. It ends when you close the window.",
            "ui_kind": view.ui_kind(),
            "subject": subject,
            "row_count": rows.len(),
            // Absent when afpsql's own page is serving. A workspace frontend
            // that has not been trusted is deliberately skipped in silence, so
            // this is how an agent tells "my override is running" from "my
            // override is inert" without opening a window to look.
            "ui_frontend_id": frontend.frontend_id(),
        }))
        .build(),
        output,
    );

    match session.window(router, &UiWindowConfig::default()).await {
        // A watch panel has no submit control, so `Completed` is unreachable;
        // both real endings mean the person is done looking.
        Ok(Outcome::Closed | Outcome::Completed(())) => {
            emit_event_or_exit(
                agent_first_data::json_result(serde_json::json!({
                    "ui_kind": view.ui_kind(),
                    "subject": subject,
                    "row_count": rows.len(),
                    "closed": true,
                }))
                .build(),
                output,
            );
            std::process::exit(0);
        }
        // `UiExpiry::Never` is what window delivery issues, so this cannot fire.
        Ok(Outcome::Expired) => fail(
            "ui_expired",
            "the inspection panel credential expired",
            None,
            output,
        ),
        Err(error) => fail("ui_failed", &error.to_string(), window_hint(&error), output),
    }
}

/// A panel over something that moves, and the state one reload needs.
///
/// The whole point of this session is that it outlives the agent's attention:
/// the agent opens it, goes back to work, and a person watches until they stop
/// caring. So the page cannot be a snapshot taken at open — it re-runs the same
/// query on every request, holding the connection for the life of the window.
struct LivePanel {
    app: Arc<App>,
    /// One reload at a time. The query and its drain are one unit — a second
    /// reload arriving mid-drain would take the first one's events.
    rx: Mutex<mpsc::Receiver<Output>>,
    sql: String,
    params: Vec<Value>,
    subject: String,
    refresh_seconds: u64,
    title: &'static str,
    frontend: Arc<PanelFrontend>,
    reloads: AtomicU64,
    last_row_count: AtomicUsize,
}

impl LivePanel {
    async fn page(&self) -> (axum::http::StatusCode, Html<String>) {
        let run = {
            let mut rx = self.rx.lock().await;
            run_query_once(&self.app, &self.sql, &self.params, &mut rx).await
        };
        self.reloads.fetch_add(1, Ordering::Relaxed);
        // Whatever else this reload emitted stays in the window. A monitor left
        // open all afternoon would otherwise put one transport log line per
        // reload into a stream the agent stopped reading hours ago.
        let (columns, rows) = run.table.unwrap_or_default();
        self.last_row_count.store(rows.len(), Ordering::Relaxed);
        let document = monitor_document(self, &columns, &rows, run.error.as_deref());
        // The first render happened before the window opened, so a frontend
        // that cannot render at all was already an error and no window exists.
        // A reload that fails after that says so in the window rather than
        // blanking, for the same reason a failed query does.
        match self.frontend.render_page(&document, None) {
            Ok(page) => (axum::http::StatusCode::OK, Html(page)),
            Err(failure) => (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Html(format!(
                    "<!doctype html><html lang=\"en\"><head><meta http-equiv=\"refresh\" \
                     content=\"{}\"></head><body><h1>This reload could not be drawn</h1>\
                     <p>{}</p></body></html>",
                    self.refresh_seconds,
                    escape(&failure.message)
                )),
            ),
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn live_panel(
    app: Arc<App>,
    mut rx: mpsc::Receiver<Output>,
    view: UiView,
    sql: String,
    params: Vec<Value>,
    subject: String,
    refresh_seconds: u64,
    frontend: Arc<PanelFrontend>,
    output: OutputFormat,
) -> ! {
    // One run before anything is drawn, for the same reason a static panel
    // drains first: a server that refuses the connection must reach the agent
    // as an error event rather than as a window that says so to nobody. The
    // page the browser then loads is its own, fresher run.
    let opening = run_query_once(&app, &sql, &params, &mut rx).await;
    opening.report_to_agent(output);
    let Some((_, rows)) = opening.table else {
        fail(
            error_code::INVALID_REQUEST,
            "the activity query returned no result set",
            Some("check the connection target and that the server allows reading pg_stat_activity"),
            output,
        );
    };

    let stylesheet = match frontend.stylesheet() {
        Ok(bytes) => bytes,
        Err(failure) => fail_frontend(failure, output),
    };
    let panel = Arc::new(LivePanel {
        app,
        rx: Mutex::new(rx),
        sql,
        params,
        subject: subject.clone(),
        refresh_seconds,
        title: view.title(),
        frontend: Arc::clone(&frontend),
        reloads: AtomicU64::new(0),
        last_row_count: AtomicUsize::new(rows.len()),
    });
    // Draw the opening snapshot before the window exists, purely so a frontend
    // that will not render is an error rather than a window full of it.
    let opening_document = monitor_document(&panel, &Vec::new(), &rows, None);
    if let Err(failure) = frontend.render_page(&opening_document, None) {
        fail_frontend(failure, output);
    }
    let router = Router::new()
        .route(
            "/",
            get(|State(panel): State<Arc<LivePanel>>| async move { panel.page().await }),
        )
        .with_state(Arc::clone(&panel))
        .merge(shared_panel_routes(&frontend, stylesheet));

    let session = match UiSession::<()>::new("afpsql", view.ui_kind()) {
        Ok(session) => session
            .with_subject(subject.clone())
            .with_security_policy(security_policy(CONTENT_SECURITY_POLICY)),
        Err(error) => fail("ui_session_invalid", &error.to_string(), None, output),
    };

    emit_event_or_exit(
        agent_first_data::json_progress(serde_json::json!({
            "phase": "ui_ready",
            "message": "The connection monitor is open and reloads itself. \
                        It ends when the person closes the window.",
            "ui_kind": view.ui_kind(),
            "subject": subject,
            "refresh_seconds": refresh_seconds,
            "row_count": rows.len(),
            "ui_frontend_id": frontend.frontend_id(),
        }))
        .build(),
        output,
    );

    match session.window(router, &UiWindowConfig::default()).await {
        Ok(Outcome::Closed | Outcome::Completed(())) => {
            // This panel held a connection open for as long as the window was,
            // so it closes it rather than letting the exit tear it down. A view
            // of somebody's connection count should not leave one behind.
            panel.app.executor.shutdown().await;
            emit_event_or_exit(
                agent_first_data::json_result(serde_json::json!({
                    "ui_kind": view.ui_kind(),
                    "subject": subject,
                    "refresh_seconds": refresh_seconds,
                    // What the person actually watched: how many times the page
                    // reloaded, and what the last one showed.
                    "reload_count": panel.reloads.load(Ordering::Relaxed),
                    "row_count": panel.last_row_count.load(Ordering::Relaxed),
                    "closed": true,
                }))
                .build(),
                output,
            );
            std::process::exit(0);
        }
        Ok(Outcome::Expired) => fail(
            "ui_expired",
            "the connection monitor credential expired",
            None,
            output,
        ),
        Err(error) => fail("ui_failed", &error.to_string(), window_hint(&error), output),
    }
}

// ═══════════════════════════════════════════
// `ui plan` — a statement a person approves
// ═══════════════════════════════════════════

/// What the person said about one resolved statement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlanDecision {
    Approve,
    Refuse,
}

/// Whether this ending is permission to run the statement.
///
/// `Completed(Approve)` is the only one. The refuse button, a closed window,
/// and a lapsed credential are the same answer — do not run — because a person
/// who never answered has not agreed to anything. Written as an exhaustive
/// match with no default arm on purpose: a new `Outcome` variant has to be
/// classified here deliberately rather than inheriting "approve" by falling
/// through.
fn approved(outcome: &Outcome<PlanDecision>) -> bool {
    match outcome {
        Outcome::Completed(PlanDecision::Approve) => true,
        Outcome::Completed(PlanDecision::Refuse) | Outcome::Closed | Outcome::Expired => false,
    }
}

/// How the window ended, in the agent's terms.
fn ending_of(outcome: &Outcome<PlanDecision>) -> &'static str {
    match outcome {
        Outcome::Completed(PlanDecision::Approve) => "approved",
        Outcome::Completed(PlanDecision::Refuse) => "refused",
        Outcome::Closed => "window_closed",
        Outcome::Expired => "credential_expired",
    }
}

/// What PostgreSQL said about the statement when afpsql prepared it.
enum Prepared {
    /// It was accepted, and described: the parameters it wants and the columns
    /// it returns.
    Described {
        param_types: Vec<String>,
        columns: Vec<ColumnInfo>,
    },
    /// It was rejected. This is shown in place of the description rather than
    /// hidden: approving still runs the statement, and it will fail the same
    /// way, so the person deciding is the one who should see it.
    Rejected(String),
}

/// Everything the window says about a statement before anyone answers.
struct PlanFacts {
    sql: String,
    /// The values that will be bound, not just how many. A person approving
    /// `insert … values ($1)` is approving whatever `$1` is.
    params: Vec<Value>,
    /// True when the statement will run inside a `READ ONLY` transaction, which
    /// is what afpsql actually enforces. Resolved from the permission policy,
    /// not guessed from the SQL.
    read_only: bool,
    /// The statement's leading keyword, when it has one.
    keyword: Option<String>,
    prepared: Prepared,
    target: String,
}

impl PlanFacts {
    /// Whether afpsql reads this statement as a write, from its leading
    /// keyword alone.
    ///
    /// Only ever an over-warning: an unrecognised keyword is reported as
    /// unclassified, never as safe. The guarantee is `read_only`, which is
    /// PostgreSQL's to enforce rather than a keyword's to promise.
    fn writes(&self) -> bool {
        self.keyword
            .as_deref()
            .is_some_and(|keyword| WRITE_KEYWORDS.contains(&keyword))
    }
}

async fn plan_panel(
    app: Arc<App>,
    mut rx: mpsc::Receiver<Output>,
    plan: UiPlan,
    session: SessionConfig,
    output: OutputFormat,
) -> ! {
    let UiPlan {
        sql,
        params,
        options,
    } = plan;

    // Resolve the permission policy once, from the same options and session the
    // execution will resolve from, so what the window claims about the
    // transaction is what `handler::execute_query` independently decides. This
    // resolution describes; it never authorizes.
    let resolved = {
        let cfg = app.config.read().await;
        cfg.resolve_options_for_session(&options, &session)
    };
    let resolved = match resolved {
        Ok(resolved) => resolved,
        Err(message) => fail(error_code::INVALID_REQUEST, &message, None, output),
    };

    // afpsql-readonly gains no route to a write through this panel. The
    // authority is still `handler::execute_query`, which makes this same
    // judgement after an approval; refusing here as well means a person is
    // never asked to approve something afpsql was always going to refuse.
    if app.capability == crate::Capability::ReadOnly
        && let Some((error, hint)) =
            crate::readonly_policy::readonly_refusal(&sql, resolved.read_only)
    {
        fail(error_code::INVALID_REQUEST, &error, Some(hint), output);
    }

    // Resolved before the statement is even described, so a frontend afpsql
    // cannot load never costs a round trip and never opens a window.
    let frontend = match PanelFrontend::resolve(PLAN_UI_KIND, PanelShape::Decide) {
        Ok(frontend) => Arc::new(frontend),
        Err(failure) => fail_frontend(failure, output),
    };

    let prepared = describe_statement(&app, &session, &sql, &params, &resolved, output).await;
    let facts = PlanFacts {
        sql: sql.clone(),
        params: params.clone(),
        read_only: resolved.read_only,
        keyword: crate::readonly_policy::leading_keyword(&sql),
        prepared,
        target: connection_subject(&session),
    };
    let subject = plan_subject(&facts);
    let stylesheet = match frontend.stylesheet() {
        Ok(bytes) => bytes,
        Err(failure) => fail_frontend(failure, output),
    };
    // The nonce is minted once per session and is the only thing that can put a
    // script on this page. The page's own markup may be a person's; the script
    // that turns a declared control into an answer is afpsql's, and a frontend
    // cannot forge the attribute that admits it.
    let nonce = match UiCspNonce::generate() {
        Ok(nonce) => nonce,
        Err(error) => fail("ui_session_invalid", &error.to_string(), None, output),
    };
    let document = plan_document(&facts, &subject);
    let page = match frontend.render_page(&document, Some(&nonce)) {
        Ok(page) => page,
        Err(failure) => fail_frontend(failure, output),
    };

    let ui_session = match UiSession::<PlanDecision>::new("afpsql", PLAN_UI_KIND) {
        Ok(ui_session) => ui_session
            .with_subject(subject.clone())
            .with_security_policy(security_policy(&plan_content_security_policy(&nonce))),
        Err(error) => fail("ui_session_invalid", &error.to_string(), None, output),
    };
    let approve = ui_session.completion();
    let refuse = ui_session.completion();
    // Two routes rather than one with a body to parse: the path *is* the
    // answer, so nothing has to read a form field to find out which control was
    // pressed — and a control's declaration is bound to one of these two paths
    // by afpsql's runtime, never by the page.
    let approve_frontend = Arc::clone(&frontend);
    let refuse_frontend = Arc::clone(&frontend);
    let router = Router::new()
        .route("/", get(move || async move { Html(page) }))
        .route(
            "/approve",
            post(move || async move {
                let recorded = approve.complete(PlanDecision::Approve).await;
                decided_page(&approve_frontend, PlanDecision::Approve, recorded)
            }),
        )
        .route(
            "/refuse",
            post(move || async move {
                let recorded = refuse.complete(PlanDecision::Refuse).await;
                decided_page(&refuse_frontend, PlanDecision::Refuse, recorded)
            }),
        )
        .merge(shared_panel_routes(&frontend, stylesheet));

    emit_event_or_exit(
        agent_first_data::json_progress(serde_json::json!({
            "phase": "ui_ready",
            "message": "A statement is waiting for a person to approve it. \
                        Closing the window refuses it.",
            "ui_kind": PLAN_UI_KIND,
            "subject": subject,
            "target": facts.target,
            "read_only_transaction": facts.read_only,
            "writes": facts.writes(),
            "param_count": facts.params.len(),
            "prepared": facts.prepared.summary(),
            "ui_frontend_id": frontend.frontend_id(),
        }))
        .build(),
        output,
    );

    let outcome = match ui_session.window(router, &UiWindowConfig::default()).await {
        Ok(outcome) => outcome,
        Err(error) => fail("ui_failed", &error.to_string(), window_hint(&error), output),
    };
    if !approved(&outcome) {
        emit_event_or_exit(
            agent_first_data::json_result(serde_json::json!({
                "code": "ui_plan_refused",
                "ui_kind": PLAN_UI_KIND,
                "subject": subject,
                "decision": "refused",
                "ending": ending_of(&outcome),
                "executed": false,
            }))
            .build(),
            output,
        );
        std::process::exit(0);
    }

    emit_event_or_exit(
        agent_first_data::json_progress(serde_json::json!({
            "phase": "ui_plan_approved",
            "message": "The statement was approved and is running now.",
            "ui_kind": PLAN_UI_KIND,
            "subject": subject,
            "ending": ending_of(&outcome),
            "executed": true,
        }))
        .build(),
        output,
    );

    // The approved statement is the statement that was shown — the same `String`
    // and the same parameters, moved here untouched. Re-reading `--sql-file`, or
    // rebuilding the request from argv now, would let a person approve one
    // statement and afpsql run another. From here it is an ordinary
    // `afpsql --sql`: the same `handler::execute_query`, so the same permission
    // resolution, the same readonly policy, the same limits and events.
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

    let mut failed = false;
    while let Some(event) = rx.recv().await {
        if matches!(event, Output::Error { .. } | Output::SqlError { .. }) {
            failed = true;
        }
        if crate::emit::emit_output(&event, output).is_err() {
            std::process::exit(4);
        }
    }
    std::process::exit(if failed { 1 } else { 0 });
}

impl Prepared {
    /// The parameter types PostgreSQL inferred, or none when it never got that
    /// far.
    fn param_types(&self) -> &[String] {
        match self {
            Self::Described { param_types, .. } => param_types,
            Self::Rejected(_) => &[],
        }
    }

    fn summary(&self) -> Value {
        match self {
            Self::Described {
                param_types,
                columns,
            } => serde_json::json!({
                "described": true,
                "param_types": param_types,
                "columns": columns.iter().map(|column| column.name.clone()).collect::<Vec<_>>(),
            }),
            Self::Rejected(error) => serde_json::json!({
                "described": false,
                "error": error,
            }),
        }
    }
}

/// Ask PostgreSQL to describe the statement without running it.
///
/// This is the `--dry-run` path: `PREPARE` inside a read-only transaction that
/// is always rolled back. It buys the person the parameter types and result
/// columns, and it catches a statement the server will refuse before anyone is
/// asked to approve it.
///
/// A failure of the *connection* is not a fact about the statement and not a
/// question for a person, so it ends the process the way any other unreachable
/// server does. A failure of the *statement* goes on the page.
async fn describe_statement(
    app: &Arc<App>,
    session: &SessionConfig,
    sql: &str,
    params: &[Value],
    resolved: &ResolvedOptions,
    output: OutputFormat,
) -> Prepared {
    let outcome = app
        .executor
        .prepare_only(ExecRequest {
            session_name: "default",
            session_cfg: session,
            sql,
            params,
            opts: resolved,
            cancel_slot: None,
            transport_log: None,
        })
        .await;
    match outcome {
        Ok(info) => Prepared::Described {
            param_types: info.param_types,
            columns: info.columns,
        },
        Err(
            error @ (ExecError::Connect(_) | ExecError::Config { .. } | ExecError::Internal(_)),
        ) => fail(
            error_code::INVALID_REQUEST,
            &error.to_string(),
            Some("the window was not opened; nothing has run"),
            output,
        ),
        Err(error) => Prepared::Rejected(error.to_string()),
    }
}

/// What this window is about, in `afui session list`.
fn plan_subject(facts: &PlanFacts) -> String {
    let compact = facts.sql.split_whitespace().collect::<Vec<_>>().join(" ");
    let statement = truncate(&compact, 60);
    format!("{statement} on {}", facts.target)
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let kept: String = text.chars().take(limit).collect();
    format!("{kept}…")
}

/// What afpsql tells the person after they answer — in afpsql's words, from
/// what afpsql recorded.
///
/// This is not a courtesy. A decide panel's page may have been written by
/// somebody other than afpsql, and this is the sentence that says what actually
/// happened, rendered from the answer the server took rather than from whatever
/// the control that was pressed claimed to be.
fn decided_document(decision: PlanDecision, recorded: bool) -> DecidedDocument {
    let (heading, message) = match (decision, recorded) {
        (PlanDecision::Approve, true) => ("Approved", "afpsql is running the statement now."),
        (PlanDecision::Refuse, true) => ("Refused", "Nothing ran."),
        (_, false) => (
            "Already answered",
            "This window returned an answer already; this press changed nothing.",
        ),
    };
    DecidedDocument {
        ui_kind: PLAN_UI_KIND,
        title: "Statement",
        heading,
        subject: message.to_owned(),
        refresh_seconds: None,
        footer: "afpsql",
        message: message.to_owned(),
    }
}

/// The page a person lands on after answering, or a plain sentence when even
/// that page will not render.
///
/// A frontend that breaks here has already had its answer taken and acted on,
/// so there is nothing to abort — the failure is reported in place, in afpsql's
/// own words, rather than swallowed.
fn decided_page(
    frontend: &PanelFrontend,
    decision: PlanDecision,
    recorded: bool,
) -> (axum::http::StatusCode, Html<String>) {
    let document = decided_document(decision, recorded);
    match frontend.render_decided(&document) {
        Ok(page) => (axum::http::StatusCode::OK, Html(page)),
        Err(failure) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Html(format!(
                "<!doctype html><html lang=\"en\"><body><h1>{}</h1><p>{}</p></body></html>",
                escape(document.heading),
                escape(&failure.message)
            )),
        ),
    }
}

// ═══════════════════════════════════════════
// The documents a panel template renders against
// ═══════════════════════════════════════════
//
// These types are `ui_api_version` — the whole of what a frontend author may
// rely on. Everything a panel worked out is here already counted, sorted,
// classified and formatted, so an override reorders, regroups or drops
// sections without recomputing anything, and cannot arrive at a different
// answer than the one the agent read. Nothing here is markup: the template
// decides what a `<table>` or a `<meter>` is, and afpsql decides what is true.

/// One column, as PostgreSQL named and typed it.
#[derive(Serialize)]
struct ColumnDocument {
    name: String,
    type_name: String,
}

/// One cell. A NULL is flagged rather than rendered as an empty string,
/// because in a database those are different answers and a page that conflates
/// them is lying quietly.
#[derive(Serialize)]
struct CellDocument {
    value: String,
    is_null: bool,
}

#[derive(Serialize)]
struct RowDocument {
    cells: Vec<CellDocument>,
}

#[derive(Serialize)]
struct TableDocument {
    columns: Vec<ColumnDocument>,
    rows: Vec<RowDocument>,
}

/// A result set, aligned to its columns.
///
/// Cells are a list rather than a map so a template can iterate them in the
/// server's column order without knowing any column's name — which is what
/// makes one template serve `schema`, `table` and `indexes`.
fn table_document(columns: &[ColumnInfo], rows: &[Value]) -> TableDocument {
    TableDocument {
        columns: columns
            .iter()
            .map(|column| ColumnDocument {
                name: column.name.clone(),
                type_name: column.type_name.clone(),
            })
            .collect(),
        rows: rows
            .iter()
            .map(|row| RowDocument {
                cells: columns
                    .iter()
                    .map(|column| match row.get(&column.name) {
                        None | Some(Value::Null) => CellDocument {
                            value: String::new(),
                            is_null: true,
                        },
                        Some(value) => CellDocument {
                            value: scalar(value),
                            is_null: false,
                        },
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// `schema_inspect`, `table_inspect`, `index_inspect`.
#[derive(Serialize)]
struct InspectDocument {
    ui_kind: &'static str,
    title: &'static str,
    heading: &'static str,
    subject: String,
    /// Always `null` here: a schema does not change while a person reads it.
    /// The key is present so one layout serves every panel.
    refresh_seconds: Option<u64>,
    footer: String,
    row_count: usize,
    table: TableDocument,
}

fn inspect_document(
    view: &UiView,
    subject: &str,
    columns: &[ColumnInfo],
    rows: &[Value],
) -> InspectDocument {
    InspectDocument {
        ui_kind: view.ui_kind(),
        title: view.title(),
        heading: view.title(),
        subject: subject.to_owned(),
        refresh_seconds: None,
        footer: format!(
            "{} row(s). Close this window when you are done.",
            rows.len()
        ),
        row_count: rows.len(),
        table: table_document(columns, rows),
    }
}

// ═══════════════════════════════════════════
// `ui connections` document
// ═══════════════════════════════════════════

/// The number the whole panel exists for: client connections against the limit
/// that governs them.
#[derive(Serialize)]
struct CapacityDocument {
    clients: usize,
    /// `null` when the server did not report `max_connections`, which is a
    /// different page rather than a zero.
    max: Option<i64>,
    /// Where "getting close" starts, at four fifths of the limit.
    high: Option<i64>,
}

#[derive(Serialize)]
struct CountDocument {
    label: String,
    count: usize,
}

#[derive(Serialize)]
struct GroupDocument {
    heading: &'static str,
    entries: Vec<CountDocument>,
}

/// One backend in the "longest running" list, with every age already written
/// the way a person reads it.
#[derive(Serialize)]
struct BackendDocument {
    pid: String,
    database: String,
    user: String,
    state: String,
    waiting_on: String,
    query_age: String,
    transaction_age: String,
    statement: String,
    /// afpsql's own connection. Worth marking, because it is always in the list
    /// and is not part of what the person came to look at.
    is_self: bool,
}

#[derive(Serialize)]
struct MonitorDocument {
    ui_kind: &'static str,
    title: &'static str,
    heading: &'static str,
    subject: String,
    refresh_seconds: Option<u64>,
    footer: String,
    /// The failure of *this* reload, if it failed. A monitor that blanks itself
    /// the first time a server hiccups is worse than one that says which reload
    /// failed and keeps trying.
    error: Option<String>,
    snapshot_at: String,
    backend_count: usize,
    client_count: usize,
    capacity: CapacityDocument,
    groups: Vec<GroupDocument>,
    waits: Vec<CountDocument>,
    longest: Vec<BackendDocument>,
    table: TableDocument,
}

fn monitor_document(
    panel: &LivePanel,
    columns: &[ColumnInfo],
    rows: &[Value],
    error: Option<&str>,
) -> MonitorDocument {
    let clients: Vec<&Value> = rows
        .iter()
        .filter(|row| text(row, "backend_type") == "client backend")
        .collect();
    let max = rows
        .iter()
        .find_map(|row| number(row, "max_connections"))
        .filter(|max| *max > 0);
    let snapshot_at = rows
        .first()
        .map(|row| text(row, "snapshot_at"))
        .unwrap_or_default();
    let mut footer = String::new();
    if snapshot_at.is_empty() {
        let _ = write!(footer, "Reloads every {}s.", panel.refresh_seconds);
    } else {
        let _ = write!(
            footer,
            "Server time {snapshot_at}. Reloads every {}s.",
            panel.refresh_seconds
        );
    }
    footer.push_str(" Close this window when you are done.");

    MonitorDocument {
        ui_kind: "connection_monitor",
        title: panel.title,
        heading: panel.title,
        subject: panel.subject.clone(),
        refresh_seconds: Some(panel.refresh_seconds),
        footer,
        error: error.map(str::to_owned),
        snapshot_at,
        backend_count: rows.len(),
        client_count: clients.len(),
        capacity: CapacityDocument {
            clients: clients.len(),
            max,
            high: max.map(|max| max.saturating_mul(4) / 5),
        },
        groups: connection_groups(&clients),
        waits: wait_events(rows),
        longest: longest_running(rows),
        table: table_document(columns, rows),
    }
}

/// Client connections grouped three ways: by what they are doing, and by whose
/// they are.
fn connection_groups(clients: &[&Value]) -> Vec<GroupDocument> {
    if clients.is_empty() {
        return Vec::new();
    }
    [
        ("By state", "state"),
        ("By database", "database"),
        ("By user", "user"),
    ]
    .into_iter()
    .map(|(heading, key)| GroupDocument {
        heading,
        entries: tally(clients, key)
            .into_iter()
            .map(|(label, count)| CountDocument { label, count })
            .collect(),
    })
    .collect()
}

/// What every waiting backend is waiting for, most common first.
fn wait_events(rows: &[Value]) -> Vec<CountDocument> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for row in rows {
        let kind = text(row, "wait_event_type");
        if kind.is_empty() {
            continue;
        }
        let event = text(row, "wait_event");
        let label = if event.is_empty() {
            kind
        } else {
            format!("{kind}: {event}")
        };
        bump(&mut counts, label);
    }
    counts.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
    counts
        .into_iter()
        .map(|(label, count)| CountDocument { label, count })
        .collect()
}

/// The backends whose age means something.
///
/// An idle connection's `query_start` is when its *last* statement began, so
/// sorting every backend by it would put a connection that has done nothing for
/// an hour at the top of a list titled "longest running". Only a backend that is
/// running a statement, or holding a transaction open, is listed.
fn longest_running(rows: &[Value]) -> Vec<BackendDocument> {
    let mut running: Vec<&Value> = rows
        .iter()
        .filter(|row| {
            let state = text(row, "state");
            state == "active" || state.starts_with("idle in transaction")
        })
        .collect();
    running.sort_by_key(|row| std::cmp::Reverse(number(row, "query_seconds").unwrap_or(0)));
    running.truncate(LONGEST_ROWS);
    running
        .into_iter()
        .map(|row| {
            let waiting_on = match (text(row, "wait_event_type"), text(row, "wait_event")) {
                (kind, _) if kind.is_empty() => "—".to_string(),
                (kind, event) if event.is_empty() => kind,
                (kind, event) => format!("{kind}: {event}"),
            };
            let query = text(row, "query");
            BackendDocument {
                pid: scalar_or_dash(row.get("pid")),
                database: scalar_or_dash(row.get("database")),
                user: scalar_or_dash(row.get("user")),
                state: scalar_or_dash(row.get("state")),
                waiting_on,
                query_age: duration(number(row, "query_seconds")),
                transaction_age: duration(number(row, "transaction_seconds")),
                statement: if query.is_empty() {
                    "—".to_string()
                } else {
                    query
                },
                is_self: boolean(row, "is_self"),
            }
        })
        .collect()
}

/// Count one string field across a set of rows, most frequent first.
fn tally(rows: &[&Value], key: &str) -> Vec<(String, usize)> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for row in rows {
        let value = text(row, key);
        bump(
            &mut counts,
            if value.is_empty() {
                "unknown".to_string()
            } else {
                value
            },
        );
    }
    counts.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
    counts
}

fn bump(counts: &mut Vec<(String, usize)>, label: String) {
    match counts.iter_mut().find(|(name, _)| *name == label) {
        Some((_, count)) => *count += 1,
        None => counts.push((label, 1)),
    }
}

/// An age a person reads at a glance. Negative ages are clamped: the snapshot's
/// `now()` is the transaction's start, so a statement that began microseconds
/// later is not "-1 seconds old".
fn duration(seconds: Option<i64>) -> String {
    let Some(seconds) = seconds else {
        return "—".to_string();
    };
    let seconds = seconds.max(0);
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m {}s", seconds / 60, seconds % 60),
        _ => format!("{}h {}m", seconds / 3600, (seconds % 3600) / 60),
    }
}

// ═══════════════════════════════════════════
// `ui plan` page
// ═══════════════════════════════════════════

/// One value that will be bound, beside the type PostgreSQL inferred for it.
#[derive(Serialize)]
struct ParamDocument {
    index: usize,
    type_name: Option<String>,
    value: String,
    /// False when the statement has a placeholder and the request had no value
    /// for it. The execution will reject that; saying so beside the placeholder
    /// beats a silent gap.
    bound: bool,
}

/// One answer a person can give, and what to call it.
///
/// `id` is the whole of the semantics: afpsql's runtime binds a control
/// declaring `data-afpsql-decision="approve"` to the route that approves, and
/// binds nothing to a declaration it does not recognise. A template may put
/// these anywhere, label them anything, and wrap them in anything — the
/// mapping is not the template's to write.
#[derive(Serialize)]
struct DecisionDocument {
    id: &'static str,
    label: &'static str,
}

/// Everything a person needs in order to refuse.
///
/// The statement exactly as afpsql will send it, the parameters it will bind,
/// the server it will reach, and whether the transaction it runs in may write
/// at all. When PostgreSQL refused to prepare it, `rejected` says so — a page
/// that dropped it would be a reassuring blank.
#[derive(Serialize)]
struct PlanDocument {
    ui_kind: &'static str,
    title: &'static str,
    heading: &'static str,
    subject: String,
    refresh_seconds: Option<u64>,
    footer: &'static str,
    target: String,
    /// What PostgreSQL will actually enforce, in a sentence.
    transaction: &'static str,
    /// What afpsql reads the statement as, in a sentence. Over-inclusive by
    /// design and never reassuring: an unrecognised keyword says so.
    classification: String,
    read_only: bool,
    writes: bool,
    keyword: Option<String>,
    sql: String,
    params: Vec<ParamDocument>,
    returns: Option<String>,
    rejected: Option<String>,
    decisions: Vec<DecisionDocument>,
}

fn plan_document(facts: &PlanFacts, subject: &str) -> PlanDocument {
    let types = facts.prepared.param_types();
    let count = facts.params.len().max(types.len());
    let params = (0..count)
        .map(|index| {
            let bound = index < facts.params.len();
            ParamDocument {
                index: index + 1,
                type_name: types.get(index).cloned(),
                value: match facts.params.get(index) {
                    Some(Value::Null) => "NULL".to_string(),
                    Some(value) => scalar(value),
                    None => "nothing bound".to_string(),
                },
                bound,
            }
        })
        .collect();
    PlanDocument {
        ui_kind: PLAN_UI_KIND,
        title: "Approve statement",
        heading: "Run this statement?",
        subject: subject.to_owned(),
        refresh_seconds: None,
        footer: "Closing this window without answering refuses the statement.",
        target: facts.target.clone(),
        transaction: if facts.read_only {
            "READ ONLY — PostgreSQL rejects any write in it"
        } else {
            "READ WRITE — this statement may change data"
        },
        classification: match (&facts.keyword, facts.writes()) {
            (Some(keyword), true) => {
                format!("{} — afpsql calls this a write", keyword.to_uppercase())
            }
            (Some(keyword), false) => format!(
                "{} — afpsql does not classify this as a write; a function it calls still can",
                keyword.to_uppercase()
            ),
            (None, _) => "afpsql could not read a leading keyword".to_string(),
        },
        read_only: facts.read_only,
        writes: facts.writes(),
        keyword: facts.keyword.clone(),
        sql: facts.sql.clone(),
        params,
        returns: match &facts.prepared {
            Prepared::Described { columns, .. } => Some(describe_columns(columns)),
            Prepared::Rejected(_) => None,
        },
        rejected: match &facts.prepared {
            Prepared::Rejected(error) => Some(error.clone()),
            Prepared::Described { .. } => None,
        },
        decisions: vec![
            DecisionDocument {
                id: "refuse",
                label: "Do not run",
            },
            DecisionDocument {
                id: "approve",
                label: "Approve and run",
            },
        ],
    }
}

/// What afpsql says once it has recorded an answer.
#[derive(Serialize)]
struct DecidedDocument {
    ui_kind: &'static str,
    title: &'static str,
    heading: &'static str,
    subject: String,
    refresh_seconds: Option<u64>,
    footer: &'static str,
    message: String,
}

fn describe_columns(columns: &[ColumnInfo]) -> String {
    if columns.is_empty() {
        return "no rows".to_string();
    }
    columns
        .iter()
        .map(|column| format!("{} {}", column.name, column.type_name))
        .collect::<Vec<_>>()
        .join(", ")
}

// ═══════════════════════════════════════════
// Value rendering
// ═══════════════════════════════════════════

/// Render one cell. Nested objects and arrays are shown as compact JSON —
/// `inspect table --full` returns constraint and index lists this way.
fn scalar(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn scalar_or_dash(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "—".to_string(),
        Some(value) => scalar(value),
    }
}

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn number(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64)
}

fn boolean(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn escape(raw: &str) -> String {
    let mut escaped = String::with_capacity(raw.len());
    for character in raw.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            other => escaped.push(other),
        }
    }
    escaped
}

impl UiView {
    /// The AFUI `ui_kind` for this panel.
    ///
    /// One panel shape per kind, so a person can replace exactly the panel they
    /// mean with `afui frontend` without affecting the others.
    #[must_use]
    pub fn ui_kind(&self) -> &'static str {
        match self {
            Self::Schema { .. } => "schema_inspect",
            Self::Table { .. } => "table_inspect",
            Self::Indexes { .. } => "index_inspect",
            Self::Connections { .. } => "connection_monitor",
        }
    }

    #[must_use]
    pub fn title(&self) -> &'static str {
        match self {
            Self::Schema { .. } => "Schema",
            Self::Table { .. } => "Table",
            Self::Indexes { .. } => "Indexes",
            Self::Connections { .. } => "Connections",
        }
    }

    /// How often this panel reloads itself, when it reloads at all.
    ///
    /// A schema does not change while someone reads it, and re-running a
    /// full-schema export every few seconds would hold a connection open for
    /// the life of a window with nothing new to show. Server activity is the
    /// opposite: the whole reason to look is that it moves.
    #[must_use]
    pub fn refresh_seconds(&self) -> Option<u64> {
        match self {
            Self::Schema { .. } | Self::Table { .. } | Self::Indexes { .. } => None,
            Self::Connections {
                refresh_seconds, ..
            } => Some(*refresh_seconds),
        }
    }

    /// What this panel is a view of, when the arguments name it.
    ///
    /// The connection monitor's subject is the server, which only the caller
    /// knows, so it is filled in there.
    #[must_use]
    pub fn subject(&self) -> String {
        match self {
            Self::Schema { schema } => schema.clone(),
            Self::Table { name } => name.clone(),
            Self::Indexes { schema, table } => match table {
                Some(table) => format!("{schema}.{table}"),
                None => schema.clone(),
            },
            Self::Connections { .. } => String::new(),
        }
    }

    /// The inspect query behind this panel.
    ///
    /// Reusing `InspectAction` is the point: a panel can never drift from the
    /// `afpsql inspect` output an agent reads for the same object.
    #[must_use]
    pub fn inspect_action(&self) -> InspectAction {
        match self {
            Self::Schema { schema } => InspectAction::Schema(InspectSchemaArgs {
                schema: schema.clone(),
                like: None,
            }),
            Self::Table { name } => InspectAction::Table(InspectTableArgs {
                name: name.clone(),
                full: true,
            }),
            Self::Indexes { schema, table } => InspectAction::Indexes(InspectIndexesArgs {
                schema: schema.clone(),
                table: table.clone(),
                stats: false,
            }),
            Self::Connections { all, .. } => {
                InspectAction::Connections(InspectConnectionsArgs { all: *all })
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use frontend::{DECISION_ATTRIBUTE, TRUSTED_RUNTIME_MARKER};

    /// afpsql's own page for a watch panel, rendered exactly as it is served.
    fn inspect_page(
        view: &UiView,
        subject: &str,
        columns: &[ColumnInfo],
        rows: &[Value],
    ) -> String {
        PanelFrontend::builtin(view.ui_kind(), PanelShape::Inspect)
            .render_page(&inspect_document(view, subject, columns, rows), None)
            .expect("afpsql's own inspect panel renders")
    }

    fn columns() -> Vec<ColumnInfo> {
        vec![
            ColumnInfo {
                name: "column".to_string(),
                type_name: "text".to_string(),
            },
            ColumnInfo {
                name: "notes".to_string(),
                type_name: "text".to_string(),
            },
        ]
    }

    #[test]
    fn each_view_names_a_distinct_ui_kind_and_reuses_the_inspect_query() {
        let schema = UiView::Schema {
            schema: "public".to_string(),
        };
        let table = UiView::Table {
            name: "public.orders".to_string(),
        };
        let indexes = UiView::Indexes {
            schema: "public".to_string(),
            table: Some("orders".to_string()),
        };
        let connections = UiView::Connections {
            all: false,
            refresh_seconds: 5,
        };
        assert_eq!(schema.ui_kind(), "schema_inspect");
        assert_eq!(table.ui_kind(), "table_inspect");
        assert_eq!(indexes.ui_kind(), "index_inspect");
        assert_eq!(connections.ui_kind(), "connection_monitor");
        assert_eq!(indexes.subject(), "public.orders");

        // Each panel must run the same SQL as the matching inspect subcommand,
        // or the window and the agent would disagree about the same object.
        let (panel_sql, _) = crate::cli::inspect_sql_for(table.inspect_action());
        let (cli_sql, _) = crate::cli::inspect_sql_for(InspectAction::Table(InspectTableArgs {
            name: "public.orders".to_string(),
            full: true,
        }));
        assert_eq!(panel_sql, cli_sql);

        let (monitor_sql, monitor_params) =
            crate::cli::inspect_sql_for(connections.inspect_action());
        let (inspect_sql, inspect_params) =
            crate::cli::inspect_sql_for(InspectAction::Connections(InspectConnectionsArgs {
                all: false,
            }));
        assert_eq!(monitor_sql, inspect_sql);
        assert_eq!(monitor_params, inspect_params);
        assert!(monitor_sql.contains("pg_catalog.pg_stat_activity"));
        assert!(monitor_sql.contains("max_connections"));
        // `--all` is the panel's own argument and has to reach the query, or
        // the flag would be accepted and ignored.
        let (all_sql, _) = crate::cli::inspect_sql_for(
            UiView::Connections {
                all: true,
                refresh_seconds: 5,
            }
            .inspect_action(),
        );
        assert!(monitor_sql.contains("where a.backend_type = 'client backend'"));
        assert!(!all_sql.contains("where a.backend_type"));
    }

    #[test]
    fn only_the_monitor_reloads_itself() {
        assert_eq!(
            UiView::Schema {
                schema: "public".to_string()
            }
            .refresh_seconds(),
            None
        );
        assert_eq!(
            UiView::Connections {
                all: false,
                refresh_seconds: 9
            }
            .refresh_seconds(),
            Some(9)
        );
    }

    #[test]
    fn cell_values_are_escaped_so_object_names_cannot_inject_markup() {
        let rows = vec![serde_json::json!({
            "column": "<script>alert(1)</script>",
            "notes": Value::Null,
        })];
        let page = inspect_page(
            &UiView::Table {
                name: "public.orders".to_string(),
            },
            "public.orders",
            &columns(),
            &rows,
        );
        assert!(!page.contains("<script>alert(1)</script>"));
        assert!(page.contains("&lt;script&gt;alert(1)&lt;&#x2f;script&gt;"));
        // A NULL must be visibly distinct from an empty string.
        assert!(page.contains("class=\"null\">NULL</td>"));
    }

    #[test]
    fn the_panel_loads_no_script_and_declares_a_policy_that_forbids_one() {
        let rows = vec![serde_json::json!({"column": "id", "notes": "primary key"})];
        let page = inspect_page(
            &UiView::Schema {
                schema: "public".to_string(),
            },
            "public",
            &columns(),
            &rows,
        );
        assert!(!page.contains("<script"));
        assert!(page.contains("<link rel=\"stylesheet\" href=\"style.css\">"));
        let nonce = UiCspNonce::generate().unwrap();
        let plan_policy = plan_content_security_policy(&nonce);
        for policy in [CONTENT_SECURITY_POLICY, plan_policy.as_str()] {
            assert!(policy.contains("default-src 'none'"));
            assert!(policy.contains("style-src 'self'"));
        }
        // Only the page that returns an answer may post one back — and only it
        // admits a script, by exact nonce rather than by kind.
        assert!(CONTENT_SECURITY_POLICY.contains("form-action 'none'"));
        assert!(!CONTENT_SECURITY_POLICY.contains("script-src"));
        assert!(plan_policy.contains("form-action 'self'"));
        assert!(plan_policy.contains(&format!("script-src 'nonce-{}'", nonce.as_str())));
    }

    #[test]
    fn an_empty_result_says_so_instead_of_rendering_a_headerless_table() {
        let page = inspect_page(
            &UiView::Indexes {
                schema: "public".to_string(),
                table: None,
            },
            "public",
            &columns(),
            &[],
        );
        assert!(page.contains("No matching objects."));
        assert!(!page.contains("<tbody>"));
    }

    #[test]
    fn a_static_panel_carries_no_refresh_directive() {
        let page = inspect_page(
            &UiView::Schema {
                schema: "public".to_string(),
            },
            "public",
            &columns(),
            &[],
        );
        assert!(!page.contains("http-equiv=\"refresh\""));
    }

    // ── ui connections ──────────────────────

    fn activity_columns() -> Vec<ColumnInfo> {
        [
            ("pid", "int4"),
            ("database", "name"),
            ("user", "name"),
            ("backend_type", "text"),
            ("state", "text"),
            ("wait_event_type", "text"),
            ("wait_event", "text"),
            ("query", "text"),
            ("query_seconds", "int8"),
            ("max_connections", "int8"),
            ("snapshot_at", "text"),
        ]
        .into_iter()
        .map(|(name, type_name)| ColumnInfo {
            name: name.to_string(),
            type_name: type_name.to_string(),
        })
        .collect()
    }

    fn backend(pid: i64, database: &str, user: &str, state: &str, query_seconds: i64) -> Value {
        serde_json::json!({
            "pid": pid,
            "database": database,
            "user": user,
            "backend_type": "client backend",
            "state": state,
            "wait_event_type": Value::Null,
            "wait_event": Value::Null,
            "query": "select 1",
            "query_seconds": query_seconds,
            "transaction_seconds": Value::Null,
            "is_self": false,
            "max_connections": 100,
            "snapshot_at": "2026-08-06 09:41:02.123 UTC",
        })
    }

    fn monitor_page(rows: &[Value], error: Option<&str>) -> String {
        let panel = LivePanel {
            app: Arc::new(App::new(
                RuntimeConfig::default(),
                mpsc::channel::<Output>(1).0,
                crate::Capability::ReadOnly,
            )),
            rx: Mutex::new(mpsc::channel::<Output>(1).1),
            sql: String::new(),
            params: Vec::new(),
            subject: "127.0.0.1:5432/orders".to_string(),
            refresh_seconds: 5,
            title: "Connections",
            frontend: Arc::new(PanelFrontend::builtin(
                "connection_monitor",
                PanelShape::Monitor,
            )),
            reloads: AtomicU64::new(0),
            last_row_count: AtomicUsize::new(0),
        };
        panel
            .frontend
            .render_page(
                &monitor_document(&panel, &activity_columns(), rows, error),
                None,
            )
            .expect("afpsql's own connection monitor renders")
    }

    /// The rule this panel exists for: a monitor frozen at open is the wrong
    /// product, and with no JavaScript the only thing that makes it move is
    /// this one line.
    #[test]
    fn the_monitor_asks_the_browser_to_reload_it() {
        let page = monitor_page(&[backend(11, "orders", "app", "active", 3)], None);
        assert!(page.contains("<meta http-equiv=\"refresh\" content=\"5\">"));
        assert!(!page.contains("<script"));
        assert!(page.contains("Reloads every 5s."));
        assert!(page.contains("Server time 2026-08-06 09:41:02.123 UTC"));
    }

    #[test]
    fn the_count_is_shown_against_the_limit_that_governs_it() {
        let rows = vec![
            backend(11, "orders", "app", "active", 3),
            backend(12, "orders", "app", "idle", 90),
            backend(13, "billing", "reporting", "idle in transaction", 400),
        ];
        let page = monitor_page(&rows, None);
        assert!(page.contains("max=\"100\" value=\"3\""));
        assert!(page.contains("of 100"));
        // Grouped three ways, all from the one snapshot.
        assert!(page.contains("By state"));
        assert!(page.contains("By database"));
        assert!(page.contains("By user"));
        assert!(page.contains("reporting"));
        // An idle backend's last-query age is not a running query.
        assert!(page.contains("Longest running"));
        assert!(page.contains("6m 40s"));
        assert!(!page.contains("1m 30s"));
    }

    #[test]
    fn a_backend_that_owns_no_running_statement_is_not_called_long_running() {
        let page = monitor_page(&[backend(11, "orders", "app", "idle", 9_000)], None);
        assert!(page.contains("No statement is running and no transaction is open."));
    }

    #[test]
    fn a_failed_reload_is_shown_and_the_page_keeps_reloading() {
        let page = monitor_page(&[], Some("connection refused"));
        assert!(page.contains("This reload failed: connection refused"));
        assert!(page.contains("<meta http-equiv=\"refresh\" content=\"5\">"));
        assert!(page.contains("The server reported no backends."));
    }

    #[test]
    fn database_and_role_names_are_escaped_so_they_cannot_inject_markup() {
        let page = monitor_page(
            &[backend(
                11,
                "<script>alert(1)</script>",
                "<img src=x>",
                "active",
                1,
            )],
            None,
        );
        assert!(!page.contains("<script>alert(1)</script>"));
        assert!(!page.contains("<img src=x>"));
        assert!(page.contains("&lt;script&gt;alert(1)&lt;&#x2f;script&gt;"));
    }

    #[test]
    fn wait_events_are_counted_and_a_quiet_server_says_so() {
        let mut waiting = backend(11, "orders", "app", "active", 1);
        waiting["wait_event_type"] = Value::from("Lock");
        waiting["wait_event"] = Value::from("transactionid");
        let page = monitor_page(&[waiting], None);
        assert!(page.contains("Lock: transactionid"));
        assert!(
            monitor_page(&[backend(11, "orders", "app", "active", 1)], None)
                .contains("Nothing is waiting.")
        );
    }

    /// A monitor's subject has to name the server, and a DSN is how most
    /// sessions name one — so it is read out of the resolved connection, and
    /// the password in that same string never comes with it.
    #[test]
    fn a_subject_names_the_endpoint_without_carrying_its_secret() {
        let subject = connection_subject(&SessionConfig {
            dsn_secret: Some("postgresql://app:hunter2@db.internal:6543/orders".to_string()),
            ..Default::default()
        });
        assert!(!subject.contains("hunter2"), "subject leaked a password");
        assert!(!subject.contains("app"));
        assert_eq!(subject, "db.internal:6543/orders");
        assert_eq!(
            connection_subject(&SessionConfig {
                host: Some("127.0.0.1".to_string()),
                port: Some(5432),
                dbname: Some("orders".to_string()),
                ..Default::default()
            }),
            "127.0.0.1:5432/orders"
        );
    }

    // ── ui plan ─────────────────────────────

    fn facts(sql: &str, read_only: bool, prepared: Prepared) -> PlanFacts {
        PlanFacts {
            sql: sql.to_string(),
            params: Vec::new(),
            read_only,
            keyword: crate::readonly_policy::leading_keyword(sql),
            prepared,
            target: "127.0.0.1:5432/orders".to_string(),
        }
    }

    /// afpsql's own decide page, rendered exactly as it is served — runtime
    /// spliced in, controls declared, nonce bound.
    fn plan_page(facts: &PlanFacts, subject: &str) -> String {
        let nonce = UiCspNonce::generate().unwrap();
        PanelFrontend::builtin(PLAN_UI_KIND, PanelShape::Decide)
            .render_page(&plan_document(facts, subject), Some(&nonce))
            .expect("afpsql's own plan panel renders")
    }

    fn described() -> Prepared {
        Prepared::Described {
            param_types: vec!["int4".to_string()],
            columns: vec![ColumnInfo {
                name: "id".to_string(),
                type_name: "int4".to_string(),
            }],
        }
    }

    /// The one rule this panel exists to keep. Closing the window is a person
    /// walking away, not a person agreeing; so is a credential that lapsed.
    #[test]
    fn only_pressing_approve_is_permission_to_run() {
        assert!(approved(&Outcome::Completed(PlanDecision::Approve)));
        assert!(!approved(&Outcome::Completed(PlanDecision::Refuse)));
        assert!(!approved(&Outcome::Closed));
        assert!(!approved(&Outcome::Expired));
    }

    #[test]
    fn every_ending_is_named_for_the_agent_that_asked() {
        assert_eq!(
            ending_of(&Outcome::Completed(PlanDecision::Approve)),
            "approved"
        );
        assert_eq!(
            ending_of(&Outcome::Completed(PlanDecision::Refuse)),
            "refused"
        );
        assert_eq!(ending_of(&Outcome::Closed), "window_closed");
        assert_eq!(ending_of(&Outcome::Expired), "credential_expired");
    }

    #[test]
    fn a_write_says_so_and_names_the_transaction_it_runs_in() {
        let mut plan = facts(
            "update orders set total = total + 1 where id = $1",
            false,
            described(),
        );
        plan.params = vec![Value::from("4711")];
        assert!(plan.writes());
        let page = plan_page(&plan, "update orders …");
        assert!(page.contains("class=\"plan writes\""));
        assert!(page.contains("UPDATE — afpsql calls this a write"));
        assert!(page.contains("READ WRITE"));
        assert!(page.contains("127.0.0.1:5432&#x2f;orders"));
        // The statement itself, verbatim, and what preparing it revealed.
        assert!(page.contains("update orders set total = total + 1 where id = $1"));
        assert!(page.contains("id int4"));
        // The value that will be bound, not just that there is one: a
        // placeholder tells a person nothing about what they are approving.
        assert!(page.contains("<dt>$1 int4</dt><dd>4711</dd>"));
        // Both answers are offered, and each is *declared* rather than wired:
        // the page says what a control is for, afpsql's runtime says what it
        // does, and the runtime is admitted by a nonce a frontend cannot forge.
        assert!(page.contains("data-afpsql-decision=\"approve\""));
        assert!(page.contains("data-afpsql-decision=\"refuse\""));
        assert!(page.contains("<script nonce=\""));
        assert!(page.contains("form.action = action"));
        assert!(!page.contains(TRUSTED_RUNTIME_MARKER));
        assert!(page.contains("Closing this window without answering refuses the statement."));
    }

    /// A keyword afpsql does not recognise must never read as "this is safe".
    #[test]
    fn an_unclassified_statement_is_reported_as_unclassified() {
        let plan = facts("select pg_terminate_backend($1)", true, described());
        assert!(!plan.writes());
        let page = plan_page(&plan, "select …");
        assert!(page.contains("does not classify this as a write"));
        assert!(page.contains("a function it calls still can"));
        assert!(page.contains("READ ONLY"));
        assert!(!page.contains("class=\"plan writes\""));
    }

    /// A statement PostgreSQL refused must not look settled, for the same
    /// reason afpay's unquotable payment does not.
    #[test]
    fn a_statement_postgres_rejected_says_so_instead_of_looking_settled() {
        let mut plan = facts(
            "insert into nope values ($1)",
            false,
            Prepared::Rejected("relation \"nope\" does not exist".to_string()),
        );
        plan.params = vec![Value::from("<b>hi</b>")];
        let page = plan_page(&plan, "insert …");
        assert!(page.contains("PostgreSQL rejected this statement"));
        assert!(page.contains("relation &quot;nope&quot; does not exist"));
        // A statement the server refused is exactly when a person wants to see
        // the values, so they are shown even with no inferred types.
        assert!(page.contains("<dt>$1</dt><dd>&lt;b&gt;hi&lt;&#x2f;b&gt;</dd>"));
        // Still a question, not an answer: refusing stays one press away.
        assert!(page.contains("data-afpsql-decision=\"refuse\""));
        assert!(page.contains("data-afpsql-decision=\"approve\""));
    }

    #[test]
    fn a_statement_is_escaped_so_it_cannot_inject_markup() {
        let plan = facts("select '<script>alert(1)</script>'", true, described());
        let page = plan_page(&plan, "select …");
        assert!(!page.contains("<script>alert(1)</script>"));
        assert!(page.contains("&lt;script&gt;alert(1)&lt;&#x2f;script&gt;"));
    }

    #[test]
    fn a_statement_with_no_result_columns_says_no_rows() {
        let plan = facts(
            "delete from orders",
            false,
            Prepared::Described {
                param_types: Vec::new(),
                columns: Vec::new(),
            },
        );
        let page = plan_page(&plan, "delete …");
        assert!(page.contains("no rows"));
        assert!(page.contains("<dd>none</dd>"));
    }

    /// The subject is what tells two waiting windows apart in `afui session
    /// list`, so it names the statement and the server — never a secret.
    #[test]
    fn the_subject_names_the_statement_and_the_server() {
        let plan = facts("update orders\n  set total = 0", false, described());
        assert_eq!(
            plan_subject(&plan),
            "update orders set total = 0 on 127.0.0.1:5432/orders"
        );
        let long = facts(&"select 1, ".repeat(40), true, described());
        assert!(plan_subject(&long).contains('…'));
    }

    /// The window returns one answer. A second press has to say so rather than
    /// implying it changed something.
    #[tokio::test]
    async fn the_first_answer_wins_and_the_second_is_told_so() {
        let session = UiSession::<PlanDecision>::new("afpsql", PLAN_UI_KIND).unwrap();
        let completion = session.completion();
        assert!(completion.complete(PlanDecision::Refuse).await);
        assert!(!completion.complete(PlanDecision::Approve).await);
        let frontend = PanelFrontend::builtin(PLAN_UI_KIND, PanelShape::Decide);
        let refused = frontend
            .render_decided(&decided_document(PlanDecision::Refuse, true))
            .unwrap();
        assert!(refused.contains("Nothing ran."));
        // afpsql's own words about what afpsql recorded, and no decision
        // control on the page that says them.
        assert!(!refused.contains(DECISION_ATTRIBUTE));
        assert!(
            frontend
                .render_decided(&decided_document(PlanDecision::Approve, false))
                .unwrap()
                .contains("already")
        );
    }

    /// afpsql-readonly may open a panel, and may not gain a write route through
    /// one. The panel asks this before it opens a window; the executor asks it
    /// again before it runs anything.
    #[test]
    fn readonly_refuses_a_write_before_a_person_is_ever_asked() {
        assert!(crate::readonly_policy::readonly_refusal("select 1", true).is_none());
        let (error, _) =
            crate::readonly_policy::readonly_refusal("update orders set total = 0", false)
                .expect("a read-write transaction must be refused");
        assert!(error.contains("write permission is unavailable"));
        let (error, _) = crate::readonly_policy::readonly_refusal("begin", true)
            .expect("transaction control must be refused");
        assert!(error.contains("transaction control"));
    }

    /// A `QueryRun` is what both the first render and every reload read, so it
    /// must keep the result apart from the failure rather than merging them.
    #[test]
    fn a_query_run_separates_the_result_from_the_failure() {
        let mut run = QueryRun::default();
        run.absorb(Output::Error {
            id: None,
            error_code: error_code::INVALID_REQUEST.to_string(),
            error: "connection refused".to_string(),
            sqlstate: None,
            message: None,
            detail: None,
            hint: None,
            retryable: false,
            trace: crate::types::Trace::only_duration(1),
        });
        assert_eq!(run.error.as_deref(), Some("connection refused"));
        assert!(run.table.is_none());
        assert_eq!(run.other.len(), 1);

        run.absorb(Output::Result {
            id: None,
            session: None,
            command_tag: "SELECT".to_string(),
            columns: columns(),
            rows: vec![serde_json::json!({"column": "id", "notes": Value::Null})],
            row_count: 1,
            truncated: false,
            truncated_at_rows: None,
            truncated_at_bytes: None,
            trace: crate::types::Trace::only_duration(1),
        });
        assert_eq!(run.table.map(|(_, rows)| rows.len()), Some(1));
    }
}
