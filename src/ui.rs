//! Windows onto PostgreSQL, in AFUI's two shapes.
//!
//! `ui schema`, `ui table`, `ui indexes` and `ui connections` are *watch*
//! sessions: afpsql opens a window onto something a person reads, nothing on
//! the page submits, and the session ends when they close it. Their value type
//! is `()` and `Outcome::Completed` never occurs. `ui connections` is live: it
//! receives fresh server snapshots while the page can send an `EXPLAIN`
//! inspection back over the same AFUI session runtime.
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
use std::time::Duration;

use agent_first_data::OutputFormat;
use agent_first_ui::{
    Outcome, UiCall, UiCallError, UiCspNonce, UiDeliveryFacts, UiDeliveryPlan, UiPagePolicy,
    UiPageScript, UiSession, UiSessionRuntime,
};
use axum::Router;
use axum::extract::State;
use axum::response::Html;
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio::task::JoinSet;

mod frontend;
mod inspect;

use frontend::{FrontendFailure, PanelFrontend, PanelShape};
use inspect::inspect_document;

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
/// Images and fonts come from this session's own origin because a frontend may
/// ship assets, and AFUI serves them from that same origin under this same
/// credential. Same origin, same credential, no network.
fn watch_page_policy() -> UiPagePolicy {
    UiPagePolicy::new(UiPageScript::None)
        .allow_images()
        .allow_fonts()
}

/// The live monitor loads one nonce-bound Provider runtime and connects only
/// to the channel under its own credential. Static inspection panels keep the
/// script-free policy above.
fn monitor_page_policy(nonce: &UiCspNonce) -> UiPagePolicy {
    watch_page_policy()
        .with_script(UiPageScript::Nonce(nonce.clone()))
        .allow_runtime()
}

/// The decide panel differs from a watch panel in exactly two switches, and
/// both are what make its answer trustworthy rather than what weaken it.
///
/// The nonce admits precisely one script — afpsql's decision runtime, spliced
/// in at the layout marker with this session's nonce. A frontend cannot supply
/// a script file (AFUI refuses one by name) or hide one in a template (refused
/// by content), and could not run it if it did: it does not know the nonce.
/// Form submission is where the answer goes.
fn plan_page_policy(nonce: &UiCspNonce) -> UiPagePolicy {
    watch_page_policy()
        .with_script(UiPageScript::Nonce(nonce.clone()))
        .allow_form_submission()
}

const PLAN_UI_KIND: &str = "plan_confirm";

/// How many backends the "longest running" table lists before it stops.
///
/// A monitor is read at a glance; the full activity table below it holds every
/// row, so nothing is hidden by this cut.
const LONGEST_ROWS: usize = 10;

/// A live inspection request is still an interactive control, not a bulk SQL
/// transport. Larger statements use the ordinary CLI or pipe protocol.
const MAX_MONITOR_EXPLAIN_SQL_BYTES: usize = 64 * 1024;

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
        delivery,
    } = request;

    // Not "opening a window is a host capability" — that reasoning only ever
    // covered the window it was written for, and it would have waved a
    // published session through instead of catching it: a published session
    // spawns no browser and writes no profile directory. What a locked
    // profile has to withhold is where the data behind a panel can end up.
    // A window puts it on the screen of whoever is running afpsql; a
    // published session puts it in front of anyone holding the
    // `afui session serve` URL — a wider exit for the same rows than
    // `--stdout-file`, which a locked profile already refuses. Judged that
    // way, every delivery a panel could ever gain is refused for the same
    // reason, so this check does not grow a case per delivery; it holds for
    // every panel, including the one that would otherwise only ever read.
    if let Some((message, hint)) = locked_profile_panel_refusal(locked_readonly_profile) {
        fail(error_code::INVALID_REQUEST, message, Some(hint), output);
    }

    // Resolved once, before any panel or query work, for the same reason the
    // refusal above runs first: a bad `AFUI_DELIVERY` value or a resolved
    // `link` should end the process before a connection is even opened, not
    // after a person was already told a panel is on its way.
    //
    // `UI_DELIVERY` is `Local`, which is where afpsql declines `link` for a
    // database panel — and it is the same value the `--mode` flag is built
    // from, so the enum a person reads and the plan they get cannot disagree.
    // The reason belongs here: a `link` panel binds every interface and
    // advertises the result on the network with no tunnel — a wider,
    // unencrypted exit for the same rows `--stdout-file` already loses under a
    // locked profile (see `locked_profile_panel_refusal`). `window` never
    // leaves this screen; `session` stays on loopback and is reached only
    // through afui's own registry. AFUI refuses the resolved mode here and
    // names the two that are on offer.
    let plan = match crate::cli::UI_DELIVERY.resolve(delivery) {
        Ok(plan) => plan,
        Err(error) => fail(
            error_code::INVALID_REQUEST,
            &error.to_string(),
            Some(DELIVERY_MODE_HINT),
            output,
        ),
    };

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
        UiPanel::View(view) => view_panel(app, rx, view, &session, output, plan).await,
        UiPanel::Plan(statement) => plan_panel(app, rx, statement, session, output, plan).await,
    }
}

/// One readiness event: what afpsql knows about this panel, spliced together
/// with what AFUI knows about the delivery reaching a person.
///
/// The delivery facts are taken rather than restated, so `mode`, `session_id`
/// and the attention intervals mean the same thing here as in any other tool
/// AFUI delivers for, under the same names.
fn ready_event(facts: &UiDeliveryFacts, event: Value) -> Value {
    // No link URL to reveal: `UI_DELIVERY` is `Local`, so a panel here never
    // has one, and the plain merge is the whole job.
    agent_first_ui::cli::ready_event(facts, event)
}

/// A panel a person only reads: no submission, no answer, and therefore no
/// outcome but "they are done looking".
///
/// The nine composition steps this used to spell out — session, subject, icon,
/// policy, stylesheet route, assets nest, page route, delivery, facts — are
/// `UiSurface`'s now. What is left is which of them afpsql has an opinion
/// about.
fn watch_surface(
    ui_kind: &'static str,
    subject: &str,
    app_icon: agent_first_ui::UiAppIcon,
) -> agent_first_ui::UiSurface<()> {
    // `UiSession::new` validates the two identifiers, and both are compile-time
    // constants here, so this cannot fail in a way a person could cause.
    #[allow(clippy::expect_used)]
    agent_first_ui::UiSurface::<()>::new("afpsql", ui_kind)
        .expect("afpsql's own provider and ui_kind identifiers are valid")
        .with_subject(subject)
        .with_app_icon(app_icon)
        .with_page_policy(watch_page_policy())
}

/// What a `--mode` refusal tells a person to do instead. Both deliveries
/// afpsql offers are named, because AFUI's own refusal names the modes but
/// not the flag they belong to.
const DELIVERY_MODE_HINT: &str = "use `--mode window` or `--mode session`";

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

