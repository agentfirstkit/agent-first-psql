use super::*;
use std::collections::HashMap;

#[test]
fn apply_update_adds_default_session_if_missing() {
    let mut cfg = RuntimeConfig::default();
    cfg.apply_update(ConfigPatch {
        default_session: Some("other".to_string()),
        ..Default::default()
    });
    assert!(cfg.sessions.contains_key("other"));
}

#[test]
fn apply_update_merges_session_fields() {
    let mut cfg = RuntimeConfig::default();
    let mut sessions = HashMap::new();
    sessions.insert(
        "s1".to_string(),
        SessionConfigPatch {
            dsn_secret: PatchField::Value("postgresql://localhost/postgres".to_string()),
            conninfo_secret: PatchField::Value(
                "host=localhost user=roger dbname=postgres".to_string(),
            ),
            host: PatchField::Value("localhost".to_string()),
            port: PatchField::Value(5432),
            user: PatchField::Value("roger".to_string()),
            dbname: PatchField::Value("postgres".to_string()),
            password_secret: PatchField::Value("pw".to_string()),
            ssh: SshConfigPatch {
                destination: PatchField::Value("user@example.com".to_string()),
                options: PatchField::Value(vec!["ProxyJump=bastion".to_string()]),
                local_host: PatchField::Value("127.0.0.1".to_string()),
                local_port: PatchField::Value(15432),
                remote_socket: PatchField::Value("/var/run/postgresql/.s.PGSQL.5432".to_string()),
                sudo_user: PatchField::Value("postgres".to_string()),
            },
            container: ContainerConfigPatch {
                target: PatchField::Value("pg-container".to_string()),
                driver: PatchField::Value("podman".to_string()),
                runtime: PatchField::Value("podman".to_string()),
                user: PatchField::Value("postgres".to_string()),
                namespace: PatchField::Value("prod".to_string()),
                context: PatchField::Value("cluster-a".to_string()),
                compose_files: PatchField::Value(vec!["compose.yml".to_string()]),
                compose_project: PatchField::Value("demo".to_string()),
                ..Default::default()
            },
        },
    );
    cfg.apply_update(ConfigPatch {
        inline_max_rows: Some(10),
        inline_max_bytes: Some(20),
        statement_timeout_ms: Some(30),
        lock_timeout_ms: Some(40),
        log: Some(vec!["a".to_string()]),
        sessions: Some(sessions),
        ..Default::default()
    });
    let maybe_s1 = cfg.sessions.get("s1");
    assert!(maybe_s1.is_some());
    if let Some(s1) = maybe_s1 {
        assert_eq!(
            s1.dsn_secret.as_deref(),
            Some("postgresql://localhost/postgres")
        );
        assert!(s1.conninfo_secret.is_some());
        assert_eq!(s1.host.as_deref(), Some("localhost"));
        assert_eq!(s1.port, Some(5432));
        assert_eq!(s1.user.as_deref(), Some("roger"));
        assert_eq!(s1.dbname.as_deref(), Some("postgres"));
        assert_eq!(s1.password_secret.as_deref(), Some("pw"));
        assert_eq!(s1.ssh.destination.as_deref(), Some("user@example.com"));
        assert_eq!(s1.ssh.options, vec!["ProxyJump=bastion".to_string()]);
        assert_eq!(s1.ssh.local_host.as_deref(), Some("127.0.0.1"));
        assert_eq!(s1.ssh.local_port, Some(15432));
        assert_eq!(
            s1.ssh.remote_socket.as_deref(),
            Some("/var/run/postgresql/.s.PGSQL.5432")
        );
        assert_eq!(s1.ssh.sudo_user.as_deref(), Some("postgres"));
        assert_eq!(s1.container.target.as_deref(), Some("pg-container"));
        assert_eq!(s1.container.driver.as_deref(), Some("podman"));
        assert_eq!(s1.container.runtime.as_deref(), Some("podman"));
        assert_eq!(s1.container.user.as_deref(), Some("postgres"));
        assert_eq!(s1.container.namespace.as_deref(), Some("prod"));
        assert_eq!(s1.container.context.as_deref(), Some("cluster-a"));
        assert_eq!(s1.container.compose_files, vec!["compose.yml".to_string()]);
        assert_eq!(s1.container.compose_project.as_deref(), Some("demo"));
    }
    assert_eq!(cfg.inline_max_rows, 10);
    assert_eq!(cfg.inline_max_bytes, 20);
    assert_eq!(cfg.statement_timeout_ms, 30);
    assert_eq!(cfg.lock_timeout_ms, 40);
    assert_eq!(cfg.log, vec!["a".to_string()]);
}

