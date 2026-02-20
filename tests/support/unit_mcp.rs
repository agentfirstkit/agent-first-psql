use super::*;

#[test]
fn tools_list_contains_expected_tools() {
    let list = tools_list();
    let text = list.to_string();
    assert!(text.contains("psql_query"));
    assert!(text.contains("psql_config"));
}

#[test]
fn tool_ok_and_error_shapes() {
    let ok = tool_ok(serde_json::json!({"k":"v"}));
    let err = tool_error("bad");
    assert_eq!(ok["isError"], false);
    assert_eq!(err["isError"], true);
}

#[test]
fn jsonrpc_wrappers() {
    let r = jsonrpc_result(serde_json::json!(1), serde_json::json!({"ok":true}));
    let e = jsonrpc_error(Some(serde_json::json!(2)), -1, "x".to_string());
    assert_eq!(r["jsonrpc"], "2.0");
    assert_eq!(e["error"]["code"], -1);
    assert_eq!(e["id"], 2);
}

#[test]
fn has_session_override_detects_values() {
    assert!(!has_session_override(&SessionConfig::default()));
    assert!(has_session_override(&SessionConfig {
        host: Some("localhost".to_string()),
        ..Default::default()
    }));
}