/// Whether an administrator-locked readonly profile refuses this panel, and
/// what to say about it.
///
/// Deliberately blind to which panel was asked for and to how it would be
/// delivered: the judgement is about where data can leave this process, not
/// about what a particular delivery costs to open, so nothing about *this*
/// panel or *this* delivery belongs in the condition. A future delivery
/// option therefore inherits the refusal by construction rather than needing
/// a new arm here.
fn locked_profile_panel_refusal(
    locked_readonly_profile: bool,
) -> Option<(&'static str, &'static str)> {
    if !locked_readonly_profile {
        return None;
    }
    Some((
        "a locked readonly profile has no exit for what the database holds, so ui panels are unavailable",
        "read the same data with the matching `afpsql inspect` command",
    ))
}

/// A hint for a delivery failure, named for the axis rather than for
/// `window` alone: [`UiDeliveryPlan::resolve`] and [`UiDeliveryPlan::deliver`]
/// can both fail for either delivery this spore offers, and every call site
/// passes their error here regardless of which one it was.
fn delivery_hint(error: &agent_first_ui::Error) -> Option<&'static str> {
    // Keyed on AFUI's classification rather than on one variant, so a window
    // that could not launch gets the same way out as one that was never found.
    // afpsql's own alternative replaces AFUI's: the useful thing to say here
    // is not only "install a browser" but "you do not need one for this".
    if error.kind() == agent_first_ui::UiErrorKind::WindowUnavailable {
        return Some(
            "install a Chromium-family browser, or read the same data with the matching \
             `afpsql inspect` command",
        );
    }
    error.hint()
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

/// The built-in page a view panel starts from.
///
/// One shape per `ui_kind` rather than one per *behaviour*: `ui table` and
/// `ui schema` read the same query and neither reloads, yet they are two things
/// to look at, so they are two pages a person can replace independently.
fn panel_shape(view: &UiView) -> PanelShape {
    match view {
        UiView::Table { .. } => PanelShape::Table,
        UiView::Schema { .. } => PanelShape::Schema,
        UiView::Indexes { .. } => PanelShape::Index,
        UiView::Connections { .. } => PanelShape::Monitor,
    }
}

async fn view_panel(
    app: Arc<App>,
    rx: mpsc::Receiver<Output>,
    view: UiView,
    session: &SessionConfig,
    output: OutputFormat,
    plan: UiDeliveryPlan,
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
    let frontend = match PanelFrontend::resolve(view.ui_kind(), panel_shape(&view)) {
        Ok(frontend) => Arc::new(frontend),
        Err(failure) => fail_frontend(failure, output),
    };
    match view.refresh_seconds() {
        Some(seconds) => {
            live_panel(
                app, rx, view, sql, params, subject, seconds, frontend, output, plan,
            )
            .await
        }
        None => static_panel(app, rx, view, sql, params, subject, frontend, output, plan).await,
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
    plan: UiDeliveryPlan,
) -> ! {
    let mode = plan.mode();
    let app_icon = match frontend.app_icon() {
        Ok(app_icon) => app_icon,
        Err(failure) => fail_frontend(failure, output),
    };
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

    // Reachable first, then announced: `start` returns at exactly the point
    // where every delivery fact is known and nobody is waiting on the person
    // yet, so the readiness event describes a panel that is already there.
    let active = match watch_surface(view.ui_kind(), &subject, app_icon)
        .start(plan, page, stylesheet, frontend.frontend())
        .await
    {
        Ok(active) => active,
        Err(error) => fail(
            "ui_failed",
            &error.to_string(),
            delivery_hint(&error),
            output,
        ),
    };
    emit_event_or_exit(
        agent_first_data::json_progress(ready_event(
            &active.facts(),
            serde_json::json!({
                "phase": "ui_ready",
                "message": format!("The inspection panel is ready: {}.", mode.description()),
                "ui_kind": view.ui_kind(),
                "subject": subject,
                "row_count": rows.len(),
                // Absent when afpsql's own page is serving. A workspace
                // frontend that has not been trusted is deliberately skipped
                // in silence, so this is how an agent tells "my override is
                // running" from "my override is inert" without opening a
                // window to look.
                "ui_frontend_id": frontend.frontend_id(),
            }),
        ))
        .build(),
        output,
    );

    match active.wait().await {
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
        // Both deliveries this panel supports issue `UiExpiry::Never`, so
        // this cannot fire.
        Ok(Outcome::Expired) => fail(
            "ui_expired",
            "the inspection panel credential expired",
            None,
            output,
        ),
        Err(error) => fail(
            "ui_failed",
            &error.to_string(),
            delivery_hint(&error),
            output,
        ),
    }
}

/// One domain action the live surface asks its controller to perform.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum LivePanelAction {
    Observe,
    Explain { sql: String },
}

/// One non-terminal or terminal value belonging to an `EXPLAIN` call.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum LivePanelReply {
    Observed,
    ExplainStarted,
    ExplainResult {
        plan_lines: Vec<String>,
        row_count: usize,
    },
}

/// The complete replaceable state of the monitor surface.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum LivePanelState {
    Snapshot { row_count: usize, page_html: String },
    Error { message: String },
}

type LiveRuntime = UiSessionRuntime<LivePanelAction, LivePanelReply, LivePanelState>;

/// A visible page checks in every ten seconds. Three missed check-ins stop
/// database refreshes, bounding the work after a browser disappears without a
/// clean close while leaving generous room for ordinary timer jitter.
const LIVE_OBSERVATION_LEASE: Duration = Duration::from_secs(30);

#[derive(Default)]
struct LiveObservation {
    active_until: Option<tokio::time::Instant>,
}

impl LiveObservation {
    fn observe(&mut self, now: tokio::time::Instant) {
        self.active_until = Some(now + LIVE_OBSERVATION_LEASE);
    }

    fn active(&self, now: tokio::time::Instant) -> bool {
        self.active_until.is_some_and(|deadline| now <= deadline)
    }
}

/// A panel over something that moves, and the state its controller updates.
///
/// The page route serves the most recent complete render. While it is open, the
/// controller re-runs the same inspection query and replaces AFUI's retained
/// state; an independent call can travel back over the runtime without turning
/// either direction into a page reload.
struct LivePanel {
    app: Arc<App>,
    /// One query at a time. A refresh and its drain are one unit — an EXPLAIN
    /// arriving mid-drain must not take the refresh's events.
    rx: Mutex<mpsc::Receiver<Output>>,
    /// EXPLAIN has its own database executor and output stream. A slow plan
    /// must not stop the monitor query whose changing result the person is
    /// looking at while they ask for it.
    explain_app: Arc<App>,
    explain_rx: Mutex<mpsc::Receiver<Output>>,
    sql: String,
    params: Vec<Value>,
    subject: String,
    refresh_seconds: u64,
    title: &'static str,
    frontend: Arc<PanelFrontend>,
    nonce: UiCspNonce,
    page: RwLock<String>,
    updates: AtomicU64,
    last_row_count: AtomicUsize,
}