#[test]
fn apply_update_normalizes_log_categories() {
    let mut cfg = RuntimeConfig::default();
    cfg.apply_update(ConfigPatch {
        log: Some(vec![
            " Query.Result ".to_string(),
            "query.result".to_string(),
            "".to_string(),
            "ALL".to_string(),
        ]),
        ..Default::default()
    });
    assert_eq!(cfg.log, vec!["query.result".to_string(), "all".to_string()]);
}

#[test]
fn resolve_options_applies_defaults_and_overrides() {
    let cfg = RuntimeConfig::default();
    let resolved = cfg.resolve_options_for_session(
        &QueryOptions {
            stream_rows: true,
            batch_rows: Some(0),
            batch_bytes: Some(1),
            statement_timeout_ms: Some(1),
            lock_timeout_ms: Some(2),
            permission: Some(Permission::Write),
            inline_max_rows: Some(3),
            inline_max_bytes: Some(4),
        },
        &SessionConfig::default(),
    );
    assert!(resolved.is_ok());
    let resolved = resolved.unwrap_or_else(|_| cfg.resolve_options(&QueryOptions::default()));
    assert!(resolved.stream_rows);
    assert_eq!(resolved.batch_rows, 1);
    assert_eq!(resolved.batch_bytes, 1024);
    assert_eq!(resolved.statement_timeout_ms, 1);
    assert_eq!(resolved.lock_timeout_ms, 2);
    assert!(!resolved.read_only);
    assert_eq!(resolved.inline_max_rows, 3);
    assert_eq!(resolved.inline_max_bytes, 4);
}

#[test]
fn resolve_options_defaults_to_read_only_and_requires_ssh_permission_for_ssh() {
    let cfg = RuntimeConfig::default();
    let direct =
        cfg.resolve_options_for_session(&QueryOptions::default(), &SessionConfig::default());
    assert!(matches!(
        direct,
        Ok(ResolvedOptions {
            read_only: true,
            ..
        })
    ));

    let ssh_session = SessionConfig {
        ssh: SshConfig {
            destination: Some("user@example.com".to_string()),
            ..Default::default()
        },
        ..Default::default()
    };
    let ssh_default = cfg.resolve_options_for_session(&QueryOptions::default(), &ssh_session);
    assert!(matches!(
        ssh_default,
        Ok(ResolvedOptions {
            read_only: true,
            ..
        })
    ));

    let ssh_write = cfg.resolve_options_for_session(
        &QueryOptions {
            permission: Some(Permission::SshWrite),
            ..Default::default()
        },
        &ssh_session,
    );
    assert!(matches!(
        ssh_write,
        Ok(ResolvedOptions {
            read_only: false,
            ..
        })
    ));

    let direct_write_on_ssh = cfg.resolve_options_for_session(
        &QueryOptions {
            permission: Some(Permission::Write),
            ..Default::default()
        },
        &ssh_session,
    );
    assert!(matches!(direct_write_on_ssh, Err(message) if message.contains("ssh-write")));
}

#[test]
fn resolve_options_rejects_ssh_permissions_for_direct_sessions() {
    let cfg = RuntimeConfig::default();
    let resolved = cfg.resolve_options_for_session(
        &QueryOptions {
            permission: Some(Permission::SshWrite),
            ..Default::default()
        },
        &SessionConfig::default(),
    );
    assert!(matches!(resolved, Err(message) if message.contains("requires SSH transport")));
}

