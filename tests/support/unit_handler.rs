use super::*;
use crate::db::{DbExecutor, ExecError, ExecOutcome};
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
    async fn execute(
        &self,
        _session_name: &str,
        _session_cfg: &SessionConfig,
        _sql: &str,
        _params: &[Value],
        _opts: &ResolvedOptions,
    ) -> Result<ExecOutcome, ExecError> {
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
        Err(ExecError::Connect("down".to_string())),
        Err(ExecError::InvalidParams("bad".to_string())),
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
        )
        .await;
        let msg_opt = rx.recv().await;
        assert!(msg_opt.is_some());
    }
}
