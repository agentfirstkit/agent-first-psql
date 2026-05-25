use std::path::Path;

pub fn test_dsn() -> Option<String> {
    std::env::var("AFPSQL_TEST_DSN_SECRET")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .ok()
        .or_else(|| read_env_key("AFPSQL_TEST_DSN_SECRET"))
        .or_else(|| read_env_key("DATABASE_URL"))
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
