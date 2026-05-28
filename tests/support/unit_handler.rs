use super::*;
use crate::db::{ConnectError, DbExecutor, ExecError, ExecOutcome, ExecRequest};
use async_trait::async_trait;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};

#[tokio::test]
async fn emit_rows_uses_db_columns_even_when_rows_empty() {
    let columns = vec![
        ColumnInfo {
            name: "a".to_string(),
            type_name: "int4".to_string(),
        },
        ColumnInfo {
            name: "b".to_string(),
            type_name: "text".to_string(),
        },
    ];
    let (tx, mut rx) = mpsc::channel(16);
    let app = Arc::new(App::new(RuntimeConfig::default(), tx));
    let opts = ResolvedOptions {
        stream_rows: false,
        batch_rows: 10,
        batch_bytes: 1024,
        statement_timeout_ms: 100,
        lock_timeout_ms: 100,
        read_only: false,
        inline_max_rows: 100,
        inline_max_bytes: 1000,
    };

    let status = emit_rows_result(
        &app,
        Some("q_empty".to_string()),
        Some("default".to_string()),
        columns.clone(),
        vec![],
        std::time::Instant::now(),
        &opts,
    )
    .await;
    assert!(matches!(status, RowEmitStatus::Sent { .. }));
    let out_opt = rx.recv().await;
    assert!(out_opt.is_some());
    if let Some(out) = out_opt {
        assert!(matches!(out, Output::Result { .. }));
        if let Output::Result { columns: got, .. } = out {
            assert_eq!(got.len(), columns.len());
        }
    }
}

#[tokio::test]
async fn emit_rows_result_paths() {
    let (tx, mut rx) = mpsc::channel(64);
    let app = Arc::new(App::new(RuntimeConfig::default(), tx));

    let stream_opts = ResolvedOptions {
        stream_rows: true,
        batch_rows: 2,
        batch_bytes: 1024,
        statement_timeout_ms: 100,
        lock_timeout_ms: 100,
        read_only: false,
        inline_max_rows: 100,
        inline_max_bytes: 100000,
    };
    let status = emit_rows_result(
        &app,
        Some("q1".to_string()),
        Some("default".to_string()),
        vec![ColumnInfo {
            name: "n".to_string(),
            type_name: "int4".to_string(),
        }],
        vec![
            serde_json::json!({"n":1}),
            serde_json::json!({"n":2}),
            serde_json::json!({"n":3}),
        ],
        std::time::Instant::now(),
        &stream_opts,
    )
    .await;
    assert!(matches!(status, RowEmitStatus::Sent { .. }));
    while rx.try_recv().is_ok() {}

    let inline_opts = ResolvedOptions {
        stream_rows: false,
        batch_rows: 100,
        batch_bytes: 1024,
        statement_timeout_ms: 100,
        lock_timeout_ms: 100,
        read_only: false,
        inline_max_rows: 1,
        inline_max_bytes: 10000,
    };
    let status = emit_rows_result(
        &app,
        Some("q2".to_string()),
        Some("default".to_string()),
        vec![ColumnInfo {
            name: "n".to_string(),
            type_name: "int4".to_string(),
        }],
        vec![serde_json::json!({"n":1}), serde_json::json!({"n":2})],
        std::time::Instant::now(),
        &inline_opts,
    )
    .await;
    assert!(matches!(status, RowEmitStatus::TooLarge { .. }));
}

struct MockExecutor {
    result: Mutex<Option<Result<ExecOutcome, ExecError>>>,
}

#[async_trait]
impl DbExecutor for MockExecutor {
    async fn execute(&self, _req: ExecRequest<'_>) -> Result<ExecOutcome, ExecError> {
        self.result
            .lock()
            .await
            .take()
            .unwrap_or(Ok(ExecOutcome::Command { affected: 0 }))
    }
}

fn test_app_with_executor(
    cfg: RuntimeConfig,
    result: Result<ExecOutcome, ExecError>,
) -> (Arc<App>, mpsc::Receiver<Output>) {
    let (tx, rx) = mpsc::channel(64);
    let app = Arc::new(App {
        config: RwLock::new(cfg),
        executor: Arc::new(MockExecutor {
            result: Mutex::new(Some(result)),
        }),
        writer: tx,
        in_flight: Mutex::new(std::collections::HashMap::new()),
        requests_total: AtomicU64::new(0),
        start_time: std::time::Instant::now(),
    });
    (app, rx)
}

