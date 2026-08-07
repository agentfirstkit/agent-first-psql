//! A person replacing an afpsql panel, driven as a real process.
//!
//! What an AFUI frontend changes is which bytes reach a browser, so nothing
//! here is checked by calling a function: every case runs a real `afpsql ui`
//! against a real PostgreSQL, with a stub standing in for the browser, and
//! asserts on the page and the stylesheet the stub actually fetched.
//!
//! `AFUI_BROWSER_BINARY` names the stub. It records the `--app=<url>` it was
//! launched with, curls the page and the stylesheet into files, and exits —
//! which is the person closing the window, so the panel ends and the command
//! returns.
//!
//! `AFUI_CONFIG_DIR` moves AFUI's global directory into the test's temp tree,
//! so the trust store these tests write is theirs and not the developer's.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

#[path = "support/env.rs"]
mod test_env;

const STUB_BROWSER: &str = r#"#!/bin/sh
set -eu
url=""
for arg in "$@"; do
  case "$arg" in
    --app=*) url="${arg#--app=}" ;;
  esac
done
printf '%s' "$url" > "$AFPSQL_STUB_DIR/url"
curl -sS -o "$AFPSQL_STUB_DIR/body" -w '%{http_code}' "$url" > "$AFPSQL_STUB_DIR/status"
curl -sS -o "$AFPSQL_STUB_DIR/style" "${url}style.css"
"#;

/// A panel nobody could mistake for afpsql's own — and, more to the point, one
/// whose *structure* is not afpsql's: the table is gone, the rows are a
/// definition list, and the footer says something else.
const CUSTOM_INSPECT: &str = "{% extends \"layout.html.j2\" %}\n\
{% block panel %}\n\
<section data-my-panel><h2>MY OWN SCHEMA PANEL</h2>\n\
<dl class=\"mine\">{% for row in document.table.rows %}\
<div><dt>{{ row.cells[0].value }}</dt><dd>{{ row.cells[1].value }}</dd></div>\
{% endfor %}</dl>\n\
</section>\n\
{% endblock %}\n";

/// A stylesheet nobody could mistake for afpsql's own either.
const CUSTOM_STYLE: &str = ":root { --mine: 1 }\nbody { background: rebeccapurple }\n";

struct Panel {
    root: PathBuf,
    config_dir: PathBuf,
    stub_dir: PathBuf,
    stub: PathBuf,
}

