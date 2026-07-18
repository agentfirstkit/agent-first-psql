use crate::cli::{
    SkillAdminAction, SkillAdminOptions, SkillAdminRequest, SkillAgentSelection, SkillScope,
};
use agent_first_data::skill::{
    self, SkillAction, SkillAgentSelection as AfSelection, SkillOptions, SkillScope as AfScope,
    SkillSpec,
};
use serde_json::Value;

const SPEC: SkillSpec = SkillSpec {
    name: "agent-first-psql",
    source: include_str!("../skills/agent-first-psql/SKILL.md"),
    title: "Agent-First PSQL",
    marker_slug: "afpsql",
};

pub fn run(req: SkillAdminRequest) -> i32 {
    let (action, options) = split_action(req.action);
    let stdout = std::io::stdout();
    let mut emitter =
        agent_first_data::CliEmitter::new(stdout.lock(), req.output).with_strict_protocol();
    match skill::run_skill_admin(&SPEC, action, &options) {
        Ok(report) => match serde_json::to_value(&report) {
            Ok(value) => match emitter.emit_result(value) {
                Ok(()) => 0,
                Err(_) => 4,
            },
            Err(error) => match emitter.emit_error(
                "serialization_failed",
                &format!("failed to serialize skill report: {error}"),
            ) {
                Ok(()) => 1,
                Err(_) => 4,
            },
        },
        Err(err) => match agent_first_data::json_error("cli_error", &err.message)
            .hint_if_some(err.hint.as_deref())
            .field(
                "partial_report",
                err.partial_report
                    .and_then(|report| serde_json::to_value(report).ok())
                    .unwrap_or(Value::Null),
            )
            .build()
            .map_err(agent_first_data::CliEmitterError::Build)
        {
            Ok(event) => match emitter.emit(event) {
                Ok(()) => 1,
                Err(_) => 4,
            },
            Err(_) => 4,
        },
    }
}

fn split_action(action: SkillAdminAction) -> (SkillAction, SkillOptions) {
    match action {
        SkillAdminAction::Status(options) => (SkillAction::Status, convert_options(options)),
        SkillAdminAction::Install(options) => (SkillAction::Install, convert_options(options)),
        SkillAdminAction::Uninstall(options) => (SkillAction::Uninstall, convert_options(options)),
    }
}

fn convert_options(options: SkillAdminOptions) -> SkillOptions {
    SkillOptions {
        agent: convert_agent(options.agent),
        scope: convert_scope(options.scope),
        skills_dir: options.skills_dir,
        force: options.force,
    }
}

fn convert_agent(agent: SkillAgentSelection) -> AfSelection {
    match agent {
        SkillAgentSelection::All => AfSelection::All,
        SkillAgentSelection::Codex => AfSelection::Codex,
        SkillAgentSelection::ClaudeCode => AfSelection::ClaudeCode,
        SkillAgentSelection::Opencode => AfSelection::Opencode,
        SkillAgentSelection::Hermes => AfSelection::Hermes,
    }
}

fn convert_scope(scope: SkillScope) -> AfScope {
    match scope {
        SkillScope::Personal => AfScope::Personal,
        SkillScope::Workspace => AfScope::Workspace,
    }
}

#[cfg(test)]
mod tests {
    use super::SPEC;

    #[test]
    fn shell_guidance_protects_placeholder_dollars() {
        assert!(
            SPEC.source
                .contains("quote SQL containing `$1..$N` placeholders with single")
        );
        assert!(SPEC.source.contains("shells expand `$1` and `$2`"));
        assert!(SPEC.source.contains("`--sql-file` / pipe mode JSON"));
    }
}