#[tokio::test]
async fn session_info_returns_resolved_defaults_for_direct_transport() {
    let mut cfg = RuntimeConfig::default();
    cfg.sessions.insert(
        "default".to_string(),
        SessionConfig {
            host: Some("127.0.0.1".to_string()),
            port: Some(5432),
            ..Default::default()
        },
    );
    let (app, mut rx) = test_app_with_executor(cfg, Ok(ExecOutcome::Command { affected: 0 }));
    handle_session_info(
        &app,
        Some("info-1".to_string()),
        Some("default".to_string()),
    )
    .await;
    let msg = rx.recv().await;
    assert!(msg.is_some(), "expected SessionInfo response");
    assert!(
        matches!(msg, Some(Output::SessionInfo { .. })),
        "expected Output::SessionInfo, got {msg:?}"
    );
    let Some(Output::SessionInfo {
        id,
        session,
        transport_kind,
        permission_default,
        stream_rows_default,
        inline_max_rows,
        inline_max_bytes,
        batch_rows,
        batch_bytes,
        ..
    }) = msg
    else {
        return;
    };
    assert_eq!(id.as_deref(), Some("info-1"));
    assert_eq!(session, "default");
    assert_eq!(transport_kind, "direct");
    assert_eq!(permission_default, "read");
    assert!(!stream_rows_default);
    assert!(inline_max_rows > 0);
    assert!(inline_max_bytes > 0);
    assert!(batch_rows > 0);
    assert!(batch_bytes > 0);
}

#[tokio::test]
async fn session_info_reports_ssh_and_container_transports() {
    let mut cfg = RuntimeConfig::default();
    cfg.sessions.insert(
        "via_ssh".to_string(),
        SessionConfig {
            ssh: SshConfig {
                destination: Some("user@bastion".to_string()),
                ..Default::default()
            },
            host: Some("127.0.0.1".to_string()),
            port: Some(5432),
            ..Default::default()
        },
    );
    cfg.sessions.insert(
        "via_container".to_string(),
        SessionConfig {
            container: ContainerConfig {
                target: Some("pg".to_string()),
                ..Default::default()
            },
            host: Some("127.0.0.1".to_string()),
            port: Some(5432),
            ..Default::default()
        },
    );
    let (app, mut rx) = test_app_with_executor(cfg, Ok(ExecOutcome::Command { affected: 0 }));

    handle_session_info(&app, None, Some("via_ssh".to_string())).await;
    let ssh_msg = rx.recv().await;
    assert!(
        matches!(ssh_msg, Some(Output::SessionInfo { .. })),
        "expected SessionInfo for ssh session, got {ssh_msg:?}"
    );
    let Some(Output::SessionInfo {
        transport_kind,
        permission_default,
        ..
    }) = ssh_msg
    else {
        return;
    };
    assert_eq!(transport_kind, "ssh");
    assert_eq!(permission_default, "ssh-read");

    handle_session_info(&app, None, Some("via_container".to_string())).await;
    let container_msg = rx.recv().await;
    assert!(
        matches!(container_msg, Some(Output::SessionInfo { .. })),
        "expected SessionInfo for container session, got {container_msg:?}"
    );
    let Some(Output::SessionInfo {
        transport_kind,
        permission_default,
        ..
    }) = container_msg
    else {
        return;
    };
    assert_eq!(transport_kind, "container");
    assert_eq!(permission_default, "container-read");
}

#[tokio::test]
async fn session_info_unknown_session_emits_invalid_request_with_hint() {
    let cfg = RuntimeConfig::default();
    let (app, mut rx) = test_app_with_executor(cfg, Ok(ExecOutcome::Command { affected: 0 }));
    handle_session_info(
        &app,
        Some("info-x".to_string()),
        Some("missing".to_string()),
    )
    .await;
    let msg = rx.recv().await;
    assert!(
        matches!(msg, Some(Output::Error { .. })),
        "expected Output::Error, got {msg:?}"
    );
    let Some(Output::Error {
        id,
        error_code,
        error,
        hint,
        retryable,
        ..
    }) = msg
    else {
        return;
    };
    assert_eq!(id.as_deref(), Some("info-x"));
    assert_eq!(error_code, "invalid_request");
    assert!(error.contains("unknown session"));
    assert!(hint.is_some_and(|h| h.contains("config")));
    assert!(!retryable);
}

#[tokio::test]
async fn execute_query_unknown_session_emits_connect_failed() {
    let cfg = RuntimeConfig {
        default_session: "missing".to_string(),
        ..Default::default()
    };
    let (app, mut rx) = test_app_with_executor(cfg, Ok(ExecOutcome::Command { affected: 1 }));
    execute_query(
        &app,
        Some("q1".to_string()),
        Some("missing".to_string()),
        "select 1".to_string(),
        vec![],
        QueryOptions::default(),
        None,
    )
    .await;
    let msg_opt = rx.recv().await;
    assert!(msg_opt.is_some());
    if let Some(msg) = msg_opt {
        assert!(matches!(msg, Output::Error { .. }));
        if let Output::Error { error_code, .. } = msg {
            assert_eq!(error_code, "connect_failed");
        }
    }
}

