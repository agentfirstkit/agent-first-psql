use agent_first_data::document::{DocumentFile, Format, Value};
use std::path::PathBuf;

const MAX_CONFIG_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretConfigRef {
    pub file: PathBuf,
    pub path: String,
}

impl SecretConfigRef {
    pub fn from_values(flag: &str, values: Option<Vec<String>>) -> Result<Option<Self>, String> {
        let Some(values) = values else {
            return Ok(None);
        };
        let [file, path]: [String; 2] = values
            .try_into()
            .map_err(|_| format!("{flag} requires exactly two values: <FILE> <DOT_PATH>"))?;
        if path.is_empty() {
            return Err(format!("{flag} requires a non-empty DOT_PATH"));
        }
        Ok(Some(Self {
            file: PathBuf::from(file),
            path,
        }))
    }

    pub fn safe_metadata(&self) -> serde_json::Value {
        serde_json::json!({
            "kind": "config",
            "file": self.file,
            "path": self.path,
        })
    }
}

pub fn resolve_config_secret(flag: &str, reference: &SecretConfigRef) -> Result<String, String> {
    let format = Format::detect(&reference.file).ok_or_else(|| {
        format!(
            "{flag} cannot detect config format for {}",
            reference.file.display()
        )
    })?;
    // afdata size-caps the read (rejecting an oversized or non-regular file
    // before reading a byte) and, for a secret-bearing file, gives content-free
    // errors: `redacted_message` drops any parser detail that could echo the
    // source. `value_at` collapses read → parse → dot-path traverse into one call.
    let doc = DocumentFile::open_capped(&reference.file, Some(format), MAX_CONFIG_BYTES).map_err(
        |error| {
            format!(
                "{flag} cannot read {} config {}: {}",
                format.name(),
                reference.file.display(),
                error.redacted_message()
            )
        },
    )?;
    let resolved = doc.value_at(&reference.path).map_err(|error| {
        if error.code() == "document_path_not_found" {
            format!(
                "{flag} path {} was not found in {}",
                reference.path,
                reference.file.display()
            )
        } else {
            format!(
                "{flag} cannot resolve path {} in {}",
                reference.path,
                reference.file.display()
            )
        }
    })?;
    match resolved {
        Value::String(secret) if secret.is_empty() => Err(format!(
            "{flag} resolved an empty string from {} at path {}",
            reference.file.display(),
            reference.path
        )),
        Value::String(secret) => Ok(secret),
        other => Err(format!(
            "{flag} requires a string at {} in {}; found {}",
            reference.path,
            reference.file.display(),
            other.kind_name()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_config(name: &str, extension: &str, content: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "afpsql-secret-config-{name}-{}.{extension}",
            std::process::id()
        ));
        std::fs::write(&path, content).expect("write test config");
        path
    }

    fn resolve(path: PathBuf, dot_path: &str) -> Result<String, String> {
        let result = resolve_config_secret(
            "--dsn-secret-config",
            &SecretConfigRef {
                file: path.clone(),
                path: dot_path.to_string(),
            },
        );
        std::fs::remove_file(path).expect("remove test config");
        result
    }

    #[test]
    fn resolves_all_supported_formats_without_trimming() {
        for (name, extension, content, dot_path, expected) in [
            (
                "json",
                "json",
                r#"{"database":{"url":" postgresql://json "}}"#,
                "database.url",
                " postgresql://json ",
            ),
            (
                "toml",
                "toml",
                "[database]\nurl = 'postgresql://toml'\n",
                "database.url",
                "postgresql://toml",
            ),
            (
                "yaml",
                "yaml",
                "database:\n  url: postgresql://yaml\n",
                "database.url",
                "postgresql://yaml",
            ),
            (
                "dotenv",
                "env",
                "DATABASE_URL=postgresql://dotenv\n",
                "DATABASE_URL",
                "postgresql://dotenv",
            ),
        ] {
            let path = temp_config(name, extension, content);
            assert_eq!(resolve(path, dot_path), Ok(expected.to_string()));
        }
    }

    #[test]
    fn resolves_secret_named_and_percent_encoded_urls_verbatim() {
        // A real DSN percent-encodes reserved characters in its userinfo
        // (%40 = '@', %3A = ':') and joins query params with '&'/'='. afdata must
        // return the string byte-for-byte: it must NOT percent-decode. The field
        // also uses afdata's `_secret` suffix — the CLI would refuse to print it,
        // but the document *library* value_at reads raw, which is exactly what
        // afpsql relies on to obtain the connection string.
        let dsn = "postgresql://user:p%40ss%3Aw0rd@host.example:5432/mydb?sslmode=require&application_name=af";
        let json = format!(r#"{{"database":{{"url_secret":"{dsn}"}}}}"#);
        let path = temp_config("json-url", "json", &json);
        assert_eq!(resolve(path, "database.url_secret"), Ok(dsn.to_string()));

        // dotenv, unquoted: no escape processing and no `$` variable expansion,
        // so a literal '$' in the password survives untouched.
        let env_dsn = "postgresql://user:se$cret@host/db?sslmode=require";
        let env = format!("DATABASE_URL={env_dsn}\n");
        let path = temp_config("dotenv-url", "env", &env);
        assert_eq!(resolve(path, "DATABASE_URL"), Ok(env_dsn.to_string()));
    }

    #[test]
    fn rejects_missing_non_string_empty_malformed_and_unknown_sources_safely() {
        for (name, content, dot_path, expected) in [
            ("missing", r#"{"database":{}}"#, "database.url", "not found"),
            ("object", r#"{"value":{}}"#, "value", "found object"),
            ("array", r#"{"value":[]}"#, "value", "found array"),
            ("bool", r#"{"value":true}"#, "value", "found boolean"),
            ("integer", r#"{"value":5432}"#, "value", "found integer"),
            ("null", r#"{"value":null}"#, "value", "found null"),
            ("empty", r#"{"value":""}"#, "value", "empty string"),
        ] {
            let path = temp_config(name, "json", content);
            let error = resolve(path, dot_path).expect_err("source should fail");
            assert!(error.contains(expected), "{name}: {error}");
            assert!(
                !error.contains(content),
                "source leaked for {name}: {error}"
            );
        }

        let canary = "AFPSQL_PARSE_CANARY_SECRET";
        let path = temp_config("malformed", "yaml", &format!("secret: [ {canary}"));
        let error = resolve(path, "secret").expect_err("malformed source should fail");
        assert!(
            !error.contains(canary),
            "parse error leaked source: {error}"
        );

        let path = temp_config("unknown", "txt", "SECRET=canary");
        let error = resolve(path, "SECRET").expect_err("unknown format should fail");
        assert!(error.contains("cannot detect config format"));
        assert!(!error.contains("canary"));
    }
}
