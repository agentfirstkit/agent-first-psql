//! Where an afpsql panel's files come from, and what a person may replace.
//!
//! AFUI owns delivery: which directory a `provider_id` + `ui_kind` override
//! lives in, whether it has been trusted, whether it declares afpsql's
//! `ui_api_version`, which file the override supplies and which falls back to
//! afpsql's own, and the rule that a frontend afpsql cannot load is an error
//! naming safe mode rather than a quiet built-in page. AFUI's
//! [`agent_first_ui::UiPage`] also owns assembly: the MiniJinja environment
//! policy, the render, the rendered-markup guard, the runtime-marker count,
//! and the order that keeps a Provider's own trusted runtime from tripping
//! the guard it splices past. None of that is restated here — [`PanelFrontend`]
//! is a thin thing wrapped around both.
//!
//! afpsql owns what a frontend *is*: MiniJinja templates rendered against the
//! typed panel document below, plus a stylesheet and static assets. It
//! supplies no behaviour: `UiPage` refuses a frontend file whose name says it
//! is a script, and refuses one hiding inside a template. The only JavaScript
//! any panel loads is afpsql's own decision runtime, injected under a
//! per-session nonce at the layout's `<!-- afpsql:trusted-runtime -->`
//! marker. A static watch panel gets that marker replaced with nothing at all;
//! the live monitor gets a separate runtime which carries snapshots and
//! inspection requests over the session channel.
//!
//! What `UiPage` does *not* take is the one thing that makes a decide panel
//! trustworthy: which declaration binds to which route. `data-afpsql-decision`
//! is a template's to *declare*; afpsql's own runtime, spliced in under a
//! nonce a frontend cannot forge, is what *binds* it. `UiPage` checks that a
//! page is structurally complete; it has no idea a decide panel exists, so
//! afpsql checks that itself, on the page `UiPage` already returned.
//!
//! What that buys is the point of the whole arrangement: a template can move,
//! rename, regroup and drop anything, and still cannot make a control mean
//! something other than what it declares — the declaration is the template's,
//! the binding is afpsql's.

use std::path::PathBuf;

use agent_first_ui::{
    Error as UiError, UiAppIcon, UiCspNonce, UiDecisionRuntime, UiErrorKind, UiFrontend, UiPage,
};
use serde::Serialize;

pub(super) const PROVIDER_ID: &str = "afpsql";

/// The panel contract a frontend is written against.
///
/// One number for all five panels, because from a frontend author's point of
/// view they are one contract: the shape of the `document` a template renders
/// against, the five template names afpsql resolves, the
/// `<!-- afpsql:trusted-runtime -->` marker, and the `data-afpsql-decision`
/// declaration the runtime binds. A change to any of those is a change to all
/// of them for the person who has to fix their frontend.
///
/// `2` is where the inspection panels stopped handing a template the snapshot
/// query's own row shape and started handing it relations with their columns,
/// constraints, indexes and triggers already attached — see [`super::inspect`].
/// `3` replaces the connection monitor's page reload with a declared live
/// snapshot/EXPLAIN surface bound by afpsql's trusted runtime. `4` separates
/// monitor freshness from request feedback and preserves the request panel
/// while live snapshots are replaced.
pub(super) const UI_API_VERSION: &str = "4";

/// Where afpsql splices its own behaviour into a page it did not necessarily
/// write.
pub(super) const TRUSTED_RUNTIME_MARKER: &str = "<!-- afpsql:trusted-runtime -->";

/// How a template declares that a control answers the question.
pub(super) const DECISION_ATTRIBUTE: &str = "data-afpsql-decision";

/// Where the decide page shows what is happening while an answer travels.
const DECISION_STATUS_ATTRIBUTE: &str = "data-afpsql-decision-status";

/// Every hook `monitor_runtime.js` looks for. A page that dropped one opens
/// with a live region nothing ever writes into.
const MONITOR_DECLARATIONS: [&str; 7] = [
    "data-afpsql-monitor-snapshot",
    "data-afpsql-explain-form",
    "data-afpsql-explain-sql",
    "data-afpsql-explain-result",
    "data-afpsql-explain-panel",
    "data-afpsql-explain-status",
    "data-afpsql-live-status",
];

/// Every template name doubles as the on-disk path an override supplies it
/// under, per `UiPage` — so a template's own `{% extends %}` / `{% include %}`
/// has to spell the same `templates/…` path a person sees in their frontend
/// directory, not a bare file name.
const ENTRY_TEMPLATE: &str = "templates/page.html.j2";
const LAYOUT_TEMPLATE: &str = "templates/layout.html.j2";
const TABLE_TEMPLATE: &str = "templates/table.html.j2";
const RELATION_TEMPLATE: &str = "templates/relation.html.j2";
const DECIDED_TEMPLATE: &str = "templates/decided.html.j2";

