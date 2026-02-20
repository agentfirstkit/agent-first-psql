use super::*;

#[test]
fn resolve_conn_uses_dsn_secret_first() {
    let cfg = SessionConfig {
        dsn_secret: Some("postgresql://a/b".to_string()),
        ..Default::default()
    };
    let out = resolve_conn_string(&cfg).unwrap();
    assert_eq!(out, "postgresql://a/b");
}

#[test]
fn resolve_conn_from_conninfo() {
    let cfg = SessionConfig {
        conninfo_secret: Some("host=localhost port=5432 user=roger dbname=postgres".to_string()),
        ..Default::default()
    };
    let out = resolve_conn_string(&cfg).unwrap();
    assert_eq!(out, "postgresql://roger@localhost:5432/postgres");
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
    let out = resolve_conn_string(&cfg).unwrap();
    assert_eq!(out, "postgresql://u:p@db:6543/d");
}

#[test]
fn resolve_session_name_default_and_requested() {
    let cfg = RuntimeConfig::default();
    assert_eq!(resolve_session_name(&cfg, None), "default");
    assert_eq!(resolve_session_name(&cfg, Some("s1")), "s1");
}

#[test]
fn resolve_conn_defaults_and_conninfo_password() {
    let cfg = SessionConfig::default();
    let out = resolve_conn_string(&cfg).unwrap();
    assert!(out.starts_with("postgresql://"));

    let cfg2 = SessionConfig {
        conninfo_secret: Some("host=localhost user=roger password=pw".to_string()),
        ..Default::default()
    };
    let out2 = resolve_conn_string(&cfg2).unwrap();
    assert_eq!(out2, "postgresql://roger:pw@localhost:5432/postgres");

    let cfg3 = SessionConfig {
        conninfo_secret: Some("host=localhost noeq user=roger password=pw".to_string()),
        ..Default::default()
    };
    assert!(resolve_conn_string(&cfg3).is_err());

    let cfg4 = SessionConfig {
        conninfo_secret: Some("host=/tmp user=roger dbname=postgres".to_string()),
        ..Default::default()
    };
    let out4 = resolve_conn_string(&cfg4).unwrap();
    assert_eq!(out4, "postgresql://roger@127.0.0.1:5432/postgres");
}
