use crate::types::*;
use agent_first_data::cli_parse_log_filters;

#[cfg(feature = "mcp")]
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
                if let Some(v) = s.dsn_secret {
                    entry.dsn_secret = Some(v);
                }
                if let Some(v) = s.conninfo_secret {
                    entry.conninfo_secret = Some(v);
                }
                if let Some(v) = s.host {
                    entry.host = Some(v);
                }
                if let Some(v) = s.port {
                    entry.port = Some(v);
                }
                if let Some(v) = s.user {
                    entry.user = Some(v);
                }
                if let Some(v) = s.dbname {
                    entry.dbname = Some(v);
                }
                if let Some(v) = s.password_secret {
                    entry.password_secret = Some(v);
                }
            }
        }
        if !self.sessions.contains_key(&self.default_session) {
            self.sessions
                .insert(self.default_session.clone(), SessionConfig::default());
        }
    }

    pub fn resolve_options(&self, q: &QueryOptions) -> ResolvedOptions {
        ResolvedOptions {
            stream_rows: q.stream_rows,
            batch_rows: q.batch_rows.unwrap_or(1000).max(1),
            batch_bytes: q.batch_bytes.unwrap_or(262_144).max(1024),
            statement_timeout_ms: q.statement_timeout_ms.unwrap_or(self.statement_timeout_ms),
            lock_timeout_ms: q.lock_timeout_ms.unwrap_or(self.lock_timeout_ms),
            read_only: q.read_only.unwrap_or(false),
            inline_max_rows: q.inline_max_rows.unwrap_or(self.inline_max_rows),
            inline_max_bytes: q.inline_max_bytes.unwrap_or(self.inline_max_bytes),
        }
    }
}

#[cfg(test)]
#[path = "../tests/support/unit_config.rs"]
mod tests;
