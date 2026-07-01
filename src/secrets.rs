//! Secret handling: redacting wrappers and `.env` parsing.
//!
//! airlock never bakes secrets into images. Instead it collects secret *values*
//! on the trusted host and injects them into the guest at launch as environment
//! variables passed through the smolvm child process's environment block (readable
//! only by the owner via `/proc/<pid>/environ`, never on the argv). See
//! [`crate::smolvm`] for how these become `--secret-env` references.

use std::fmt;
use std::path::Path;

use crate::error::{Error, Result};

/// A secret string that refuses to reveal itself via `Debug`/`Display`.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wrap a value as a secret.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Deliberately expose the underlying value. Call sites should be rare and
    /// obvious (e.g. setting a child process environment variable).
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(\"[redacted]\")")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

/// A guest environment variable name paired with its secret value, resolved on
/// the host and injected at launch.
#[derive(Clone, Debug)]
pub struct SecretEnv {
    /// The environment variable name as seen inside the guest.
    pub guest_name: String,
    /// The secret value (redacted in logs).
    pub value: Secret,
}

impl SecretEnv {
    /// Build a secret env entry.
    pub fn new(guest_name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            guest_name: guest_name.into(),
            value: Secret::new(value),
        }
    }
}

/// Parse a `.env` file into a list of [`SecretEnv`] entries **without** mutating
/// the current process environment. Later duplicate keys win, matching typical
/// shell `.env` semantics.
pub fn parse_env_file(path: &Path) -> Result<Vec<SecretEnv>> {
    let iter = dotenvy::from_path_iter(path).map_err(|source| Error::EnvFile {
        path: path.to_path_buf(),
        source: Box::new(source),
    })?;

    let mut out: Vec<SecretEnv> = Vec::new();
    for item in iter {
        let (key, value) = item.map_err(|source| Error::EnvFile {
            path: path.to_path_buf(),
            source: Box::new(source),
        })?;
        // De-duplicate: last assignment wins.
        if let Some(existing) = out.iter_mut().find(|e| e.guest_name == key) {
            existing.value = Secret::new(value);
        } else {
            out.push(SecretEnv::new(key, value));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn secret_redacts_in_debug_and_display() {
        let s = Secret::new("hunter2");
        assert_eq!(format!("{s:?}"), "Secret(\"[redacted]\")");
        assert_eq!(format!("{s}"), "[redacted]");
        // The value is still retrievable when explicitly exposed.
        assert_eq!(s.expose(), "hunter2");
    }

    #[test]
    fn secret_env_debug_hides_value() {
        let e = SecretEnv::new("API_KEY", "super-secret-value");
        let rendered = format!("{e:?}");
        assert!(
            !rendered.contains("super-secret-value"),
            "leaked: {rendered}"
        );
        assert!(rendered.contains("API_KEY"));
    }

    #[test]
    fn parses_env_file_with_comments_and_quotes() -> anyhow::Result<()> {
        let mut f = tempfile::NamedTempFile::new()?;
        writeln!(f, "# a comment")?;
        writeln!(f, "GH_TOKEN=abc123")?;
        writeln!(f, "QUOTED=\"with spaces\"")?;
        writeln!(f, "EMPTY=")?;
        let parsed = parse_env_file(f.path())?;

        let get = |k: &str| {
            parsed
                .iter()
                .find(|e| e.guest_name == k)
                .map(|e| e.value.expose())
        };
        assert_eq!(get("GH_TOKEN"), Some("abc123"));
        assert_eq!(get("QUOTED"), Some("with spaces"));
        assert_eq!(get("EMPTY"), Some(""));
        assert_eq!(parsed.len(), 3);
        Ok(())
    }

    #[test]
    fn later_duplicate_key_wins() -> anyhow::Result<()> {
        let mut f = tempfile::NamedTempFile::new()?;
        writeln!(f, "K=first")?;
        writeln!(f, "K=second")?;
        let parsed = parse_env_file(f.path())?;
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].value.expose(), "second");
        Ok(())
    }
}
