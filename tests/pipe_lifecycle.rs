#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_postgres::{Client, Config};

#[path = "support/env.rs"]
mod test_env;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
const EVENT_TIMEOUT: Duration = Duration::from_secs(3);
const EXIT_TIMEOUT: Duration = Duration::from_secs(7);

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    format!(
        "{}_{}_{}",
        std::process::id(),
        nanos,
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
    )
}

fn lock_key() -> i64 {
    NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed) as i64 + i64::from(std::process::id()) * 1_000_000
}

fn unavailable_dsn() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("unused loopback endpoint");
    let port = listener.local_addr().expect("unused port").port();
    drop(listener);
    format!("host=127.0.0.1 port={port} connect_timeout=1")
}

struct TestDb {
    client: Client,
    connection: JoinHandle<()>,
}

impl TestDb {
    async fn connect() -> Self {
        let tls = native_tls::TlsConnector::builder()
            .danger_accept_invalid_certs(true)
            .danger_accept_invalid_hostnames(true)
            .build()
            .expect("test TLS");
        let (client, connection) = tokio_postgres::connect(
            &test_env::required_test_dsn(),
            postgres_native_tls::MakeTlsConnector::new(tls),
        )
        .await
        .expect("fixed test PostgreSQL");
        let connection = tokio::spawn(async move {
            let _ = connection.await;
        });
        Self { client, connection }
    }

    async fn running_pid(&self, marker: &str) -> i32 {
        let observe = async {
            loop {
                let row = self.client.query_opt(
                    "select pid from pg_stat_activity where pid <> pg_backend_pid() and state = 'active' and strpos(query, $1) > 0",
                    &[&marker],
                ).await.expect("observe running backend");
                if let Some(row) = row {
                    return row.get(0);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(EVENT_TIMEOUT, observe)
            .await
            .expect("query reached real PostgreSQL")
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        self.connection.abort();
    }
}

struct Pipe {
    child: Child,
    stdin: Option<ChildStdin>,
    incoming: mpsc::UnboundedReceiver<Value>,
    readers: Vec<JoinHandle<()>>,
    events: Vec<Value>,
}

impl Pipe {
    fn spawn(dsn: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_afpsql"))
            .args(["--mode", "pipe", "--output-to", "stdout", "--dsn", dsn])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("pipe process");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("stdout");
        let stderr = child.stderr.take().expect("stderr");
        let (sender, incoming) = mpsc::unbounded_channel();
        let out = sender.clone();
        let readers = vec![
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let event = serde_json::from_str(&line)
                        .unwrap_or_else(|_error| json!({"unparsed": line}));
                    if out.send(event).is_err() {
                        break;
                    }
                }
            }),
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let event = serde_json::from_str(&line)
                        .unwrap_or_else(|_error| json!({"unparsed": line}));
                    if sender.send(event).is_err() {
                        break;
                    }
                }
            }),
        ];
        Self {
            child,
            stdin,
            incoming,
            readers,
            events: Vec::new(),
        }
    }

    async fn send(&mut self, request: Value) {
        let bytes = format!("{request}\n");
        self.stdin
            .as_mut()
            .expect("open stdin")
            .write_all(bytes.as_bytes())
            .await
            .expect("pipe request");
    }

    async fn event_since(&mut self, start: usize, matches: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + EVENT_TIMEOUT;
        loop {
            if let Some(event) = self.events[start..].iter().find(|event| matches(event)) {
                return event.clone();
            }
            let event = tokio::time::timeout_at(deadline, self.incoming.recv())
                .await
                .unwrap_or_else(|_error| panic!("timely pipe event missing: {:?}", self.events))
                .unwrap_or_else(|| panic!("pipe ended before expected event: {:?}", self.events));
            self.events.push(event);
        }
    }

    async fn result(&mut self, id: &str) -> Value {
        self.event_since(0, |event| {
            event["kind"] == "result" && event["result"]["id"] == id
        })
        .await
    }

    async fn queue_error(&mut self, id: &str) {
        let error = self
            .event_since(0, |event| {
                event["kind"] == "error" && event["error"]["id"] == id
            })
            .await;
        assert_eq!(error["error"]["code"], "invalid_request", "{error}");
        assert_eq!(
            error["error"]["message"], "session worker queue is full",
            "{error}"
        );
        assert_eq!(error["error"]["retryable"], true, "{error}");
    }

    async fn exit(&mut self) {
        let status = tokio::time::timeout(EXIT_TIMEOUT, self.child.wait())
            .await
            .expect("bounded shutdown")
            .expect("wait pipe");
        assert!(status.success(), "pipe exit: {status}; {:?}", self.events);
        for reader in &mut self.readers {
            tokio::time::timeout(EVENT_TIMEOUT, reader)
                .await
                .expect("drain pipe output")
                .expect("output reader");
        }
        while let Ok(event) = self.incoming.try_recv() {
            self.events.push(event);
        }
        assert!(
            self.events
                .iter()
                .any(|event| event["result"]["code"] == "close"),
            "{:?}",
            self.events
        );
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        for reader in &self.readers {
            reader.abort();
        }
    }
}

