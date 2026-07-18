use crate::conn::resolve_pg_config;
use crate::db::ConnectError;
use crate::types::SessionConfig;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf};
use tokio::process::{ChildStderr, ChildStdin, ChildStdout};
use tokio::sync::oneshot;
use tokio_postgres::config::Host;
use tokio_postgres::{Client, NoTls};

const DEFAULT_DRIVER: &str = "docker";
const DEFAULT_REMOTE_HOST: &str = "127.0.0.1";
const DEFAULT_REMOTE_PORT: u16 = 5432;
const STDERR_CAPTURE_LIMIT: usize = 8 * 1024;
const STDERR_HINT_BYTES: usize = 512;
const BRIDGE_READY_PREFIX: &str = "AFPSQL_BRIDGE_OK";
const BRIDGE_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);

static BRIDGE_NONCE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn generate_bridge_nonce() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let counter = BRIDGE_NONCE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mixed = nanos
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(counter);
    format!("{mixed:016x}")
}

const PYTHON_BRIDGE: &str = r#"import os,select,socket,sys
mode=sys.argv[1]
if mode=="tcp":
    s=socket.create_connection((sys.argv[2], int(sys.argv[3])))
elif mode=="unix":
    s=socket.socket(socket.AF_UNIX)
    s.connect(sys.argv[2])
else:
    sys.stderr.write("unsupported bridge mode: "+mode+"\n")
    sys.exit(2)
stdin_obj=getattr(sys.stdin,"buffer",sys.stdin)
stdout_obj=getattr(sys.stdout,"buffer",sys.stdout)
stdin_fd=stdin_obj.fileno()
stdout_fd=stdout_obj.fileno()
stdin_open=True
while True:
    readers=[s]
    if stdin_open:
        readers.append(stdin_fd)
    ready,_,_=select.select(readers,[],[])
    if stdin_fd in ready:
        data=os.read(stdin_fd,65536)
        if data:
            s.sendall(data)
        else:
            stdin_open=False
            try:
                s.shutdown(socket.SHUT_WR)
            except OSError:
                pass
    if s in ready:
        data=s.recv(65536)
        if data:
            os.write(stdout_fd,data)
        else:
            break
"#;

const PERL_BRIDGE: &str = r#"use strict; use warnings; use IO::Socket::INET; use IO::Socket::UNIX; use IO::Select; use Socket qw(SOCK_STREAM);
my $mode = shift @ARGV;
my $sock;
if ($mode eq "tcp") {
    my ($host, $port) = @ARGV;
    $sock = IO::Socket::INET->new(PeerHost => $host, PeerPort => $port, Proto => "tcp") or die "connect tcp failed: $!";
} elsif ($mode eq "unix") {
    my ($path) = @ARGV;
    $sock = IO::Socket::UNIX->new(Type => SOCK_STREAM, Peer => $path) or die "connect unix failed: $!";
} else {
    die "unsupported bridge mode: $mode";
}
binmode STDIN; binmode STDOUT; binmode $sock;
my $select = IO::Select->new($sock, \*STDIN);
while (1) {
    for my $fh ($select->can_read) {
        if ($fh == \*STDIN) {
            my $buf = "";
            my $n = sysread(STDIN, $buf, 65536);
            die "read stdin failed: $!" unless defined $n;
            if ($n == 0) {
                $select->remove(\*STDIN);
                shutdown($sock, 1);
            } else {
                write_all($sock, $buf);
            }
        } else {
            my $buf = "";
            my $n = sysread($sock, $buf, 65536);
            die "read socket failed: $!" unless defined $n;
            exit 0 if $n == 0;
            write_all(\*STDOUT, $buf);
        }
    }
}
sub write_all {
    my ($fh, $buf) = @_;
    my $off = 0;
    my $len = length($buf);
    while ($off < $len) {
        my $n = syswrite($fh, $buf, $len - $off, $off);
        die "write failed: $!" unless defined $n;
        $off += $n;
    }
}
"#;

const SHELL_BRIDGE_BODY: &str = r#"if command -v python3 >/dev/null 2>&1; then
  echo "AFPSQL_BRIDGE_OK $AFPSQL_BRIDGE_NONCE" >&2
  exec python3 -c "$AFPSQL_CONTAINER_PY_BRIDGE" "$@"
fi
if command -v python >/dev/null 2>&1; then
  echo "AFPSQL_BRIDGE_OK $AFPSQL_BRIDGE_NONCE" >&2
  exec python -c "$AFPSQL_CONTAINER_PY_BRIDGE" "$@"
fi
if command -v perl >/dev/null 2>&1; then
  echo "AFPSQL_BRIDGE_OK $AFPSQL_BRIDGE_NONCE" >&2
  exec perl -e "$AFPSQL_CONTAINER_PERL_BRIDGE" "$@"
fi
echo "afpsql container bridge requires python3, python, or perl in the container" >&2
exit 127
"#;

pub struct ContainerBridgeGuard {
    child: Option<tokio::process::Child>,
    connection_task: Option<tokio::task::JoinHandle<()>>,
    stderr_task: Option<tokio::task::JoinHandle<()>>,
}

impl ContainerBridgeGuard {
    pub fn is_finished(&self) -> bool {
        self.connection_task
            .as_ref()
            .map(|task| task.is_finished())
            .unwrap_or(true)
    }

    pub async fn shutdown(mut self, timeout: Duration) {
        if let Some(task) = self.connection_task.take() {
            let mut task = task;
            if tokio::time::timeout(timeout, &mut task).await.is_err() {
                task.abort();
            }
        }
        if let Some(task) = self.stderr_task.take() {
            let mut task = task;
            if tokio::time::timeout(timeout, &mut task).await.is_err() {
                task.abort();
            }
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(timeout, child.wait()).await;
        }
    }
}

