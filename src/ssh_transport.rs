use crate::conn::resolve_pg_config;
use crate::types::SessionConfig;
use std::net::{TcpListener, TcpStream};
use std::pin::Pin;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Mutex;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::process::{ChildStdin, ChildStdout};
use tokio_postgres::{Client, NoTls};

const DEFAULT_LOCAL_HOST: &str = "127.0.0.1";
const DEFAULT_REMOTE_HOST: &str = "127.0.0.1";
const DEFAULT_REMOTE_PORT: u16 = 5432;
const TUNNEL_READY_TIMEOUT: Duration = Duration::from_secs(5);
const TUNNEL_READY_POLL: Duration = Duration::from_millis(50);
const TUNNEL_READY_SETTLE: Duration = Duration::from_millis(100);

const PYTHON_UNIX_BRIDGE: &str = r#"import os,select,socket,sys
paths=sys.argv[1:]
last_error=""
s=None
for p in paths:
    s=socket.socket(socket.AF_UNIX)
    try:
        s.connect(p)
        break
    except OSError as e:
        last_error=f"{p}: {e}"
        s.close()
        s=None
if s is None:
    sys.stderr.write("could not connect PostgreSQL socket; tried "+", ".join(paths)+"; "+last_error+"\n")
    sys.exit(1)
stdin_fd=sys.stdin.buffer.fileno()
stdout_fd=sys.stdout.buffer.fileno()
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

pub struct SshTunnelGuard {
    child: Mutex<Child>,
    pub local_host: String,
    pub local_port: u16,
}

impl Drop for SshTunnelGuard {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub struct SshBridgeGuard {
    child: tokio::process::Child,
    connection_task: tokio::task::JoinHandle<()>,
}

impl SshBridgeGuard {
    pub fn is_finished(&self) -> bool {
        self.connection_task.is_finished()
    }
}

impl Drop for SshBridgeGuard {
    fn drop(&mut self) {
        self.connection_task.abort();
        let _ = self.child.start_kill();
    }
}

struct SshStdioStream {
    stdout: ChildStdout,
    stdin: ChildStdin,
}

impl AsyncRead for SshStdioStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stdout).poll_read(cx, buf)
    }
}

impl AsyncWrite for SshStdioStream {
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct SshSettings {
    destination: String,
    options: Vec<String>,
    local_host: String,
    local_port: Option<u16>,
    remote_socket: Option<String>,
    sudo_user: Option<String>,
}

enum TunnelTarget {
    Tcp { host: String, port: u16 },
    UnixSocket { path: String },
}

pub fn needs_stdio_bridge(cfg: &SessionConfig) -> bool {
    resolve_ssh_settings(cfg)
        .ok()
        .flatten()
        .and_then(|settings| settings.sudo_user)
        .is_some()
}

pub async fn prepare_session(
    cfg: &SessionConfig,
) -> Result<(SessionConfig, Option<SshTunnelGuard>), String> {
    let Some(settings) = resolve_ssh_settings(cfg)? else {
        return Ok((cfg.clone(), None));
    };

    if settings.sudo_user.is_some() {
        return Err("--ssh-sudo-user requires SSH Unix-socket bridge mode".to_string());
    }
    reject_secret_conn_strings_with_ssh(cfg)?;

    let target = if let Some(path) = settings.remote_socket.clone() {
        TunnelTarget::UnixSocket { path }
    } else {
        let host = effective_remote_host(cfg);
        let port = effective_remote_port(cfg);
        if host.starts_with('/') {
            TunnelTarget::UnixSocket {
                path: socket_file_from_dir(&host, port),
            }
        } else {
            TunnelTarget::Tcp { host, port }
        }
    };
    let tunnel = start_tunnel(&settings, target).await?;

    let mut local_cfg = cfg.clone();
    local_cfg.dsn_secret = None;
    local_cfg.conninfo_secret = None;
    local_cfg.host = Some(tunnel.local_host.clone());
    local_cfg.port = Some(tunnel.local_port);
    Ok((local_cfg, Some(tunnel)))
}

pub async fn connect_stdio_bridge(cfg: &SessionConfig) -> Result<(Client, SshBridgeGuard), String> {
    let Some(settings) = resolve_ssh_settings(cfg)? else {
        return Err("--ssh is required for SSH Unix-socket bridge mode".to_string());
    };
    let Some(sudo_user) = settings.sudo_user.clone() else {
        return Err("--ssh-sudo-user is required for SSH Unix-socket bridge mode".to_string());
    };
    let remote_sockets = bridge_socket_candidates(&settings, cfg)?;
    reject_secret_conn_strings_with_ssh(cfg)?;

    let args = build_bridge_ssh_args(&settings, &sudo_user, &remote_sockets);
    let mut child = tokio::process::Command::new("ssh")
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("start ssh socket bridge failed: {e}"))?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "start ssh socket bridge failed: stdin pipe unavailable".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "start ssh socket bridge failed: stdout pipe unavailable".to_string())?;
    let stream = SshStdioStream { stdout, stdin };
    let pg_cfg =
        resolve_pg_config(cfg).map_err(|e| format!("invalid bridge connection config: {e}"))?;
    let (client, connection) = pg_cfg
        .connect_raw(stream, NoTls)
        .await
        .map_err(|e| format!("connect through ssh socket bridge failed: {e}"))?;
    let connection_task = tokio::spawn(async move {
        let _ = connection.await;
    });

    Ok((
        client,
        SshBridgeGuard {
            child,
            connection_task,
        },
    ))
}