async fn fill_control_queue(pipe: &mut Pipe) {
    for index in 0..64 {
        pipe.send(json!({"code": "session_info", "id": format!("queued_info_{index}")}))
            .await;
    }
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires fixed PostgreSQL test database"
)]
#[tokio::test]
async fn a_full_control_queue_rejects_queries_without_leaking_inflight_and_still_accepts_cancel() {
    let db = TestDb::connect().await;
    let marker = format!("queue_full_{}", unique_suffix());
    let mut pipe = Pipe::spawn(&test_env::required_test_dsn());
    pipe.send(json!({"code":"query", "id":"running", "sql":format!("select pg_sleep(20) /* {marker} */")})).await;
    db.running_pid(&marker).await;
    fill_control_queue(&mut pipe).await;
    for (code, id) in [
        ("session_info", "full_info"),
        ("begin", "full_begin"),
        ("commit", "full_commit"),
        ("rollback", "full_rollback"),
    ] {
        pipe.send(json!({"code":code,"id":id})).await;
        pipe.queue_error(id).await;
    }
    pipe.send(json!({"code":"query", "id":"full_query", "sql":"select 1"}))
        .await;
    pipe.queue_error("full_query").await;
    let before_ping = pipe.events.len();
    pipe.send(json!({"code":"ping"})).await;
    let pong = pipe
        .event_since(before_ping, |event| event["result"]["code"] == "pong")
        .await;
    assert_eq!(
        pong["trace"]["in_flight"], 1,
        "rejected query must not remain registered: {pong}"
    );
    pipe.send(json!({"code":"cancel", "id":"running"})).await;
    pipe.event_since(0, |event| {
        event["result"]["code"] == "cancel_requested" && event["result"]["id"] == "running"
    })
    .await;
    pipe.result("queued_info_63").await;
    pipe.send(json!({"code":"query", "id":"full_query", "sql":"select 7::int as n"}))
        .await;
    assert_eq!(pipe.result("full_query").await["result"]["rows"][0]["n"], 7);
    pipe.send(json!({"code":"close"})).await;
    pipe.exit().await;
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires fixed PostgreSQL test database"
)]
#[tokio::test]
async fn config_reservation_failure_keeps_all_prior_values_and_releases_other_queue_reservations() {
    let db = TestDb::connect().await;
    let marker = format!("config_full_{}", unique_suffix());
    let dsn = test_env::required_test_dsn();
    let mut pipe = Pipe::spawn(&dsn);
    pipe.send(
        json!({"code":"config", "inline_max_rows":5, "sessions":{"a_idle":{"dsn_secret":dsn}}}),
    )
    .await;
    pipe.event_since(0, |event| event["result"]["code"] == "config")
        .await;
    pipe.send(json!({"code":"session_info", "id":"idle_ready", "session":"a_idle"}))
        .await;
    pipe.result("idle_ready").await;
    pipe.send(json!({"code":"begin","id":"idle_begin","session":"a_idle","read_only":false,"permission":"write"})).await;
    pipe.result("idle_begin").await;
    pipe.send(json!({"code":"query","id":"idle_create","session":"a_idle","sql":"create temporary table config_sentinel(n int)","options":{"permission":"write"}})).await;
    pipe.result("idle_create").await;
    pipe.send(json!({"code":"query","id":"idle_insert","session":"a_idle","sql":"insert into config_sentinel values (1)","options":{"permission":"write"}})).await;
    pipe.result("idle_insert").await;
    pipe.send(json!({"code":"query", "id":"running", "sql":format!("select pg_sleep(20) /* {marker} */")})).await;
    db.running_pid(&marker).await;
    fill_control_queue(&mut pipe).await;
    let before_config = pipe.events.len();
    let invalid_dsn = unavailable_dsn();
    pipe.send(
        json!({"code":"config", "inline_max_rows":1, "default_session":"a_idle", "sessions":{
            "default":{"dsn_secret":invalid_dsn},
            "a_idle":{"dsn_secret":invalid_dsn}
        }}),
    )
    .await;
    let failed = pipe
        .event_since(before_config, |event| {
            event["kind"] == "error" && event["error"]["message"] == "session worker queue is full"
        })
        .await;
    assert_eq!(failed["error"]["code"], "invalid_request");
    assert_eq!(failed["error"]["retryable"], true);
    assert!(failed["error"]["id"].is_null());
    assert!(
        !pipe.events[before_config..]
            .iter()
            .any(|event| event["result"]["code"] == "config")
    );
    let before_snapshot = pipe.events.len();
    pipe.send(json!({"code":"config"})).await;
    let unchanged = pipe
        .event_since(before_snapshot, |event| event["result"]["code"] == "config")
        .await;
    assert_eq!(unchanged["result"]["inline_max_rows"], 5);
    assert_eq!(unchanged["result"]["default_session"], "default");
    pipe.send(json!({"code":"query", "id":"idle_after_failure", "session":"a_idle", "sql":"select x from generate_series(1,3) x where exists (select 1 from config_sentinel)","options":{"permission":"write"}})).await;
    assert_eq!(
        pipe.result("idle_after_failure").await["result"]["row_count"],
        3
    );
    // A failed all-queue reservation neither invalidates the other live
    // transaction nor consumes one of that worker's 64 future queue slots.
    let idle_marker = format!("idle_capacity_{}", unique_suffix());
    pipe.send(json!({"code":"query","id":"idle_blocked","session":"a_idle","sql":format!("select pg_sleep(20) /* {idle_marker} */"),"options":{"permission":"write"}})).await;
    db.running_pid(&idle_marker).await;
    let before_capacity = pipe.events.len();
    for index in 0..64 {
        pipe.send(
            json!({"code":"session_info","id":format!("idle_capacity_{index}"),"session":"a_idle"}),
        )
        .await;
    }
    pipe.send(json!({"code":"ping"})).await;
    pipe.event_since(before_capacity, |event| event["result"]["code"] == "pong")
        .await;
    assert!(
        !pipe.events[before_capacity..]
            .iter()
            .any(|event| event["kind"] == "error"
                && event["error"]["id"]
                    .as_str()
                    .is_some_and(|id| id.starts_with("idle_capacity_"))),
        "reservation failure must release every earlier queue permit: {:?}",
        pipe.events
    );
    pipe.send(json!({"code":"cancel","id":"idle_blocked"}))
        .await;
    pipe.result("idle_capacity_63").await;
    pipe.send(json!({"code":"cancel", "id":"running"})).await;
    pipe.result("queued_info_63").await;
    pipe.send(json!({"code":"query", "id":"default_after_failure", "sql":"select x from generate_series(1,3) x"})).await;
    assert_eq!(
        pipe.result("default_after_failure").await["result"]["row_count"],
        3
    );
    pipe.send(json!({"code":"close"})).await;
    pipe.exit().await;
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires fixed PostgreSQL test database"
)]
#[tokio::test]
async fn queued_begin_reconnects_with_its_accepted_configuration_after_the_old_backend_ends() {
    let db = TestDb::connect().await;
    let key = lock_key();
    db.client
        .query("select pg_advisory_lock($1)", &[&key])
        .await
        .expect("hold begin gate");
    let marker = format!("begin_snapshot_{}", unique_suffix());
    let mut pipe = Pipe::spawn(&test_env::required_test_dsn());
    pipe.send(json!({"code":"query", "id":"old_backend", "params":[key], "sql":format!(
        "with gated as materialized (select pg_advisory_lock($1::bigint)) select pg_terminate_backend(pg_backend_pid()) from gated /* {marker} */"
    )})).await;
    db.running_pid(&marker).await;
    pipe.send(
        json!({"code":"begin", "id":"accepted_begin", "read_only":false,"permission":"write"}),
    )
    .await;
    let before_patch = pipe.events.len();
    pipe.send(json!({"code":"config", "sessions":{"default":{"dsn_secret":unavailable_dsn()}}}))
        .await;
    pipe.event_since(before_patch, |event| event["result"]["code"] == "config")
        .await;
    db.client
        .query("select pg_advisory_unlock($1)", &[&key])
        .await
        .expect("release begin gate");
    let outcome = pipe
        .event_since(0, |event| {
            event["result"]["id"] == "accepted_begin" || event["error"]["id"] == "accepted_begin"
        })
        .await;
    assert_eq!(
        outcome["result"]["command_tag"], "BEGIN",
        "BEGIN must use its already-approved direct write configuration: {outcome}"
    );
    pipe.send(json!({"code":"close"})).await;
    pipe.exit().await;
}

