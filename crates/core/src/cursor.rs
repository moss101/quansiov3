//! Opaque resumable stream cursors (DOMAIN.md §1.2 `cursor`, §9.3).
//!
//! A cursor encodes `(stream_id, sequence)` as base64 so clients can resume a stream
//! exactly where they stopped. It is deliberately opaque: clients must not construct
//! or parse it, only present the last one they received.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use core::fmt;
use core::str::FromStr;

use crate::error::CoreError;
use crate::generation::Sequence;

/// A resumable position in one tenant stream.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Cursor {
    stream_id: String,
    sequence: Sequence,
}

impl Cursor {
    /// Build a cursor for a stream position.
    #[must_use]
    pub fn new(stream_id: impl Into<String>, sequence: Sequence) -> Self {
        Self {
            stream_id: stream_id.into(),
            sequence,
        }
    }

    /// The stream this cursor belongs to.
    #[must_use]
    pub fn stream_id(&self) -> &str {
        &self.stream_id
    }

    /// The last delivered sequence.
    #[must_use]
    pub const fn sequence(&self) -> Sequence {
        self.sequence
    }

    /// Encode as an opaque base64 token.
    #[must_use]
    pub fn encode(&self) -> String {
        URL_SAFE_NO_PAD.encode(format!("{}:{}", self.stream_id, self.sequence))
    }

    /// Decode a token produced by [`Cursor::encode`].
    ///
    /// # Errors
    /// Returns [`CoreError::InvalidCursor`] for malformed base64, a missing separator
    /// or a non-positive sequence.
    pub fn decode(token: &str) -> Result<Self, CoreError> {
        let invalid = || CoreError::InvalidCursor(token.to_string());
        let raw = URL_SAFE_NO_PAD.decode(token).map_err(|_| invalid())?;
        let text = String::from_utf8(raw).map_err(|_| invalid())?;
        let (stream_id, sequence) = text.rsplit_once(':').ok_or_else(invalid)?;
        if stream_id.is_empty() {
            return Err(invalid());
        }
        let sequence =
            Sequence::new(sequence.parse().map_err(|_| invalid())?).map_err(|_| invalid())?;
        Ok(Self::new(stream_id, sequence))
    }
}

impl fmt::Display for Cursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.encode())
    }
}

impl FromStr for Cursor {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::decode(s)
    }
}
