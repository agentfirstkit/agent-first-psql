//! Where an afpsql panel's files come from, and what a person may replace.
//!
//! AFUI owns delivery: which directory a `provider_id` + `ui_kind` override
//! lives in, whether it has been trusted, whether it declares afpsql's
//! `ui_api_version`, which file the override supplies and which falls back to
//! afpsql's own, and the rule that a frontend afpsql cannot load is an error
//! naming safe mode rather than a quiet built-in page. None of that is restated
//! here — [`PanelFrontend`] is a thin thing wrapped around
//! `agent_first_ui::UiFrontend`, which is the one implementation of it.
//!
//! afpsql owns what a frontend *is*: MiniJinja templates rendered against the
//! typed panel document below, plus a stylesheet and static assets. That is the
//! whole of what an override supplies. It supplies no behaviour: AFUI refuses a
//! frontend file whose name says it is a script, and
//! [`agent_first_ui::reject_frontend_script`] refuses one hiding inside a
//! template. The only JavaScript any panel loads is afpsql's own decision
//! runtime, injected under a per-session nonce at the layout's
//! `<!-- afpsql:trusted-runtime -->` marker.
//!
//! What that buys is the point of the whole arrangement: a template can move,
//! rename, regroup and drop anything, and still cannot make a control mean
//! something other than what it declares — the declaration is the template's,
//! the binding is afpsql's.

use std::collections::BTreeSet;
use std::path::PathBuf;

use agent_first_ui::{
    Error as UiError, SAFE_MODE_HINT, UiCspNonce, UiFrontend, reject_frontend_script,
    reject_rendered_script,
};
use axum::Router;
use minijinja::{AutoEscape, Environment, UndefinedBehavior};
use serde::Serialize;

pub(super) const PROVIDER_ID: &str = "afpsql";

/// The panel contract a frontend is written against.
///
/// One number for all five panels, because from a frontend author's point of
/// view they are one contract: the shape of the `document` a template renders
/// against, the four template names afpsql resolves, the
/// `<!-- afpsql:trusted-runtime -->` marker, and the `data-afpsql-decision`
/// declaration the runtime binds. A change to any of those is a change to all
/// of them for the person who has to fix their frontend.
pub(super) const UI_API_VERSION: &str = "1";

/// Where afpsql splices its own behaviour into a page it did not necessarily
/// write.
pub(super) const TRUSTED_RUNTIME_MARKER: &str = "<!-- afpsql:trusted-runtime -->";

/// How a template declares that a control answers the question.
pub(super) const DECISION_ATTRIBUTE: &str = "data-afpsql-decision";

/// The entry template every override supplies under `templates/`.
///
/// One name for every `ui_kind` rather than five, because an override
/// directory is already keyed by `ui_kind`: the person editing
/// `.afui/frontends/afpsql/plan_confirm/templates/page.html.j2` has already
/// said which panel they mean by being in that directory.
const ENTRY_TEMPLATE: &str = "page.html.j2";

const LAYOUT_TEMPLATE: &str = "layout.html.j2";
const TABLE_TEMPLATE: &str = "table.html.j2";
const DECIDED_TEMPLATE: &str = "decided.html.j2";

const BUILTIN_LAYOUT: &str = include_str!("templates/layout.html.j2");
const BUILTIN_TABLE: &str = include_str!("templates/table.html.j2");
const BUILTIN_INSPECT: &str = include_str!("templates/inspect.html.j2");
const BUILTIN_MONITOR: &str = include_str!("templates/monitor.html.j2");
const BUILTIN_PLAN: &str = include_str!("templates/plan.html.j2");
const BUILTIN_DECIDED: &str = include_str!("templates/decided.html.j2");

/// The stylesheet is a route rather than an inline `<style>` so the page needs
/// no style exemption in its policy, and so a frontend can replace presentation
/// without touching structure — or structure without touching presentation.
const BUILTIN_STYLE: &str = include_str!("style.css");