fn socket_file_from_dir(dir: &str, port: u16) -> String {
    format!("{}/.s.PGSQL.{port}", dir.trim_end_matches('/'))
}

fn bridge_socket_candidates(
    settings: &SshSettings,
    cfg: &SessionConfig,
) -> Result<Vec<String>, String> {
    if let Some(remote_socket) = settings.remote_socket.clone() {
        return Ok(vec![remote_socket]);
    }

    let host = effective_remote_host(cfg);
    let port = effective_remote_port(cfg);
    if host.starts_with('/') {
        return Ok(vec![socket_file_from_dir(&host, port)]);
    }

    Err("--ssh-sudo-user requires an explicit remote PostgreSQL Unix socket".to_string())
}

async fn start_tunnel(
    settings: &SshSettings,
    target: TunnelTarget,
) -> Result<SshTunnelGuard, String> {
    let local_port = match settings.local_port {
        Some(port) => {
            ensure_local_port_available(&settings.local_host, port)?;
            port
        }
        None => allocate_local_port(&settings.local_host)?,
    };
    let args = build_tunnel_ssh_args(settings, &target, local_port);
    let child = Command::new("ssh")
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("start ssh tunnel failed: {e}"))?;

    let guard = SshTunnelGuard {
        child: Mutex::new(child),
        local_host: settings.local_host.clone(),
        local_port,
    };
    wait_for_tunnel_ready(&guard).await?;
    Ok(guard)
}

fn allocate_local_port(local_host: &str) -> Result<u16, String> {
    TcpListener::bind((local_host, 0))
        .map_err(|e| format!("allocate ssh local port on {local_host} failed: {e}"))?
        .local_addr()
        .map(|addr| addr.port())
        .map_err(|e| format!("read allocated ssh local port failed: {e}"))
}

fn ensure_local_port_available(local_host: &str, local_port: u16) -> Result<(), String> {
    TcpListener::bind((local_host, local_port))
        .map(|_| ())
        .map_err(|e| format!("ssh local port {local_host}:{local_port} is not available: {e}"))
}

async fn wait_for_tunnel_ready(guard: &SshTunnelGuard) -> Result<(), String> {
    let start = Instant::now();
    loop {
        if let Some(status) = tunnel_child_status(guard)? {
            return Err(format!(
                "ssh tunnel exited before it became ready with status {status}"
            ));
        }
        if TcpStream::connect((guard.local_host.as_str(), guard.local_port)).is_ok() {
            tokio::time::sleep(TUNNEL_READY_SETTLE).await;
            if let Some(status) = tunnel_child_status(guard)? {
                return Err(format!(
                    "ssh tunnel exited after local port became reachable with status {status}"
                ));
            }
            return Ok(());
        }
        if start.elapsed() >= TUNNEL_READY_TIMEOUT {
            return Err(format!(
                "ssh tunnel did not become ready on {}:{}",
                guard.local_host, guard.local_port
            ));
        }
        tokio::time::sleep(TUNNEL_READY_POLL).await;
    }
}

fn tunnel_child_status(guard: &SshTunnelGuard) -> Result<Option<ExitStatus>, String> {
    let mut child = guard
        .child
        .lock()
        .map_err(|_| "ssh tunnel child lock poisoned".to_string())?;
    child
        .try_wait()
        .map_err(|e| format!("check ssh tunnel status failed: {e}"))
}