struct BlackholeRelay {
    dsn: String,
    blocked: Arc<AtomicBool>,
    task: JoinHandle<()>,
}

fn conninfo_value(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

impl BlackholeRelay {
    async fn start() -> Self {
        let cfg = Config::from_str(&test_env::required_test_dsn()).expect("fixed PostgreSQL DSN");
        let host = match cfg.get_hosts().first().expect("test PostgreSQL host") {
            tokio_postgres::config::Host::Tcp(host) => host.clone(),
            _ => panic!("the fixed relay fixture requires the disposable TCP PostgreSQL"),
        };
        let upstream_port = cfg.get_ports().first().copied().unwrap_or(5432);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback relay");
        let port = listener.local_addr().expect("relay address").port();
        let sslmode = match cfg.get_ssl_mode() {
            tokio_postgres::config::SslMode::Disable => "disable",
            tokio_postgres::config::SslMode::Require => "require",
            _ => "prefer",
        };
        let user = cfg.get_user().expect("fixed test DSN user");
        let dbname = cfg.get_dbname().unwrap_or(user);
        let password =
            std::str::from_utf8(cfg.get_password().unwrap_or_default()).expect("test password");
        let mut dsn = format!(
            "host=127.0.0.1 port={port} user={} dbname={} password={} sslmode={sslmode}",
            conninfo_value(user),
            conninfo_value(dbname),
            conninfo_value(password)
        );
        if let Some(options) = cfg.get_options() {
            dsn.push_str(&format!(" options={}", conninfo_value(options)));
        }
        let blocked = Arc::new(AtomicBool::new(false));
        let gate = Arc::clone(&blocked);
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((downstream, _peer)) = accepted else { break; };
                        let host = host.clone();
                        let gate = Arc::clone(&gate);
                        connections.spawn(async move {
                            let upstream = tokio::net::TcpStream::connect((host.as_str(), upstream_port)).await.expect("relay upstream");
                            let (mut from_client, mut to_client) = downstream.into_split();
                            let (mut from_server, mut to_server) = upstream.into_split();
                            let client_to_server = async {
                                let mut bytes = [0u8; 8192];
                                loop {
                                    let count = from_client.read(&mut bytes).await?;
                                    if count == 0 { return Ok::<(), std::io::Error>(()); }
                                    to_server.write_all(&bytes[..count]).await?;
                                }
                            };
                            let server_to_client = async {
                                let mut bytes = [0u8; 8192];
                                loop {
                                    let count = from_server.read(&mut bytes).await?;
                                    if count == 0 { return Ok::<(), std::io::Error>(()); }
                                    if !gate.load(Ordering::SeqCst) { to_client.write_all(&bytes[..count]).await?; }
                                }
                            };
                            // Client EOF always closes the upstream, even while
                            // responses are being discarded. TLS stays opaque.
                            tokio::select! { _ = client_to_server => {}, _ = server_to_client => {} }
                        });
                    }
                    _ = connections.join_next(), if !connections.is_empty() => {}
                }
            }
        });
        Self { dsn, blocked, task }
    }
}

