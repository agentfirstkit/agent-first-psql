use super::*;
use tokio_postgres::config::{Host, SslMode};

#[test]
fn resolve_conn_uses_dsn_secret_first() {
    let cfg = SessionConfig {
        dsn_secret: Some("postgresql://a/b".to_string()),
        ..Default::default()
    };
    let out_res = resolve_pg_config(&cfg);
    assert!(out_res.is_ok());
    if let Ok(out) = out_res {
        assert_eq!(out.get_dbname(), Some("b"));
    }
}

#[test]
fn resolve_conn_from_conninfo() {
    let cfg = SessionConfig {
        conninfo_secret: Some("host=localhost port=5432 user=roger dbname=postgres".to_string()),
        ..Default::default()
    };
    let out_res = resolve_pg_config(&cfg);
    assert!(out_res.is_ok());
    if let Ok(out) = out_res {
        assert_eq!(out.get_user(), Some("roger"));
        assert_eq!(out.get_dbname(), Some("postgres"));
        assert_eq!(out.get_ports(), &[5432]);
        assert_eq!(out.get_hosts(), &[Host::Tcp("localhost".to_string())]);
    }
}

#[test]
fn resolve_conn_accepts_supported_sslmodes() {
    let cfg = SessionConfig {
        dsn_secret: Some("postgresql://localhost/postgres?sslmode=require".to_string()),
        ..Default::default()
    };
    let out = resolve_pg_config(&cfg);
    assert!(out.is_ok());
    if let Ok(out) = out {
        assert_eq!(out.get_ssl_mode(), SslMode::Require);
    }

    let cfg2 = SessionConfig {
        conninfo_secret: Some("host=localhost dbname=postgres sslmode=disable".to_string()),
        ..Default::default()
    };
    let out2 = resolve_pg_config(&cfg2);
    assert!(out2.is_ok());
    if let Ok(out2) = out2 {
        assert_eq!(out2.get_ssl_mode(), SslMode::Disable);
    }
}

#[test]
fn resolve_conn_rejects_unsupported_tls_options_with_hint() {
    let cfg = SessionConfig {
        dsn_secret: Some("postgresql://localhost/postgres?sslmode=verify-full".to_string()),
        ..Default::default()
    };
    let err = resolve_pg_config(&cfg);
    assert!(err.is_err());
    if let Err(err) = err {
        assert!(err.message().contains("unsupported dsn sslmode"));
        assert!(err
            .hint()
            .unwrap_or_default()
            .contains("disable, prefer, and require"));
    }

    let cfg2 = SessionConfig {
        conninfo_secret: Some("host=localhost sslrootcert=/tmp/root.crt".to_string()),
        ..Default::default()
    };
    let err2 = resolve_pg_config(&cfg2);
    assert!(err2.is_err());
    if let Err(err2) = err2 {
        assert!(err2.message().contains("sslrootcert"));
        assert!(err2
            .hint()
            .unwrap_or_default()
            .contains("verify-ca/verify-full"));
    }
}

#[test]
fn resolve_conn_from_discrete_fields() {
    let cfg = SessionConfig {
        host: Some("db".to_string()),
        port: Some(6543),
        user: Some("u".to_string()),
        dbname: Some("d".to_string()),
        password_secret: Some("p".to_string()),
        ..Default::default()
    };
    let out_res = resolve_pg_config(&cfg);
    assert!(out_res.is_ok());
    if let Ok(out) = out_res {
        assert_eq!(out.get_user(), Some("u"));
        assert_eq!(out.get_dbname(), Some("d"));
        assert_eq!(out.get_password(), Some("p".as_bytes()));
        assert_eq!(out.get_ports(), &[6543]);
        assert_eq!(out.get_hosts(), &[Host::Tcp("db".to_string())]);
    }
}