/// afpsql's own behaviour, and the only script any panel ever loads.
const DECISION_RUNTIME: &str = include_str!("runtime.js");

/// A panel that failed before it could be shown, in afpsql's error vocabulary.
#[derive(Debug)]
pub(super) struct FrontendFailure {
    pub(super) code: &'static str,
    pub(super) message: String,
    pub(super) hint: Option<&'static str>,
}

impl FrontendFailure {
    fn new(code: &'static str, message: String) -> Self {
        Self {
            code,
            message,
            hint: Some(SAFE_MODE_HINT),
        }
    }
}

impl From<UiError> for FrontendFailure {
    fn from(error: UiError) -> Self {
        let code = match error {
            UiError::FrontendIncompatible { .. } => "ui_frontend_incompatible",
            UiError::FrontendScript { .. } | UiError::FrontendFileName { .. } => {
                "ui_frontend_unsafe"
            }
            _ => "ui_frontend_unreadable",
        };
        Self::new(code, error.to_string())
    }
}

/// Which built-in page a `ui_kind` starts from when no override supplies one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PanelShape {
    /// One result set, read once: `schema_inspect`, `table_inspect`,
    /// `index_inspect`.
    Inspect,
    /// The same query re-run on every reload: `connection_monitor`.
    Monitor,
    /// One statement and a person's answer: `plan_confirm`.
    Decide,
}

impl PanelShape {
    fn builtin_entry(self) -> &'static str {
        match self {
            Self::Inspect => BUILTIN_INSPECT,
            Self::Monitor => BUILTIN_MONITOR,
            Self::Decide => BUILTIN_PLAN,
        }
    }

    /// Whether a page of this shape must declare the controls the runtime
    /// binds. Only a panel that returns an answer has an answer to lose.
    fn needs_decision_controls(self) -> bool {
        matches!(self, Self::Decide)
    }
}