impl Panel {
    fn new(name: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let base = std::env::temp_dir().join(format!(
            "afpsql-frontend-{name}-{}-{stamp}",
            std::process::id()
        ));
        let root = base.join("workspace");
        let config_dir = base.join("afui-config");
        let stub_dir = base.join("stub");
        for directory in [&root, &config_dir, &stub_dir] {
            fs::create_dir_all(directory).expect("test directory");
        }
        let stub = stub_dir.join("stub-browser.sh");
        fs::write(&stub, STUB_BROWSER).expect("write stub browser");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).expect("chmod stub");
        }
        ensure_fixture_object();
        Self {
            root,
            config_dir,
            stub_dir,
            stub,
        }
    }

    /// Run one panel to completion and return what the stub fetched.
    fn open(&self, args: &[&str], env: &[(&str, &str)]) -> Drive {
        for name in ["body", "style", "url"] {
            let _ = fs::remove_file(self.stub_dir.join(name));
        }
        // `--dsn` belongs to the panel subcommand, so it goes after `ui <verb>`
        // and before whatever positional the verb takes.
        let dsn = test_env::required_test_dsn();
        let mut full: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        full.splice(2..2, ["--dsn".to_owned(), dsn]);
        let mut command = Command::new(binary());
        command
            .current_dir(&self.root)
            .args(&full)
            .env("AFUI_BROWSER_BINARY", &self.stub)
            .env("AFUI_CONFIG_DIR", &self.config_dir)
            .env("AFPSQL_STUB_DIR", &self.stub_dir)
            .env_remove("AFUI_SAFE_MODE");
        for (name, value) in env {
            command.env(name, value);
        }
        let output = command.output().expect("run afpsql ui");
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let events = format!("{stdout}{stderr}")
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line).unwrap_or_else(|error| panic!("{line:?}: {error}"))
            })
            .collect();
        Drive {
            status: output.status.code().unwrap_or(99),
            events,
            page: fs::read_to_string(self.stub_dir.join("body")).unwrap_or_default(),
            style: fs::read_to_string(self.stub_dir.join("style")).unwrap_or_default(),
            opened: self.stub_dir.join("url").exists(),
        }
    }

    fn frontend_root(&self, ui_kind: &str) -> PathBuf {
        self.root.join(".afui/frontends/afpsql").join(ui_kind)
    }

    /// What `afui frontend init` writes, plus files of the person's own.
    fn install(&self, ui_kind: &str, ui_api_version: &str, files: &[(&str, &str)]) -> PathBuf {
        let root = self.frontend_root(ui_kind);
        fs::create_dir_all(root.join("templates")).expect("frontend templates directory");
        fs::write(
            root.join("frontend.json"),
            serde_json::to_string_pretty(&json!({
                "frontend_id": "my_psql_panel",
                "ui_api_version": ui_api_version,
            }))
            .expect("frontend manifest"),
        )
        .expect("write frontend manifest");
        for (name, text) in files {
            fs::write(root.join(name), text).expect("write frontend file");
        }
        root
    }

    /// What `afui frontend enable` records: the exact contents, trusted.
    ///
    /// The fingerprint is recomputed here rather than shelled out to `afui`, so
    /// a change to AFUI's algorithm shows up as this suite failing to serve a
    /// trusted frontend — loudly, not as a quiet pass.
    fn trust(&self, ui_kind: &str) {
        let trust = json!({
            "frontends": {
                format!("workspace:afpsql:{ui_kind}"): {
                    "fingerprint": fingerprint(&self.frontend_root(ui_kind)),
                },
            },
        });
        fs::write(
            self.config_dir.join("trust.json"),
            serde_json::to_string_pretty(&trust).expect("trust store"),
        )
        .expect("write trust store");
    }
}

struct Drive {
    status: i32,
    events: Vec<Value>,
    page: String,
    style: String,
    opened: bool,
}

impl Drive {
    fn ready(&self) -> Value {
        self.events
            .iter()
            .find(|event| event["progress"]["phase"] == "ui_ready")
            .unwrap_or_else(|| panic!("no ui_ready progress in {:?}", self.events))["progress"]
            .clone()
    }

    fn error(&self) -> Value {
        self.events
            .iter()
            .find(|event| event["kind"] == "error")
            .unwrap_or_else(|| panic!("no error event in {:?}", self.events))["error"]
            .clone()
    }

    fn is_builtin_page(&self) -> bool {
        self.page.contains("<span class=\"name\">") && !self.page.contains("MY OWN SCHEMA PANEL")
    }
}

