use crate::conn::resolve_pg_config;
use crate::types::SessionConfig;
use std::io::Read as _;
use std::net::{TcpListener, TcpStream};
use std::pin::Pin;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf};
use tokio::process::{ChildStderr, ChildStdin, ChildStdout};
use tokio_postgres::{Client, NoTls};

const DEFAULT_LOCAL_HOST: &str = "127.0.0.1";
const DEFAULT_REMOTE_HOST: &str = "127.0.0.1";
const DEFAULT_REMOTE_PORT: u16 = 5432;
const TUNNEL_READY_TIMEOUT: Duration = Duration::from_secs(5);
const TUNNEL_READY_POLL: Duration = Duration::from_millis(50);
const TUNNEL_READY_SETTLE: Duration = Duration::from_millis(100);
const STDERR_CAPTURE_LIMIT: usize = 8192;
const STDERR_HINT_BYTES: usize = 2048;

const PYTHON_STREAM_BRIDGE: &str = r#"import os,select,socket,sys
mode=sys.argv[1] if len(sys.argv)>1 else ""
last_error=""
s=None
timeout=float(os.environ.get("AFPSQL_SSH_BRIDGE_CONNECT_TIMEOUT","10"))
if mode=="tcp":
    host=sys.argv[2]
    port=int(sys.argv[3])
    s=socket.socket(socket.AF_INET,socket.SOCK_STREAM)
    s.settimeout(timeout)
    try:
        s.connect((host,port))
        s.settimeout(None)
    except OSError as e:
        sys.stderr.write("could not connect PostgreSQL tcp "+host+":"+str(port)+": "+str(e)+"\n")
        sys.exit(1)
elif mode=="unix":
    paths=sys.argv[2:]
    for p in paths:
        s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM)
        s.settimeout(timeout)
        try:
            s.connect(p)
            s.settimeout(None)
            break
        except OSError as e:
            last_error=p+": "+str(e)
            s.close()
            s=None
    if s is None:
        sys.stderr.write("could not connect PostgreSQL socket; tried "+", ".join(paths)+"; "+last_error+"\n")
        sys.exit(1)
else:
    sys.stderr.write("unsupported afpsql ssh bridge mode: "+mode+"\n")
    sys.exit(1)
stdin=getattr(sys.stdin,"buffer",sys.stdin)
stdout=getattr(sys.stdout,"buffer",sys.stdout)
stdin_fd=stdin.fileno()
stdout_fd=stdout.fileno()
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

const PERL_STREAM_BRIDGE: &str = r#"my $mode=shift @ARGV||"";
my $s;
if ($mode eq "tcp") {
    my ($host,$port)=@ARGV;
    socket($s, PF_INET, SOCK_STREAM, getprotobyname("tcp")) or die "socket tcp: $!\n";
    my $addr=inet_aton($host) or die "could not resolve PostgreSQL tcp $host:$port\n";
    eval {
        local $SIG{ALRM}=sub { die "connect timeout\n" };
        alarm(10);
        connect($s, sockaddr_in($port,$addr)) or die "could not connect PostgreSQL tcp $host:$port: $!\n";
        alarm(0);
    };
    if ($@) { print STDERR $@; exit 1; }
} elsif ($mode eq "unix") {
    my $last="";
    for my $path (@ARGV) {
        socket($s, PF_UNIX, SOCK_STREAM, 0) or die "socket unix: $!\n";
        if (connect($s, sockaddr_un($path))) { $last=""; last; }
        $last="$path: $!";
        close($s);
        undef $s;
    }
    if (!$s) {
        print STDERR "could not connect PostgreSQL socket; tried ".join(", ", @ARGV)."; $last\n";
        exit 1;
    }
} else {
    print STDERR "unsupported afpsql ssh bridge mode: $mode\n";
    exit 1;
}
binmode STDIN;
binmode STDOUT;
binmode $s;
sub write_all {
    my ($fh,$buf)=@_;
    my $off=0;
    while ($off < length($buf)) {
        my $n=syswrite($fh,$buf,length($buf)-$off,$off);
        if (!defined $n) { print STDERR "bridge write failed: $!\n"; exit 1; }
        $off += $n;
    }
}
my $sel=IO::Select->new($s, \*STDIN);
while (1) {
    for my $fh ($sel->can_read) {
        if (fileno($fh) == fileno(STDIN)) {
            my $buf="";
            my $n=sysread(STDIN,$buf,65536);
            if ($n) {
                write_all($s,$buf);
            } else {
                $sel->remove(\*STDIN);
                shutdown($s,1);
            }
        } else {
            my $buf="";
            my $n=sysread($s,$buf,65536);
            if ($n) {
                write_all(\*STDOUT,$buf);
            } else {
                exit 0;
            }
        }
    }
}
"#;

