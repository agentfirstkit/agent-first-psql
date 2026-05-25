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

#[test]
fn query_options_accept_permission_and_reject_removed_read_only() {
    let input = serde_json::from_value::<Input>(serde_json::json!({
        "code": "query",
        "id": "q1",
        "sql": "select 1",
        "options": {"permission": "write"}
    }));
    let permission = match input {
        Ok(Input::Query { options, .. }) => options.permission,
        _ => None,
    };
    assert_eq!(permission, Some(Permission::Write));

    let err = serde_json::from_value::<Input>(serde_json::json!({
        "code": "query",
        "id": "q1",
        "sql": "select 1",
        "options": {"read_only": true}
    }));
    assert!(err.is_err(), "removed read_only option should be rejected");
    if let Err(err) = err {
        assert!(err.to_string().contains("unknown field"));
    }
}

#[test]
fn input_rejects_unknown_protocol_fields() {
    let query = serde_json::from_value::<Input>(serde_json::json!({
        "code": "query",
        "id": "q1",
        "sessoin": "work",
        "sql": "select 1"
    }));
    assert_unknown_field(query, "sessoin");

    let cancel = serde_json::from_value::<Input>(serde_json::json!({
        "code": "cancel",
        "id": "q1",
        "extra": true
    }));
    assert_unknown_field(cancel, "extra");
}

#[test]
fn config_rejects_unknown_patch_fields() {
    let top_level = serde_json::from_value::<Input>(serde_json::json!({
        "code": "config",
        "inline_max_row": 10
    }));
    assert_unknown_field(top_level, "inline_max_row");

    let session_field = serde_json::from_value::<Input>(serde_json::json!({
        "code": "config",
        "sessions": {
            "default": {
                "password_secrect": "pw"
            }
        }
    }));
    assert_unknown_field(session_field, "password_secrect");
}

#[test]
fn session_config_rejects_unknown_fields() {
    let cfg = serde_json::from_value::<SessionConfig>(serde_json::json!({
        "host": "127.0.0.1",
        "password_secrect": "pw"
    }));
    assert_unknown_field(cfg, "password_secrect");
}

fn assert_unknown_field<T>(result: Result<T, serde_json::Error>, field: &str) {
    assert!(result.is_err(), "unknown field {field} should be rejected");
    if let Err(err) = result {
        let message = err.to_string();
        assert!(message.contains("unknown field"), "{message}");
        assert!(message.contains(field), "{message}");
    }
}