impl LivePanel {
    async fn page(&self) -> Html<String> {
        Html(self.page.read().await.clone())
    }

    async fn push_snapshot(&self, runtime: &LiveRuntime) -> bool {
        let run = {
            let mut rx = self.rx.lock().await;
            run_query_once(&self.app, &self.sql, &self.params, &mut rx).await
        };
        self.updates.fetch_add(1, Ordering::Relaxed);
        // Whatever else this refresh emitted stays in the window. A monitor
        // left open all afternoon would otherwise put one transport log line
        // per refresh into a stream its caller stopped reading hours ago.
        let (columns, rows) = run.table.unwrap_or_default();
        self.last_row_count.store(rows.len(), Ordering::Relaxed);
        let document = monitor_document(self, &columns, &rows, run.error.as_deref());
        let page = match self.frontend.render_page(&document, Some(&self.nonce)) {
            Ok(page) => page,
            Err(failure) => {
                return runtime
                    .publish_state(&LivePanelState::Error {
                        message: failure.message,
                    })
                    .is_ok();
            }
        };
        *self.page.write().await = page.clone();
        publish_live_state(
            runtime,
            LivePanelState::Snapshot {
                row_count: rows.len(),
                page_html: page,
            },
        )
    }

    async fn explain(&self, call: UiCall<LivePanelAction, LivePanelReply>, sql: String) -> bool {
        if sql.trim().is_empty() || sql.len() > MAX_MONITOR_EXPLAIN_SQL_BYTES {
            let message = if sql.trim().is_empty() {
                "SQL must not be empty".to_string()
            } else {
                format!(
                    "SQL is {} bytes, exceeding the {}-byte live inspection limit",
                    sql.len(),
                    MAX_MONITOR_EXPLAIN_SQL_BYTES
                )
            };
            return call
                .fail(UiCallError::new("explain_request_invalid", message))
                .await
                .is_ok();
        }
        if call
            .progress(&LivePanelReply::ExplainStarted)
            .await
            .is_err()
        {
            return false;
        }
        let explain_sql = crate::cli::wrap_explain_text_sql(&sql);
        let run = {
            let mut rx = self.explain_rx.lock().await;
            run_query_once(&self.explain_app, &explain_sql, &[], &mut rx).await
        };
        if let Some(message) = run.error {
            return call
                .fail(UiCallError::new("explain_failed", message))
                .await
                .is_ok();
        }
        let Some((columns, rows)) = run.table else {
            return call
                .fail(UiCallError::new(
                    "explain_failed",
                    "PostgreSQL returned no EXPLAIN result set",
                ))
                .await
                .is_ok();
        };
        let Some(plan_column) = columns.first() else {
            return call
                .fail(UiCallError::new(
                    "explain_failed",
                    "PostgreSQL returned an empty EXPLAIN result",
                ))
                .await
                .is_ok();
        };
        let plan_lines = rows
            .iter()
            .map(|row| row.get(&plan_column.name).map(scalar).unwrap_or_default())
            .collect::<Vec<_>>();
        let row_count = plan_lines.len();
        call.finish(&LivePanelReply::ExplainResult {
            plan_lines,
            row_count,
        })
        .await
        .is_ok()
    }
}

fn publish_live_state(runtime: &LiveRuntime, state: LivePanelState) -> bool {
    match runtime.publish_state(&state) {
        Ok(_) => true,
        Err(agent_first_ui::Error::SessionRuntimeMessageTooLarge { .. }) => runtime
            .publish_state(&LivePanelState::Error {
                message: "The live update is too large for this session; narrow the inspection and try again."
                    .to_string(),
            })
            .is_ok(),
        Err(_) => false,
    }
}

