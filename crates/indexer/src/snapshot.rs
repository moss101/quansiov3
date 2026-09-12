//! Index epoch snapshots.
//!
//! Every tenant index records a monotonically increasing `epoch` plus a corpus
//! fingerprint. The pair is the snapshot a search result is pinned to; a caller
//! that holds an older snapshot can detect that it is stale by comparing it with
//! [`crate::SearchIndex::snapshot`].

use std::fmt;

use crate::error::{IndexError, IndexResult};

/// Identifies one rebuildable state of a tenant's derived index.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Snapshot {
    epoch: u64,
    fingerprint: String,
}

impl Snapshot {
    /// Build a snapshot from an epoch and corpus fingerprint.
    #[must_use]
    pub fn new(epoch: u64, fingerprint: impl Into<String>) -> Self {
        Self {
            epoch,
            fingerprint: fingerprint.into(),
        }
    }

    /// The tenant-monotonic epoch.
    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The corpus fingerprint for this epoch.
    #[must_use]
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Canonical string form, stored in provenance and accepted by a program.
    #[must_use]
    pub fn as_string(&self) -> String {
        format!("e{}:{}", self.epoch, self.fingerprint)
    }

    /// Parse the canonical string form.
    ///
    /// # Errors
    /// Returns [`IndexError::Encoding`] when the value is not `e<epoch>:<fingerprint>`.
    pub fn parse(value: &str) -> IndexResult<Self> {
        let Some((epoch, fingerprint)) = value.split_once(':') else {
            return Err(IndexError::Encoding(format!("invalid snapshot '{value}'")));
        };
        let epoch = epoch
            .strip_prefix('e')
            .and_then(|digits| digits.parse::<u64>().ok())
            .ok_or_else(|| IndexError::Encoding(format!("invalid snapshot '{value}'")))?;
        if fingerprint.is_empty() {
            return Err(IndexError::Encoding(format!("invalid snapshot '{value}'")));
        }
        Ok(Self::new(epoch, fingerprint))
    }
}

impl fmt::Display for Snapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.as_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_canonical_form() {
        let snapshot = Snapshot::new(7, "abc123");
        assert_eq!(snapshot.as_string(), "e7:abc123"); // exact wire form
        assert_eq!(Snapshot::parse("e7:abc123").expect("parses"), snapshot);
    }

    #[test]
    fn rejects_malformed_snapshot() {
        assert!(Snapshot::parse("nope").is_err());
        assert!(Snapshot::parse("e:abc").is_err());
        assert!(Snapshot::parse("e3:").is_err());
    }
}
