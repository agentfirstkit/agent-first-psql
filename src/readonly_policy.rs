use crate::types::SessionConfig;
use std::path::{Path, PathBuf};

const PROFILE_PREFIX: &str = "afpsql-readonly-";
const PROFILE_MAX_BYTES: u64 = 65_536;

pub fn validate_raw_args(args: &[String]) -> Result<(), String> {
    validate_raw_args_for_profile(args, false)
}

pub fn validate_raw_args_for_profile(args: &[String], locked_profile: bool) -> Result<(), String> {
    if !locked_profile {
        return Ok(());
    }
    reject_stream_redirect(args)?;
    let mut index = 1;
    while index < args.len() {
        let arg = &args[index];
        if let Some((flag, value)) = arg.split_once('=') {
            if locked_profile && is_connection_or_transport_flag(flag) {
                return Err(locked_profile_override_error(flag));
            }
            if is_value_flag(flag) {
                validate_raw_value(flag, value)?;
            }
            index += 1;
            continue;
        }
        if locked_profile && is_connection_or_transport_flag(arg) {
            return Err(locked_profile_override_error(arg));
        }
        if is_opaque_value_flag(arg) {
            index += 2;
            continue;
        }
        if is_value_flag(arg) {
            let value = args
                .get(index + 1)
                .ok_or_else(|| format!("{arg} requires a value"))?;
            validate_raw_value(arg, value)?;
            index += 2;
            continue;
        }
        index += 1;
    }
    Ok(())
}

fn locked_profile_override_error(flag: &str) -> String {
    format!("{flag} cannot override an administrator-locked afpsql-readonly profile")
}

/// Reject stdout/stderr redirection before the argv is parsed at all.
///
/// A locked profile fixes the endpoint and the transport, so it must also refuse
/// to create local files. This runs ahead of the registry rather than reading
/// the resolved output plan, because failing closed here needs no agreement with
/// the parser about which token is a flag: any `--stdout-file`/`--stderr-file`
/// anywhere in argv — including where a value-skipping walk would read it as
/// another argument's value — is refused. The redirect itself is installed only
/// from a resolved plan, well after this guard has had its say.
fn reject_stream_redirect(args: &[String]) -> Result<(), String> {
    match agent_first_data::stream_redirect::config_from_raw_args(args.iter().cloned()) {
        Ok(None) => Ok(()),
        _ => Err(
            "--stdout-file and --stderr-file are unavailable in afpsql-readonly because they create or truncate local files"
                .to_string(),
        ),
    }
}

fn is_connection_or_transport_flag(flag: &str) -> bool {
    matches!(
        flag,
        "--dsn"
            | "--conninfo"
            | "--host"
            | "--port"
            | "--user"
            | "--dbname"
            | "--password"
            | "--ssh"
            | "--ssh-via"
            | "--ssh-option"
            | "--ssh-remote-socket"
            | "--ssh-sudo-user"
            | "--container-docker-name"
            | "--container-docker-user"
            | "--container-docker-context"
            | "--container-docker-runtime"
            | "--container-apple-name"
            | "--container-apple-user"
            | "--container-apple-runtime"
            | "--container-podman-name"
            | "--container-podman-user"
            | "--container-podman-runtime"
            | "--container-nerdctl-name"
            | "--container-nerdctl-user"
            | "--container-nerdctl-runtime"
            | "--container-compose-service"
            | "--container-compose-user"
            | "--container-compose-file"
            | "--container-compose-project"
            | "--container-compose-runtime"
            | "--container-kubectl-pod"
            | "--container-kubectl-container"
            | "--container-kubectl-namespace"
            | "--container-kubectl-context"
            | "--container-kubectl-runtime"
    )
}

/// Where a locked profile is willing to find a transport program.
///
/// Not `PATH`. Under a locked profile the caller is the party being
/// constrained, and `PATH` is theirs: the profile can pin the endpoint, the
/// transport and the permissions and still hand the first `ssh` or `docker` on
/// the caller's `PATH` a process on this machine. Whether the PostgreSQL
/// handshake then succeeds is beside the point — the program already ran.
const TRUSTED_PROGRAM_DIRS: &[&str] = &["/usr/bin", "/bin", "/usr/sbin", "/sbin"];