#[tokio::test]
async fn execute_query_maps_executor_outcomes() {
    let mut cfg = RuntimeConfig::default();
    cfg.sessions
        .insert("default".to_string(), SessionConfig::default());

    for result in [
        Ok(ExecOutcome::Rows {
            columns: vec![ColumnInfo {
                name: "n".to_string(),
                type_name: "int4".to_string(),
            }],
            rows: vec![serde_json::json!({"n":1})],
        }),
        Ok(ExecOutcome::Command { affected: 2 }),
        Err(ExecError::Connect(Box::new(ConnectError::new("down")))),
        Err(ExecError::Config {
            message: "unsupported sslmode".to_string(),
            hint: Some("use sslmode=require".to_string()),
        }),
        Err(ExecError::InvalidParams("bad".to_string())),
        Err(ExecError::ResultTooLarge {
            row_count: 2,
            payload_bytes: 200,
        }),
        Err(ExecError::Sql {
            sqlstate: "22023".to_string(),
            message: "bad".to_string(),
            detail: None,
            hint: None,
            position: None,
        }),
        Err(ExecError::Internal("boom".to_string())),
    ] {
        let (app, mut rx) = test_app_with_executor(cfg.clone(), result);
        execute_query(
            &app,
            Some("q1".to_string()),
            Some("default".to_string()),
            "select 1".to_string(),
            vec![],
            QueryOptions::default(),
            None,
        )
        .await;
        let msg_opt = rx.recv().await;
        assert!(msg_opt.is_some());
    }
}

#[tokio::test]
async fn execute_query_emits_structured_connect_error_details() {
    let mut cfg = RuntimeConfig::default();
    cfg.sessions
        .insert("default".to_string(), SessionConfig::default());
    let (app, mut rx) = test_app_with_executor(
        cfg,
        Err(ExecError::Connect(Box::new(ConnectError {
            error: "connect failed: role \"root\" does not exist".to_string(),
            sqlstate: Some("28000".to_string()),
            message: Some("role \"root\" does not exist".to_string()),
            detail: Some("connection matched pg_hba peer rule".to_string()),
            hint: Some("try --user postgres or configure peer auth".to_string()),
            retryable: false,
        }))),
    );

    execute_query(
        &app,
        Some("q1".to_string()),
        Some("default".to_string()),
        "select 1".to_string(),
        vec![],
        QueryOptions::default(),
        None,
    )
    .await;

    let msg_opt = rx.recv().await;
    assert!(matches!(msg_opt, Some(Output::ConnectError { .. })));
    if let Some(Output::ConnectError {
        error_code,
        sqlstate,
        message,
        detail,
        hint,
        retryable,
        ..
    }) = msg_opt
    {
        assert_eq!(error_code, "connect_failed");
        assert_eq!(sqlstate.as_deref(), Some("28000"));
        assert_eq!(message.as_deref(), Some("role \"root\" does not exist"));
        assert_eq!(
            detail.as_deref(),
            Some("connection matched pg_hba peer rule")
        );
        assert!(hint
            .as_deref()
            .unwrap_or_default()
            .contains("--user postgres"));
        assert!(!retryable);
    }
}

#[tokio::test]
async fn execute_query_maps_executor_result_too_large() {
    let mut cfg = RuntimeConfig::default();
    cfg.sessions
        .insert("default".to_string(), SessionConfig::default());
    let (app, mut rx) = test_app_with_executor(
        cfg,
        Err(ExecError::ResultTooLarge {
            row_count: 3,
            payload_bytes: 300,
        }),
    );

    execute_query(
        &app,
        Some("q1".to_string()),
        Some("default".to_string()),
        "select 1".to_string(),
        vec![],
        QueryOptions::default(),
        None,
    )
    .await;

    let msg_opt = rx.recv().await;
    assert!(matches!(msg_opt, Some(Output::Error { .. })));
    if let Some(Output::Error {
        error_code,
        retryable,
        trace,
        ..
    }) = msg_opt
    {
        assert_eq!(error_code, "result_too_large");
        assert!(!retryable);
        assert_eq!(trace.row_count, Some(3));
        assert_eq!(trace.payload_bytes, Some(300));
    }
}