const BUILTIN_LAYOUT: &str = include_str!("templates/layout.html.j2");
const BUILTIN_TABLE: &str = include_str!("templates/table.html.j2");
const BUILTIN_RELATION: &str = include_str!("templates/relation.html.j2");
const BUILTIN_TABLE_INSPECT: &str = include_str!("templates/table-inspect.html.j2");
const BUILTIN_SCHEMA_INSPECT: &str = include_str!("templates/schema-inspect.html.j2");
const BUILTIN_INDEX_INSPECT: &str = include_str!("templates/index-inspect.html.j2");
const BUILTIN_MONITOR: &str = include_str!("templates/monitor.html.j2");
const BUILTIN_PLAN: &str = include_str!("templates/plan.html.j2");
const BUILTIN_DECIDED: &str = include_str!("templates/decided.html.j2");

/// The stylesheet is a route rather than an inline `<style>` so the page needs
/// no style exemption in its policy, and so a frontend can replace presentation
/// without touching structure — or structure without touching presentation.
const BUILTIN_STYLE: &str = include_str!("style.css");
const BUILTIN_APP_ICON: &str = include_str!("app-icon.svg");

/// afpsql's own behaviour, and the only script a monitor panel ever loads.
///
/// Served with AFUI's page kernel ahead of it, inside one function scope: the
/// session root, state synchronization, reconnect and call correlation are not
/// facts about PostgreSQL, and this file is shorter for not restating them.
const MONITOR_RUNTIME: &str = include_str!("monitor_runtime.js");

/// The decide panel's runtime: which declaration means what, and afpsql's own
/// words for what is happening while it travels.
///
/// The rules around it — exact declaration match, an unrecognised declaration
/// bound to nothing, one answer per page, every control disabled the moment
/// one is pressed, a form navigation rather than a fetch — are AFUI's, so they
/// hold identically for every decide page in this kit rather than being
/// re-derived here. What stays afpsql's is the vocabulary: `approve` and
/// `refuse`, the routes they post to, and what a person reads while waiting.
pub(super) fn decision_runtime() -> Result<UiDecisionRuntime, FrontendFailure> {
    UiDecisionRuntime::new(DECISION_ATTRIBUTE)
        .and_then(|runtime| runtime.with_status_attribute(DECISION_STATUS_ATTRIBUTE))
        .and_then(|runtime| runtime.with_decision("approve", "approve"))
        .and_then(|runtime| runtime.with_pending_text("approve", "Approving…"))
        .and_then(|runtime| runtime.with_decision("refuse", "refuse"))
        .and_then(|runtime| runtime.with_pending_text("refuse", "Refusing…"))
        .map_err(FrontendFailure::from)
}

/// A panel that failed before it could be shown, in afpsql's error vocabulary.
#[derive(Debug)]
pub(super) struct FrontendFailure {
    pub(super) code: &'static str,
    pub(super) message: String,
    pub(super) hint: Option<&'static str>,
}

/// Every AFUI failure, in afpsql's vocabulary.
///
/// AFUI classifies its own errors; afpsql prefixes that classification and
/// takes the recovery action AFUI already knows about. What used to be here
/// was a match over AFUI's variants that fell into `unreadable` for anything
/// it had not thought about, plus a second conversion path whose whole job was
/// to strip a hint AFUI had embedded in its own message.
impl From<UiError> for FrontendFailure {
    fn from(error: UiError) -> Self {
        Self {
            code: ui_error_code(&error),
            message: error.to_string(),
            hint: error.hint(),
        }
    }
}

