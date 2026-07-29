#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! A locked administrator profile must own the whole connection.
//!
//! This lives in its own integration binary because it mutates the process
//! environment: `resolve_pg_config` reads those variables directly, and the
//! shared lib-test binary runs readers of them in parallel.

use agent_first_psql::conn::resolve_pg_config;
use agent_first_psql::types::SessionConfig;
use tokio_postgres::config::{Host, SslMode};

const HOSTILE_ENV: [(&str, &str); 6] = [
    (
        "AFPSQL_DSN_SECRET",
        "postgresql://evil:pw@attacker.example/loot",
    ),
    ("PGHOST", "attacker.example"),
    ("PGPORT", "6543"),
    ("PGUSER", "evil"),
    ("PGDATABASE", "loot"),
    ("PGSSLMODE", "disable"),
];

#[test]
fn locked_profile_endpoint_is_not_redirected_by_environment() {
    for (name, value) in HOSTILE_ENV {
        // SAFETY: this test binary is the only user of its own environment.
        unsafe { std::env::set_var(name, value) };
    }

    // The administrator's profile: discrete fields, no DSN of its own.
    let pinned = resolve_pg_config(&SessionConfig {
        profile_pinned: true,
        host: Some("db.internal".to_string()),
        port: Some(5432),
        user: Some("app_reader".to_string()),
        dbname: Some("app".to_string()),
        ..Default::default()
    })
    .expect("pinned config resolves");

    assert_eq!(
        pinned.get_hosts(),
        &[Host::Tcp("db.internal".to_string())],
        "an environment DSN redirected a locked profile"
    );
    assert_eq!(pinned.get_ports(), &[5432]);
    assert_eq!(pinned.get_user(), Some("app_reader"));
    assert_eq!(pinned.get_dbname(), Some("app"));
    assert_eq!(
        pinned.get_ssl_mode(),
        SslMode::Prefer,
        "PGSSLMODE downgraded TLS on a locked profile"
    );

    // The ordinary readonly executable keeps its documented env fallbacks, so
    // the same environment must still reach the unpinned session. Without this
    // the assertions above could pass simply because nothing was set.
    let unpinned = resolve_pg_config(&SessionConfig {
        host: Some("db.internal".to_string()),
        ..Default::default()
    })
    .expect("unpinned config resolves");
    assert_eq!(
        unpinned.get_dbname(),
        Some("loot"),
        "the environment was not actually in effect, so the pinned case proves nothing"
    );
}