/// One object in `public`, created once per run and never dropped.
///
/// Every case here reads `ui schema public`, and the page these tests
/// recognise as afpsql's own is the one that *lists objects*: an empty schema
/// renders "No matching objects." instead, which carries none of the markers.
/// The suite therefore passed on a developer's populated database and failed
/// against a freshly-created one, which is the database CI has.
///
/// Deliberately one shared object rather than one per test, and deliberately
/// never dropped. `ui schema public` enumerates the whole schema, so a fixture
/// dropped at the end of one test is a relation that disappears underneath a
/// panel another test is still enumerating — the suite runs in parallel, and
/// that race fails as `relation ... does not exist`. A single stable object
/// races with nothing.
fn ensure_fixture_object() {
    static FIXTURE: OnceLock<()> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let sql = "create table if not exists afpsql_ui_fixture(id int primary key, label text)";
        let output = Command::new(binary())
            .args(["--dsn", &test_env::required_test_dsn()])
            .args(["--permission", "write"])
            .args(["--sql", sql])
            .output()
            .expect("run fixture sql");
        assert!(
            output.status.success(),
            "fixture statement failed: {sql}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    });
}

fn binary() -> PathBuf {
    let exe = std::env::current_exe().expect("current exe");
    exe.parent()
        .and_then(Path::parent)
        .expect("target debug dir")
        .join("afpsql")
}

/// AFUI's content fingerprint: blake3 per file, keyed by relative path.
fn fingerprint(root: &Path) -> String {
    fn walk(root: &Path, directory: &Path, files: &mut BTreeMap<String, String>) {
        for entry in fs::read_dir(directory).expect("read frontend directory") {
            let path = entry.expect("frontend entry").path();
            if path.is_dir() {
                walk(root, &path, files);
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .expect("frontend file is inside the frontend")
                .to_string_lossy()
                .replace('\\', "/");
            files.insert(
                relative,
                blake3::hash(&fs::read(&path).expect("read frontend file"))
                    .to_hex()
                    .to_string(),
            );
        }
    }
    let mut files = BTreeMap::new();
    walk(root, root, &mut files);
    let mut hasher = blake3::Hasher::new();
    for (path, hash) in files {
        hasher.update(path.as_bytes());
        hasher.update(&[0]);
        hasher.update(hash.as_bytes());
        hasher.update(&[0]);
    }
    hasher.finalize().to_hex().to_string()
}

/// The whole lifecycle of a watch panel, in the order a person lives it.
///
/// One test rather than six, because each step is the previous step's workspace
/// one edit later: what makes the trust gate meaningful is that the same
/// directory served afpsql's own page a moment earlier.
#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn a_user_frontend_serves_a_panel_only_once_it_is_installed_compatible_and_trusted() {
    let panel = Panel::new("lifecycle");
    let open = || panel.open(&["ui", "schema", "public"], &[]);

    // 1. Nothing installed: afpsql's own panel and afpsql's own stylesheet,
    //    and nothing claimed otherwise.
    let builtin = open();
    assert_eq!(builtin.status, 0, "{:?}", builtin.events);
    assert!(builtin.is_builtin_page(), "{}", builtin.page);
    assert!(builtin.style.contains("color-scheme"), "{}", builtin.style);
    assert!(
        builtin
            .ready()
            .get("ui_frontend_id")
            .is_none_or(Value::is_null)
    );

    // 2. Installed but not trusted: still afpsql's. A workspace frontend is
    //    inert until someone says otherwise, and the readiness event is where
    //    an agent can see that it is not serving.
    panel.install(
        "schema_inspect",
        "1",
        &[
            ("templates/page.html.j2", CUSTOM_INSPECT),
            ("style.css", CUSTOM_STYLE),
        ],
    );
    let untrusted = open();
    assert_eq!(untrusted.status, 0, "{:?}", untrusted.events);
    assert!(untrusted.is_builtin_page(), "{}", untrusted.page);
    assert_eq!(untrusted.style, builtin.style);
    assert!(
        untrusted
            .ready()
            .get("ui_frontend_id")
            .is_none_or(Value::is_null)
    );

    // 3. Trusted: the person's own structure is what a browser receives — a
    //    definition list where afpsql had a table, and their stylesheet.
    panel.trust("schema_inspect");
    let trusted = open();
    assert_eq!(trusted.status, 0, "{:?}", trusted.events);
    assert!(
        trusted.page.contains("MY OWN SCHEMA PANEL"),
        "{}",
        trusted.page
    );
    assert!(trusted.page.contains("data-my-panel"), "{}", trusted.page);
    // Structure, not colour: afpsql's table is gone from the page entirely.
    assert!(
        !trusted.page.contains("<span class=\"name\">"),
        "{}",
        trusted.page
    );
    assert!(
        trusted.page.contains("<dl class=\"mine\">"),
        "{}",
        trusted.page
    );
    assert_ne!(trusted.page, untrusted.page, "the override changed nothing");
    assert_eq!(trusted.style, CUSTOM_STYLE);
    assert_ne!(trusted.style, builtin.style);
    assert_eq!(trusted.ready()["ui_frontend_id"], "my_psql_panel");
    // The frame is still afpsql's, because the override did not replace it:
    // per file, not per directory.
    assert!(
        trusted
            .page
            .contains("<link rel=\"stylesheet\" href=\"style.css\">")
    );
    // And a panel with no decision on it still loads no script at all.
    assert!(!trusted.page.contains("<script"), "{}", trusted.page);

    // 4. Edited after being trusted: the fingerprint no longer matches, so the
    //    frontend is inert again and afpsql's panel is back.
    fs::write(
        panel
            .frontend_root("schema_inspect")
            .join("templates/page.html.j2"),
        CUSTOM_INSPECT.replace("MY OWN SCHEMA PANEL", "EDITED AFTER TRUST"),
    )
    .expect("edit the trusted frontend");
    let edited = open();
    assert_eq!(edited.status, 0, "{:?}", edited.events);
    assert!(
        !edited.page.contains("EDITED AFTER TRUST"),
        "{}",
        edited.page
    );
    assert!(edited.is_builtin_page(), "{}", edited.page);
    assert_eq!(edited.style, builtin.style);

    // 5. Safe mode with a trusted frontend: afpsql's panel, no questions asked.
    panel.trust("schema_inspect");
    let safe = panel.open(&["ui", "schema", "public"], &[("AFUI_SAFE_MODE", "1")]);
    assert_eq!(safe.status, 0, "{:?}", safe.events);
    assert!(safe.is_builtin_page(), "{}", safe.page);
    assert_eq!(safe.style, builtin.style);

    // …and the same frontend still serves when safe mode is not set, so step 5
    // proved safe mode rather than another revoked fingerprint.
    assert!(open().page.contains("EDITED AFTER TRUST"));
}

/// 6. The one behaviour a fallback would destroy.
///
/// A frontend afpsql cannot use is an error naming safe mode. It is never a
/// quietly substituted afpsql page, because that is indistinguishable from the
/// override having worked.
#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn an_incompatible_frontend_is_an_error_naming_safe_mode_and_never_a_quiet_builtin_page() {
    let panel = Panel::new("incompatible");
    panel.install(
        "schema_inspect",
        "99",
        &[("templates/page.html.j2", CUSTOM_INSPECT)],
    );
    panel.trust("schema_inspect");

    let drive = panel.open(&["ui", "schema", "public"], &[]);
    assert_eq!(drive.status, 1, "{:?}", drive.events);
    assert!(
        drive.page.is_empty() && !drive.opened,
        "no window may open onto a panel afpsql could not load"
    );
    let error = drive.error();
    assert_eq!(error["code"], "ui_frontend_incompatible");
    assert!(
        error["message"]
            .as_str()
            .unwrap_or_default()
            .contains("ui_api_version 99"),
        "{error}"
    );
    assert!(
        error["hint"]
            .as_str()
            .unwrap_or_default()
            .contains("AFUI_SAFE_MODE=1"),
        "{error}"
    );

    // Safe mode is the documented way out, and it works on exactly this
    // workspace without touching the frontend.
    let safe = panel.open(&["ui", "schema", "public"], &[("AFUI_SAFE_MODE", "1")]);
    assert_eq!(safe.status, 0, "{:?}", safe.events);
    assert!(safe.is_builtin_page(), "{}", safe.page);
}

