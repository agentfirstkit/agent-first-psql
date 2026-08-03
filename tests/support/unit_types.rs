use super::*;

#[test]
fn runtime_config_default_has_default_session() {
    let cfg = RuntimeConfig::default();
    assert_eq!(cfg.default_session, "default");
    assert!(cfg.sessions.contains_key("default"));
}

#[test]
fn trace_only_duration_sets_optional_fields_none() {
    let t = Trace::only_duration(12);
    assert_eq!(t.duration_ms, 12);
    assert!(t.row_count.is_none());
    assert!(t.payload_bytes.is_none());
}

#[test]
fn query_options_accept_permission_and_reject_removed_read_only() {
    let input = serde_json::from_value::<Input>(serde_json::json!({
        "code": "query",
        "id": "q1",
        "sql": "select 1",
        "options": {"permission": "write"}
    }));
    let permission = match input {
        Ok(Input::Query { options, .. }) => options.permission,
        _ => None,
    };
    assert_eq!(permission, Some(Permission::Write));

    let err = serde_json::from_value::<Input>(serde_json::json!({
        "code": "query",
        "id": "q1",
        "sql": "select 1",
        "options": {"read_only": true}
    }));
    assert!(err.is_err(), "removed read_only option should be rejected");
    if let Err(err) = err {
        assert!(err.to_string().contains("unknown field"));
    }
}

#[test]
fn begin_defaults_to_read_only() {
    let input = serde_json::from_value::<Input>(serde_json::json!({
        "code": "begin",
        "id": "b1"
    }));
    assert!(matches!(
        input,
        Ok(Input::Begin {
            read_only: true,
            permission: None,
            ..
        })
    ));
}

#[test]
fn permission_parses_container_values() {
    assert_eq!(
        "container-read".parse::<Permission>(),
        Ok(Permission::ContainerRead)
    );
    assert_eq!(
        "container-write".parse::<Permission>(),
        Ok(Permission::ContainerWrite)
    );
    assert!(Permission::ContainerRead.is_read_only());
    assert!(!Permission::ContainerWrite.is_read_only());
    assert!(Permission::ContainerWrite.allows_container());
}

#[test]
fn ssh_plus_container_selects_container_transport_kind() {
    let cfg = SessionConfig {
        ssh: SshConfig {
            destination: Some("root@example.com".to_string()),
            ..Default::default()
        },
        container: ContainerConfig {
            docker_name: Some("pg".to_string()),
            ..Default::default()
        },
        ..Default::default()
    };
    assert_eq!(cfg.transport_kind(), Ok(TransportKind::Container));
}

/// A pipe session carrying two families names two drivers, so it has no
/// transport at all — rejected on the request, before any connect attempt.
#[test]
fn two_container_families_have_no_transport_kind() {
    let cfg = SessionConfig {
        container: ContainerConfig {
            compose_service: Some("db".to_string()),
            kubectl_pod: Some("app".to_string()),
            ..Default::default()
        },
        ..Default::default()
    };
    assert_eq!(
        cfg.transport_kind(),
        Err(
            "--container-compose-service cannot be combined with --container-kubectl-pod; each container driver has its own flag family"
                .to_string()
        )
    );
}

/// The kubectl family has no exec-user field, so a session cannot express one.
#[test]
fn kubectl_family_has_no_user_field() {
    let cfg = ContainerConfig {
        kubectl_pod: Some("app".to_string()),
        kubectl_container: Some("postgres".to_string()),
        ..Default::default()
    };
    assert_eq!(
        cfg.selected_driver(),
        Ok(Some(crate::types::ContainerDriver::Kubectl))
    );
    // Only the four non-kubectl families carry a user, and none of them can be
    // set alongside a kubectl field.
    assert!(
        serde_json::from_value::<SessionConfig>(serde_json::json!({
            "kubectl_pod": "app",
            "kubectl_user": "postgres"
        }))
        .is_err()
    );
}

#[test]
fn input_rejects_unknown_protocol_fields() {
    let query = serde_json::from_value::<Input>(serde_json::json!({
        "code": "query",
        "id": "q1",
        "sessoin": "work",
        "sql": "select 1"
    }));
    assert_unknown_field(query, "sessoin");

    let cancel = serde_json::from_value::<Input>(serde_json::json!({
        "code": "cancel",
        "id": "q1",
        "extra": true
    }));
    assert_unknown_field(cancel, "extra");
}

#[test]
fn config_rejects_unknown_patch_fields() {
    let top_level = serde_json::from_value::<Input>(serde_json::json!({
        "code": "config",
        "inline_max_row": 10
    }));
    assert_unknown_field(top_level, "inline_max_row");

    let session_field = serde_json::from_value::<Input>(serde_json::json!({
        "code": "config",
        "sessions": {
            "default": {
                "password_secrect": "pw"
            }
        }
    }));
    assert_unknown_field(session_field, "password_secrect");
}

#[test]
fn session_config_rejects_unknown_fields() {
    let cfg = serde_json::from_value::<SessionConfig>(serde_json::json!({
        "host": "127.0.0.1",
        "password_secrect": "pw"
    }));
    assert_unknown_field(cfg, "password_secrect");
}