/// `ui_<afpsql's word for what AFUI classified>`, interned so the code stays
/// `&'static str`.
///
/// A `String` here would ripple through every event this failure reaches.
/// Exhaustive over AFUI's closed classification, with **no wildcard arm**:
/// this used to default to `ui_frontend_unreadable`, which reported a closed
/// session runtime, a machine with no browser and a bad `--mode` as a frontend
/// that could not be read — and sent a person to look at a file that was fine.
/// Now a classification AFUI adds stops this compiling until afpsql decides
/// what it is called here.
fn ui_error_code(error: &UiError) -> &'static str {
    match error.kind() {
        UiErrorKind::FrontendIncompatible => "ui_frontend_incompatible",
        UiErrorKind::FrontendUnsafe => "ui_frontend_unsafe",
        UiErrorKind::FrontendUnreadable => "ui_frontend_unreadable",
        UiErrorKind::PageRender => "ui_frontend_template",
        UiErrorKind::PageIncomplete => "ui_frontend_incomplete",
        UiErrorKind::WindowUnavailable => "ui_window_unavailable",
        UiErrorKind::WindowWaitFailed => "ui_window_wait_failed",
        UiErrorKind::DeliveryModeInvalid | UiErrorKind::DeliveryModeNotOffered => {
            "ui_delivery_unavailable"
        }
        UiErrorKind::LinkAddressUnavailable | UiErrorKind::UpstreamNotProxyable => {
            "ui_delivery_unreachable"
        }
        UiErrorKind::RuntimeClosed => "ui_runtime_closed",
        UiErrorKind::RuntimeBusy => "ui_runtime_busy",
        UiErrorKind::RuntimeMisconfigured
        | UiErrorKind::RuntimeMessageTooLarge
        | UiErrorKind::RuntimeBlob
        | UiErrorKind::RuntimePayload => "ui_runtime_failed",
        UiErrorKind::ConfigUnreadable => "ui_config_unreadable",
        UiErrorKind::InvalidArgument | UiErrorKind::Io => "ui_failed",
    }
}

/// Which built-in page a `ui_kind` starts from when no override supplies one.
///
/// The three inspection panels are separate shapes rather than one, because
/// their documents are: a snapshot read as one relation, the same snapshot read
/// as a whole schema, and a list of indexes are three things to look at, not one
/// grid rendered three times.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PanelShape {
    /// One relation and everything attached to it: `table_inspect`.
    Table,
    /// Every object in one schema: `schema_inspect`.
    Schema,
    /// Indexes under the tables they belong to: `index_inspect`.
    Index,
    /// The same query pushed on a live interval: `connection_monitor`.
    Monitor,
    /// One statement and a person's answer: `plan_confirm`.
    Decide,
}

impl PanelShape {
    fn builtin_entry(self) -> &'static str {
        match self {
            Self::Table => BUILTIN_TABLE_INSPECT,
            Self::Schema => BUILTIN_SCHEMA_INSPECT,
            Self::Index => BUILTIN_INDEX_INSPECT,
            Self::Monitor => BUILTIN_MONITOR,
            Self::Decide => BUILTIN_PLAN,
        }
    }

    /// Whether a page of this shape must declare the controls the runtime
    /// binds. Only a panel that returns an answer has an answer to lose.
    fn needs_decision_controls(self) -> bool {
        matches!(self, Self::Decide)
    }

    fn needs_monitor_contract(self) -> bool {
        matches!(self, Self::Monitor)
    }
}

/// One panel's file source: a person's frontend, or afpsql's own.
pub(super) struct PanelFrontend {
    frontend: UiFrontend,
    shape: PanelShape,
}

impl PanelFrontend {
    /// Resolve the frontend for this panel before anything is served.
    ///
    /// Called before the window opens, so a frontend afpsql cannot load ends
    /// the command with an error rather than opening a window onto a page
    /// nobody asked for.
    pub(super) fn resolve(
        ui_kind: &'static str,
        shape: PanelShape,
    ) -> Result<Self, FrontendFailure> {
        let workspace_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let frontend = UiFrontend::resolve(&workspace_root, PROVIDER_ID, ui_kind, UI_API_VERSION)?;
        Ok(Self { frontend, shape })
    }

    /// afpsql's own panel, for a caller that is not reading any frontend.
    #[cfg(test)]
    pub(super) fn builtin(ui_kind: &'static str, shape: PanelShape) -> Self {
        Self {
            frontend: UiFrontend::builtin(PROVIDER_ID, ui_kind, UI_API_VERSION),
            shape,
        }
    }

    /// The override serving this panel, or `None` for afpsql's own page.
    ///
    /// This goes in the readiness event. A workspace frontend that has not been
    /// trusted is deliberately silent — AFUI skips it before parsing anything
    /// inside it — so this is how an agent tells "my override is running" from
    /// "my override is inert" without opening a window to look.
    pub(super) fn frontend_id(&self) -> Option<&str> {
        self.frontend.frontend_id()
    }

    /// The stylesheet this panel serves: the frontend's, or afpsql's.
    pub(super) fn stylesheet(&self) -> Result<Vec<u8>, FrontendFailure> {
        Ok(self
            .frontend
            .file("style.css")?
            .unwrap_or_else(|| BUILTIN_STYLE.as_bytes().to_vec()))
    }

    /// The application icon from this panel's resolved frontend overlay.
    pub(super) fn app_icon(&self) -> Result<UiAppIcon, FrontendFailure> {
        self.frontend
            .app_icon(BUILTIN_APP_ICON)
            .map_err(FrontendFailure::from)
    }

