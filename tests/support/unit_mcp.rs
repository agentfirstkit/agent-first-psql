use super::*;
use rmcp::ServerHandler;

fn test_app() -> Arc<crate::handler::App> {
    let config = RuntimeConfig::default();
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    Arc::new(crate::handler::App::new(config, tx))
}

#[test]
fn get_info_exposes_tools_and_instructions() {
    let app = test_app();
    let mcp = AfpsqlMcp::new(app);
    let info = mcp.get_info();
    assert!(info.instructions.is_some());
}

#[tokio::test]
async fn psql_config_returns_current_config() {
    let app = test_app();
    let mcp = AfpsqlMcp::new(app);

    let res = mcp
        .psql_config(rmcp::handler::server::wrapper::Parameters(
            PsqlConfigParams {
                default_session: None,
                sessions: None,
                inline_max_rows: None,
                inline_max_bytes: None,
                statement_timeout_ms: None,
                lock_timeout_ms: None,
                log: None,
            },
        ))
        .await;
    assert!(res.is_ok());
}

#[tokio::test]
async fn psql_config_applies_update() {
    let app = test_app();
    let mcp = AfpsqlMcp::new(app.clone());

    let res = mcp
        .psql_config(rmcp::handler::server::wrapper::Parameters(
            PsqlConfigParams {
                default_session: None,
                sessions: None,
                inline_max_rows: Some(42),
                inline_max_bytes: None,
                statement_timeout_ms: Some(5000),
                lock_timeout_ms: None,
                log: None,
            },
        ))
        .await;
    assert!(res.is_ok());

    let cfg = app.config.read().await;
    assert_eq!(cfg.inline_max_rows, 42);
    assert_eq!(cfg.statement_timeout_ms, 5000);
}