impl Drop for ContainerBridgeGuard {
    fn drop(&mut self) {
        if let Some(task) = self.connection_task.as_ref() {
            task.abort();
        }
        if let Some(task) = self.stderr_task.as_ref() {
            task.abort();
        }
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
        }
    }
}

struct ContainerStdioStream {
    stdout: ChildStdout,
    stdin: ChildStdin,
}

impl AsyncRead for ContainerStdioStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stdout).poll_read(cx, buf)
    }
}

impl AsyncWrite for ContainerStdioStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.stdin).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stdin).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stdin).poll_shutdown(cx)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContainerDriver {
    Docker,
    Podman,
    Nerdctl,
    Compose,
    Kubectl,
}

impl ContainerDriver {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "docker" => Ok(Self::Docker),
            "podman" => Ok(Self::Podman),
            "nerdctl" => Ok(Self::Nerdctl),
            "compose" | "docker-compose" => Ok(Self::Compose),
            "kubectl" | "kubernetes" | "k8s" => Ok(Self::Kubectl),
            _ => Err(format!(
                "unsupported container driver `{value}`; expected docker, podman, nerdctl, compose, or kubectl"
            )),
        }
    }

    fn default_runtime(self) -> &'static str {
        match self {
            Self::Docker | Self::Compose => "docker",
            Self::Podman => "podman",
            Self::Nerdctl => "nerdctl",
            Self::Kubectl => "kubectl",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ContainerSettings {
    driver: ContainerDriver,
    runtime: String,
    target: String,
    user: Option<String>,
    namespace: Option<String>,
    context: Option<String>,
    compose_files: Vec<String>,
    compose_project: Option<String>,
    pod_container: Option<String>,
    ssh_destination: Option<String>,
    ssh_options: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ContainerTarget {
    Tcp { host: String, port: u16 },
    UnixSocket { path: String },
}

pub async fn connect_stdio_bridge(
    cfg: &SessionConfig,
) -> Result<(Client, ContainerBridgeGuard), ConnectError> {
    let settings = resolve_container_settings(cfg)?;
    let target = resolve_container_target(cfg)?;
    let nonce = generate_bridge_nonce();
    let (program, args) =
        build_bridge_process(&settings, &target, &nonce).map_err(ConnectError::new)?;
    let mut child = tokio::process::Command::new(&program)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| ConnectError::new(format!("start container bridge failed: {e}")))?;
    let stderr = child.stderr.take();
    let stderr_capture = Arc::new(Mutex::new(Vec::new()));
    let (stderr_task, handshake_rx) = stderr
        .map(|stderr| spawn_stderr_capture(stderr, Arc::clone(&stderr_capture), nonce.clone()))
        .map(|(task, rx)| (Some(task), Some(rx)))
        .unwrap_or((None, None));

    let stdin = child.stdin.take().ok_or_else(|| {
        ConnectError::new("start container bridge failed: stdin pipe unavailable")
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        ConnectError::new("start container bridge failed: stdout pipe unavailable")
    })?;
    if let Err(mut err) = wait_bridge_handshake(&mut child, &stderr_capture, handshake_rx).await {
        // If the runtime could not find/exec the target, list the real container
        // names instead of telling the agent to "check the target name".
        let stderr_text = captured_stderr(&stderr_capture);
        if stderr_indicates_missing_target(&stderr_text)
            && let Some(list) = list_container_targets(&settings).await
        {
            err.hint = Some(match err.hint.take() {
                Some(base) => format!("{base}; available containers: {list}"),
                None => format!("available containers: {list}"),
            });
        }
        return Err(err);
    }
    let stream = ContainerStdioStream { stdout, stdin };
    let pg_cfg = resolve_pg_config(cfg)
        .map_err(|e| ConnectError::new(format!("invalid container connection config: {e}")))?;
    let (client, connection) = match pg_cfg.connect_raw(stream, NoTls).await {
        Ok(connection) => connection,
        Err(e) => {
            let status = match tokio::time::timeout(Duration::from_millis(100), child.wait()).await
            {
                Ok(Ok(status)) => Some(status),
                _ => child.try_wait().ok().flatten(),
            };
            tokio::time::sleep(Duration::from_millis(20)).await;
            let stderr_text = captured_stderr(&stderr_capture);
            return Err(enrich_container_connect_error(
                ConnectError::from_pg_error("connect through container bridge failed", e),
                status,
                &stderr_text,
            ));
        }
    };
    let connection_task = tokio::spawn(async move {
        let _ = connection.await;
    });

    Ok((
        client,
        ContainerBridgeGuard {
            child: Some(child),
            connection_task: Some(connection_task),
            stderr_task,
        },
    ))
}

fn spawn_stderr_capture(
    mut stderr: ChildStderr,
    capture: Arc<Mutex<Vec<u8>>>,
    nonce: String,
) -> (tokio::task::JoinHandle<()>, oneshot::Receiver<bool>) {
    let (handshake_tx, handshake_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        let mut buf = [0u8; 1024];
        let mut pending = Vec::new();
        let mut handshake_tx = Some(handshake_tx);
        while let Ok(n) = stderr.read(&mut buf).await {
            if n == 0 {
                break;
            }
            pending.extend_from_slice(&buf[..n]);
            while let Some(pos) = pending.iter().position(|b| *b == b'\n') {
                let line = pending.drain(..=pos).collect::<Vec<_>>();
                handle_stderr_line(line, &capture, &mut handshake_tx, &nonce);
            }
        }
        if !pending.is_empty() {
            handle_stderr_line(pending, &capture, &mut handshake_tx, &nonce);
        }
        if let Some(tx) = handshake_tx.take() {
            let _ = tx.send(false);
        }
    });
    (task, handshake_rx)
}