#[test]
fn resolve_options_defaults_to_container_read_and_requires_container_permission() {
    let cfg = RuntimeConfig::default();
    let container_session = SessionConfig {
        container: ContainerConfig {
            target: Some("pg".to_string()),
            ..Default::default()
        },
        ..Default::default()
    };
    let container_default =
        cfg.resolve_options_for_session(&QueryOptions::default(), &container_session);
    assert!(matches!(
        container_default,
        Ok(ResolvedOptions {
            read_only: true,
            ..
        })
    ));

    let container_write = cfg.resolve_options_for_session(
        &QueryOptions {
            permission: Some(Permission::ContainerWrite),
            ..Default::default()
        },
        &container_session,
    );
    assert!(matches!(
        container_write,
        Ok(ResolvedOptions {
            read_only: false,
            ..
        })
    ));

    let direct_write_on_container = cfg.resolve_options_for_session(
        &QueryOptions {
            permission: Some(Permission::Write),
            ..Default::default()
        },
        &container_session,
    );
    assert!(
        matches!(direct_write_on_container, Err(message) if message.contains("container-write"))
    );

    let container_write_on_direct = cfg.resolve_options_for_session(
        &QueryOptions {
            permission: Some(Permission::ContainerWrite),
            ..Default::default()
        },
        &SessionConfig::default(),
    );
    assert!(
        matches!(container_write_on_direct, Err(message) if message.contains("requires container transport"))
    );
}

#[test]
fn resolve_options_treats_ssh_plus_container_as_container_transport() {
    let cfg = RuntimeConfig::default();
    let mixed = SessionConfig {
        ssh: SshConfig {
            destination: Some("user@example.com".to_string()),
            ..Default::default()
        },
        container: ContainerConfig {
            target: Some("pg".to_string()),
            ..Default::default()
        },
        ..Default::default()
    };
    let resolved = cfg.resolve_options_for_session(&QueryOptions::default(), &mixed);
    assert!(matches!(
        resolved,
        Ok(ResolvedOptions {
            read_only: true,
            ..
        })
    ));

    let container_write = cfg.resolve_options_for_session(
        &QueryOptions {
            permission: Some(Permission::ContainerWrite),
            ..Default::default()
        },
        &mixed,
    );
    assert!(matches!(
        container_write,
        Ok(ResolvedOptions {
            read_only: false,
            ..
        })
    ));

    let ssh_write = cfg.resolve_options_for_session(
        &QueryOptions {
            permission: Some(Permission::SshWrite),
            ..Default::default()
        },
        &mixed,
    );
    assert!(matches!(ssh_write, Err(message) if message.contains("container-write")));

    let direct_write = cfg.resolve_options_for_session(
        &QueryOptions {
            permission: Some(Permission::Write),
            ..Default::default()
        },
        &mixed,
    );
    assert!(matches!(direct_write, Err(message) if message.contains("container-write")));
}

#[test]
fn resolve_options_keeps_shape_limits_with_read_permission() {
    let cfg = RuntimeConfig::default();
    let resolved = cfg.resolve_options_for_session(
        &QueryOptions {
            stream_rows: true,
            batch_rows: Some(0),
            batch_bytes: Some(1),
            statement_timeout_ms: Some(1),
            lock_timeout_ms: Some(2),
            permission: Some(Permission::Read),
            inline_max_rows: Some(3),
            inline_max_bytes: Some(4),
        },
        &SessionConfig::default(),
    );
    assert!(resolved.is_ok());
    let resolved = resolved.unwrap_or_else(|_| cfg.resolve_options(&QueryOptions::default()));
    assert!(resolved.stream_rows);
    assert_eq!(resolved.batch_rows, 1);
    assert_eq!(resolved.batch_bytes, 1024);
    assert_eq!(resolved.statement_timeout_ms, 1);
    assert_eq!(resolved.lock_timeout_ms, 2);
    assert!(resolved.read_only);
    assert_eq!(resolved.inline_max_rows, 3);
    assert_eq!(resolved.inline_max_bytes, 4);
}

