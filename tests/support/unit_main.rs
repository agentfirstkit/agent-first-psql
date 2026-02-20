use super::*;

#[test]
fn has_session_override_false_for_empty() {
    assert!(!has_session_override(&SessionConfig::default()));
}

#[test]
fn has_session_override_true_for_host() {
    assert!(has_session_override(&SessionConfig {
        host: Some("localhost".to_string()),
        ..Default::default()
    }));
}
