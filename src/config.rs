use crate::types::*;
use agent_first_data::cli_parse_log_filters;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

impl RuntimeConfig {
    #[cfg(test)]
    pub fn resolve_options(&self, q: &QueryOptions) -> ResolvedOptions {
        self.resolve_options_with_permission(q, q.permission.unwrap_or(Permission::Read))
    }

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
                if let Some(v) = s.ssh.destination.into_update() {
                    entry.ssh.destination = v;
                }
                if let Some(v) = s.ssh.via.into_update() {
                    entry.ssh.via = v.unwrap_or_default();
                }
                if let Some(v) = s.ssh.options.into_update() {
                    entry.ssh.options = v.unwrap_or_default();
                }
                if let Some(v) = s.ssh.local_host.into_update() {
                    entry.ssh.local_host = v;
                }
                if let Some(v) = s.ssh.local_port.into_update() {
                    entry.ssh.local_port = v;
                }
                if let Some(v) = s.ssh.remote_socket.into_update() {
                    entry.ssh.remote_socket = v;
                }
                if let Some(v) = s.ssh.sudo_user.into_update() {
                    entry.ssh.sudo_user = v;
                }
                let container = s.container;
                apply_patch(&mut entry.container.docker_name, container.docker_name);
                apply_patch(&mut entry.container.docker_user, container.docker_user);
                apply_patch(
                    &mut entry.container.docker_context,
                    container.docker_context,
                );
                apply_patch(
                    &mut entry.container.docker_runtime,
                    container.docker_runtime,
                );
                apply_patch(&mut entry.container.podman_name, container.podman_name);
                apply_patch(&mut entry.container.podman_user, container.podman_user);
                apply_patch(
                    &mut entry.container.podman_runtime,
                    container.podman_runtime,
                );
                apply_patch(&mut entry.container.nerdctl_name, container.nerdctl_name);
                apply_patch(&mut entry.container.nerdctl_user, container.nerdctl_user);
                apply_patch(
                    &mut entry.container.nerdctl_runtime,
                    container.nerdctl_runtime,
                );
                apply_patch(
                    &mut entry.container.compose_service,
                    container.compose_service,
                );
                apply_patch(&mut entry.container.compose_user, container.compose_user);
                if let Some(v) = container.compose_files.into_update() {
                    entry.container.compose_files = v.unwrap_or_default();
                }
                apply_patch(
                    &mut entry.container.compose_project,
                    container.compose_project,
                );
                apply_patch(
                    &mut entry.container.compose_runtime,
                    container.compose_runtime,
                );
                apply_patch(&mut entry.container.kubectl_pod, container.kubectl_pod);
                apply_patch(
                    &mut entry.container.kubectl_container,
                    container.kubectl_container,
                );
                apply_patch(
                    &mut entry.container.kubectl_namespace,
                    container.kubectl_namespace,
                );
                apply_patch(
                    &mut entry.container.kubectl_context,
                    container.kubectl_context,
                );
                apply_patch(
                    &mut entry.container.kubectl_runtime,
                    container.kubectl_runtime,
                );
            }
        }
        if !self.sessions.contains_key(&self.default_session) {
            self.sessions
                .insert(self.default_session.clone(), SessionConfig::default());
        }
    }

    pub fn resolve_options_for_session(
        &self,
        q: &QueryOptions,
        session: &SessionConfig,
    ) -> Result<ResolvedOptions, String> {
        let transport = session.transport_kind()?;
        let permission = q.permission.unwrap_or(match transport {
            TransportKind::Direct => Permission::Read,
            TransportKind::Ssh => Permission::SshRead,
            TransportKind::Container => Permission::ContainerRead,
        });
        match transport {
            TransportKind::Direct if permission.allows_ssh() => {
                return Err(format!(
                    "permission `{}` requires SSH transport; use `read` or `write` for direct connections",
                    permission.as_str()
                ));
            }
            TransportKind::Direct if permission.allows_container() => {
                return Err(format!(
                    "permission `{}` requires container transport; use `read` or `write` for direct connections",
                    permission.as_str()
                ));
            }
            TransportKind::Ssh if !permission.allows_ssh() => {
                return Err(format!(
                    "permission `{}` does not allow SSH transport; use `ssh-read` or `ssh-write`",
                    permission.as_str()
                ));
            }
            TransportKind::Container if !permission.allows_container() => {
                return Err(format!(
                    "permission `{}` does not allow container transport; use `container-read` or `container-write`",
                    permission.as_str()
                ));
            }
            _ => {}
        }
        Ok(self.resolve_options_with_permission(q, permission))
    }

    pub fn resolve_write_options_for_session(
        &self,
        q: &QueryOptions,
        session: &SessionConfig,
    ) -> Result<ResolvedOptions, String> {
        let resolved = self.resolve_options_for_session(q, session)?;
        if resolved.read_only {
            return Err(
                "read-write transaction requires an explicit write permission matching the session transport"
                    .to_string(),
            );
        }
        Ok(resolved)
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

/// Apply one patch field in place: absent leaves the value, null clears it.
fn apply_patch<T>(field: &mut Option<T>, patch: PatchField<T>) {
    if let Some(update) = patch.into_update() {
        *field = update;
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
