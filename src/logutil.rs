use crate::config;
use crate::protocol::log_event;
use crate::types::{Output, Trace};

pub fn build_startup_log(
    session: Option<&str>,
    args: &serde_json::Value,
    env: &serde_json::Value,
) -> Output {
    Output::Log {
        event: log_event::STARTUP.to_string(),
        request_id: None,
        session: session.map(std::string::ToString::to_string),
        error_code: None,
        command_tag: None,
        version: Some(config::VERSION.to_string()),
        config: None,
        args: Some(args.clone()),
        env: Some(env.clone()),
        trace: Trace::only_duration(0),
    }
}
