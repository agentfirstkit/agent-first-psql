#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Connection resolution against a controlled process environment.
//!
//! Every test that mutates environment variables lives here rather than in the
//! shared lib-test binary. `resolve_pg_config` reads those variables directly,
//! and the lib binary runs readers of them in parallel without taking a lock,
//! so a mutation there races whatever else happens to be running. Tests in this
//! file take `env_lock()` so they serialize against each other.

use agent_first_psql::conn::{libpq_env_fallbacks_in_use, resolve_pg_config};
use agent_first_psql::types::SessionConfig;
use tokio_postgres::config::{Host, SslMode};

#[path = "support/env.rs"]
mod test_env;

const CONNECTION_VARS: [&str; 13] = [
    "PGHOST",
    "PGPORT",
    "PGUSER",
    "PGDATABASE",
    "PGPASSWORD",
    "PGSSLMODE",
    "AFPSQL_HOST",
    "AFPSQL_PORT",
    "AFPSQL_USER",
    "AFPSQL_DBNAME",
    "AFPSQL_PASSWORD_SECRET",
    "AFPSQL_DSN_SECRET",
    "AFPSQL_CONNINFO_SECRET",
];

/// Clear every connection variable, apply `values`, and restore on the way out.
fn with_connection_env<T>(values: &[(&str, &str)], body: impl FnOnce() -> T) -> T {
    let _guard = test_env::env_lock();
    let saved: Vec<_> = CONNECTION_VARS
        .iter()
        .map(|name| (*name, std::env::var_os(name)))
        .collect();
    for name in CONNECTION_VARS {
        // SAFETY: the shared test environment lock is held for this mutation.
        unsafe { std::env::remove_var(name) };
    }
    for (name, value) in values {
        // SAFETY: the shared test environment lock is held for this mutation.
        unsafe { std::env::set_var(name, value) };
    }

    let result = body();

    for (name, value) in saved {
        match value {
            // SAFETY: the shared test environment lock is still held here.
            Some(value) => unsafe { std::env::set_var(name, value) },
            // SAFETY: the shared test environment lock is still held here.
            None => unsafe { std::env::remove_var(name) },
        }
    }
    result
}

#[test]
fn locked_profile_endpoint_is_not_redirected_by_environment() {
    let hostile = [
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

    let (pinned, unpinned) = with_connection_env(&hostile, || {
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

        let unpinned = resolve_pg_config(&SessionConfig {
            host: Some("db.internal".to_string()),
            ..Default::default()
        })
        .expect("unpinned config resolves");

        (pinned, unpinned)
    });

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
    assert_eq!(
        unpinned.get_dbname(),
        Some("loot"),
        "the environment was not actually in effect, so the pinned case proves nothing"
    );
}

#[test]
fn resolve_conn_uses_pgpassword_fallback() {
    let out = with_connection_env(&[("PGPASSWORD", "pgpass-test")], || {
        resolve_pg_config(&SessionConfig {
            host: Some("localhost".to_string()),
            user: Some("roger".to_string()),
            dbname: Some("postgres".to_string()),
            ..Default::default()
        })
    })
    .expect("config resolves");
    assert_eq!(out.get_password(), Some("pgpass-test".as_bytes()));
}

#[test]
fn libpq_env_fallbacks_lists_only_unfilled_pg_vars() {
    let (only_explicit, with_defaults, with_dsn) =
        with_connection_env(&[("PGHOST", "envhost"), ("PGPASSWORD", "envpw")], || {
            let only_explicit = libpq_env_fallbacks_in_use(&SessionConfig {
                host: Some("explicit".to_string()),
                password_secret: Some("explicit-secret".to_string()),
                ..Default::default()
            });
            let with_defaults = libpq_env_fallbacks_in_use(&SessionConfig::default());
            let with_dsn = libpq_env_fallbacks_in_use(&SessionConfig {
                dsn_secret: Some("postgresql://u:p@h/db".to_string()),
                ..Default::default()
            });
            (only_explicit, with_defaults, with_dsn)
        });

    assert!(
        only_explicit.is_empty(),
        "explicit fields must suppress fallback report, got {only_explicit:?}"
    );
    assert!(with_defaults.contains(&"PGHOST"));
    assert!(with_defaults.contains(&"PGPASSWORD"));
    assert!(
        with_dsn.is_empty(),
        "dsn_secret short-circuits libpq fallback reporting, got {with_dsn:?}"
    );
}