#[tokio::test]
async fn execute_query_rejects_permission_mismatched_to_transport() {
    let mut cfg = RuntimeConfig::default();
    cfg.sessions.insert(
        "default".to_string(),
        SessionConfig {
            ssh: SshConfig {
                destination: Some("user@example.com".to_string()),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let (app, mut rx) = test_app_with_executor(cfg, Ok(ExecOutcome::Command { affected: 1 }));

    execute_query(
        &app,
        Some("q1".to_string()),
        Some("default".to_string()),
        "select 1".to_string(),
        vec![],
        QueryOptions {
            permission: Some(Permission::Write),
            ..Default::default()
        },
        None,
    )
    .await;

    let msg_opt = rx.recv().await;
    assert!(matches!(msg_opt, Some(Output::Error { .. })));
    if let Some(Output::Error {
        error_code,
        error,
        hint,
        retryable,
        ..
    }) = msg_opt
    {
        assert_eq!(error_code, "invalid_request");
        assert!(error.contains("does not allow SSH transport"));
        let hint = hint.as_deref().unwrap_or_default();
        assert!(hint.contains("uses afpsql SSH transport"));
        assert!(hint.contains("ssh-write"));
        assert!(!retryable);
    }
}

#[tokio::test]
async fn execute_query_rejects_ssh_permission_without_ssh_hint() {
    let mut cfg = RuntimeConfig::default();
    cfg.sessions
        .insert("default".to_string(), SessionConfig::default());
    let (app, mut rx) = test_app_with_executor(cfg, Ok(ExecOutcome::Command { affected: 1 }));

    execute_query(
        &app,
        Some("q1".to_string()),
        Some("default".to_string()),
        "select 1".to_string(),
        vec![],
        QueryOptions {
            permission: Some(Permission::SshWrite),
            ..Default::default()
        },
        None,
    )
    .await;

    let msg_opt = rx.recv().await;
    assert!(matches!(msg_opt, Some(Output::Error { .. })));
    if let Some(Output::Error {
        error_code,
        error,
        hint,
        retryable,
        ..
    }) = msg_opt
    {
        assert_eq!(error_code, "invalid_request");
        assert!(error.contains("requires SSH transport"));
        let hint = hint.as_deref().unwrap_or_default();
        assert!(hint.contains("does not use afpsql SSH transport"));
        assert!(hint.contains("write"));
        assert!(!retryable);
    }
}

#[tokio::test]
async fn execute_query_rejects_permission_mismatched_to_container_transport() {
    let mut cfg = RuntimeConfig::default();
    cfg.sessions.insert(
        "default".to_string(),
        SessionConfig {
            container: ContainerConfig {
                target: Some("pg".to_string()),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let (app, mut rx) = test_app_with_executor(cfg, Ok(ExecOutcome::Command { affected: 1 }));

    execute_query(
        &app,
        Some("q1".to_string()),
        Some("default".to_string()),
        "select 1".to_string(),
        vec![],
        QueryOptions {
            permission: Some(Permission::Write),
            ..Default::default()
        },
        None,
    )
    .await;

    let msg_opt = rx.recv().await;
    assert!(matches!(msg_opt, Some(Output::Error { .. })));
    if let Some(Output::Error {
        error_code,
        error,
        hint,
        retryable,
        ..
    }) = msg_opt
    {
        assert_eq!(error_code, "invalid_request");
        assert!(error.contains("does not allow container transport"));
        let hint = hint.as_deref().unwrap_or_default();
        assert!(hint.contains("uses afpsql container transport"));
        assert!(hint.contains("container-write"));
        assert!(!retryable);
    }
}

#[tokio::test]
async fn execute_query_emits_config_hint() {
    let mut cfg = RuntimeConfig::default();
    cfg.sessions
        .insert("default".to_string(), SessionConfig::default());
    let (app, mut rx) = test_app_with_executor(
        cfg,
        Err(ExecError::Config {
            message: "unsupported dsn sslmode `verify-full`".to_string(),
            hint: Some("afpsql supports sslmode=disable, prefer, and require".to_string()),
        }),
    );
    execute_query(
        &app,
        Some("q1".to_string()),
        Some("default".to_string()),
        "select 1".to_string(),
        vec![],
        QueryOptions::default(),
        None,
    )
    .await;

    let msg_opt = rx.recv().await;
    assert!(matches!(msg_opt, Some(Output::Error { .. })));
    if let Some(Output::Error {
        error_code,
        error,
        hint,
        retryable,
        ..
    }) = msg_opt
    {
        assert_eq!(error_code, "invalid_request");
        assert!(error.contains("verify-full"));
        assert_eq!(
            hint.as_deref(),
            Some("afpsql supports sslmode=disable, prefer, and require")
        );
        assert!(!retryable);
    }
}
