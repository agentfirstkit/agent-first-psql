use crate::types::*;
use agent_first_data::cli_parse_log_filters;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

impl RuntimeConfig {
    pub fn apply_update(&mut self, patch: ConfigPatch) {
        if let Some(v) = patch.default_session {
            self.default_session = v;
        }
        if let Some(v) = patch.inline_max_rows {
            self.inline_max_rows = v;
        }
        if let Some(v) = patch.inline_max_bytes {
            self.inline_max_bytes = v;
        }
        if let Some(v) = patch.statement_timeout_ms {
            self.statement_timeout_ms = v;
        }
        if let Some(v) = patch.lock_timeout_ms {
            self.lock_timeout_ms = v;
        }
        if let Some(v) = patch.log {
            self.log = cli_parse_log_filters(&v);
        }
        if let Some(sessions) = patch.sessions {
            for (name, s) in sessions {
                let entry = self.sessions.entry(name).or_default();
                if let Some(v) = s.dsn_secret.into_update() {
                    entry.dsn_secret = v;
                }
                if let Some(v) = s.conninfo_secret.into_update() {
                    entry.conninfo_secret = v;
                }
                if let Some(v) = s.host.into_update() {
                    entry.host = v;
                }
                if let Some(v) = s.port.into_update() {
                    entry.port = v;
                }
                if let Some(v) = s.user.into_update() {
                    entry.user = v;
                }
                if let Some(v) = s.dbname.into_update() {
                    entry.dbname = v;
                }
                if let Some(v) = s.password_secret.into_update() {
                    entry.password_secret = v;
                }
                if let Some(v) = s.ssh.into_update() {
                    entry.ssh = v;
                }
                if let Some(v) = s.ssh_options.into_update() {
                    entry.ssh_options = v.unwrap_or_default();
                }
                if let Some(v) = s.ssh_local_host.into_update() {
                    entry.ssh_local_host = v;
                }
                if let Some(v) = s.ssh_local_port.into_update() {
                    entry.ssh_local_port = v;
                }
                if let Some(v) = s.ssh_remote_socket.into_update() {
                    entry.ssh_remote_socket = v;
                }
                if let Some(v) = s.ssh_sudo_user.into_update() {
                    entry.ssh_sudo_user = v;
                }
            }
        }
        if !self.sessions.contains_key(&self.default_session) {
            self.sessions
                .insert(self.default_session.clone(), SessionConfig::default());
        }
    }

    #[allow(dead_code)]
    pub fn resolve_options(&self, q: &QueryOptions) -> ResolvedOptions {
        self.resolve_options_with_permission(q, q.permission.unwrap_or(Permission::Read))
    }

    pub fn resolve_options_for_session(
        &self,
        q: &QueryOptions,
        session: &SessionConfig,
    ) -> Result<ResolvedOptions, String> {
        let uses_ssh = session.uses_ssh_transport();
        let permission = q.permission.unwrap_or(if uses_ssh {
            Permission::SshRead
        } else {
            Permission::Read
        });
        if uses_ssh && !permission.allows_ssh() {
            return Err(format!(
                "permission `{}` does not allow SSH transport; use `ssh-read` or `ssh-write`",
                permission.as_str()
            ));
        }
        if !uses_ssh && permission.allows_ssh() {
            return Err(format!(
                "permission `{}` requires SSH transport; use `read` or `write` for direct connections",
                permission.as_str()
            ));
        }
        Ok(self.resolve_options_with_permission(q, permission))
    }

    fn resolve_options_with_permission(
        &self,
        q: &QueryOptions,
        permission: Permission,
    ) -> ResolvedOptions {
        ResolvedOptions {
            stream_rows: q.stream_rows,
            batch_rows: q.batch_rows.unwrap_or(1000).max(1),
            batch_bytes: q.batch_bytes.unwrap_or(262_144).max(1024),
            statement_timeout_ms: q.statement_timeout_ms.unwrap_or(self.statement_timeout_ms),
            lock_timeout_ms: q.lock_timeout_ms.unwrap_or(self.lock_timeout_ms),
            read_only: permission.is_read_only(),
            inline_max_rows: q.inline_max_rows.unwrap_or(self.inline_max_rows),
            inline_max_bytes: q.inline_max_bytes.unwrap_or(self.inline_max_bytes),
        }
    }
}

pub fn sessions_to_invalidate(patch: &ConfigPatch) -> Vec<String> {
    let mut sessions: Vec<String> = vec![];
    if let Some(default_session) = patch.default_session.as_ref() {
        sessions.push(default_session.clone());
    }
    if let Some(update_sessions) = patch.sessions.as_ref() {
        sessions.extend(update_sessions.keys().cloned());
    }
    sessions.sort();
    sessions.dedup();
    sessions
}

#[cfg(test)]
#[path = "../tests/support/unit_config.rs"]
mod tests;
