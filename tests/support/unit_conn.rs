use super::*;
use tokio_postgres::config::Host;

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
