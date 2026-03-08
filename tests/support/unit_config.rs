use super::*;
use std::collections::HashMap;

#[test]
fn apply_update_adds_default_session_if_missing() {
    let mut cfg = RuntimeConfig::default();
    cfg.apply_update(ConfigPatch {
        default_session: Some("other".to_string()),
        ..Default::default()
    });
    assert!(cfg.sessions.contains_key("other"));
}

#[test]
fn apply_update_merges_session_fields() {
    let mut cfg = RuntimeConfig::default();
    let mut sessions = HashMap::new();
    sessions.insert(
        "s1".to_string(),
        SessionConfigPatch {
            dsn_secret: PatchField::Value("postgresql://localhost/postgres".to_string()),
            conninfo_secret: PatchField::Value(
                "host=localhost user=roger dbname=postgres".to_string(),
            ),
            host: PatchField::Value("localhost".to_string()),
            port: PatchField::Value(5432),
            user: PatchField::Value("roger".to_string()),
            dbname: PatchField::Value("postgres".to_string()),
            password_secret: PatchField::Value("pw".to_string()),
        },
    );
    cfg.apply_update(ConfigPatch {
        inline_max_rows: Some(10),
        inline_max_bytes: Some(20),
        statement_timeout_ms: Some(30),
        lock_timeout_ms: Some(40),
        log: Some(vec!["a".to_string()]),
        sessions: Some(sessions),
        ..Default::default()
    });
    let maybe_s1 = cfg.sessions.get("s1");
    assert!(maybe_s1.is_some());
    if let Some(s1) = maybe_s1 {
        assert_eq!(
            s1.dsn_secret.as_deref(),
            Some("postgresql://localhost/postgres")
        );
        assert!(s1.conninfo_secret.is_some());
        assert_eq!(s1.host.as_deref(), Some("localhost"));
        assert_eq!(s1.port, Some(5432));
        assert_eq!(s1.user.as_deref(), Some("roger"));
        assert_eq!(s1.dbname.as_deref(), Some("postgres"));
        assert_eq!(s1.password_secret.as_deref(), Some("pw"));
    }
    assert_eq!(cfg.inline_max_rows, 10);
    assert_eq!(cfg.inline_max_bytes, 20);
    assert_eq!(cfg.statement_timeout_ms, 30);
    assert_eq!(cfg.lock_timeout_ms, 40);
    assert_eq!(cfg.log, vec!["a".to_string()]);
}

#[test]
fn apply_update_normalizes_log_categories() {
    let mut cfg = RuntimeConfig::default();
    cfg.apply_update(ConfigPatch {
        log: Some(vec![
            " Query.Result ".to_string(),
            "query.result".to_string(),
            "".to_string(),
            "ALL".to_string(),
        ]),
        ..Default::default()
    });
    assert_eq!(cfg.log, vec!["query.result".to_string(), "all".to_string()]);
}

#[test]
fn resolve_options_applies_defaults_and_overrides() {
    let cfg = RuntimeConfig::default();
    let resolved = cfg.resolve_options(&QueryOptions {
        stream_rows: true,
        batch_rows: Some(0),
        batch_bytes: Some(1),
        statement_timeout_ms: Some(1),
        lock_timeout_ms: Some(2),
        read_only: Some(true),
        inline_max_rows: Some(3),
        inline_max_bytes: Some(4),
    });
    assert!(resolved.stream_rows);
    assert_eq!(resolved.batch_rows, 1);
    assert_eq!(resolved.batch_bytes, 1024);
    assert_eq!(resolved.statement_timeout_ms, 1);
    assert_eq!(resolved.lock_timeout_ms, 2);
    assert!(resolved.read_only);
    assert_eq!(resolved.inline_max_rows, 3);
    assert_eq!(resolved.inline_max_bytes, 4);
}

#[test]
fn sessions_to_invalidate_collects_default_and_session_keys() {
    let mut sessions = HashMap::new();
    sessions.insert("s1".to_string(), SessionConfigPatch::default());
    sessions.insert("s2".to_string(), SessionConfigPatch::default());
    let patch = ConfigPatch {
        default_session: Some("s2".to_string()),
        sessions: Some(sessions),
        ..Default::default()
    };
    let names = sessions_to_invalidate(&patch);
    assert_eq!(names, vec!["s1".to_string(), "s2".to_string()]);
}

#[test]
fn apply_update_can_clear_session_fields_with_null() {
    let mut cfg = RuntimeConfig::default();
    cfg.sessions.insert(
        "s1".to_string(),
        SessionConfig {
            dsn_secret: Some("postgresql://localhost/postgres".to_string()),
            conninfo_secret: Some("host=localhost user=roger dbname=postgres".to_string()),
            host: Some("localhost".to_string()),
            port: Some(5432),
            user: Some("roger".to_string()),
            dbname: Some("postgres".to_string()),
            password_secret: Some("pw".to_string()),
        },
    );
    let mut sessions = HashMap::new();
    sessions.insert(
        "s1".to_string(),
        SessionConfigPatch {
            dsn_secret: PatchField::Null,
            conninfo_secret: PatchField::Null,
            host: PatchField::Null,
            port: PatchField::Null,
            user: PatchField::Null,
            dbname: PatchField::Null,
            password_secret: PatchField::Null,
        },
    );
    cfg.apply_update(ConfigPatch {
        sessions: Some(sessions),
        ..Default::default()
    });
    let maybe_s1 = cfg.sessions.get("s1");
    assert!(maybe_s1.is_some());
    if let Some(s1) = maybe_s1 {
        assert!(s1.dsn_secret.is_none());
        assert!(s1.conninfo_secret.is_none());
        assert!(s1.host.is_none());
        assert!(s1.port.is_none());
        assert!(s1.user.is_none());
        assert!(s1.dbname.is_none());
        assert!(s1.password_secret.is_none());
    }
}