    /// The resolved overlay behind this panel, for AFUI's own page routes.
    pub(super) fn frontend(&self) -> &UiFrontend {
        &self.frontend
    }

    /// Render this panel's page from a typed document.
    ///
    /// `nonce` is the session's, and the only thing that can put a script on
    /// the page. A panel with no decision to make passes `None` — `UiPage`
    /// still requires the marker to appear exactly once, and only replaces it
    /// with nothing rather than skipping the check.
    pub(super) fn render_page<T: Serialize>(
        &self,
        document: &T,
        nonce: Option<&UiCspNonce>,
    ) -> Result<String, FrontendFailure> {
        let decisions = self
            .shape
            .needs_decision_controls()
            .then(decision_runtime)
            .transpose()?;
        let runtime = self.trusted_runtime(nonce, decisions.as_ref())?;
        let mut builder = UiPage::builder(&self.frontend)
            .entry(ENTRY_TEMPLATE)
            .fallback(self.shape.builtin_entry())
            .template(LAYOUT_TEMPLATE, BUILTIN_LAYOUT)
            .template(TABLE_TEMPLATE, BUILTIN_TABLE)
            .template(RELATION_TEMPLATE, BUILTIN_RELATION)
            .template(DECIDED_TEMPLATE, BUILTIN_DECIDED)
            .override_templates_under("templates", ".j2")?;
        // Both contracts are now `UiPage`'s to check, on the parse tree it
        // already built, before it returns a page at all — rather than two
        // hand-written scans over the rendered string afterwards.
        if let Some(decisions) = &decisions {
            builder = builder.requires_decisions(decisions);
        }
        if self.shape.needs_monitor_contract() {
            builder = builder.requires_attributes(MONITOR_DECLARATIONS.iter().copied());
        }
        builder
            .runtime_marker(TRUSTED_RUNTIME_MARKER)
            .runtime(runtime)
            .render(document)
            .map_err(FrontendFailure::from)
    }

    /// Render the page afpsql shows once an answer has been recorded.
    ///
    /// Rendered through the same pipeline, so an override's layout still
    /// applies — but never a decision control, because this page has no
    /// question on it and loads no runtime at all.
    pub(super) fn render_decided<T: Serialize>(
        &self,
        document: &T,
    ) -> Result<String, FrontendFailure> {
        UiPage::builder(&self.frontend)
            .entry(DECIDED_TEMPLATE)
            .fallback(BUILTIN_DECIDED)
            .template(LAYOUT_TEMPLATE, BUILTIN_LAYOUT)
            .template(ENTRY_TEMPLATE, self.shape.builtin_entry())
            .template(TABLE_TEMPLATE, BUILTIN_TABLE)
            .template(RELATION_TEMPLATE, BUILTIN_RELATION)
            .override_templates_under("templates", ".j2")?
            .runtime_marker(TRUSTED_RUNTIME_MARKER)
            .runtime(None)
            .render(document)
            .map_err(FrontendFailure::from)
    }

    /// Register every additional `.j2` file a frontend put under
    /// `templates/`.  The five contract templates above keep their built-in
    /// fallbacks; all other names are override-only partials discovered at
    /// runtime and may be included by exactly their frontend-relative path.
    /// The nonce-bound decision runtime for a shape with a question on it, or
    /// `None` for one that has none. Either way `UiPage` still requires the
    /// marker to appear — `None` is what tells it to replace the marker with
    /// nothing rather than a script tag.
    fn trusted_runtime(
        &self,
        nonce: Option<&UiCspNonce>,
        decisions: Option<&UiDecisionRuntime>,
    ) -> Result<Option<String>, FrontendFailure> {
        let (runtime, purpose) = match self.shape {
            PanelShape::Monitor => (
                format!(
                    "(function(){{\n{}\n{MONITOR_RUNTIME}\n}})();",
                    agent_first_ui::page_kernel_source()
                ),
                "live monitor",
            ),
            PanelShape::Decide => match decisions {
                Some(decisions) => (decisions.source(), "decision"),
                None => return Ok(None),
            },
            PanelShape::Table | PanelShape::Schema | PanelShape::Index => return Ok(None),
        };
        let nonce = nonce.ok_or_else(|| FrontendFailure {
            code: "internal",
            message: format!("a {purpose} panel was rendered without a session nonce"),
            hint: None,
        })?;
        Ok(Some(format!(
            "<script nonce=\"{}\">\n{runtime}</script>",
            nonce.as_str()
        )))
    }
}