fn resolve_ssh_settings(cfg: &SessionConfig) -> Result<Option<SshSettings>, String> {
    let destination = cfg.ssh.clone();
    let has_ssh_fields = cfg.ssh.is_some()
        || !cfg.ssh_options.is_empty()
        || cfg.ssh_local_host.is_some()
        || cfg.ssh_local_port.is_some()
        || cfg.ssh_remote_socket.is_some()
        || cfg.ssh_sudo_user.is_some();

    let Some(destination) = destination else {
        if has_ssh_fields {
            return Err("--ssh is required when SSH transport options are set".to_string());
        }
        return Ok(None);
    };
    if destination.trim().is_empty() {
        return Err("--ssh requires a non-empty USER@HOST destination".to_string());
    }

    Ok(Some(SshSettings {
        destination,
        options: cfg.ssh_options.clone(),
        local_host: cfg
            .ssh_local_host
            .clone()
            .unwrap_or_else(|| DEFAULT_LOCAL_HOST.to_string()),
        local_port: cfg.ssh_local_port,
        remote_socket: cfg.ssh_remote_socket.clone(),
        sudo_user: cfg.ssh_sudo_user.clone(),
    }))
}

fn reject_secret_conn_strings_with_ssh(cfg: &SessionConfig) -> Result<(), String> {
    if cfg.dsn_secret.is_some()
        || cfg.conninfo_secret.is_some()
        || env_nonempty("AFPSQL_DSN_SECRET").is_some()
        || env_nonempty("AFPSQL_CONNINFO_SECRET").is_some()
    {
        return Err("SSH transport currently supports discrete connection fields only; use --host/--port/--user/--dbname/--password-secret-env instead of --dsn-secret or --conninfo-secret".to_string());
    }
    Ok(())
}

fn effective_remote_host(cfg: &SessionConfig) -> String {
    cfg.host
        .clone()
        .or_else(|| env_nonempty("AFPSQL_HOST"))
        .or_else(|| env_nonempty("PGHOST"))
        .unwrap_or_else(|| DEFAULT_REMOTE_HOST.to_string())
}

fn effective_remote_port(cfg: &SessionConfig) -> u16 {
    cfg.port
        .or_else(|| env_u16("AFPSQL_PORT").and_then(Result::ok))
        .or_else(|| env_u16("PGPORT").and_then(Result::ok))
        .unwrap_or(DEFAULT_REMOTE_PORT)
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn env_u16(name: &str) -> Option<Result<u16, String>> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse::<u16>()
                .map_err(|_| format!("{name} must be a valid TCP port"))
        })
}

fn build_tunnel_ssh_args(
    settings: &SshSettings,
    target: &TunnelTarget,
    local_port: u16,
) -> Vec<String> {
    let mut args = base_ssh_args(settings);
    args.push("-N".to_string());
    args.push("-L".to_string());
    args.push(match target {
        TunnelTarget::Tcp { host, port } => {
            format!("{}:{local_port}:{host}:{port}", settings.local_host)
        }
        TunnelTarget::UnixSocket { path } => {
            format!("{}:{local_port}:{path}", settings.local_host)
        }
    });
    args.push(settings.destination.clone());
    args
}

fn build_bridge_ssh_args(
    settings: &SshSettings,
    sudo_user: &str,
    remote_sockets: &[String],
) -> Vec<String> {
    let mut args = base_ssh_args(settings);
    args.push(settings.destination.clone());
    args.push(remote_bridge_command(sudo_user, remote_sockets));
    args
}

fn base_ssh_args(settings: &SshSettings) -> Vec<String> {
    let mut args = vec![
        "-T".to_string(),
        "-o".to_string(),
        "BatchMode=yes".to_string(),
        "-o".to_string(),
        "ExitOnForwardFailure=yes".to_string(),
    ];
    for option in &settings.options {
        args.push("-o".to_string());
        args.push(option.clone());
    }
    args
}

fn remote_bridge_command(sudo_user: &str, remote_sockets: &[String]) -> String {
    let mut parts = vec![
        "sudo".to_string(),
        "-n".to_string(),
        "-u".to_string(),
        shell_quote(sudo_user),
        "python3".to_string(),
        "-c".to_string(),
        shell_quote(PYTHON_UNIX_BRIDGE),
    ];
    parts.extend(remote_sockets.iter().map(|socket| shell_quote(socket)));
    parts.join(" ")
}

fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    if value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'/' | b':' | b'@'))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunnel_args_target_remote_tcp() {
        let settings = SshSettings {
            destination: "app@example.com".to_string(),
            options: vec!["ProxyJump=bastion".to_string()],
            local_host: "127.0.0.1".to_string(),
            local_port: Some(15432),
            remote_socket: None,
            sudo_user: None,
        };
        let args = build_tunnel_ssh_args(
            &settings,
            &TunnelTarget::Tcp {
                host: "127.0.0.1".to_string(),
                port: 5432,
            },
            15432,
        );
        assert!(args.contains(&"ProxyJump=bastion".to_string()));
        assert!(args.contains(&"127.0.0.1:15432:127.0.0.1:5432".to_string()));
        assert_eq!(args.last(), Some(&"app@example.com".to_string()));
    }

    #[test]
    fn bridge_args_use_sudo_noninteractive_python_socket_bridge() {
        let settings = SshSettings {
            destination: "user@example.com".to_string(),
            options: vec![],
            local_host: "127.0.0.1".to_string(),
            local_port: None,
            remote_socket: Some("/var/run/postgresql/.s.PGSQL.5432".to_string()),
            sudo_user: Some("postgres".to_string()),
        };
        let args = build_bridge_ssh_args(
            &settings,
            "postgres",
            &["/var/run/postgresql/.s.PGSQL.5432".to_string()],
        );
        assert!(args.iter().any(|arg| arg == "BatchMode=yes"));
        let command = args.last().cloned().unwrap_or_default();
        assert!(command.contains("sudo -n -u postgres python3 -c"));
        assert!(command.contains("/var/run/postgresql/.s.PGSQL.5432"));
    }

    #[test]
    fn bridge_candidates_require_explicit_socket_for_sudo_bridge() -> Result<(), String> {
        let settings = SshSettings {
            destination: "user@example.com".to_string(),
            options: vec![],
            local_host: "127.0.0.1".to_string(),
            local_port: None,
            remote_socket: None,
            sudo_user: Some("postgres".to_string()),
        };
        let cfg = SessionConfig {
            ssh: Some("user@example.com".to_string()),
            ssh_sudo_user: Some("postgres".to_string()),
            ..Default::default()
        };

        let Err(err) = bridge_socket_candidates(&settings, &cfg) else {
            return Err("expected explicit socket error".to_string());
        };
        assert!(err.contains("explicit remote PostgreSQL Unix socket"));
        Ok(())
    }

    #[test]
    fn bridge_candidates_honor_explicit_socket_and_host_dir() -> Result<(), String> {
        let explicit_settings = SshSettings {
            destination: "user@example.com".to_string(),
            options: vec![],
            local_host: "127.0.0.1".to_string(),
            local_port: None,
            remote_socket: Some("/custom/.s.PGSQL.6543".to_string()),
            sudo_user: Some("postgres".to_string()),
        };
        let cfg = SessionConfig::default();
        assert_eq!(
            bridge_socket_candidates(&explicit_settings, &cfg)?,
            vec!["/custom/.s.PGSQL.6543".to_string()]
        );

        let dir_settings = SshSettings {
            destination: "user@example.com".to_string(),
            options: vec![],
            local_host: "127.0.0.1".to_string(),
            local_port: None,
            remote_socket: None,
            sudo_user: Some("postgres".to_string()),
        };
        let cfg = SessionConfig {
            host: Some("/run/postgresql".to_string()),
            port: Some(5433),
            ..Default::default()
        };
        assert_eq!(
            bridge_socket_candidates(&dir_settings, &cfg)?,
            vec!["/run/postgresql/.s.PGSQL.5433".to_string()]
        );
        Ok(())
    }

    #[test]
    fn shell_quote_handles_spaces_and_quotes() {
        assert_eq!(shell_quote("postgres"), "postgres");
        assert_eq!(shell_quote("/tmp/socket path"), "'/tmp/socket path'");
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn socket_dir_maps_to_postgres_socket_file() {
        assert_eq!(
            socket_file_from_dir("/var/run/postgresql/", 5433),
            "/var/run/postgresql/.s.PGSQL.5433"
        );
    }

    #[test]
    fn local_port_availability_rejects_bound_port() -> Result<(), String> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();

        let unavailable = ensure_local_port_available("127.0.0.1", port);
        assert!(matches!(
            unavailable,
            Err(message) if message.contains("not available")
        ));

        drop(listener);
        assert!(ensure_local_port_available("127.0.0.1", port).is_ok());
        Ok(())
    }
}