impl Drop for BlackholeRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn blackholed_shutdown_rolls_back_the_real_transaction(close: bool) {
    let db = TestDb::connect().await;
    let table = format!("afpsql_lifecycle_{}", unique_suffix());
    db.client
        .batch_execute(&format!("create table {table}(n int)"))
        .await
        .expect("disposable transaction table");
    let relay = BlackholeRelay::start().await;
    let mut pipe = Pipe::spawn(&relay.dsn);
    pipe.send(json!({"code":"config","statement_timeout_ms":0,"sessions":{"other":{"dsn_secret":relay.dsn}}})).await;
    pipe.event_since(0, |event| event["result"]["code"] == "config")
        .await;
    pipe.send(json!({"code":"begin","id":"begin_write","read_only":false,"permission":"write"}))
        .await;
    assert_eq!(
        pipe.result("begin_write").await["result"]["command_tag"],
        "BEGIN"
    );
    pipe.send(json!({"code":"query","id":"insert_pending","sql":format!("insert into {table} values (1)"),"options":{"permission":"write"}})).await;
    pipe.result("insert_pending").await;
    let first_key = lock_key();
    let second_key = lock_key();
    db.client
        .query("select pg_advisory_lock($1)", &[&first_key])
        .await
        .expect("hold first response gate");
    db.client
        .query("select pg_advisory_lock($1)", &[&second_key])
        .await
        .expect("hold second response gate");
    let first_marker = format!("blackhole_first_{}", unique_suffix());
    let second_marker = format!("blackhole_second_{}", unique_suffix());
    pipe.send(json!({"code":"query","id":"pending_first","params":[first_key],"sql":format!("select pg_advisory_xact_lock($1::bigint) /* {first_marker} */"),"options":{"permission":"write","lock_timeout_ms":0}})).await;
    pipe.send(json!({"code":"query","id":"pending_second","session":"other","params":[second_key],"sql":format!("select pg_advisory_xact_lock($1::bigint) /* {second_marker} */"),"options":{"lock_timeout_ms":0}})).await;
    let first_pid = db.running_pid(&first_marker).await;
    let second_pid = db.running_pid(&second_marker).await;
    relay.blocked.store(true, Ordering::SeqCst);
    db.client
        .query("select pg_advisory_unlock($1)", &[&first_key])
        .await
        .expect("release first response gate");
    db.client
        .query("select pg_advisory_unlock($1)", &[&second_key])
        .await
        .expect("release second response gate");
    // Both workers now hold their client lock forever waiting for a response
    // that is discarded. PostgreSQL itself can finish both released locks.
    let started = Instant::now();
    if close {
        pipe.send(json!({"code":"close"})).await;
    } else {
        drop(pipe.stdin.take());
    }
    pipe.exit().await;
    assert!(
        started.elapsed() < EXIT_TIMEOUT,
        "one shutdown budget covers both workers and client locks"
    );
    assert!(
        !pipe.events.iter().any(|event| matches!(
            event["result"]["id"].as_str(),
            Some("pending_first" | "pending_second")
        )),
        "no fabricated successful outcome for replies the client did not receive: {:?}",
        pipe.events
    );
    assert!(
        !pipe.events.iter().any(|event| matches!(
            event["result"]["command_tag"].as_str(),
            Some("COMMIT" | "ROLLBACK")
        )),
        "shutdown must not manufacture a transaction receipt"
    );
    let disappeared = async {
        loop {
            let count: i64 = db
                .client
                .query_one(
                    "select count(*) from pg_stat_activity where pid = $1 or pid = $2",
                    &[&first_pid, &second_pid],
                )
                .await
                .expect("independent backend check")
                .get(0);
            if count == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(EVENT_TIMEOUT, disappeared)
        .await
        .expect("both real backends were released");
    let count: i64 = db
        .client
        .query_one(&format!("select count(*) from {table}"), &[])
        .await
        .expect("actual transaction result")
        .get(0);
    assert_eq!(
        count, 0,
        "the uncommitted insert really rolled back after backend teardown"
    );
    db.client
        .batch_execute(&format!("drop table {table}"))
        .await
        .expect("remove disposable fixture");
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires fixed PostgreSQL test database"
)]
#[tokio::test]
async fn eof_closes_two_blackholed_workers_with_one_budget_and_real_rollback() {
    blackholed_shutdown_rolls_back_the_real_transaction(false).await;
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires fixed PostgreSQL test database"
)]
#[tokio::test]
async fn close_closes_two_blackholed_workers_with_one_budget_and_real_rollback() {
    blackholed_shutdown_rolls_back_the_real_transaction(true).await;
}