#[test]
fn sessions_to_invalidate_collects_default_and_session_keys() {
    let mut sessions = HashMap::new();
    sessions.insert("s1".to_string(), SessionConfigPatch::default());
    sessions.insert("s2".to_string(), SessionConfigPatch::default());
    let patch = ConfigPatch {
        default_session: Some("s2".to_string()),
        sessions: Some(sessions),
        ..Default::default()
    };
    let names = sessions_to_invalidate(&patch);
    assert_eq!(names, vec!["s1".to_string(), "s2".to_string()]);
}

#[test]
fn apply_update_can_clear_session_fields_with_null() {
    let mut cfg = RuntimeConfig::default();
    cfg.sessions.insert(
        "s1".to_string(),
        SessionConfig {
            dsn_secret: Some("postgresql://localhost/postgres".to_string()),
            conninfo_secret: Some("host=localhost user=roger dbname=postgres".to_string()),
            host: Some("localhost".to_string()),
            port: Some(5432),
            user: Some("roger".to_string()),
            dbname: Some("postgres".to_string()),
            password_secret: Some("pw".to_string()),
            ssh: SshConfig {
                destination: Some("user@example.com".to_string()),
                options: vec!["ProxyJump=bastion".to_string()],
                local_host: Some("127.0.0.1".to_string()),
                local_port: Some(15432),
                remote_socket: Some("/var/run/postgresql/.s.PGSQL.5432".to_string()),
                sudo_user: Some("postgres".to_string()),
            },
            container: ContainerConfig {
                target: Some("pg-container".to_string()),
                driver: Some("podman".to_string()),
                runtime: Some("podman".to_string()),
                user: Some("postgres".to_string()),
                namespace: Some("prod".to_string()),
                context: Some("cluster-a".to_string()),
                compose_files: vec!["compose.yml".to_string()],
                compose_project: Some("demo".to_string()),
                ..Default::default()
            },
        },
    );
    let mut sessions = HashMap::new();
    sessions.insert(
        "s1".to_string(),
        SessionConfigPatch {
            dsn_secret: PatchField::Null,
            conninfo_secret: PatchField::Null,
            host: PatchField::Null,
            port: PatchField::Null,
            user: PatchField::Null,
            dbname: PatchField::Null,
            password_secret: PatchField::Null,
            ssh: SshConfigPatch {
                destination: PatchField::Null,
                options: PatchField::Null,
                local_host: PatchField::Null,
                local_port: PatchField::Null,
                remote_socket: PatchField::Null,
                sudo_user: PatchField::Null,
            },
            container: ContainerConfigPatch {
                target: PatchField::Null,
                driver: PatchField::Null,
                runtime: PatchField::Null,
                user: PatchField::Null,
                namespace: PatchField::Null,
                context: PatchField::Null,
                compose_files: PatchField::Null,
                compose_project: PatchField::Null,
                ..Default::default()
            },
        },
    );
    cfg.apply_update(ConfigPatch {
        sessions: Some(sessions),
        ..Default::default()
    });
    let maybe_s1 = cfg.sessions.get("s1");
    assert!(maybe_s1.is_some());
    if let Some(s1) = maybe_s1 {
        assert!(s1.dsn_secret.is_none());
        assert!(s1.conninfo_secret.is_none());
        assert!(s1.host.is_none());
        assert!(s1.port.is_none());
        assert!(s1.user.is_none());
        assert!(s1.dbname.is_none());
        assert!(s1.password_secret.is_none());
        assert!(s1.ssh.destination.is_none());
        assert!(s1.ssh.options.is_empty());
        assert!(s1.ssh.local_host.is_none());
        assert!(s1.ssh.local_port.is_none());
        assert!(s1.ssh.remote_socket.is_none());
        assert!(s1.ssh.sudo_user.is_none());
        assert!(s1.container.target.is_none());
        assert!(s1.container.driver.is_none());
        assert!(s1.container.runtime.is_none());
        assert!(s1.container.user.is_none());
        assert!(s1.container.namespace.is_none());
        assert!(s1.container.context.is_none());
        assert!(s1.container.compose_files.is_empty());
        assert!(s1.container.compose_project.is_none());
    }
}