/// A frontend that will not parse fails the same way, for the same reason.
#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn an_unreadable_frontend_manifest_is_an_error_rather_than_a_fallback() {
    let panel = Panel::new("unreadable");
    let root = panel.install(
        "schema_inspect",
        "1",
        &[("templates/page.html.j2", CUSTOM_INSPECT)],
    );
    fs::write(root.join("frontend.json"), "{ not json").expect("break the manifest");
    panel.trust("schema_inspect");

    let drive = panel.open(&["ui", "schema", "public"], &[]);
    assert_eq!(drive.status, 1, "{:?}", drive.events);
    assert!(!drive.opened, "{}", drive.page);
    assert_eq!(drive.error()["code"], "ui_frontend_unreadable");
}

/// `ui_kind` is the override key, so replacing one panel replaces one panel.
#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn a_frontend_replaces_exactly_the_panel_it_was_installed_for() {
    let panel = Panel::new("per-kind");
    panel.install(
        "table_inspect",
        "1",
        &[("templates/page.html.j2", CUSTOM_INSPECT)],
    );
    panel.trust("table_inspect");

    let drive = panel.open(&["ui", "schema", "public"], &[]);
    assert_eq!(drive.status, 0, "{:?}", drive.events);
    assert!(drive.is_builtin_page(), "{}", drive.page);
    assert!(
        drive
            .ready()
            .get("ui_frontend_id")
            .is_none_or(Value::is_null)
    );
}