#[cfg_attr(
    not(feature = "db-tests"),
    ignore = "requires fixed PostgreSQL test database"
)]
#[tokio::test]
async fn failed_explicit_transaction_rejects_queued_writes() {
    let db = TestDb::connect().await;
    let table = format!("failed_tx_queue_{}", unique_suffix());
    db.client
        .batch_execute(&format!("create table {table}(n int)"))
        .await
        .unwrap();
    let mut pipe = Pipe::spawn(&test_env::required_test_dsn());
    pipe.send(json!({"code":"begin","id":"begin","read_only":false,"permission":"write"}))
        .await;
    pipe.result("begin").await;
    let marker = format!("failed_tx_running_{}", unique_suffix());
    pipe.send(json!({"code":"query","id":"running","sql":format!("select pg_sleep(20) /* {marker} */"),"options":{"permission":"write"}})).await;
    let pid = db.running_pid(&marker).await;
    pipe.send(json!({"code":"query","id":"write1","sql":format!("insert into {table} values (1)"),"options":{"permission":"write"}})).await;
    pipe.send(json!({"code":"query","id":"write2","sql":format!("insert into {table} values (2) returning n"),"options":{"permission":"write","stream_rows":true,"batch_rows":1}})).await;
    pipe.send(json!({"code":"commit","id":"commit"})).await;
    db.client
        .query_one("select pg_terminate_backend($1)", &[&pid])
        .await
        .unwrap();
    for id in ["write1", "write2", "commit"] {
        let event = pipe
            .event_since(0, |event| {
                event["error"]["id"] == id || event["result"]["id"] == id
            })
            .await;
        assert_eq!(
            event["error"]["code"],
            if id == "commit" {
                "transaction_lost_rolled_back"
            } else {
                "invalid_request"
            },
            "{event}"
        );
    }
    let row = db
        .client
        .query_one(&format!("select count(*)::bigint from {table}"), &[])
        .await
        .unwrap();
    assert_eq!(row.get::<_, i64>(0), 0);
    pipe.send(json!({"code":"query","id":"recovered","sql":format!("insert into {table} values (3)"),"options":{"permission":"write"}})).await;
    pipe.result("recovered").await;
    pipe.send(json!({"code":"close"})).await;
    pipe.exit().await;
    db.client
        .batch_execute(&format!("drop table {table}"))
        .await
        .unwrap();
}
