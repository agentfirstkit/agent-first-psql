use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};

static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[allow(dead_code)]
pub fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn test_dsn() -> Option<String> {
    env_value("AFPSQL_TEST_DSN_SECRET").or_else(|| env_value("DATABASE_URL"))
}

#[allow(dead_code)]
pub fn required_test_dsn() -> String {
    let dsn = test_dsn();
    assert!(
        dsn.is_some(),
        "set AFPSQL_TEST_DSN_SECRET or DATABASE_URL, or create tests/.env.local for PostgreSQL integration tests",
    );
    dsn.unwrap_or_default()
}

/// Extract `(user, dbname)` from a libpq URL DSN such as
/// `postgresql://user:pw@host:port/dbname?params`, so tests can assert the
/// connection identity reported back without hardcoding an environment-specific
/// name (local `.env.local` uses `afpsql_test`, CI uses `test`).
#[allow(dead_code)]
pub fn dsn_identity(dsn: &str) -> (String, String) {
    let after_scheme = dsn.split_once("://").map_or(dsn, |(_, rest)| rest);
    let (authority, path) = after_scheme.split_once('/').unwrap_or((after_scheme, ""));
    let userinfo = authority.rsplit_once('@').map_or("", |(ui, _)| ui);
    let user = userinfo.split(':').next().unwrap_or("").to_string();
    let dbname = path.split_once('?').map_or(path, |(db, _)| db).to_string();
    (user, dbname)
}

pub fn env_value(key: &str) -> Option<String> {
    std::env::var(key).ok().or_else(|| read_env_key(key))
}

fn read_env_key(key: &str) -> Option<String> {
    for path in [env_file_path(".env.local"), env_file_path(".env")] {
        if let Some(value) = read_env_key_from_file(&path, key) {
            return Some(value);
        }
    }
    None
}

fn env_file_path(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(name)
}

fn read_env_key_from_file(path: &Path, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name.trim() == key {
            return Some(unquote_env_value(value.trim()));
        }
    }
    None
}

fn unquote_env_value(value: &str) -> String {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if (bytes[0] == b'"' && bytes[value.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[value.len() - 1] == b'\'')
        {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}
