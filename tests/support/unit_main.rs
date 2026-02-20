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

#[test]
fn build_startup_log_has_afdata_fields() {
    let cfg = RuntimeConfig::default();
    let out = build_startup_log(
        Some("default"),
        &cfg,
        &[
            "afpsql".to_string(),
            "--log".to_string(),
            "startup".to_string(),
        ],
        &serde_json::json!({"mode":"cli"}),
        &serde_json::json!({"AFPSQL_DSN_SECRET": null}),
    );
    match out {
        Output::Log {
            event,
            version,
            argv,
            config,
            args,
            env,
            ..
        } => {
            assert_eq!(event, "startup");
            assert!(version.is_some());
            assert!(argv.is_some());
            assert!(config.is_some());
            assert!(args.is_some());
            assert!(env.is_some());
        }
        _ => panic!("expected startup log"),
    }
}