fn handle_stderr_line(
    line: Vec<u8>,
    capture: &Arc<Mutex<Vec<u8>>>,
    handshake_tx: &mut Option<oneshot::Sender<bool>>,
    nonce: &str,
) {
    if stderr_line_is_banner(&line, nonce) {
        if let Some(tx) = handshake_tx.take() {
            let _ = tx.send(true);
        }
        return;
    }
    append_stderr_capture(capture, &line);
}

fn stderr_line_is_banner(line: &[u8], nonce: &str) -> bool {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let expected = format!("{BRIDGE_READY_PREFIX} {nonce}");
    line == expected.as_bytes()
}

fn append_stderr_capture(capture: &Arc<Mutex<Vec<u8>>>, bytes: &[u8]) {
    if let Ok(mut captured) = capture.lock() {
        let remaining = STDERR_CAPTURE_LIMIT.saturating_sub(captured.len());
        if remaining > 0 {
            captured.extend_from_slice(&bytes[..bytes.len().min(remaining)]);
        }
    }
}

async fn wait_bridge_handshake(
    child: &mut tokio::process::Child,
    stderr_capture: &Arc<Mutex<Vec<u8>>>,
    handshake_rx: Option<oneshot::Receiver<bool>>,
) -> Result<(), ConnectError> {
    let Some(handshake_rx) = handshake_rx else {
        return Err(handshake_connect_error(
            child,
            stderr_capture,
            "container bridge stderr pipe unavailable for startup handshake",
        )
        .await);
    };

    match tokio::time::timeout(BRIDGE_HANDSHAKE_TIMEOUT, handshake_rx).await {
        Ok(Ok(true)) => Ok(()),
        Ok(Ok(false)) => Err(handshake_connect_error(
            child,
            stderr_capture,
            "container bridge exited before startup handshake",
        )
        .await),
        Ok(Err(_)) => Err(handshake_connect_error(
            child,
            stderr_capture,
            "container bridge startup handshake channel closed",
        )
        .await),
        Err(_) => Err(handshake_connect_error(
            child,
            stderr_capture,
            "container bridge did not emit startup handshake before timeout",
        )
        .await),
    }
}

async fn handshake_connect_error(
    child: &mut tokio::process::Child,
    stderr_capture: &Arc<Mutex<Vec<u8>>>,
    message: &str,
) -> ConnectError {
    let status = child.try_wait().ok().flatten();
    let stderr_text = captured_stderr(stderr_capture);
    let mut err = ConnectError::new(message);
    err.hint = Some(if stderr_text.is_empty() {
        format!(
            "the container bridge never reported {BRIDGE_READY_PREFIX}; check target name, runtime access, /bin/sh, and bridge interpreter prerequisites"
        )
    } else {
        format!(
            "the container bridge wrote diagnostics before {BRIDGE_READY_PREFIX}; container bridge stderr: {stderr_text}"
        )
    });
    enrich_container_connect_error(err, status, &stderr_text)
}

fn captured_stderr(capture: &Arc<Mutex<Vec<u8>>>) -> String {
    capture
        .lock()
        .ok()
        .map(|captured| sanitize_diagnostic(&captured))
        .filter(|text| !text.is_empty())
        .unwrap_or_default()
}

fn sanitize_diagnostic(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(STDERR_HINT_BYTES);
    for ch in text.chars() {
        let mapped = if matches!(ch, '\n' | '\t') {
            ch
        } else if ch.is_control() {
            ' '
        } else {
            ch
        };
        if out.len() + mapped.len_utf8() > STDERR_HINT_BYTES {
            break;
        }
        out.push(mapped);
    }
    out.trim().to_string()
}

fn enrich_container_connect_error(
    mut err: ConnectError,
    status: Option<std::process::ExitStatus>,
    stderr: &str,
) -> ConnectError {
    if let Some(hint) = container_bridge_hint(status, stderr) {
        err.hint = Some(match err.hint.take() {
            Some(base) => format!("{base}; {hint}"),
            None => hint,
        });
    }
    err
}

fn container_bridge_hint(status: Option<std::process::ExitStatus>, stderr: &str) -> Option<String> {
    let trimmed = stderr.trim();
    let lower = trimmed.to_ascii_lowercase();
    let base = if lower.contains("requires python3, python, or perl") {
        "the container bridge started but the container has no supported interpreter; install python3/python/perl, use a sidecar, or connect through the host instead"
    } else if lower.contains("sh:") && lower.contains("not found") {
        "the container bridge requires /bin/sh or compatible shell in the target container"
    } else if lower.contains("no such container")
        || lower.contains("not found")
        || lower.contains("is not running")
    {
        "the container runtime could not exec into the target; check the container target name and running state"
    } else if lower.contains("error from server") || lower.contains("pods") {
        "kubectl could not exec into the target; check context, namespace, pod name, and cluster access"
    } else if matches!(status.and_then(|s| s.code()), Some(125..=127)) {
        "the container runtime or bridge command exited before PostgreSQL handshake; check runtime access, target name, shell, and bridge prerequisites"
    } else if !trimmed.is_empty() {
        "the container bridge wrote diagnostics before PostgreSQL handshake failed"
    } else {
        return None;
    };

    if trimmed.is_empty() {
        Some(base.to_string())
    } else {
        Some(format!("{base}; container bridge stderr: {trimmed}"))
    }
}

fn stderr_indicates_missing_target(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    lower.contains("no such container")
        || lower.contains("is not running")
        || (lower.contains("not found") && lower.contains("container"))
}

/// A best-effort diagnostic listing is not worth blocking the error path on.
const CONTAINER_LIST_TIMEOUT: Duration = Duration::from_secs(3);