pub struct SshTunnelGuard {
    child: Mutex<Child>,
    stderr_capture: Arc<Mutex<Vec<u8>>>,
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
    child: Option<tokio::process::Child>,
    connection_task: Option<tokio::task::JoinHandle<()>>,
    stderr_task: Option<tokio::task::JoinHandle<()>>,
}

impl SshBridgeGuard {
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
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(timeout, child.wait()).await;
        }
        if let Some(task) = self.stderr_task.take() {
            task.abort();
        }
    }
}

impl Drop for SshBridgeGuard {
    fn drop(&mut self) {
        if let Some(task) = self.connection_task.as_ref() {
            task.abort();
        }
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
        }
        if let Some(task) = self.stderr_task.as_ref() {
            task.abort();
        }
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
    via: Vec<String>,
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
        .map(|settings| settings.sudo_user.is_some() || !settings.via.is_empty())
        .unwrap_or(false)
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
    if !settings.via.is_empty() {
        return Err("--ssh-via requires SSH stdio bridge mode".to_string());
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
        return Err("--ssh is required for SSH stdio bridge mode".to_string());
    };
    if settings.sudo_user.is_none() && settings.via.is_empty() {
        return Err(
            "--ssh-via or --ssh-sudo-user is required for SSH stdio bridge mode".to_string(),
        );
    }
    reject_secret_conn_strings_with_ssh(cfg)?;

    let target = bridge_target(&settings, cfg)?;
    let command = remote_stream_bridge_command(&settings, &target);
    let args = build_bridge_ssh_args(&settings, &command);
    let mut child = tokio::process::Command::new("ssh")
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("start ssh bridge failed: {e}"))?;
    let stderr_capture = Arc::new(Mutex::new(Vec::new()));
    let stderr_task = child
        .stderr
        .take()
        .map(|stderr| spawn_async_stderr_capture(stderr, Arc::clone(&stderr_capture)));

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "start ssh bridge failed: stdin pipe unavailable".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "start ssh bridge failed: stdout pipe unavailable".to_string())?;
    let stream = SshStdioStream { stdout, stdin };
    let pg_cfg =
        resolve_pg_config(cfg).map_err(|e| format!("invalid bridge connection config: {e}"))?;
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
            return Err(ssh_bridge_error(
                format!("connect through ssh bridge failed: {e}"),
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
        SshBridgeGuard {
            child: Some(child),
            connection_task: Some(connection_task),
            stderr_task,
        },
    ))
}

enum BridgeTarget {
    Tcp { host: String, port: u16 },
    UnixSocket { paths: Vec<String> },
}

fn bridge_target(settings: &SshSettings, cfg: &SessionConfig) -> Result<BridgeTarget, String> {
    if settings.sudo_user.is_some() {
        return bridge_socket_candidates(settings, cfg)
            .map(|paths| BridgeTarget::UnixSocket { paths });
    }
    if let Some(remote_socket) = settings.remote_socket.clone() {
        return Ok(BridgeTarget::UnixSocket {
            paths: vec![remote_socket],
        });
    }
    let host = effective_remote_host(cfg);
    let port = effective_remote_port(cfg);
    if host.starts_with('/') {
        Ok(BridgeTarget::UnixSocket {
            paths: vec![socket_file_from_dir(&host, port)],
        })
    } else {
        Ok(BridgeTarget::Tcp { host, port })
    }
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
    let mut child = Command::new("ssh")
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("start ssh tunnel failed: {e}"))?;
    let stderr_capture = Arc::new(Mutex::new(Vec::new()));
    if let Some(stderr) = child.stderr.take() {
        spawn_blocking_stderr_capture(stderr, Arc::clone(&stderr_capture));
    }

    let guard = SshTunnelGuard {
        child: Mutex::new(child),
        stderr_capture,
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
            return Err(tunnel_error(
                guard,
                format!("ssh tunnel exited before it became ready with status {status}"),
            ));
        }
        if TcpStream::connect((guard.local_host.as_str(), guard.local_port)).is_ok() {
            tokio::time::sleep(TUNNEL_READY_SETTLE).await;
            if let Some(status) = tunnel_child_status(guard)? {
                return Err(tunnel_error(
                    guard,
                    format!(
                        "ssh tunnel exited after local port became reachable with status {status}"
                    ),
                ));
            }
            return Ok(());
        }
        if start.elapsed() >= TUNNEL_READY_TIMEOUT {
            return Err(tunnel_error(
                guard,
                format!(
                    "ssh tunnel did not become ready on {}:{}",
                    guard.local_host, guard.local_port
                ),
            ));
        }
        tokio::time::sleep(TUNNEL_READY_POLL).await;
    }
}