async fn run_live_runtime(panel: Arc<LivePanel>, runtime: LiveRuntime) {
    let runtime = Arc::new(runtime);
    let mut explain_tasks = JoinSet::new();
    let mut observation = LiveObservation::default();
    let mut refresh = tokio::time::interval(Duration::from_secs(panel.refresh_seconds.max(1)));
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // The opening page is already the first snapshot; consume the immediate
    // tick so the next query happens after the requested interval.
    refresh.tick().await;
    loop {
        tokio::select! {
            _ = refresh.tick() => {
                if observation.active(tokio::time::Instant::now())
                    && !panel.push_snapshot(runtime.as_ref()).await
                {
                    return;
                }
            }
            call = runtime.recv() => {
                match call {
                    Ok(Some(call)) => {
                        match call.action() {
                            LivePanelAction::Observe => {
                                observation.observe(tokio::time::Instant::now());
                                if call.finish(&LivePanelReply::Observed).await.is_err() {
                                    return;
                                }
                            }
                            LivePanelAction::Explain { sql } => {
                                observation.observe(tokio::time::Instant::now());
                                let sql = sql.clone();
                                if explain_tasks.is_empty() {
                                    let panel = Arc::clone(&panel);
                                    explain_tasks.spawn(async move {
                                        let _sent = panel.explain(call, sql).await;
                                    });
                                } else if call
                                    .fail(UiCallError::new(
                                        "explain_busy",
                                        "Wait for the current plan before asking for another.",
                                    ))
                                    .await
                                    .is_err()
                                {
                                    return;
                                }
                            }
                        }
                    }
                    Ok(None) => return,
                    Err(_) => return,
                }
            }
            Some(_finished) = explain_tasks.join_next(), if !explain_tasks.is_empty() => {}
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
    plan: UiDeliveryPlan,
) -> ! {
    // One run before anything is drawn, for the same reason a static panel
    // drains first: a server that refuses the connection must reach the agent
    // as an error event rather than as a window that says so to nobody. The
    // page the browser then loads starts from exactly this successful run.
    let opening = run_query_once(&app, &sql, &params, &mut rx).await;
    opening.report_to_agent(output);
    let Some((columns, rows)) = opening.table else {
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
    let app_icon = match frontend.app_icon() {
        Ok(app_icon) => app_icon,
        Err(failure) => fail_frontend(failure, output),
    };
    let nonce = match UiCspNonce::generate() {
        Ok(nonce) => nonce,
        Err(error) => fail("ui_session_invalid", &error.to_string(), None, output),
    };
    let explain_config = app.config.read().await.clone();
    let (explain_tx, explain_rx) = mpsc::channel::<Output>(OUTPUT_CHANNEL_CAPACITY);
    let explain_app = Arc::new(App::new(explain_config, explain_tx, app.capability));
    explain_app.locked_readonly_profile.store(
        app.locked_readonly_profile.load(Ordering::Relaxed),
        Ordering::Relaxed,
    );
    let panel = Arc::new(LivePanel {
        app,
        rx: Mutex::new(rx),
        explain_app,
        explain_rx: Mutex::new(explain_rx),
        sql,
        params,
        subject: subject.clone(),
        refresh_seconds,
        title: view.title(),
        frontend: Arc::clone(&frontend),
        nonce: nonce.clone(),
        page: RwLock::new(String::new()),
        updates: AtomicU64::new(0),
        last_row_count: AtomicUsize::new(rows.len()),
    });
    // Draw the opening snapshot before the window exists, purely so a frontend
    // that will not render is an error rather than a window full of it.
    let opening_document = monitor_document(&panel, &columns, &rows, None);
    let opening_page = match frontend.render_page(&opening_document, Some(&nonce)) {
        Ok(page) => page,
        Err(failure) => fail_frontend(failure, output),
    };
    *panel.page.write().await = opening_page.clone();
    let router = Router::new()
        .route(
            "/",
            get(|State(panel): State<Arc<LivePanel>>| async move { panel.page().await }),
        )
        .with_state(Arc::clone(&panel))
        .merge(agent_first_ui::page_asset_routes(
            stylesheet,
            frontend.frontend(),
        ));

    let session = match UiSession::<()>::new("afpsql", view.ui_kind()) {
        Ok(session) => session
            .with_subject(subject.clone())
            .with_app_icon(app_icon)
            .with_security_policy(monitor_page_policy(&nonce).into_security_policy()),
        Err(error) => fail("ui_session_invalid", &error.to_string(), None, output),
    };
    let (session, runtime) =
        match session.with_runtime::<LivePanelAction, LivePanelReply, LivePanelState>() {
            Ok(runtime) => runtime,
            Err(error) => fail("ui_session_invalid", &error.to_string(), None, output),
        };
    if !publish_live_state(
        &runtime,
        LivePanelState::Snapshot {
            row_count: rows.len(),
            page_html: opening_page,
        },
    ) {
        fail(
            "ui_runtime_failed",
            "could not publish the connection monitor's opening state",
            None,
            output,
        );
    }

    let mode = plan.mode();
    let active = match plan.start(session, router).await {
        Ok(active) => active,
        Err(error) => fail(
            "ui_failed",
            &error.to_string(),
            delivery_hint(&error),
            output,
        ),
    };

    emit_event_or_exit(
        agent_first_data::json_progress(ready_event(
            &active.facts(),
            serde_json::json!({
                "phase": "ui_ready",
                "message": format!(
                    "The connection monitor is ready and updates live: {}.",
                    mode.description()
                ),
                "ui_kind": view.ui_kind(),
                "subject": subject,
                "refresh_seconds": refresh_seconds,
                "row_count": rows.len(),
                "ui_frontend_id": frontend.frontend_id(),
            }),
        ))
        .build(),
        output,
    );

    let runtime_task = tokio::spawn(run_live_runtime(Arc::clone(&panel), runtime));
    let outcome = active.wait().await;
    runtime_task.abort();
    let _ignored = runtime_task.await;
    match outcome {
        Ok(Outcome::Closed | Outcome::Completed(())) => {
            // This panel held a connection open for as long as the window was,
            // so it closes it rather than letting the exit tear it down. A view
            // of somebody's connection count should not leave one behind.
            panel.app.executor.shutdown().await;
            panel.explain_app.executor.shutdown().await;
            let update_count = panel.updates.load(Ordering::Relaxed);
            emit_event_or_exit(
                agent_first_data::json_result(serde_json::json!({
                    "ui_kind": view.ui_kind(),
                    "subject": subject,
                    "refresh_seconds": refresh_seconds,
                    // What the person actually watched: how many snapshots the
                    // controller pushed, and what the last one showed.
                    "update_count": update_count,
                    // Compatibility for callers of the former meta-refresh
                    // monitor. New callers should read `update_count`; keeping
                    // this alias avoids turning a presentation transport change
                    // into an unrelated AFDATA field removal.
                    "reload_count": update_count,
                    "row_count": panel.last_row_count.load(Ordering::Relaxed),
                    "closed": true,
                }))
                .build(),
                output,
            );
            std::process::exit(0);
        }
        // Both deliveries this panel supports issue `UiExpiry::Never`, so
        // this cannot fire.
        Ok(Outcome::Expired) => fail(
            "ui_expired",
            "the connection monitor credential expired",
            None,
            output,
        ),
        Err(error) => fail(
            "ui_failed",
            &error.to_string(),
            delivery_hint(&error),
            output,
        ),
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
///
/// Only the decided endings are afpsql's to name. Every other way a UI stops
/// — a person closing it, a credential lapsing — means the same thing in
/// every tool, so those words come from AFUI rather than being respelled per
/// delivery here.
fn ending_of(outcome: &Outcome<PlanDecision>) -> &'static str {
    match outcome {
        Outcome::Completed(PlanDecision::Approve) => "approved",
        Outcome::Completed(PlanDecision::Refuse) => "refused",
        ended => ended.ending(),
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
    statement: UiPlan,
    session: SessionConfig,
    output: OutputFormat,
    plan: UiDeliveryPlan,
) -> ! {
    let mode = plan.mode();
    let UiPlan {
        sql,
        params,
        options,
    } = statement;

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
    let app_icon = match frontend.app_icon() {
        Ok(app_icon) => app_icon,
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
            .with_app_icon(app_icon)
            .with_security_policy(plan_page_policy(&nonce).into_security_policy()),
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
        .merge(agent_first_ui::page_asset_routes(
            stylesheet,
            frontend.frontend(),
        ));

    let active = match plan.start(ui_session, router).await {
        Ok(active) => active,
        Err(error) => fail(
            "ui_failed",
            &error.to_string(),
            delivery_hint(&error),
            output,
        ),
    };
    emit_event_or_exit(
        agent_first_data::json_progress(ready_event(
            &active.facts(),
            serde_json::json!({
                "phase": "ui_ready",
                "message": format!(
                    "A statement is waiting for a person to approve it — {}. Closing it refuses \
                     the statement.",
                    mode.description()
                ),
                "ui_kind": PLAN_UI_KIND,
                "subject": subject,
                "target": facts.target,
                "read_only_transaction": facts.read_only,
                "writes": facts.writes(),
                "param_count": facts.params.len(),
                "prepared": facts.prepared.summary(),
                "ui_frontend_id": frontend.frontend_id(),
            }),
        ))
        .build(),
        output,
    );

    let outcome = match active.wait().await {
        Ok(outcome) => outcome,
        Err(error) => fail(
            "ui_failed",
            &error.to_string(),
            delivery_hint(&error),
            output,
        ),
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
        footer: "",
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
    /// The failure of *this* update, if it failed. A monitor that blanks itself
    /// the first time a server hiccups is worse than one that says which update
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
        let _ = write!(
            footer,
            "Updates every {}s while a page is visible.",
            panel.refresh_seconds
        );
    } else {
        let _ = write!(
            footer,
            "Server time {snapshot_at}. Updates every {}s while a page is visible.",
            panel.refresh_seconds
        );
    }
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

    /// How often this panel pushes a snapshot, when it is live at all.
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
        PanelFrontend::builtin(view.ui_kind(), panel_shape(view))
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

    fn named_columns(names: &[(&str, &str)]) -> Vec<ColumnInfo> {
        names
            .iter()
            .map(|(name, type_name)| ColumnInfo {
                name: (*name).to_string(),
                type_name: (*type_name).to_string(),
            })
            .collect()
    }

    /// The generic columns every schema-snapshot row arrives in.
    fn snapshot_columns() -> Vec<ColumnInfo> {
        named_columns(&[
            ("kind", "text"),
            ("schema", "text"),
            ("relation", "text"),
            ("name", "text"),
            ("object_type", "text"),
            ("position", "int4"),
            ("definition", "text"),
            ("size_bytes", "int8"),
            ("size", "text"),
            ("estimated_rows", "int8"),
            ("payload", "jsonb"),
        ])
    }

    /// One row of the snapshot query, with the generic columns a given kind
    /// leaves null actually left null — the shape this panel exists to unpack.
    fn snapshot_row(kind: &str, name: &str, object_type: &str, payload: Value) -> Value {
        serde_json::json!({
            "kind": kind,
            "schema": "public",
            "relation": "orders",
            "name": name,
            "object_type": object_type,
            "position": Value::Null,
            "definition": Value::Null,
            "size_bytes": Value::Null,
            "size": Value::Null,
            "estimated_rows": Value::Null,
            "payload": payload,
        })
    }

    /// A table with an identity primary key, one nullable column, and the
    /// PostgreSQL 17 `NOT NULL` constraint row that restates the first.
    fn table_snapshot() -> Vec<Value> {
        let mut relation = snapshot_row(
            "relation",
            "orders",
            "table",
            serde_json::json!({
                "owner": "app",
                "persistence": "p",
                "comment": "customer orders",
            }),
        );
        relation["size"] = Value::from("16 kB");
        relation["size_bytes"] = Value::from(16384);
        // Never analyzed. `-1` is not a row count, and a panel that shows it as
        // one is the reason this document exists.
        relation["estimated_rows"] = Value::from(-1);

        let mut id = snapshot_row(
            "column",
            "id",
            "integer",
            serde_json::json!({
                "nullable": false,
                "primary_key": true,
                "identity": "a",
                "generated": "",
                "serial_sequence": Value::Null,
                "comment": Value::Null,
            }),
        );
        id["position"] = Value::from(1);

        let mut label = snapshot_row(
            "column",
            "label",
            "text",
            serde_json::json!({
                "nullable": true,
                "primary_key": false,
                "identity": "",
                "generated": "",
                "serial_sequence": Value::Null,
                "comment": Value::Null,
            }),
        );
        label["position"] = Value::from(2);
        label["definition"] = Value::from("'none'::text");

        let mut not_null = snapshot_row(
            "constraint",
            "orders_id_not_null",
            "n",
            serde_json::json!({
                "type": "n",
                "deferrable": false,
                "deferred_by_default": false,
                "validated": true,
            }),
        );
        not_null["definition"] = Value::from("NOT NULL id");

        let mut primary_key = snapshot_row(
            "constraint",
            "orders_pkey",
            "primary key",
            serde_json::json!({
                "type": "p",
                "deferrable": false,
                "deferred_by_default": false,
                "validated": true,
            }),
        );
        primary_key["definition"] = Value::from("PRIMARY KEY (id)");

        let mut index = snapshot_row(
            "index",
            "orders_pkey",
            "btree",
            serde_json::json!({
                "primary": true,
                "ready": true,
                "unique": true,
                "valid": true,
            }),
        );
        index["definition"] =
            Value::from("CREATE UNIQUE INDEX orders_pkey ON public.orders USING btree (id)");
        index["size"] = Value::from("8192 bytes");

        vec![relation, id, label, not_null, primary_key, index]
    }

    fn table_view() -> UiView {
        UiView::Table {
            name: "public.orders".to_string(),
        }
    }

    /// The page down to where the unaltered result set begins.
    ///
    /// Every claim about what a person no longer has to decode belongs to this
    /// part: the raw grid below it still shows the payload, the byte counts and
    /// the catalog codes verbatim, and is meant to.
    fn summarized_part(page: &str) -> &str {
        page.split("<details class=\"raw\">")
            .next()
            .expect("a page always has a part before its raw table")
    }

    /// The whole point of the inspection redesign: a person reads an object,
    /// not the union of eight object kinds the query returns.
    #[test]
    fn a_table_panel_reads_as_one_object_rather_than_as_the_rows_it_arrived_in() {
        let page = inspect_page(
            &table_view(),
            "public.orders",
            &snapshot_columns(),
            &table_snapshot(),
        );

        // The card says what the object is, in words rather than catalog codes.
        assert!(page.contains("<h1>Table</h1>"), "{page}");
        assert!(page.contains("<dt>owner</dt><dd>app</dd>"), "{page}");
        assert!(page.contains("<dt>size</dt><dd>16 kB</dd>"), "{page}");
        assert!(page.contains("customer orders"), "{page}");
        // `-1` is PostgreSQL for "never analyzed", and saying it as a number
        // would be a lie about the table rather than a fact about the catalog.
        assert!(page.contains("unknown — never analyzed"), "{page}");
        assert!(!page.contains("<dd>-1</dd>"), "{page}");
        // `persistence: "p"` is the default and carries no information; a code
        // that does — `u`, `t` — is the one that gets a line.
        assert!(!page.contains(">p<"), "{page}");

        // Every fact a person came for is out of the payload JSON and into the
        // column it belongs to.
        assert!(page.contains("<h3>Columns</h3>"), "{page}");
        assert!(page.contains("not null"), "{page}");
        assert!(page.contains("primary key"), "{page}");
        assert!(page.contains("identity, always"), "{page}");
        assert!(page.contains("&#x27;none&#x27;::text"), "{page}");
        // …and nowhere above the raw result set is a person asked to read JSON
        // to find out. Asserted on the summarized part alone, because the raw
        // grid at the bottom is *supposed* to still show the payload verbatim.
        let summarized = summarized_part(&page);
        assert!(!summarized.contains("primary_key"), "{summarized}");
        assert!(!summarized.contains("&quot;"), "{summarized}");
        assert!(!page.contains("$serde_json"), "{page}");

        // The one constraint row that only restates a column's own `not null`
        // is dropped; the one that says something new is kept.
        assert!(page.contains("orders_pkey"), "{page}");
        assert!(page.contains("PRIMARY KEY (id)"), "{page}");
        assert!(!summarized.contains("orders_id_not_null"), "{summarized}");

        // An index's definition is the only place its columns appear.
        assert!(page.contains("<h3>Indexes</h3>"), "{page}");
        assert!(page.contains("USING btree (id)"), "{page}");
        assert!(page.contains("8192 bytes"), "{page}");

        // The footer counts objects, not the rows the union happened to need.
        assert!(
            page.contains("2 columns · 1 constraint · 1 index"),
            "{page}"
        );
        assert!(!page.contains("6 row(s)"), "{page}");

        // Nothing summarized is thereby hidden: the exact result set the agent
        // read is still on the page.
        assert!(page.contains("Every row afpsql read (6)"), "{page}");
        assert!(page.contains("<tbody>"), "{page}");
    }

    /// A panel pointed at a view is not a table, and does not say it is.
    #[test]
    fn a_view_is_named_for_what_it_is_and_shows_the_query_behind_it() {
        let mut view_row = snapshot_row(
            "relation",
            "recent",
            "view",
            serde_json::json!({
                "owner": "app",
                "persistence": "p",
                "comment": Value::Null,
            }),
        );
        view_row["relation"] = Value::from("recent");
        view_row["definition"] = Value::from("SELECT id FROM orders WHERE id > 100");
        let page = inspect_page(
            &UiView::Table {
                name: "public.recent".to_string(),
            },
            "public.recent",
            &snapshot_columns(),
            &[view_row],
        );
        assert!(page.contains("<h1>View</h1>"), "{page}");
        // The window's own label agrees with its heading rather than insisting
        // on the verb that was typed.
        assert!(
            page.contains("<title>View — public.recent</title>"),
            "{page}"
        );
        assert!(page.contains("<h3>Definition</h3>"), "{page}");
        assert!(page.contains("SELECT id FROM orders"), "{page}");
        // A view stores nothing, so a size and a row estimate would both be
        // facts about something that does not exist.
        assert!(!page.contains("<dt>size</dt>"), "{page}");
        assert!(!page.contains("<dt>rows</dt>"), "{page}");
    }

    /// A schema panel is a list first and a detail second, because that is the
    /// order a person reads one in.
    #[test]
    fn a_schema_panel_summarizes_each_relation_before_opening_any_of_them() {
        let mut rows = table_snapshot();
        for index in 0..4 {
            let mut extra = snapshot_row(
                "relation",
                &format!("ledger_{index}"),
                "table",
                serde_json::json!({"owner": "app", "persistence": "p", "comment": Value::Null}),
            );
            extra["relation"] = Value::from(format!("ledger_{index}"));
            extra["size"] = Value::from("8192 bytes");
            rows.push(extra);
        }
        rows.push(snapshot_row(
            "extension",
            "pgcrypto",
            "extension",
            serde_json::json!({"version": "1.3"}),
        ));

        let page = inspect_page(
            &UiView::Schema {
                schema: "public".to_string(),
            },
            "public",
            &snapshot_columns(),
            &rows,
        );
        // Each relation says what it holds without being opened.
        assert!(
            page.contains("table · 2 columns · 1 constraint · 1 index · 16 kB"),
            "{page}"
        );
        assert!(page.contains("pgcrypto"), "{page}");
        assert!(page.contains("1.3"), "{page}");
        // Five relations is a wall if they all open at once.
        assert!(page.contains("<details class=\"relation\">"), "{page}");
        assert!(
            !page.contains("<details class=\"relation\" open>"),
            "{page}"
        );
        assert!(page.contains("5 tables · 1 extension"), "{page}");

        // A schema small enough to take in at a glance opens itself.
        let small = inspect_page(
            &UiView::Schema {
                schema: "public".to_string(),
            },
            "public",
            &snapshot_columns(),
            &table_snapshot(),
        );
        assert!(
            small.contains("<details class=\"relation\" open>"),
            "{small}"
        );
    }

    /// `ui indexes` returns one shape of row, so its work is grouping them
    /// under the table each belongs to and saying which ones are not working.
    #[test]
    fn an_index_panel_groups_by_table_and_names_the_states_worth_noticing() {
        let columns = named_columns(&[
            ("schema", "text"),
            ("table", "text"),
            ("name", "text"),
            ("method", "text"),
            ("unique", "bool"),
            ("primary", "bool"),
            ("valid", "bool"),
            ("ready", "bool"),
            ("definition", "text"),
            ("size_bytes", "int8"),
            ("size", "text"),
        ]);
        let index = |table: &str, name: &str, valid: bool| {
            serde_json::json!({
                "schema": "public",
                "table": table,
                "name": name,
                "method": "btree",
                "unique": false,
                "primary": false,
                "valid": valid,
                "ready": true,
                "definition": format!("CREATE INDEX {name} ON public.{table} USING btree (id)"),
                "size_bytes": 8192,
                "size": "8192 bytes",
            })
        };
        let page = inspect_page(
            &UiView::Indexes {
                schema: "public".to_string(),
                table: None,
            },
            "public",
            &columns,
            &[
                index("orders", "orders_id_idx", true),
                index("orders", "orders_total_idx", false),
                index("invoices", "invoices_id_idx", true),
            ],
        );
        assert!(page.contains("<h2>public.orders</h2>"), "{page}");
        assert!(page.contains("<h2>public.invoices</h2>"), "{page}");
        assert!(page.contains("invalid — the planner ignores it"), "{page}");
        assert!(page.contains("3 indexes on 2 tables"), "{page}");
        // A size is stated once, in the unit a person reads it in — the raw
        // byte count stays in the raw table where the agent's copy is.
        assert!(!summarized_part(&page).contains(">8192<"), "{page}");
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
    fn only_the_monitor_has_a_live_refresh_interval() {
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

    /// An object name is a person's to choose, so it reaches the page as text
    /// whatever it says — in the summary a template writes as well as in the
    /// cell it always went through.
    #[test]
    fn object_names_are_escaped_wherever_the_panel_repeats_them() {
        let mut rows = table_snapshot();
        rows[1]["name"] = Value::from("<script>alert(1)</script>");
        let page = inspect_page(&table_view(), "public.orders", &snapshot_columns(), &rows);
        assert!(!page.contains("<script>alert(1)</script>"));
        assert!(page.contains("&lt;script&gt;alert(1)&lt;&#x2f;script&gt;"));
        // A NULL must be visibly distinct from an empty string.
        assert!(page.contains("class=\"null\">NULL</td>"));
    }

    #[test]
    fn the_panel_loads_no_script_and_declares_a_policy_that_forbids_one() {
        let page = inspect_page(
            &UiView::Schema {
                schema: "public".to_string(),
            },
            "public",
            &snapshot_columns(),
            &table_snapshot(),
        );
        assert!(!page.contains("<script"));
        assert!(page.contains("<link rel=\"stylesheet\" href=\"style.css\">"));
        let nonce = UiCspNonce::generate().unwrap();
        let watch = watch_page_policy().header_value();
        let watch = watch.to_str().unwrap();
        let plan = plan_page_policy(&nonce).header_value();
        let plan = plan.to_str().unwrap();
        for policy in [watch, plan] {
            assert!(policy.contains("default-src 'none'"), "{policy}");
            assert!(policy.contains("style-src 'self'"), "{policy}");
        }
        // Only the page that returns an answer may post one back — and only it
        // admits a script, by exact nonce rather than by kind.
        assert!(watch.contains("form-action 'none'"), "{watch}");
        assert!(!watch.contains("script-src"), "{watch}");
        assert!(plan.contains("form-action 'self'"), "{plan}");
        assert!(
            plan.contains(&format!("script-src 'nonce-{}'", nonce.as_str())),
            "{plan}"
        );
    }

    #[test]
    fn every_panel_follows_system_theme_and_keeps_decisions_keyboard_visible() {
        for (kind, shape) in [
            ("table_inspect", PanelShape::Table),
            ("schema_inspect", PanelShape::Schema),
            ("index_inspect", PanelShape::Index),
            ("connection_monitor", PanelShape::Monitor),
            (PLAN_UI_KIND, PanelShape::Decide),
        ] {
            // Both themes and the focus ring come from the stylesheet AFUI
            // serves every session, so what a panel has to get right is
            // linking it — and a panel carrying a second copy of the palette
            // would be one whose appearance could drift from the rest of the
            // machine with nothing to notice.
            let style = PanelFrontend::builtin(kind, shape).stylesheet().unwrap();
            let style = String::from_utf8_lossy(&style);
            assert!(
                !style.contains("color-scheme: light dark") && !style.contains("--afui-focus:"),
                "{kind}: the panel is carrying a second copy of the theme"
            );
        }
        // The one place a panel can forget it: the frame every panel extends.
        let page = inspect_page(
            &UiView::Schema {
                schema: "public".to_string(),
            },
            "public",
            &snapshot_columns(),
            &table_snapshot(),
        );
        assert!(
            page.contains("<link rel=\"stylesheet\" href=\"__afui/base.css\">"),
            "{page}"
        );
        let baseline = agent_first_ui::page_base_style_source();
        assert!(baseline.contains("color-scheme: light dark"));
        assert!(baseline.contains("@media (prefers-color-scheme: dark)"));
        assert!(baseline.contains(":focus-visible"));
    }

    /// Nothing found is three different answers, and a panel that gave all
    /// three the same sentence would send a person looking for the wrong
    /// mistake — a typo in a name, an empty schema, an unindexed table.
    #[test]
    fn an_empty_panel_says_which_kind_of_empty_it_is() {
        let indexes = inspect_page(
            &UiView::Indexes {
                schema: "public".to_string(),
                table: None,
            },
            "public",
            &columns(),
            &[],
        );
        assert!(
            indexes.contains("Nothing in public is indexed."),
            "{indexes}"
        );
        assert!(!indexes.contains("<tbody>"), "{indexes}");

        let table = inspect_page(&table_view(), "public.orders", &snapshot_columns(), &[]);
        assert!(
            table.contains("No table, view or sequence named public.orders exists"),
            "{table}"
        );

        let schema = inspect_page(
            &UiView::Schema {
                schema: "public".to_string(),
            },
            "public",
            &snapshot_columns(),
            &[],
        );
        assert!(
            schema.contains("The schema public holds no objects."),
            "{schema}"
        );
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
        let nonce = UiCspNonce::generate().expect("monitor nonce");
        let (writer, rx) = mpsc::channel::<Output>(1);
        let (explain_writer, explain_rx) = mpsc::channel::<Output>(1);
        let panel = LivePanel {
            app: Arc::new(App::new(
                RuntimeConfig::default(),
                writer,
                crate::Capability::ReadOnly,
            )),
            rx: Mutex::new(rx),
            explain_app: Arc::new(App::new(
                RuntimeConfig::default(),
                explain_writer,
                crate::Capability::ReadOnly,
            )),
            explain_rx: Mutex::new(explain_rx),
            sql: String::new(),
            params: Vec::new(),
            subject: "127.0.0.1:5432/orders".to_string(),
            refresh_seconds: 5,
            title: "Connections",
            frontend: Arc::new(PanelFrontend::builtin(
                "connection_monitor",
                PanelShape::Monitor,
            )),
            nonce,
            page: RwLock::new(String::new()),
            updates: AtomicU64::new(0),
            last_row_count: AtomicUsize::new(0),
        };
        panel
            .frontend
            .render_page(
                &monitor_document(&panel, &activity_columns(), rows, error),
                Some(&panel.nonce),
            )
            .expect("afpsql's own connection monitor renders")
    }

    /// The rule this panel exists for: fresh state and a request can travel in
    /// opposite directions without either one turning into a page navigation.
    #[test]
    fn the_monitor_declares_and_binds_its_full_duplex_surface() {
        let page = monitor_page(&[backend(11, "orders", "app", "active", 3)], None);
        assert!(!page.contains("http-equiv=\"refresh\""));
        assert!(page.contains("data-afpsql-monitor-snapshot"));
        assert!(page.contains("data-afpsql-explain-form"));
        assert!(page.contains("data-afpsql-live-status"));
        assert!(page.contains("data-afpsql-explain-status"));
        assert!(page.contains("aria-keyshortcuts=\"Meta+Enter Control+Enter\""));
        assert!(page.contains("<details class=\"raw\"><summary>All backends (1)"));
        assert!(page.contains("afui.connect"));
        assert!(page.find("data-afpsql-explain-panel") < page.find("Longest running"));
        assert!(page.contains("Updates every 5s while a page is visible."));
        assert!(page.contains("Server time 2026-08-06 09:41:02.123 UTC"));
    }

    #[test]
    fn the_monitor_protocol_offers_plain_explain_but_not_analyze() {
        let request: LivePanelAction = serde_json::from_value(serde_json::json!({
            "type": "explain",
            "sql": "select * from orders",
        }))
        .expect("plain EXPLAIN request parses");
        let LivePanelAction::Explain { sql } = request else {
            panic!("explain action");
        };
        let wrapped = crate::cli::wrap_explain_text_sql(&sql);
        assert_eq!(wrapped, "explain (format text) select * from orders");
        assert!(!wrapped.to_ascii_lowercase().contains("analyze"));

        assert!(
            serde_json::from_value::<LivePanelAction>(serde_json::json!({
                "type": "explain",
                "sql": "select * from orders",
                "analyze": true,
            }))
            .is_err(),
            "the live surface must not grow an ANALYZE switch"
        );
    }

    #[test]
    fn the_monitor_refresh_lease_requires_a_recent_observer() {
        let now = tokio::time::Instant::now();
        let mut observation = LiveObservation::default();
        assert!(!observation.active(now));
        observation.observe(now);
        assert!(observation.active(now + Duration::from_secs(30)));
        assert!(!observation.active(now + Duration::from_secs(31)));
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
    fn a_failed_update_is_shown_and_the_runtime_keeps_the_page_live() {
        let page = monitor_page(&[], Some("connection refused"));
        assert!(page.contains("This update failed: connection refused"));
        assert!(page.contains("afui.connect"));
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

    /// The declaration vocabulary a decide page must carry is `UiPage`'s to
    /// check now, on the parse tree it already built. What is still afpsql's
    /// is that its runtime and its own page agree on that vocabulary: a
    /// runtime binding a word no page declares is a control that never
    /// appears, and a page declaring a word no runtime binds does nothing.
    #[test]
    fn the_built_in_decide_page_declares_exactly_what_the_runtime_binds() {
        let runtime = frontend::decision_runtime().expect("the decision runtime builds");
        let page = plan_page(&facts("select 1", true, described()), "select 1");
        for declared in runtime.declarations() {
            assert!(
                page.contains(&format!("{DECISION_ATTRIBUTE}=\"{declared}\"")),
                "the built-in page declares no `{declared}` control: {page}"
            );
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

    /// Only the decided endings are afpsql's own word; the rest are AFUI's,
    /// spelled once for every tool rather than once per delivery here.
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
        assert_eq!(ending_of(&Outcome::Closed), "closed");
        assert_eq!(ending_of(&Outcome::Expired), "expired");
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

    /// A locked profile's refusal is decided once, before `run` looks at which
    /// panel was asked for — `view_panel` vs `plan_panel` today, and a future
    /// window-vs-published-session delivery choice tomorrow.
    /// `locked_profile_panel_refusal` takes neither a panel nor a delivery, on
    /// purpose: that is what proves the same `error.code` and the same
    /// message cover every panel through every delivery it could ever gain,
    /// rather than needing a new case each time a delivery is added.
    #[test]
    fn locked_profile_refuses_every_panel_regardless_of_delivery() {
        assert!(locked_profile_panel_refusal(false).is_none());
        let (message, hint) =
            locked_profile_panel_refusal(true).expect("a locked profile must refuse every panel");
        assert!(message.contains("no exit for what the database holds"));
        assert_eq!(
            hint,
            "read the same data with the matching `afpsql inspect` command"
        );
        // Not "a published session also needs a browser" — that reasoning
        // would have waved a published session through rather than catching
        // it, so the refusal must not lean on window-only vocabulary.
        assert!(!message.contains("browser"));
        assert!(!message.contains("profile directory"));
    }

    // ── delivery ─────────────────────────────

    use agent_first_ui::UiDeliveryMode;

    #[test]
    fn an_explicit_mode_wins_over_the_environment() {
        let _guard = crate::test_env::env_lock();
        // SAFETY: this test module's environment lock is held for the mutation.
        unsafe { std::env::set_var(agent_first_ui::DELIVERY_ENV, "session") };
        let plan = UiDeliveryPlan::resolve(Some(UiDeliveryMode::Window));
        // SAFETY: the lock is still held.
        unsafe { std::env::remove_var(agent_first_ui::DELIVERY_ENV) };
        assert_eq!(plan.unwrap().mode(), UiDeliveryMode::Window);
    }

    #[test]
    fn no_mode_and_no_environment_defaults_to_window() {
        let _guard = crate::test_env::env_lock();
        // SAFETY: this test module's environment lock is held for the mutation.
        unsafe { std::env::remove_var(agent_first_ui::DELIVERY_ENV) };
        assert_eq!(
            UiDeliveryPlan::resolve(None).unwrap().mode(),
            UiDeliveryMode::Window
        );
    }

    #[test]
    fn no_mode_falls_back_to_the_environment() {
        let _guard = crate::test_env::env_lock();
        // SAFETY: this test module's environment lock is held for the mutation.
        unsafe { std::env::set_var(agent_first_ui::DELIVERY_ENV, "session") };
        let plan = UiDeliveryPlan::resolve(None);
        // SAFETY: the lock is still held.
        unsafe { std::env::remove_var(agent_first_ui::DELIVERY_ENV) };
        assert_eq!(plan.unwrap().mode(), UiDeliveryMode::Session);
    }

    /// The one AFUI delivery this spore does not offer, from either source,
    /// refused while the plan is being built — before a person is ever told
    /// anything is ready. afpsql declines it by choosing `resolve` over
    /// `resolve_allowing_link`; the refusal itself, and the list of what is on
    /// offer, is AFUI's.
    #[test]
    fn link_is_refused_whichever_source_it_came_from() {
        let error = UiDeliveryPlan::resolve(Some(UiDeliveryMode::Link)).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("`link`"), "{message}");
        assert!(message.contains("`window`"), "{message}");
        assert!(message.contains("`session`"), "{message}");

        let _guard = crate::test_env::env_lock();
        // SAFETY: this test module's environment lock is held for the mutation.
        unsafe { std::env::set_var(agent_first_ui::DELIVERY_ENV, "link") };
        let plan = UiDeliveryPlan::resolve(None);
        // SAFETY: the lock is still held.
        unsafe { std::env::remove_var(agent_first_ui::DELIVERY_ENV) };
        assert!(plan.is_err());
    }

    #[test]
    fn an_unrecognised_environment_value_is_rejected_not_defaulted() {
        let _guard = crate::test_env::env_lock();
        // SAFETY: this test module's environment lock is held for the mutation.
        unsafe { std::env::set_var(agent_first_ui::DELIVERY_ENV, "lan") };
        let plan = UiDeliveryPlan::resolve(None);
        // SAFETY: the lock is still held.
        unsafe { std::env::remove_var(agent_first_ui::DELIVERY_ENV) };
        let message = plan.unwrap_err().to_string();
        assert!(message.contains("window"));
        assert!(message.contains("session"));
    }

    /// A readiness event carries the delivery facts under AFUI's names, beside
    /// the panel's own — neither side overwrites the other.
    #[test]
    fn a_readiness_event_carries_the_delivery_facts_beside_the_panel_facts() {
        let facts = UiDeliveryFacts {
            mode: UiDeliveryMode::Session,
            session_id: "abc".to_owned(),
            link_url_secret: None,
            idle_timeout_s: None,
            grace_period_s: None,
        };
        let event = ready_event(&facts, serde_json::json!({ "ui_kind": "table", "rows": 3 }));
        assert_eq!(event["ui_kind"], "table");
        assert_eq!(event["rows"], 3);
        assert_eq!(event["mode"], "session");
        assert_eq!(event["session_id"], "abc");
        // A delivery with no clock and no URL says so by omission.
        assert!(event.get("link_url_secret").is_none());
        assert!(event.get("idle_timeout_s").is_none());
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