/// An override may not ship behaviour, by file name or by content.
#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn a_frontend_that_tries_to_ship_javascript_is_refused_rather_than_served() {
    let panel = Panel::new("script");
    panel.install(
        "schema_inspect",
        "1",
        &[(
            "templates/page.html.j2",
            "{% extends \"layout.html.j2\" %}{% block panel %}\
             <script>fetch('/approve',{method:'POST'})</script>{% endblock %}",
        )],
    );
    panel.trust("schema_inspect");

    let drive = panel.open(&["ui", "schema", "public"], &[]);
    assert_eq!(drive.status, 1, "{:?}", drive.events);
    assert!(!drive.opened);
    assert_eq!(drive.error()["code"], "ui_frontend_unsafe");
}

// ── the decide panel ────────────────────────
//
// The panel where being able to restructure the page matters most, and where
// what the page may decide matters most.

/// A person's own confirm page — reordered, regrouped, with afpsql's parameter
/// list dropped and the two controls in the opposite order — still answers
/// through afpsql.
#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn a_decide_panel_can_be_restructured_and_still_cannot_write_its_own_semantics() {
    let panel = Panel::new("decide");
    let custom_plan = "{% extends \"layout.html.j2\" %}\n\
{% block panel %}\n\
<article data-my-plan><h2>MY OWN CONFIRM PAGE</h2>\n\
<p class=\"where\">{{ document.target }}</p>\n\
<pre>{{ document.sql }}</pre>\n\
<nav>{% for decision in document.decisions | reverse %}\
<button type=\"button\" data-afpsql-decision=\"{{ decision.id }}\">{{ decision.label }}</button>\
{% endfor %}</nav>\n\
</article>\n\
{% endblock %}\n";
    panel.install(
        "plan_confirm",
        "1",
        &[("templates/page.html.j2", custom_plan)],
    );
    panel.trust("plan_confirm");

    let drive = panel.open(&["ui", "plan", "--sql", "select 1 as one"], &[]);
    // The stub closes the window without answering, which is a refusal.
    assert_eq!(drive.status, 0, "{:?}", drive.events);
    assert!(drive.page.contains("MY OWN CONFIRM PAGE"), "{}", drive.page);
    assert!(drive.page.contains("data-my-plan"), "{}", drive.page);
    // afpsql's own parameter list is gone: this is structure, not colour.
    assert!(!drive.page.contains("class=\"details\""), "{}", drive.page);
    // Both answers are still declared, and the runtime that binds them is
    // afpsql's, admitted by a nonce the frontend has no way to know.
    assert!(drive.page.contains("data-afpsql-decision=\"approve\""));
    assert!(drive.page.contains("data-afpsql-decision=\"refuse\""));
    assert!(drive.page.contains("<script nonce=\""), "{}", drive.page);
    assert!(
        drive.page.contains("form.action = action"),
        "{}",
        drive.page
    );
    assert_eq!(drive.ready()["ui_frontend_id"], "my_psql_panel");

    // Closing the window is not consent, whatever the page said.
    let result = drive
        .events
        .iter()
        .find(|event| event["kind"] == "result")
        .expect("a result")["result"]
        .clone();
    assert_eq!(result["decision"], "refused");
    assert_eq!(result["executed"], false);
    assert_eq!(result["ending"], "window_closed");
}

/// A decide page that declares no control is a broken override, and is reported
/// as one rather than opened as a question nobody can answer.
#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires PostgreSQL test database"
)]
#[test]
fn a_decide_panel_with_no_declared_control_is_a_broken_override_not_a_silent_window() {
    let panel = Panel::new("decide-empty");
    panel.install(
        "plan_confirm",
        "1",
        &[(
            "templates/page.html.j2",
            "{% extends \"layout.html.j2\" %}{% block panel %}\
             <p>Trust me, it is fine.</p>{% endblock %}",
        )],
    );
    panel.trust("plan_confirm");

    let drive = panel.open(&["ui", "plan", "--sql", "select 1 as one"], &[]);
    assert_eq!(drive.status, 1, "{:?}", drive.events);
    assert!(
        !drive.opened,
        "no window may open onto an unanswerable question"
    );
    let error = drive.error();
    assert_eq!(error["code"], "ui_frontend_incomplete");
    assert!(
        error["message"]
            .as_str()
            .unwrap_or_default()
            .contains("data-afpsql-decision"),
        "{error}"
    );
}