#[test]
fn resolve_conn_from_unix_socket_discrete_fields() {
    let cfg = SessionConfig {
        host: Some("/var/run/postgresql".to_string()),
        port: Some(5432),
        user: Some("roger".to_string()),
        dbname: Some("appdb".to_string()),
        ..Default::default()
    };
    let out_res = resolve_pg_config(&cfg);
    assert!(out_res.is_ok());
    if let Ok(out) = out_res {
        #[cfg(unix)]
        assert_eq!(
            out.get_hosts(),
            &[Host::Unix(std::path::PathBuf::from("/var/run/postgresql"))]
        );
        assert_eq!(out.get_ports(), &[5432]);
        assert_eq!(out.get_user(), Some("roger"));
        assert_eq!(out.get_dbname(), Some("appdb"));
    }
}

#[test]
fn resolve_session_name_default_and_requested() {
    let cfg = RuntimeConfig::default();
    assert_eq!(resolve_session_name(&cfg, None), "default");
    assert_eq!(resolve_session_name(&cfg, Some("s1")), "s1");
}

#[test]
fn resolve_conn_defaults_and_conninfo_password() {
    let cfg = SessionConfig {
        conninfo_secret: Some("host=localhost user=roger password=pw".to_string()),
        ..Default::default()
    };
    let out_res = resolve_pg_config(&cfg);
    assert!(out_res.is_ok());
    if let Ok(out) = out_res {
        assert_eq!(out.get_user(), Some("roger"));
        assert_eq!(out.get_password(), Some("pw".as_bytes()));
        assert_eq!(out.get_hosts(), &[Host::Tcp("localhost".to_string())]);
    }

    let cfg2 = SessionConfig {
        conninfo_secret: Some("host=localhost noeq user=roger password=pw".to_string()),
        ..Default::default()
    };
    assert!(resolve_pg_config(&cfg2).is_err());

    let cfg3 = SessionConfig {
        conninfo_secret: Some("host=/tmp user=roger dbname=postgres".to_string()),
        ..Default::default()
    };
    let out_res2 = resolve_pg_config(&cfg3);
    assert!(out_res2.is_ok());
    if let Ok(out2) = out_res2 {
        #[cfg(unix)]
        assert_eq!(
            out2.get_hosts(),
            &[Host::Unix(std::path::PathBuf::from("/tmp"))]
        );
        assert_eq!(out2.get_user(), Some("roger"));
        assert_eq!(out2.get_dbname(), Some("postgres"));
    }
}

#[test]
fn resolve_conn_discrete_fields_are_not_conninfo_or_url_interpolated() {
    let cfg = SessionConfig {
        host: Some("db host".to_string()),
        port: Some(6543),
        user: Some("u@x".to_string()),
        dbname: Some("d/name".to_string()),
        password_secret: Some("p@ ss/word".to_string()),
        ..Default::default()
    };
    let out_res = resolve_pg_config(&cfg);
    assert!(out_res.is_ok());
    if let Ok(out) = out_res {
        assert_eq!(out.get_hosts(), &[Host::Tcp("db host".to_string())]);
        assert_eq!(out.get_user(), Some("u@x"));
        assert_eq!(out.get_dbname(), Some("d/name"));
        assert_eq!(out.get_password(), Some("p@ ss/word".as_bytes()));
    }
}

#[test]
fn resolve_conn_uses_pgpassword_fallback() {
    let old = std::env::var_os("PGPASSWORD");
    std::env::set_var("PGPASSWORD", "pgpass-test");

    let out_res = resolve_pg_config(&SessionConfig {
        host: Some("localhost".to_string()),
        user: Some("roger".to_string()),
        dbname: Some("postgres".to_string()),
        ..Default::default()
    });

    match old {
        Some(value) => std::env::set_var("PGPASSWORD", value),
        None => std::env::remove_var("PGPASSWORD"),
    }

    assert!(out_res.is_ok());
    if let Ok(out) = out_res {
        assert_eq!(out.get_password(), Some("pgpass-test".as_bytes()));
    }
}

#[test]
fn libpq_env_fallbacks_lists_only_unfilled_pg_vars() {
    let names = [
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
    let saved: Vec<_> = names.iter().map(|n| (*n, std::env::var_os(n))).collect();
    for n in &names {
        std::env::remove_var(n);
    }
    std::env::set_var("PGHOST", "envhost");
    std::env::set_var("PGPASSWORD", "envpw");

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

    for (n, value) in saved {
        match value {
            Some(v) => std::env::set_var(n, v),
            None => std::env::remove_var(n),
        }
    }

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