/// List the runtime's actual container names so a wrong-target error can name the
/// real options. Best-effort; `None` on any failure or unsupported driver.
async fn list_container_targets(settings: &ContainerSettings) -> Option<String> {
    if settings.ssh_destination.is_some() {
        return None;
    }
    let mut args = Vec::new();
    match settings.driver {
        ContainerDriver::Docker | ContainerDriver::Podman | ContainerDriver::Nerdctl => {
            if settings.driver == ContainerDriver::Docker
                && let Some(context) = settings.context.as_ref()
            {
                args.push(format!("--context={context}"));
            }
            // Running only — you can only exec into a running container, and the
            // generic hint already covers the stopped-container case.
            args.push("ps".to_string());
            args.push("--format".to_string());
            args.push("{{.Names}}".to_string());
        }
        // Compose service names and kubectl pods need different listings; skip
        // for now and fall back to the generic hint.
        ContainerDriver::Compose | ContainerDriver::Kubectl => return None,
    }
    let output = tokio::time::timeout(
        CONTAINER_LIST_TIMEOUT,
        tokio::process::Command::new(&settings.runtime)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    let names: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .map(|line| line.to_string())
        .collect();
    if names.is_empty() {
        return None;
    }
    // Cap the list but never hide truncation — a silently cut list reads as
    // "these are all of them" when the real target was dropped.
    const MAX_LISTED: usize = 30;
    if names.len() > MAX_LISTED {
        let shown = names[..MAX_LISTED].join(", ");
        let extra = names.len() - MAX_LISTED;
        Some(format!("{shown} (+{extra} more)"))
    } else {
        Some(names.join(", "))
    }
}

fn resolve_container_settings(cfg: &SessionConfig) -> Result<ContainerSettings, String> {
    let target = cfg
        .container
        .target
        .clone()
        .or_else(|| env_nonempty("AFPSQL_CONTAINER"));
    let ssh_destination = cfg
        .ssh
        .destination
        .clone()
        .or_else(|| env_nonempty("AFPSQL_SSH"));
    let namespace = cfg
        .container
        .namespace
        .clone()
        .or_else(|| env_nonempty("AFPSQL_CONTAINER_NAMESPACE"));
    let context = cfg
        .container
        .context
        .clone()
        .or_else(|| env_nonempty("AFPSQL_CONTAINER_CONTEXT"));
    let compose_files = if cfg.container.compose_files.is_empty() {
        env_colon_list("AFPSQL_CONTAINER_COMPOSE_FILE")
    } else {
        cfg.container.compose_files.clone()
    };
    let compose_project = cfg
        .container
        .compose_project
        .clone()
        .or_else(|| env_nonempty("AFPSQL_CONTAINER_COMPOSE_PROJECT"));
    let pod_container = cfg
        .container
        .pod_container
        .clone()
        .or_else(|| env_nonempty("AFPSQL_CONTAINER_POD_CONTAINER"));
    let has_container_fields = target.is_some()
        || cfg.container.driver.is_some()
        || cfg.container.runtime.is_some()
        || cfg.container.user.is_some()
        || namespace.is_some()
        || context.is_some()
        || !compose_files.is_empty()
        || compose_project.is_some()
        || pod_container.is_some();

    let Some(target) = target else {
        if has_container_fields {
            return Err(
                "--container is required when container transport options are set".to_string(),
            );
        }
        return Err("--container is required for container transport".to_string());
    };
    if target.trim().is_empty() {
        return Err("--container requires a non-empty target name".to_string());
    }
    if let Some(destination) = ssh_destination.as_ref() {
        if destination.trim().is_empty() {
            return Err("--ssh requires a non-empty USER@HOST destination".to_string());
        }
    } else if !cfg.ssh.options.is_empty() {
        return Err(
            "--ssh is required when --ssh-option is combined with container transport".to_string(),
        );
    }
    if cfg.ssh.has_tunnel_or_bridge_options() {
        return Err("container transport with --ssh supports only --ssh and --ssh-option; SSH tunnel and sudo bridge options are for non-container SSH transport".to_string());
    }

    let driver_name = cfg
        .container
        .driver
        .clone()
        .or_else(|| env_nonempty("AFPSQL_CONTAINER_DRIVER"))
        .unwrap_or_else(|| DEFAULT_DRIVER.to_string());
    let driver = ContainerDriver::parse(&driver_name)?;
    let user = cfg
        .container
        .user
        .clone()
        .or_else(|| env_nonempty("AFPSQL_CONTAINER_USER"));
    if driver == ContainerDriver::Kubectl && user.is_some() {
        return Err(
            "--container-user is not supported with --container-driver kubectl".to_string(),
        );
    }
    validate_driver_scoped_options(
        driver,
        namespace.as_ref(),
        context.as_ref(),
        &compose_files,
        compose_project.as_ref(),
        pod_container.as_ref(),
    )?;

    Ok(ContainerSettings {
        runtime: cfg
            .container
            .runtime
            .clone()
            .or_else(|| env_nonempty("AFPSQL_CONTAINER_RUNTIME"))
            .unwrap_or_else(|| driver.default_runtime().to_string()),
        driver,
        target,
        user,
        namespace,
        context,
        compose_files,
        compose_project,
        pod_container,
        ssh_destination,
        ssh_options: cfg.ssh.options.clone(),
    })
}

fn validate_driver_scoped_options(
    driver: ContainerDriver,
    namespace: Option<&String>,
    context: Option<&String>,
    compose_files: &[String],
    compose_project: Option<&String>,
    pod_container: Option<&String>,
) -> Result<(), String> {
    match driver {
        ContainerDriver::Docker => {
            reject_present(
                namespace,
                "--container-namespace requires --container-driver kubectl",
            )?;
            reject_present(
                pod_container,
                "--container-pod-container requires --container-driver kubectl",
            )?;
            reject_nonempty(
                compose_files,
                "--container-compose-file requires --container-driver compose",
            )?;
            reject_present(
                compose_project,
                "--container-compose-project requires --container-driver compose",
            )?;
        }
        ContainerDriver::Compose => {
            reject_present(
                namespace,
                "--container-namespace requires --container-driver kubectl",
            )?;
            reject_present(
                context,
                "--container-context requires --container-driver docker or kubectl",
            )?;
            reject_present(
                pod_container,
                "--container-pod-container requires --container-driver kubectl",
            )?;
        }
        ContainerDriver::Kubectl => {
            reject_nonempty(
                compose_files,
                "--container-compose-file requires --container-driver compose",
            )?;
            reject_present(
                compose_project,
                "--container-compose-project requires --container-driver compose",
            )?;
        }
        ContainerDriver::Podman | ContainerDriver::Nerdctl => {
            reject_present(
                namespace,
                "--container-namespace requires --container-driver kubectl",
            )?;
            reject_present(
                context,
                "--container-context requires --container-driver docker or kubectl",
            )?;
            reject_nonempty(
                compose_files,
                "--container-compose-file requires --container-driver compose",
            )?;
            reject_present(
                compose_project,
                "--container-compose-project requires --container-driver compose",
            )?;
            reject_present(
                pod_container,
                "--container-pod-container requires --container-driver kubectl",
            )?;
        }
    }
    Ok(())
}

fn reject_present<T>(value: Option<T>, message: &str) -> Result<(), String> {
    if value.is_some() {
        Err(message.to_string())
    } else {
        Ok(())
    }
}

fn reject_nonempty<T>(value: &[T], message: &str) -> Result<(), String> {
    if value.is_empty() {
        Ok(())
    } else {
        Err(message.to_string())
    }
}

fn resolve_container_target(cfg: &SessionConfig) -> Result<ContainerTarget, String> {
    let pg_cfg =
        resolve_pg_config(cfg).map_err(|e| format!("invalid container connection config: {e}"))?;
    let hosts = pg_cfg.get_hosts();
    if hosts.len() > 1 {
        return Err("container transport supports a single PostgreSQL host".to_string());
    }
    let ports = pg_cfg.get_ports();
    if ports.len() > 1 {
        return Err("container transport supports a single PostgreSQL port".to_string());
    }
    let port = ports.first().copied().unwrap_or(DEFAULT_REMOTE_PORT);
    match hosts.first() {
        Some(Host::Tcp(host)) => {
            if host.starts_with('/') {
                Ok(ContainerTarget::UnixSocket {
                    path: socket_file_from_dir(host, port),
                })
            } else {
                Ok(ContainerTarget::Tcp {
                    host: host.clone(),
                    port,
                })
            }
        }
        #[cfg(unix)]
        Some(Host::Unix(path)) => Ok(ContainerTarget::UnixSocket {
            path: socket_file_from_dir(&path.to_string_lossy(), port),
        }),
        None => Ok(ContainerTarget::Tcp {
            host: DEFAULT_REMOTE_HOST.to_string(),
            port,
        }),
    }
}

fn socket_file_from_dir(dir: &str, port: u16) -> String {
    format!("{}/.s.PGSQL.{port}", dir.trim_end_matches('/'))
}

fn build_bridge_process(
    settings: &ContainerSettings,
    target: &ContainerTarget,
    nonce: &str,
) -> Result<(String, Vec<String>), String> {
    if settings.ssh_destination.is_some() {
        Ok((
            "ssh".to_string(),
            build_bridge_ssh_args(settings, target, nonce)?,
        ))
    } else {
        Ok((
            settings.runtime.clone(),
            build_container_exec_args(settings, target, nonce)?,
        ))
    }
}

fn build_container_exec_args(
    settings: &ContainerSettings,
    target: &ContainerTarget,
    nonce: &str,
) -> Result<Vec<String>, String> {
    let command = bridge_command_args(target, nonce);
    let mut args = Vec::new();
    match settings.driver {
        ContainerDriver::Docker | ContainerDriver::Podman | ContainerDriver::Nerdctl => {
            if settings.driver == ContainerDriver::Docker
                && let Some(context) = settings.context.as_ref()
            {
                args.push(format!("--context={context}"));
            }
            args.push("exec".to_string());
            args.push("-i".to_string());
            if let Some(user) = settings.user.as_ref() {
                args.push("--user".to_string());
                args.push(user.clone());
            }
            args.push(settings.target.clone());
            args.extend(command);
        }
        ContainerDriver::Compose => {
            if !is_docker_compose_runtime(&settings.runtime) {
                args.push("compose".to_string());
            }
            for file in &settings.compose_files {
                args.push("-f".to_string());
                args.push(file.clone());
            }
            if let Some(project) = settings.compose_project.as_ref() {
                args.push("-p".to_string());
                args.push(project.clone());
            }
            args.push("exec".to_string());
            args.push("-T".to_string());
            if let Some(user) = settings.user.as_ref() {
                args.push("--user".to_string());
                args.push(user.clone());
            }
            args.push(settings.target.clone());
            args.extend(command);
        }
        ContainerDriver::Kubectl => {
            if let Some(context) = settings.context.as_ref() {
                args.push(format!("--context={context}"));
            }
            if let Some(namespace) = settings.namespace.as_ref() {
                args.push(format!("--namespace={namespace}"));
            }
            args.push("exec".to_string());
            args.push("-i".to_string());
            args.push(settings.target.clone());
            if let Some(container) = settings.pod_container.as_ref() {
                args.push("-c".to_string());
                args.push(container.clone());
            }
            args.push("--".to_string());
            args.extend(command);
        }
    }
    Ok(args)
}

fn is_docker_compose_runtime(runtime: &str) -> bool {
    runtime
        .rsplit(['/', '\\'])
        .next()
        .is_some_and(|name| name == "docker-compose")
}

fn bridge_command_args(target: &ContainerTarget, nonce: &str) -> Vec<String> {
    let mut args = vec![
        "sh".to_string(),
        "-c".to_string(),
        shell_bridge_script(nonce),
        "afpsql-container-bridge".to_string(),
    ];
    match target {
        ContainerTarget::Tcp { host, port } => {
            args.push("tcp".to_string());
            args.push(host.clone());
            args.push(port.to_string());
        }
        ContainerTarget::UnixSocket { path } => {
            args.push("unix".to_string());
            args.push(path.clone());
        }
    }
    args
}

fn shell_bridge_script(nonce: &str) -> String {
    format!(
        "AFPSQL_BRIDGE_NONCE={}; AFPSQL_CONTAINER_PY_BRIDGE={}; AFPSQL_CONTAINER_PERL_BRIDGE={}; {}",
        shell_quote(nonce),
        shell_quote(PYTHON_BRIDGE),
        shell_quote(PERL_BRIDGE),
        SHELL_BRIDGE_BODY
    )
}

fn build_bridge_ssh_args(
    settings: &ContainerSettings,
    target: &ContainerTarget,
    nonce: &str,
) -> Result<Vec<String>, String> {
    let mut args = vec![
        "-T".to_string(),
        "-o".to_string(),
        "BatchMode=yes".to_string(),
    ];
    for option in &settings.ssh_options {
        args.push("-o".to_string());
        args.push(option.clone());
    }
    if let Some(destination) = settings.ssh_destination.as_ref() {
        args.push(destination.clone());
    }
    args.push(remote_container_command(settings, target, nonce)?);
    Ok(args)
}

fn remote_container_command(
    settings: &ContainerSettings,
    target: &ContainerTarget,
    nonce: &str,
) -> Result<String, String> {
    Ok(std::iter::once(settings.runtime.clone())
        .chain(build_container_exec_args(settings, target, nonce)?)
        .map(|arg| shell_quote(&arg))
        .collect::<Vec<_>>()
        .join(" "))
}

fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    if value.bytes().all(|b| {
        b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'/' | b':' | b'@' | b'=')
    }) {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn env_colon_list(name: &str) -> Vec<String> {
    std::env::var(name)
        .ok()
        .map(|value| {
            value
                .split(':')
                .filter(|part| !part.is_empty())
                .map(std::string::ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_NONCE: &str = "deadbeefcafef00d";

    fn settings(driver: ContainerDriver) -> ContainerSettings {
        ContainerSettings {
            driver,
            runtime: driver.default_runtime().to_string(),
            target: "pg".to_string(),
            user: Some("postgres".to_string()),
            namespace: None,
            context: None,
            compose_files: vec![],
            compose_project: None,
            pod_container: None,
            ssh_destination: None,
            ssh_options: vec![],
        }
    }

    #[test]
    fn typed_drivers_select_fixed_default_runtimes() {
        for (driver, runtime) in [
            (ContainerDriver::Docker, "docker"),
            (ContainerDriver::Podman, "podman"),
            (ContainerDriver::Nerdctl, "nerdctl"),
            (ContainerDriver::Compose, "docker"),
            (ContainerDriver::Kubectl, "kubectl"),
        ] {
            assert_eq!(driver.default_runtime(), runtime);
        }
    }

    #[test]
    fn missing_target_detection() {
        assert!(stderr_indicates_missing_target(
            "Error response from daemon: No such container: pg-typo"
        ));
        assert!(stderr_indicates_missing_target("container is not running"));
        assert!(!stderr_indicates_missing_target(
            "sh: python3: not found in PATH"
        ));
    }

    #[test]
    fn bridge_args_target_tcp_for_container_cli_like_driver() -> Result<(), String> {
        let args = build_container_exec_args(
            &settings(ContainerDriver::Docker),
            &ContainerTarget::Tcp {
                host: "127.0.0.1".to_string(),
                port: 5432,
            },
            TEST_NONCE,
        )?;
        assert_eq!(args[0], "exec");
        assert!(args.contains(&"-i".to_string()));
        assert!(args.contains(&"--user".to_string()));
        assert!(args.contains(&"postgres".to_string()));
        assert!(args.contains(&"pg".to_string()));
        assert!(args.contains(&"tcp".to_string()));
        assert!(args.contains(&"127.0.0.1".to_string()));
        assert!(args.contains(&"5432".to_string()));
        assert!(!args.contains(&"-e".to_string()));
        Ok(())
    }

    #[test]
    fn bridge_args_target_unix_socket() -> Result<(), String> {
        let args = build_container_exec_args(
            &ContainerSettings {
                user: None,
                ..settings(ContainerDriver::Docker)
            },
            &ContainerTarget::UnixSocket {
                path: "/var/run/postgresql/.s.PGSQL.5432".to_string(),
            },
            TEST_NONCE,
        )?;
        assert!(args.contains(&"unix".to_string()));
        assert!(args.contains(&"/var/run/postgresql/.s.PGSQL.5432".to_string()));
        Ok(())
    }

    #[test]
    fn compose_driver_uses_compose_exec_without_tty() -> Result<(), String> {
        let settings = ContainerSettings {
            compose_files: vec!["compose.yml".to_string(), "compose.prod.yml".to_string()],
            compose_project: Some("demo".to_string()),
            ..settings(ContainerDriver::Compose)
        };
        let (program, args) = build_bridge_process(
            &settings,
            &ContainerTarget::Tcp {
                host: "db".to_string(),
                port: 5432,
            },
            TEST_NONCE,
        )?;
        assert_eq!(program, "docker");
        assert_eq!(
            &args[0..8],
            [
                "compose",
                "-f",
                "compose.yml",
                "-f",
                "compose.prod.yml",
                "-p",
                "demo",
                "exec"
            ]
        );
        assert_eq!(args[8], "-T");
        assert!(args.contains(&"pg".to_string()));
        Ok(())
    }

    #[test]
    fn compose_runtime_docker_compose_skips_subcommand_prefix() -> Result<(), String> {
        let settings = ContainerSettings {
            runtime: "/usr/local/bin/docker-compose".to_string(),
            ..settings(ContainerDriver::Compose)
        };
        let (program, args) = build_bridge_process(
            &settings,
            &ContainerTarget::Tcp {
                host: "db".to_string(),
                port: 5432,
            },
            TEST_NONCE,
        )?;
        assert_eq!(program, "/usr/local/bin/docker-compose");
        assert_eq!(&args[0..2], ["exec", "-T"]);
        Ok(())
    }

    #[test]
    fn kubectl_driver_uses_exec_separator() -> Result<(), String> {
        let settings = ContainerSettings {
            user: None,
            namespace: Some("prod".to_string()),
            context: Some("cluster-a".to_string()),
            ..settings(ContainerDriver::Kubectl)
        };
        let (program, args) = build_bridge_process(
            &settings,
            &ContainerTarget::Tcp {
                host: "127.0.0.1".to_string(),
                port: 5432,
            },
            TEST_NONCE,
        )?;
        assert_eq!(program, "kubectl");
        assert_eq!(
            &args[0..6],
            [
                "--context=cluster-a",
                "--namespace=prod",
                "exec",
                "-i",
                "pg",
                "--"
            ]
        );
        assert!(args.contains(&"sh".to_string()));
        Ok(())
    }

    #[test]
    fn kubectl_driver_inserts_pod_container_before_separator() -> Result<(), String> {
        let settings = ContainerSettings {
            user: None,
            pod_container: Some("postgres".to_string()),
            ..settings(ContainerDriver::Kubectl)
        };
        let (_, args) = build_bridge_process(
            &settings,
            &ContainerTarget::Tcp {
                host: "127.0.0.1".to_string(),
                port: 5432,
            },
            TEST_NONCE,
        )?;
        assert_eq!(
            &args[0..7],
            ["exec", "-i", "pg", "-c", "postgres", "--", "sh"]
        );
        Ok(())
    }

    #[test]
    fn bridge_banner_line_is_not_captured_as_diagnostic() {
        let capture = Arc::new(Mutex::new(Vec::new()));
        let (tx, mut rx) = oneshot::channel();
        let mut tx = Some(tx);
        let line = format!("AFPSQL_BRIDGE_OK {TEST_NONCE}\n").into_bytes();
        handle_stderr_line(line, &capture, &mut tx, TEST_NONCE);
        assert!(matches!(rx.try_recv(), Ok(true)));
        assert!(captured_stderr(&capture).is_empty());
    }

    #[test]
    fn bridge_banner_without_matching_nonce_is_treated_as_diagnostic() {
        let capture = Arc::new(Mutex::new(Vec::new()));
        let (tx, mut rx) = oneshot::channel();
        let mut tx = Some(tx);
        let line = b"AFPSQL_BRIDGE_OK 0000000000000000\n".to_vec();
        handle_stderr_line(line, &capture, &mut tx, TEST_NONCE);
        assert!(rx.try_recv().is_err());
        assert!(captured_stderr(&capture).contains("AFPSQL_BRIDGE_OK"));
    }

    #[test]
    fn bridge_banner_without_nonce_is_treated_as_diagnostic() {
        let capture = Arc::new(Mutex::new(Vec::new()));
        let (tx, mut rx) = oneshot::channel();
        let mut tx = Some(tx);
        handle_stderr_line(
            b"AFPSQL_BRIDGE_OK\n".to_vec(),
            &capture,
            &mut tx,
            TEST_NONCE,
        );
        assert!(rx.try_recv().is_err());
        assert!(captured_stderr(&capture).contains("AFPSQL_BRIDGE_OK"));
    }

    #[test]
    fn sanitize_diagnostic_strips_control_chars_and_truncates() {
        let input = b"\x1b[31mboom\x1b[0m\nnext line\x00trailing";
        let cleaned = sanitize_diagnostic(input);
        assert!(!cleaned.contains('\x1b'));
        assert!(!cleaned.contains('\x00'));
        assert!(cleaned.contains("boom"));
        assert!(cleaned.contains("next line"));

        let big = vec![b'A'; 4096];
        let cleaned = sanitize_diagnostic(&big);
        assert!(cleaned.len() <= STDERR_HINT_BYTES);
    }

    #[test]
    fn bridge_process_uses_remote_ssh_container_command() -> Result<(), String> {
        let settings = ContainerSettings {
            driver: ContainerDriver::Podman,
            runtime: "podman".to_string(),
            target: "pg remote".to_string(),
            user: Some("postgres".to_string()),
            namespace: None,
            context: None,
            compose_files: vec![],
            compose_project: None,
            pod_container: None,
            ssh_destination: Some("root@example.com".to_string()),
            ssh_options: vec!["ProxyJump=bastion".to_string()],
        };
        let (program, args) = build_bridge_process(
            &settings,
            &ContainerTarget::Tcp {
                host: "host.containers.internal".to_string(),
                port: 5432,
            },
            TEST_NONCE,
        )?;

        assert_eq!(program, "ssh");
        assert!(args.contains(&"BatchMode=yes".to_string()));
        assert!(args.contains(&"ProxyJump=bastion".to_string()));
        assert_eq!(
            args.iter().rev().nth(1),
            Some(&"root@example.com".to_string())
        );

        let command = args.last().cloned().unwrap_or_default();
        assert!(command.starts_with("podman exec -i "));
        assert!(command.contains("'pg remote'"));
        assert!(command.contains("host.containers.internal"));
        Ok(())
    }

    #[test]
    fn remote_container_command_quotes_shell_torture_values() -> Result<(), String> {
        let target = "pg 'quoted' \"$USER\" `whoami`\nnext".to_string();
        let user = "postgres '$HOME' `id`\nnext".to_string();
        let context = "ctx '$VAR' `cmd`\nnext".to_string();
        let settings = ContainerSettings {
            driver: ContainerDriver::Docker,
            runtime: "docker".to_string(),
            target: target.clone(),
            user: Some(user.clone()),
            namespace: None,
            context: Some(context.clone()),
            compose_files: vec![],
            compose_project: None,
            pod_container: None,
            ssh_destination: Some("root@example.com".to_string()),
            ssh_options: vec![],
        };

        let command = remote_container_command(
            &settings,
            &ContainerTarget::Tcp {
                host: "127.0.0.1".to_string(),
                port: 5432,
            },
            TEST_NONCE,
        )?;

        assert!(command.contains(&shell_quote(&format!("--context={context}"))));
        assert!(command.contains(&shell_quote(&target)));
        assert!(command.contains(&shell_quote(&user)));
        Ok(())
    }

    #[test]
    fn target_from_dsn_tcp() -> Result<(), String> {
        let cfg = SessionConfig {
            container: crate::types::ContainerConfig {
                target: Some("pg".to_string()),
                ..Default::default()
            },
            dsn_secret: Some("postgresql://u:p@127.0.0.1:6543/db".to_string()),
            ..Default::default()
        };
        assert_eq!(
            resolve_container_target(&cfg)?,
            ContainerTarget::Tcp {
                host: "127.0.0.1".to_string(),
                port: 6543,
            }
        );
        Ok(())
    }

    #[test]
    fn target_from_unix_socket_dir() -> Result<(), String> {
        let cfg = SessionConfig {
            container: crate::types::ContainerConfig {
                target: Some("pg".to_string()),
                ..Default::default()
            },
            host: Some("/var/run/postgresql".to_string()),
            port: Some(5433),
            ..Default::default()
        };
        assert_eq!(
            resolve_container_target(&cfg)?,
            ContainerTarget::UnixSocket {
                path: "/var/run/postgresql/.s.PGSQL.5433".to_string(),
            }
        );
        Ok(())
    }

    #[test]
    fn settings_require_container_when_options_set() {
        let cfg = SessionConfig {
            container: crate::types::ContainerConfig {
                user: Some("postgres".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };
        let err = resolve_container_settings(&cfg);
        assert!(matches!(err, Err(message) if message.contains("--container is required")));
    }

    #[test]
    fn settings_require_ssh_destination_when_ssh_options_set() {
        let cfg = SessionConfig {
            container: crate::types::ContainerConfig {
                target: Some("pg".to_string()),
                ..Default::default()
            },
            ssh: crate::types::SshConfig {
                options: vec!["ProxyJump=bastion".to_string()],
                ..Default::default()
            },
            ..Default::default()
        };
        let err = resolve_container_settings(&cfg);
        assert!(matches!(err, Err(message) if message.contains("--ssh is required")));
    }

    #[test]
    fn settings_reject_tunnel_only_ssh_options_for_container_transport() {
        let cfg = SessionConfig {
            container: crate::types::ContainerConfig {
                target: Some("pg".to_string()),
                ..Default::default()
            },
            ssh: crate::types::SshConfig {
                destination: Some("user@example.com".to_string()),
                local_port: Some(15432),
                ..Default::default()
            },
            ..Default::default()
        };
        let err = resolve_container_settings(&cfg);
        assert!(matches!(err, Err(message) if message.contains("supports only --ssh")));
    }

    #[test]
    fn settings_reject_kubectl_user() {
        let cfg = SessionConfig {
            container: crate::types::ContainerConfig {
                target: Some("pod/app".to_string()),
                driver: Some("kubectl".to_string()),
                user: Some("postgres".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };
        let err = resolve_container_settings(&cfg);
        assert!(matches!(err, Err(message) if message.contains("not supported")));
    }

    #[test]
    fn settings_reject_scoped_flags_for_wrong_driver() {
        let cfg = SessionConfig {
            container: crate::types::ContainerConfig {
                target: Some("pg".to_string()),
                driver: Some("podman".to_string()),
                context: Some("prod".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };
        let err = resolve_container_settings(&cfg);
        assert!(matches!(err, Err(message) if message.contains("--container-context requires")));
    }

    #[test]
    fn settings_accept_compose_file_env_fallback() {
        let _guard = crate::test_env::env_lock();
        let old = std::env::var("AFPSQL_CONTAINER_COMPOSE_FILE").ok();
        // SAFETY: this test module's environment lock is held for the mutation.
        unsafe {
            std::env::set_var("AFPSQL_CONTAINER_COMPOSE_FILE", "base.yml:prod.yml");
        }
        let cfg = SessionConfig {
            container: crate::types::ContainerConfig {
                target: Some("pg".to_string()),
                driver: Some("compose".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };
        let settings = resolve_container_settings(&cfg);
        match old {
            // SAFETY: this test module's environment lock is still held here.
            Some(value) => unsafe { std::env::set_var("AFPSQL_CONTAINER_COMPOSE_FILE", value) },
            // SAFETY: this test module's environment lock is still held here.
            None => unsafe { std::env::remove_var("AFPSQL_CONTAINER_COMPOSE_FILE") },
        }
        assert!(matches!(
            settings,
            Ok(ContainerSettings {
                compose_files,
                ..
            }) if compose_files == vec!["base.yml".to_string(), "prod.yml".to_string()]
        ));
    }

    #[test]
    fn container_bridge_hint_classifies_interpreter_failure() {
        let hint = container_bridge_hint(
            None,
            "afpsql container bridge requires python3, python, or perl in the container",
        )
        .unwrap_or_default();
        assert!(hint.contains("no supported interpreter"));
        assert!(hint.contains("container bridge stderr"));
    }
}
