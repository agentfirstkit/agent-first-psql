use super::*;

#[test]
fn runtime_config_default_has_default_session() {
    let cfg = RuntimeConfig::default();
    assert_eq!(cfg.default_session, "default");
    assert!(cfg.sessions.contains_key("default"));
}

#[test]
fn trace_only_duration_sets_optional_fields_none() {
    let t = Trace::only_duration(12);
    assert_eq!(t.duration_ms, 12);
    assert!(t.row_count.is_none());
    assert!(t.payload_bytes.is_none());
}