#[test]
fn session_config_flat_wire_round_trips_through_substructs() {
    let wire = serde_json::json!({
        "host": "127.0.0.1",
        "port": 5432,
        "user": "postgres",
        "dbname": "app",
        "password_secret": "PG_PASSWORD",
        "ssh": "root@bastion",
        "ssh_via": ["root@jump"],
        "ssh_options": ["ProxyJump=jump"],
        "ssh_local_host": "127.0.0.1",
        "ssh_local_port": 15432,
        "ssh_remote_socket": "/var/run/postgresql/.s.PGSQL.5432",
        "ssh_sudo_user": "postgres",
        "docker_name": "pg",
        "docker_user": "postgres",
        "docker_context": "cluster-a",
        "docker_runtime": "docker",
        "podman_name": "pg-podman",
        "podman_user": "postgres",
        "podman_runtime": "podman",
        "nerdctl_name": "pg-nerdctl",
        "nerdctl_user": "postgres",
        "nerdctl_runtime": "nerdctl",
        "compose_service": "db",
        "compose_user": "postgres",
        "compose_files": ["base.yml", "prod.yml"],
        "compose_project": "demo",
        "compose_runtime": "docker-compose",
        "kubectl_pod": "pg-pod",
        "kubectl_container": "postgres",
        "kubectl_namespace": "prod",
        "kubectl_context": "cluster-a",
        "kubectl_runtime": "kubectl"
    });

    let parsed: Result<SessionConfig, _> = serde_json::from_value(wire.clone());
    assert!(
        parsed.is_ok(),
        "flat wire should parse, got {:?}",
        parsed.err()
    );
    let Ok(cfg) = parsed else { return };
    assert_eq!(cfg.host.as_deref(), Some("127.0.0.1"));
    assert_eq!(cfg.ssh.destination.as_deref(), Some("root@bastion"));
    assert_eq!(cfg.ssh.via, vec!["root@jump".to_string()]);
    assert_eq!(cfg.ssh.options, vec!["ProxyJump=jump".to_string()]);
    assert_eq!(cfg.ssh.local_port, Some(15432));
    assert_eq!(cfg.ssh.sudo_user.as_deref(), Some("postgres"));
    assert_eq!(cfg.container.docker_name.as_deref(), Some("pg"));
    assert_eq!(cfg.container.compose_service.as_deref(), Some("db"));
    assert_eq!(
        cfg.container.compose_files,
        vec!["base.yml".to_string(), "prod.yml".to_string()]
    );
    assert_eq!(cfg.container.kubectl_pod.as_deref(), Some("pg-pod"));
    assert_eq!(cfg.container.kubectl_namespace.as_deref(), Some("prod"));

    let reserialized_res = serde_json::to_value(&cfg);
    assert!(
        reserialized_res.is_ok(),
        "cfg should serialize, got {:?}",
        reserialized_res.as_ref().err()
    );
    let Ok(reserialized) = reserialized_res else {
        return;
    };
    let object = reserialized.as_object();
    assert!(
        object.is_some(),
        "reserialized must be an object, got {reserialized}"
    );
    let Some(object) = object else { return };
    assert!(
        !object.contains_key("ssh_destination"),
        "ssh substruct must serialize as 'ssh', not 'ssh_destination'"
    );
    assert!(
        !object.values().any(|v| v.is_object()),
        "wire format must stay flat, got nested object in {reserialized}"
    );
    let mut expected = wire;
    expected["password_secret"] = serde_json::Value::String("***".to_string());
    assert_eq!(reserialized, expected, "output wire must redact secrets");
}

#[test]
fn input_session_info_accepts_optional_id_and_session() {
    let with_both = serde_json::from_value::<Input>(serde_json::json!({
        "code": "session_info",
        "id": "info-1",
        "session": "work"
    }));
    assert!(matches!(
        with_both,
        Ok(Input::SessionInfo { id: Some(id), session: Some(s) }) if id == "info-1" && s == "work"
    ));

    let neither = serde_json::from_value::<Input>(serde_json::json!({
        "code": "session_info"
    }));
    assert!(matches!(
        neither,
        Ok(Input::SessionInfo {
            id: None,
            session: None
        })
    ));

    let unknown = serde_json::from_value::<Input>(serde_json::json!({
        "code": "session_info",
        "extra": true
    }));
    assert_unknown_field(unknown, "extra");
}

#[test]
fn session_config_empty_serializes_to_empty_object() {
    let cfg = SessionConfig::default();
    let json_res = serde_json::to_value(&cfg);
    assert!(
        json_res.is_ok(),
        "default cfg should serialize, got {:?}",
        json_res.as_ref().err()
    );
    let Ok(json) = json_res else { return };
    assert_eq!(json, serde_json::json!({}));
}

fn assert_unknown_field<T>(result: Result<T, serde_json::Error>, field: &str) {
    assert!(result.is_err(), "unknown field {field} should be rejected");
    if let Err(err) = result {
        let message = err.to_string();
        assert!(message.contains("unknown field"), "{message}");
        assert!(message.contains(field), "{message}");
    }
}