/// One panel's file source: a person's frontend, or afpsql's own.
pub(super) struct PanelFrontend {
    frontend: UiFrontend,
    shape: PanelShape,
    ui_kind: &'static str,
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
        Ok(Self {
            frontend,
            shape,
            ui_kind,
        })
    }

    /// afpsql's own panel, for a caller that is not reading any frontend.
    #[cfg(test)]
    pub(super) fn builtin(ui_kind: &'static str, shape: PanelShape) -> Self {
        Self {
            frontend: UiFrontend::builtin(PROVIDER_ID, ui_kind, UI_API_VERSION),
            shape,
            ui_kind,
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

    /// The frontend's `assets/` tree. Every path 404s when there is no
    /// frontend, so a panel mounts this without asking whether one exists.
    pub(super) fn assets_router(&self) -> Router {
        self.frontend.assets_router()
    }

    /// Render this panel's page from a typed document.
    ///
    /// `nonce` is the session's, and the only thing that can put a script on
    /// the page. A panel with no decision to make passes `None` and the marker
    /// resolves to nothing at all.
    pub(super) fn render_page<T: Serialize>(
        &self,
        document: &T,
        nonce: Option<&UiCspNonce>,
    ) -> Result<String, FrontendFailure> {
        self.render(ENTRY_TEMPLATE, self.shape, document, nonce)
    }

    /// Render the page afpsql shows once an answer has been recorded.
    ///
    /// Rendered through the same pipeline, so an override's layout and
    /// stylesheet still apply — but never a decision control, because this page
    /// has no question on it.
    pub(super) fn render_decided<T: Serialize>(
        &self,
        document: &T,
    ) -> Result<String, FrontendFailure> {
        self.render(DECIDED_TEMPLATE, PanelShape::Inspect, document, None)
    }

    fn render<T: Serialize>(
        &self,
        entry: &str,
        shape: PanelShape,
        document: &T,
        nonce: Option<&UiCspNonce>,
    ) -> Result<String, FrontendFailure> {
        let mut environment = Environment::new();
        environment.set_undefined_behavior(UndefinedBehavior::SemiStrict);
        // Every panel template is HTML and every value in a document is a
        // domain value, so escaping is unconditional and `safe` is not a filter
        // a frontend can reach for.
        environment.set_auto_escape_callback(|_| AutoEscape::Html);
        environment.remove_filter("safe");
        for (name, text) in self.template_sources()? {
            environment
                .add_template_owned(name.clone(), text)
                .map_err(|error| self.template_failure(&name, &error))?;
        }
        let template = environment
            .get_template(entry)
            .map_err(|error| self.template_failure(entry, &error))?;
        let page = template
            .render(minijinja::context! { document => minijinja::Value::from_serialize(document) })
            .map_err(|error| self.template_failure(entry, &error))?;

        // A value that arrived from PostgreSQL is checked after rendering as
        // well as before: escaping is what stops a table name from becoming
        // markup, and this is what notices when it did not.
        reject_rendered_script("rendered page", &page).map_err(FrontendFailure::from)?;
        self.finish(shape, page, nonce)
    }

    /// Splice afpsql's behaviour into the rendered page, and refuse a page that
    /// left no room for it.
    fn finish(
        &self,
        shape: PanelShape,
        page: String,
        nonce: Option<&UiCspNonce>,
    ) -> Result<String, FrontendFailure> {
        if !shape.needs_decision_controls() {
            return Ok(page.replace(TRUSTED_RUNTIME_MARKER, ""));
        }
        let Some(nonce) = nonce else {
            return Err(FrontendFailure {
                code: "internal",
                message: "a decision panel was rendered without a session nonce".to_owned(),
                hint: None,
            });
        };
        if !page.contains(TRUSTED_RUNTIME_MARKER) {
            return Err(self.broken_override(format!(
                "the rendered page does not contain `{TRUSTED_RUNTIME_MARKER}`, so afpsql has \
                 nowhere to bind the controls that answer it"
            )));
        }
        for declared in ["approve", "refuse"] {
            if !declares_decision(&page, declared) {
                return Err(self.broken_override(format!(
                    "the rendered page declares no `{DECISION_ATTRIBUTE}=\"{declared}\"` control, \
                     so a person could not {declared} the statement"
                )));
            }
        }
        let runtime = format!(
            "<script nonce=\"{}\">\n{DECISION_RUNTIME}</script>",
            nonce.as_str()
        );
        Ok(page.replace(TRUSTED_RUNTIME_MARKER, &runtime))
    }

    /// Every template the environment needs: the frontend's where it has one,
    /// afpsql's where it does not.
    ///
    /// Per file, not per directory. Replacing `page.html.j2` alone keeps
    /// afpsql's layout and table partial; replacing the layout alone keeps
    /// afpsql's page. A frontend may also add templates of its own and
    /// `{% include %}` them, which is what makes a real restructure possible
    /// rather than a reskin.
    fn template_sources(&self) -> Result<Vec<(String, String)>, FrontendFailure> {
        let mut sources = Vec::new();
        let mut supplied = BTreeSet::new();
        for name in self.frontend_template_names() {
            let Some(text) = self.frontend.text(&format!("templates/{name}"))? else {
                continue;
            };
            // Both halves of "a frontend supplies no behaviour" — smuggled
            // markup and the MiniJinja directives that switch escaping off —
            // are one call, because they are one rule and every Provider needs
            // the same one.
            reject_frontend_script(&name, &text).map_err(FrontendFailure::from)?;
            supplied.insert(name.clone());
            sources.push((name, text));
        }
        for (name, builtin) in [
            (ENTRY_TEMPLATE, self.shape.builtin_entry()),
            (LAYOUT_TEMPLATE, BUILTIN_LAYOUT),
            (TABLE_TEMPLATE, BUILTIN_TABLE),
            (DECIDED_TEMPLATE, BUILTIN_DECIDED),
        ] {
            if !supplied.contains(name) {
                sources.push((name.to_owned(), builtin.to_owned()));
            }
        }
        Ok(sources)
    }

    /// The template file names the frontend actually ships.
    ///
    /// Read from the directory rather than from a fixed list, so a frontend can
    /// break its page into partials afpsql has never heard of. Names go back
    /// through `UiFrontend::text`, which is what validates them.
    fn frontend_template_names(&self) -> Vec<String> {
        let Some(location) = self.frontend.location() else {
            return Vec::new();
        };
        let Ok(entries) = std::fs::read_dir(location.root.join("templates")) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name.ends_with(".j2"))
            .collect();
        names.sort();
        names
    }

    fn template_failure(&self, label: &str, error: &minijinja::Error) -> FrontendFailure {
        let mut detail = error.to_string();
        let mut source = std::error::Error::source(error);
        while let Some(next) = source {
            detail.push_str(": ");
            detail.push_str(&next.to_string());
            source = next.source();
        }
        FrontendFailure::new(
            "ui_frontend_template",
            match self.frontend.frontend_id() {
                Some(frontend_id) => format!(
                    "the {PROVIDER_ID} {} frontend `{frontend_id}` could not render `{label}`: \
                     {detail}",
                    self.ui_kind
                ),
                None => format!("{PROVIDER_ID} could not render its own `{label}`: {detail}"),
            },
        )
    }

    fn broken_override(&self, what: String) -> FrontendFailure {
        FrontendFailure::new(
            "ui_frontend_incomplete",
            match self.frontend.frontend_id() {
                Some(frontend_id) => format!(
                    "the {PROVIDER_ID} {} frontend `{frontend_id}` is incomplete: {what}",
                    self.ui_kind
                ),
                None => format!(
                    "{PROVIDER_ID}'s own {} page is incomplete: {what}",
                    self.ui_kind
                ),
            },
        )
    }
}