fn tunnel_error(guard: &SshTunnelGuard, base: String) -> String {
    append_ssh_stderr(base, &captured_stderr(&guard.stderr_capture))
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
    let destination = cfg.ssh.destination.clone();
    let has_ssh_fields = cfg.ssh.has_transport_fields();

    let Some(destination) = destination else {
        if has_ssh_fields {
            return Err("--ssh is required when SSH transport options are set".to_string());
        }
        return Ok(None);
    };
    if destination.trim().is_empty() {
        return Err("--ssh requires a non-empty USER@HOST destination".to_string());
    }
    let via = cfg
        .ssh
        .via
        .iter()
        .map(|hop| hop.trim())
        .filter(|hop| !hop.is_empty())
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>();
    if !cfg.ssh.via.is_empty() && via.len() != cfg.ssh.via.len() {
        return Err("--ssh-via requires non-empty USER@HOST hop values".to_string());
    }
    if !via.is_empty() && (cfg.ssh.local_host.is_some() || cfg.ssh.local_port.is_some()) {
        return Err("--ssh-via uses SSH stdio bridge mode and cannot be combined with --ssh-local-host or --ssh-local-port".to_string());
    }

    Ok(Some(SshSettings {
        destination,
        via,
        options: cfg.ssh.options.clone(),
        local_host: cfg
            .ssh
            .local_host
            .clone()
            .unwrap_or_else(|| DEFAULT_LOCAL_HOST.to_string()),
        local_port: cfg.ssh.local_port,
        remote_socket: cfg.ssh.remote_socket.clone(),
        sudo_user: cfg.ssh.sudo_user.clone(),
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

fn build_bridge_ssh_args(settings: &SshSettings, final_command: &str) -> Vec<String> {
    let mut args = base_ssh_args(settings);
    let mut chain = settings.via.clone();
    chain.push(settings.destination.clone());
    let Some(first_hop) = chain.first() else {
        return args;
    };
    let mut command = final_command.to_string();
    for hop in chain.iter().skip(1).rev() {
        command = remote_ssh_command(hop, &command);
    }
    args.push(first_hop.clone());
    args.push(command);
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

fn remote_ssh_command(destination: &str, remote_command: &str) -> String {
    shell_join(&[
        "ssh".to_string(),
        "-T".to_string(),
        "-o".to_string(),
        "BatchMode=yes".to_string(),
        "-o".to_string(),
        "ExitOnForwardFailure=yes".to_string(),
        "-o".to_string(),
        "ConnectTimeout=10".to_string(),
        destination.to_string(),
        remote_command.to_string(),
    ])
}

fn remote_stream_bridge_command(settings: &SshSettings, target: &BridgeTarget) -> String {
    let bridge_args = match target {
        BridgeTarget::Tcp { host, port } => {
            vec!["tcp".to_string(), host.clone(), port.to_string()]
        }
        BridgeTarget::UnixSocket { paths } => {
            let mut args = vec!["unix".to_string()];
            args.extend(paths.iter().cloned());
            args
        }
    };
    let launcher = bridge_launcher_command(&bridge_args);
    if let Some(sudo_user) = settings.sudo_user.as_ref() {
        return shell_join(&[
            "sudo".to_string(),
            "-n".to_string(),
            "-u".to_string(),
            sudo_user.clone(),
            "sh".to_string(),
            "-c".to_string(),
            launcher,
        ]);
    }
    launcher
}

fn bridge_launcher_command(args: &[String]) -> String {
    let bridge_args = shell_join(args);
    format!(
        "if command -v python3 >/dev/null 2>&1; then exec python3 -c {} {}; fi; \
         if command -v python >/dev/null 2>&1; then exec python -c {} {}; fi; \
         if command -v perl >/dev/null 2>&1; then exec perl -MIO::Select -MSocket -e {} {}; fi; \
         echo {} >&2; exit 127",
        shell_quote(PYTHON_STREAM_BRIDGE),
        bridge_args,
        shell_quote(PYTHON_STREAM_BRIDGE),
        bridge_args,
        shell_quote(PERL_STREAM_BRIDGE),
        bridge_args,
        shell_quote("afpsql ssh bridge requires python3, python, or perl on the remote host")
    )
}

fn shell_join(parts: &[String]) -> String {
    parts
        .iter()
        .map(|part| shell_quote(part))
        .collect::<Vec<_>>()
        .join(" ")
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

fn spawn_blocking_stderr_capture(
    mut stderr: std::process::ChildStderr,
    capture: Arc<Mutex<Vec<u8>>>,
) {
    let _handle = std::thread::spawn(move || {
        let mut buf = [0u8; 1024];
        loop {
            match stderr.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => append_stderr_capture(&capture, &buf[..n]),
            }
        }
    });
}

fn spawn_async_stderr_capture(
    mut stderr: ChildStderr,
    capture: Arc<Mutex<Vec<u8>>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut buf = [0u8; 1024];
        while let Ok(n) = stderr.read(&mut buf).await {
            if n == 0 {
                break;
            }
            append_stderr_capture(&capture, &buf[..n]);
        }
    })
}

fn append_stderr_capture(capture: &Arc<Mutex<Vec<u8>>>, bytes: &[u8]) {
    if let Ok(mut captured) = capture.lock() {
        let remaining = STDERR_CAPTURE_LIMIT.saturating_sub(captured.len());
        if remaining > 0 {
            captured.extend_from_slice(&bytes[..bytes.len().min(remaining)]);
        }
    }
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

fn ssh_bridge_error(base: String, status: Option<ExitStatus>, stderr: &str) -> String {
    let with_status = match status {
        Some(status) => format!("{base}; ssh status: {status}"),
        None => base,
    };
    append_ssh_stderr(with_status, stderr)
}

fn append_ssh_stderr(base: String, stderr: &str) -> String {
    if stderr.trim().is_empty() {
        base
    } else {
        format!("{base}; ssh stderr: {stderr}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunnel_args_target_remote_tcp() {
        let settings = SshSettings {
            destination: "app@example.com".to_string(),
            via: vec![],
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
            via: vec![],
            options: vec![],
            local_host: "127.0.0.1".to_string(),
            local_port: None,
            remote_socket: Some("/var/run/postgresql/.s.PGSQL.5432".to_string()),
            sudo_user: Some("postgres".to_string()),
        };
        let command = remote_stream_bridge_command(
            &settings,
            &BridgeTarget::UnixSocket {
                paths: vec!["/var/run/postgresql/.s.PGSQL.5432".to_string()],
            },
        );
        let args = build_bridge_ssh_args(&settings, &command);
        assert!(args.iter().any(|arg| arg == "BatchMode=yes"));
        let command = args.last().cloned().unwrap_or_default();
        assert!(command.contains("sudo -n -u postgres sh -c"));
        assert!(command.contains("python3 -c"));
        assert!(command.contains("/var/run/postgresql/.s.PGSQL.5432"));
    }

    #[test]
    fn bridge_args_chain_via_hosts_and_final_tcp_bridge() {
        let settings = SshSettings {
            destination: "ubuntu@db.internal".to_string(),
            via: vec!["ubuntu@bastion".to_string()],
            options: vec!["ConnectTimeout=10".to_string()],
            local_host: "127.0.0.1".to_string(),
            local_port: None,
            remote_socket: None,
            sudo_user: None,
        };
        let command = remote_stream_bridge_command(
            &settings,
            &BridgeTarget::Tcp {
                host: "localhost".to_string(),
                port: 5432,
            },
        );
        let args = build_bridge_ssh_args(&settings, &command);
        assert!(args.contains(&"ubuntu@bastion".to_string()));
        assert!(args.iter().any(|arg| arg == "ConnectTimeout=10"));
        let remote = args.last().cloned().unwrap_or_default();
        assert!(remote.contains("ssh -T -o 'BatchMode=yes'"));
        assert!(remote.contains("ubuntu@db.internal"));
        assert!(remote.contains("python3 -c"));
        assert!(remote.contains("tcp localhost 5432"));
    }

    #[test]
    fn bridge_candidates_require_explicit_socket_for_sudo_bridge() -> Result<(), String> {
        let settings = SshSettings {
            destination: "user@example.com".to_string(),
            via: vec![],
            options: vec![],
            local_host: "127.0.0.1".to_string(),
            local_port: None,
            remote_socket: None,
            sudo_user: Some("postgres".to_string()),
        };
        let cfg = SessionConfig {
            ssh: crate::types::SshConfig {
                destination: Some("user@example.com".to_string()),
                sudo_user: Some("postgres".to_string()),
                ..Default::default()
            },
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
            via: vec![],
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
            via: vec![],
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
    fn ssh_bridge_error_includes_sanitized_stderr() {
        let capture = Arc::new(Mutex::new(Vec::new()));
        append_stderr_capture(&capture, b"Permission denied (publickey).\x1b\n");
        let err = ssh_bridge_error(
            "connect through ssh bridge failed".to_string(),
            None,
            &captured_stderr(&capture),
        );
        assert!(err.contains("connect through ssh bridge failed"));
        assert!(err.contains("ssh stderr: Permission denied (publickey)."));
        assert!(!err.contains('\x1b'));
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