/// Resolve a transport program for a locked profile, or refuse to start one.
///
/// An absolute path is taken as given and checked; a bare name is looked up in
/// [`TRUSTED_PROGRAM_DIRS`] only. Either way the file, and every directory
/// above it, must be root-owned and not writable by group or others — the same
/// trust rule the profile file itself is held to, because a program a caller
/// can replace is a profile a caller can escape.
pub fn trusted_program(program: &str) -> Result<PathBuf, String> {
    let candidates: Vec<PathBuf> = if Path::new(program).is_absolute() {
        vec![PathBuf::from(program)]
    } else if program.contains('/') {
        return Err(format!(
            "an administrator-locked profile will not run `{program}`: a relative path is resolved against the caller's working directory"
        ));
    } else {
        TRUSTED_PROGRAM_DIRS
            .iter()
            .map(|dir| Path::new(dir).join(program))
            .collect()
    };

    let mut refusal = None;
    for candidate in candidates {
        match std::fs::symlink_metadata(&candidate) {
            Ok(_) => {}
            Err(_) => continue,
        }
        // Canonicalized before it is judged, so a link into a writable
        // directory is checked where it lands rather than where it points from.
        let resolved = candidate
            .canonicalize()
            .map_err(|error| format!("cannot resolve `{}`: {error}", candidate.display()))?;
        match trusted_program_path(&resolved) {
            Ok(()) => return Ok(resolved),
            Err(error) => refusal = Some(error),
        }
    }
    Err(refusal.unwrap_or_else(|| {
        format!(
            "an administrator-locked profile found no trusted `{program}` in {}",
            TRUSTED_PROGRAM_DIRS.join(", ")
        )
    }))
}

#[cfg(unix)]
fn trusted_program_path(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;

    let mut current = Some(path);
    while let Some(entry) = current {
        let metadata = std::fs::metadata(entry)
            .map_err(|error| format!("cannot inspect `{}`: {error}", entry.display()))?;
        if metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err(format!(
                "an administrator-locked profile will not run `{}`: it, or a directory above it, is not owned by root or is writable by group or others",
                path.display()
            ));
        }
        current = entry
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty());
    }
    Ok(())
}

#[cfg(not(unix))]
fn trusted_program_path(path: &Path) -> Result<(), String> {
    Err(format!(
        "administrator-locked profiles require Unix ownership checks; unsupported for {}",
        path.display()
    ))
}

pub fn locked_profile_name(executable: &str) -> Result<Option<String>, String> {
    let file_name = Path::new(executable)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let Some(name) = file_name.strip_prefix(PROFILE_PREFIX) else {
        return Ok(None);
    };
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(
            "locked readonly profile name may contain only ASCII letters, digits, `-`, and `_`"
                .to_string(),
        );
    }
    Ok(Some(name.to_string()))
}

pub fn locked_profile_path(name: &str) -> PathBuf {
    Path::new("/etc/afpsql/readonly-profiles").join(format!("{name}.json"))
}

pub fn load_locked_profile(name: &str) -> Result<SessionConfig, String> {
    let path = locked_profile_path(name);
    let metadata = std::fs::metadata(&path).map_err(|error| {
        format!(
            "cannot read locked readonly profile {}: {error}",
            path.display()
        )
    })?;
    if !metadata.is_file() || metadata.len() > PROFILE_MAX_BYTES {
        return Err(format!(
            "locked readonly profile {} must be a regular file no larger than {PROFILE_MAX_BYTES} bytes",
            path.display()
        ));
    }
    validate_profile_permissions(&path, &metadata)?;
    let bytes = std::fs::read(&path).map_err(|error| {
        format!(
            "cannot read locked readonly profile {}: {error}",
            path.display()
        )
    })?;
    let session: SessionConfig = serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "invalid locked readonly profile {}: {error}",
            path.display()
        )
    })?;
    Ok(session)
}