/// Whether the page declares a control for this answer.
///
/// Tolerant about quoting because a person writes this by hand, and strict
/// about the value: an unrecognised declaration binds to nothing, so a page
/// that only declares `approve` is reported rather than opened.
fn declares_decision(page: &str, declared: &str) -> bool {
    page.match_indices(DECISION_ATTRIBUTE).any(|(index, _)| {
        let after = &page[index + DECISION_ATTRIBUTE.len()..];
        let after = after.trim_start();
        let Some(after) = after.strip_prefix('=') else {
            return false;
        };
        let after = after.trim_start();
        // The value has to be exactly the declaration, not merely start with
        // it. A prefix match accepts `approve-all`, which the runtime maps to
        // nothing — so the page would satisfy this check and then open with a
        // control that silently does nothing. That is precisely the failure
        // `ui_frontend_incomplete` exists to prevent, so matching loosely here
        // would defeat the check it guards.
        let value = match after.chars().next() {
            Some(quote @ ('"' | '\'')) => after[quote.len_utf8()..].split(quote).next(),
            _ => after
                .split(|character: char| character.is_ascii_whitespace() || character == '>')
                .next(),
        };
        value == Some(declared)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_declaration_must_be_the_whole_value_not_a_prefix() {
        let declared =
            |value: &str| format!("<button {DECISION_ATTRIBUTE}=\"{value}\">go</button>");
        assert!(declares_decision(&declared("approve"), "approve"));
        assert!(declares_decision(&declared("refuse"), "refuse"));
        assert!(declares_decision(
            &format!("<button {DECISION_ATTRIBUTE}='approve'>go</button>"),
            "approve"
        ));

        // Regression: this used to pass on a prefix match. The runtime maps
        // only the exact values, so such a page satisfied the completeness
        // check and then opened with a control that did nothing at all — the
        // one outcome `ui_frontend_incomplete` exists to prevent.
        assert!(!declares_decision(&declared("approve-all"), "approve"));
        assert!(!declares_decision(&declared("refuse_everything"), "refuse"));
        assert!(!declares_decision(&declared("approved"), "approve"));
        assert!(!declares_decision(&declared(""), "approve"));
        assert!(!declares_decision("<button>go</button>", "approve"));
    }
}
