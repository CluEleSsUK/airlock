//! Validated VM names and fleet-member naming.
//!
//! smolvm derives on-disk overlay/storage paths from the machine name, so names
//! must be filesystem-safe. A [`VmName`] can only be constructed through
//! validation, making an invalid name unrepresentable downstream.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Maximum length smolvm/airlock allow for a machine name.
pub const MAX_NAME_LEN: usize = 63;

/// A validated smolvm machine name: `[A-Za-z0-9][A-Za-z0-9_-]{0,62}`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct VmName(String);

impl VmName {
    /// Validate `s` and wrap it as a [`VmName`].
    pub fn new(s: impl Into<String>) -> Result<Self> {
        let s = s.into();
        Self::validate(&s)?;
        Ok(Self(s))
    }

    /// Construct the name of fleet member `index` within `profile`
    /// (e.g. `"web", 3` → `"web-03"`).
    pub fn member(profile: &str, index: u32) -> Result<Self> {
        Self::new(format!("{profile}-{index:02}"))
    }

    /// Borrow the underlying string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn validate(s: &str) -> Result<()> {
        let invalid = |reason: &str| Error::InvalidVmName {
            name: s.to_owned(),
            reason: reason.to_owned(),
        };

        if s.len() > MAX_NAME_LEN {
            return Err(invalid("must be at most 63 characters"));
        }
        let first = s
            .chars()
            .next()
            .ok_or_else(|| invalid("must not be empty"))?;
        if !first.is_ascii_alphanumeric() {
            return Err(invalid("must start with an ASCII letter or digit"));
        }
        if !s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(invalid("only [A-Za-z0-9_-] are allowed"));
        }
        Ok(())
    }
}

impl fmt::Display for VmName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for VmName {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        Self::new(value)
    }
}

impl From<VmName> for String {
    fn from(value: VmName) -> Self {
        value.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_simple_names() {
        for ok in ["web", "web-01", "a", "Proj_2", "x-y-z-99"] {
            assert!(VmName::new(ok).is_ok(), "{ok} should be valid");
        }
    }

    #[test]
    fn rejects_empty_name() {
        assert!(VmName::new("").is_err());
    }

    #[test]
    fn rejects_leading_symbol() {
        assert!(VmName::new("-web").is_err());
        assert!(VmName::new("_web").is_err());
    }

    #[test]
    fn rejects_disallowed_characters() {
        for bad in ["web 01", "web/01", "web.01", "café", "a:b"] {
            assert!(VmName::new(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn rejects_overlong_name() {
        let long = "a".repeat(MAX_NAME_LEN + 1);
        assert!(VmName::new(long).is_err());
    }

    #[test]
    fn member_zero_pads_small_indices() -> anyhow::Result<()> {
        assert_eq!(VmName::member("web", 3)?.as_str(), "web-03");
        assert_eq!(VmName::member("web", 42)?.as_str(), "web-42");
        assert_eq!(VmName::member("web", 100)?.as_str(), "web-100");
        Ok(())
    }
}