#[cfg(unix)]
fn validate_profile_permissions(path: &Path, metadata: &std::fs::Metadata) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    if metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err(format!(
            "locked readonly profile {} must be owned by root and not writable by group or others",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_profile_permissions(path: &Path, _metadata: &std::fs::Metadata) -> Result<(), String> {
    Err(format!(
        "locked readonly profiles require Unix ownership checks; unsupported for {}",
        path.display()
    ))
}

fn is_opaque_value_flag(flag: &str) -> bool {
    matches!(
        flag,
        "--sql"
            | "--param"
            | "--dsn"
            | "--conninfo"
            | "--password"
            | "--host"
            | "--port"
            | "--user"
            | "--dbname"
            | "--ssh"
            | "--ssh-via"
            | "--ssh-option"
            | "--ssh-remote-socket"
            | "--ssh-sudo-user"
            | "--container-docker-name"
            | "--container-docker-user"
            | "--container-docker-context"
            | "--container-apple-name"
            | "--container-apple-user"
            | "--container-podman-name"
            | "--container-podman-user"
            | "--container-nerdctl-name"
            | "--container-nerdctl-user"
            | "--container-compose-service"
            | "--container-compose-user"
            | "--container-compose-file"
            | "--container-compose-project"
            | "--container-kubectl-pod"
            | "--container-kubectl-container"
            | "--container-kubectl-namespace"
            | "--container-kubectl-context"
            | "--permission"
            | "--explain"
            | "--mode"
            | "--output"
            | "--log"
            | "--batch-rows"
            | "--batch-bytes"
            | "--statement-timeout-ms"
            | "--lock-timeout-ms"
            | "--inline-max-rows"
            | "--inline-max-bytes"
            | "--command"
            | "--set"
            | "-c"
            | "-v"
            | "-h"
            | "-p"
            | "-U"
            | "-d"
    )
}

fn is_value_flag(flag: &str) -> bool {
    matches!(
        flag,
        "--stdout-file" | "--stderr-file" | "--sql-file" | "--file" | "-f"
    ) || is_container_runtime_flag(flag)
}

/// Every driver family's runtime override: naming the executable is what makes
/// container transport able to run an arbitrary command.
fn is_container_runtime_flag(flag: &str) -> bool {
    matches!(
        flag,
        "--container-docker-runtime"
            | "--container-apple-runtime"
            | "--container-podman-runtime"
            | "--container-nerdctl-runtime"
            | "--container-compose-runtime"
            | "--container-kubectl-runtime"
    )
}

fn validate_raw_value(flag: &str, value: &str) -> Result<(), String> {
    match flag {
        "--stdout-file" | "--stderr-file" => Err(format!(
            "{flag} is unavailable in afpsql-readonly because it can create or truncate local files"
        )),
        "--sql-file" | "--file" | "-f" if value != "-" => Err(format!(
            "{flag} only accepts `-` in afpsql-readonly; use inline SQL or stdin"
        )),
        _ if is_container_runtime_flag(flag) => Err(format!(
            "{flag} is unavailable in afpsql-readonly; each container driver family runs its own fixed runtime"
        )),
        _ => Ok(()),
    }
}

pub fn validate_session(session: &SessionConfig) -> Result<(), String> {
    validate_session_with_trust(session, false)
}

pub fn validate_session_with_trust(
    session: &SessionConfig,
    trusted_profile: bool,
) -> Result<(), String> {
    if session.container.runtime_override().is_some() && !trusted_profile {
        return Err(
            "custom container runtime is unavailable in afpsql-readonly; use a container driver family with its fixed runtime"
                .to_string(),
        );
    }
    for option in &session.ssh.options {
        let key = option
            .split_once('=')
            .map_or(option.as_str(), |(key, _)| key)
            .trim();
        if !matches!(
            key.to_ascii_lowercase().as_str(),
            "connectionattempts"
                | "connecttimeout"
                | "port"
                | "proxyjump"
                | "serveralivecountmax"
                | "serveraliveinterval"
                | "tcpkeepalive"
        ) {
            return Err(format!(
                "SSH option `{key}` is unavailable in afpsql-readonly; allowed options are ConnectionAttempts, ConnectTimeout, Port, ProxyJump, ServerAliveCountMax, ServerAliveInterval, and TCPKeepAlive"
            ));
        }
    }
    Ok(())
}

/// Everything afpsql-readonly refuses about one statement, decided before it
/// runs, as the message and hint the refusal is reported with.
///
/// `handler::execute_query` makes this same judgement and is the authority: a
/// route that skipped it would be a hole, not a shortcut. This exists for the
/// callers that must refuse *earlier* than that — a dry run, which never
/// reaches the executor, and `ui plan`, which would otherwise open a window and
/// let a person approve a statement afpsql was always going to refuse.
///
/// Call it only when the capability is [`crate::Capability::ReadOnly`];
/// `read_only_transaction` is the resolved permission's own answer, not a
/// second reading of the arguments.
#[must_use]
pub fn readonly_refusal(sql: &str, read_only_transaction: bool) -> Option<(String, &'static str)> {
    if let Err(error) = validate_sql(sql) {
        return Some((error, crate::readonly_hint()));
    }
    if !read_only_transaction {
        return Some((
            "write permission is unavailable in afpsql-readonly".to_string(),
            crate::readonly_hint(),
        ));
    }
    None
}

pub fn validate_sql(sql: &str) -> Result<(), String> {
    let keywords = leading_keywords(sql, 4);
    let is_transaction_control = matches!(
        keywords.first().map(String::as_str),
        Some("begin" | "commit" | "end" | "rollback" | "abort" | "savepoint" | "release")
    ) || matches!(keywords.as_slice(), [first, second, ..]
            if (first == "start" && second == "transaction")
                || (first == "prepare" && second == "transaction")
                || (first == "set" && second == "transaction"))
        || matches!(keywords.as_slice(), [first, second, third, ..]
            if first == "set"
                && ((second == "session" && third == "characteristics")
                    || (matches!(second.as_str(), "local" | "session")
                        && third == "transaction")));
    if is_transaction_control {
        Err(
            "transaction control SQL is unavailable in afpsql-readonly; use pipe begin/commit/rollback requests so the readonly state machine remains authoritative"
                .to_string(),
        )
    } else {
        Ok(())
    }
}

/// The first SQL keyword, lowercased, skipping leading comments.
///
/// Exposed so a caller that wants to *describe* a statement reads it the same
/// way this module classifies one, rather than writing a second scanner that
/// disagrees about `/* … */ update …`. Describing is all it is good for: the
/// keyword says what the statement starts with, never what it is allowed to do.
#[must_use]
pub fn leading_keyword(sql: &str) -> Option<String> {
    leading_keywords(sql, 1).into_iter().next()
}

fn leading_keywords(sql: &str, limit: usize) -> Vec<String> {
    let bytes = sql.as_bytes();
    let mut index = 0;
    let mut words = Vec::with_capacity(limit);
    while index < bytes.len() && words.len() < limit {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if bytes.get(index..index + 2) == Some(b"--") {
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            continue;
        }
        if bytes.get(index..index + 2) == Some(b"/*") {
            index += 2;
            let mut depth = 1usize;
            while index < bytes.len() && depth > 0 {
                if bytes.get(index..index + 2) == Some(b"/*") {
                    depth += 1;
                    index += 2;
                } else if bytes.get(index..index + 2) == Some(b"*/") {
                    depth -= 1;
                    index += 2;
                } else {
                    index += 1;
                }
            }
            continue;
        }
        let start = index;
        while index < bytes.len() && (bytes[index].is_ascii_alphabetic() || bytes[index] == b'_') {
            index += 1;
        }
        if start == index {
            break;
        }
        words.push(sql[start..index].to_ascii_lowercase());
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ContainerConfig, SshConfig};

    #[cfg(unix)]
    #[test]
    fn a_locked_profile_will_not_take_a_transport_program_from_the_caller() {
        // The whole point of a locked profile is that the caller cannot choose
        // what runs. A bare name goes through the caller's `PATH`, so an
        // `ssh` planted anywhere on it would run before any of the profile's
        // endpoint or permission rules were consulted.
        let planted = std::env::temp_dir().join(format!(
            "afpsql_evil_path_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        assert!(std::fs::create_dir_all(&planted).is_ok());
        let evil = planted.join("ssh");
        assert!(std::fs::write(&evil, "#!/bin/sh\nexit 0\n").is_ok());

        // Not on the trusted list, so it is never found by name...
        let by_name = trusted_program("ssh");
        assert!(
            by_name.as_deref() != Ok(evil.as_path()),
            "a locked profile resolved `ssh` to a file the caller planted"
        );
        // ...and it is refused even when named outright, because a caller can
        // write to it.
        let error = trusted_program(&evil.to_string_lossy())
            .expect_err("a caller-writable program must be refused");
        assert!(error.contains("not owned by root"), "{error}");

        // A relative path never resolves at all: it would be taken against
        // whatever working directory the caller happened to start in.
        assert!(trusted_program("./ssh").is_err());

        let _ = std::fs::remove_dir_all(&planted);
    }

    #[cfg(unix)]
    #[test]
    fn a_locked_profile_still_finds_the_systems_own_program() {
        // The check has to be one a real machine passes, or it is a check
        // nobody can turn on. `/bin/sh` is root-owned everywhere this runs.
        match trusted_program("sh") {
            Ok(path) => assert!(path.is_absolute(), "{}", path.display()),
            Err(error) => panic!("a trusted `sh` must resolve: {error}"),
        }
    }

    #[test]
    fn ordinary_raw_policy_allows_host_capabilities() {
        for args in [
            vec!["afpsql-readonly", "--stdout-file", "/tmp/out"],
            vec!["afpsql-readonly", "--stderr-file=/tmp/err"],
            vec!["afpsql-readonly", "--sql-file", "/tmp/query.sql"],
            vec!["afpsql-readonly", "--mode", "psql", "-f", "/tmp/query.sql"],
            // A redirect flag placed where a value-skipping walk would treat it
            // as the SQL/param value is still installed by the independent
            // stream-redirect scanner, so it must be rejected in every form.
            vec!["afpsql-readonly", "--sql", "--stdout-file=/tmp/out"],
            vec!["afpsql-readonly", "--sql", "--stdout-file", "/tmp/out"],
            vec!["afpsql-readonly", "--param", "x", "--stderr-file=/tmp/err"],
        ] {
            let args = args.into_iter().map(str::to_string).collect::<Vec<_>>();
            assert!(validate_raw_args(&args).is_ok(), "rejected {args:?}");
        }
        assert!(
            validate_raw_args(&["afpsql-readonly".to_string(), "--sql-file=-".to_string()]).is_ok()
        );
        for args in [
            // `--sql-file` has no scanner independent of the CLI parser, so an
            // inert value that merely looks like a flag stays inert.
            vec!["afpsql-readonly", "-c", "--sql-file=/tmp/not-a-flag"],
            vec![
                "afpsql-readonly",
                "--param",
                "1=--container-docker-runtime=touch",
            ],
        ] {
            let args = args.into_iter().map(str::to_string).collect::<Vec<_>>();
            assert!(validate_raw_args(&args).is_ok(), "rejected value {args:?}");
        }
    }

    #[test]
    fn ordinary_raw_policy_allows_arbitrary_explicit_secret_env_names() {
        for name in ["DATABASE_URL", "AFPSQL_DSN_SECRET", "AWS_SECRET_ACCESS_KEY"] {
            assert!(
                validate_raw_args(&["afpsql-readonly".to_string(), format!("--dsn=env:{name}")])
                    .is_ok()
            );
        }
    }

    #[test]
    fn locked_raw_policy_rejects_host_capabilities_in_any_order() {
        for prohibited in [
            vec!["--stdout-file", "/tmp/out"],
            vec!["--sql-file", "/tmp/query.sql"],
            vec!["--container-docker-runtime", "custom-runtime"],
            vec!["--container-kubectl-runtime", "custom-runtime"],
            vec!["--dsn", "env:AWS_SECRET_ACCESS_KEY"],
        ] {
            for args in [
                [
                    vec!["afpsql-readonly"],
                    prohibited.clone(),
                    vec!["--sql", "select 1"],
                ]
                .concat(),
                [
                    vec!["afpsql-readonly", "--sql", "select 1"],
                    prohibited.clone(),
                ]
                .concat(),
            ] {
                let args = args.into_iter().map(str::to_string).collect::<Vec<_>>();
                assert!(
                    validate_raw_args_for_profile(&args, true).is_err(),
                    "accepted {args:?}"
                );
            }
        }
    }

    #[test]
    fn readonly_session_policy_allows_only_safe_ssh_options_and_fixed_runtimes() {
        let allowed = SessionConfig {
            ssh: SshConfig {
                options: vec![
                    "ProxyJump=bastion".to_string(),
                    "ConnectTimeout=5".to_string(),
                ],
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(validate_session(&allowed).is_ok());

        for option in [
            "ProxyCommand=touch /tmp/pwned",
            "LocalCommand=touch /tmp/pwned",
            "Unknown=x",
        ] {
            let session = SessionConfig {
                ssh: SshConfig {
                    options: vec![option.to_string()],
                    ..Default::default()
                },
                ..Default::default()
            };
            assert!(validate_session(&session).is_err(), "accepted {option}");
        }

        let custom_runtime = SessionConfig {
            container: ContainerConfig {
                compose_runtime: Some("touch".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(validate_session(&custom_runtime).is_err());
        assert!(validate_session_with_trust(&custom_runtime, true).is_ok());
    }

    #[test]
    fn sql_policy_classifies_transaction_control_without_blocking_normal_sql() {
        for sql in [
            "BEGIN",
            "/* outer /* nested */ comment */ COMMIT",
            "-- comment\nROLLBACK TO SAVEPOINT s",
            "START TRANSACTION READ WRITE",
            "SAVEPOINT s",
            "RELEASE SAVEPOINT s",
            "PREPARE TRANSACTION 'x'",
            "SET TRANSACTION READ WRITE",
            "SET LOCAL TRANSACTION READ WRITE",
            "SET SESSION TRANSACTION READ WRITE",
            "SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE",
        ] {
            assert!(validate_sql(sql).is_err(), "accepted {sql}");
        }
        for sql in [
            "select 'commit'",
            "select begin from keywords",
            "set statement_timeout = 1000",
            "notify channel",
        ] {
            assert!(validate_sql(sql).is_ok(), "rejected {sql}");
        }
    }

    #[test]
    fn locked_profile_is_selected_by_executable_and_rejects_overrides() {
        assert_eq!(
            locked_profile_name("/usr/local/bin/afpsql-readonly").ok(),
            Some(None)
        );
        assert_eq!(
            locked_profile_name("/usr/local/bin/afpsql-readonly-production").ok(),
            Some(Some("production".to_string()))
        );
        assert!(locked_profile_name("afpsql-readonly-bad$name").is_err());
        for flag in [
            "--host",
            "--ssh",
            "--container-docker-runtime",
            "--password",
            "--dsn",
        ] {
            let args = vec![
                "afpsql-readonly-production".to_string(),
                flag.to_string(),
                "value".to_string(),
                "--sql".to_string(),
                "select 1".to_string(),
            ];
            assert!(
                validate_raw_args_for_profile(&args, true).is_err(),
                "accepted {flag}"
            );
        }
    }
}
