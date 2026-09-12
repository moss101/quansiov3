//! Canonical ULID: a 26-character Crockford base32 identifier, time-sortable to the
//! millisecond (DOMAIN.md §1.1).
//!
//! The encoding, monotonicity and clock/entropy injection are implemented here rather
//! than taken from a crate so that generation is deterministic under test: a fixed
//! clock plus fixed entropy must always yield the same id, and two ids created in the
//! same millisecond must still be strictly ordered.

use crate::error::CoreError;
use core::fmt;

/// Crockford base32 alphabet, excluding `I`, `L`, `O` and `U`.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const ENCODED_LEN: usize = 26;
/// Bytes of the 48-bit timestamp at the start of a ULID.
const TIME_BYTES: usize = 6;

/// Milliseconds since the Unix epoch.
pub trait Clock: Send + Sync {
    /// Current time in milliseconds since the Unix epoch.
    fn now_ms(&self) -> u64;
}

/// Wall-clock implementation used by the runtime.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before the Unix epoch")
            .as_millis() as u64
    }
}

/// Source of the 80 random bits in a ULID.
pub trait EntropySource: Send + Sync {
    /// Fill the buffer with cryptographically strong random bytes.
    ///
    /// # Panics
    /// Implementations may panic if the operating system cannot provide entropy; a
    /// runtime that cannot generate identifiers must fail closed rather than mint
    /// predictable ones.
    fn fill(&self, buf: &mut [u8]);
}

/// Operating-system entropy.
#[derive(Debug, Default, Clone, Copy)]
pub struct OsEntropy;

impl EntropySource for OsEntropy {
    fn fill(&self, buf: &mut [u8]) {
        getrandom::getrandom(buf).expect("os entropy unavailable");
    }
}

/// A parsed, validated ULID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ulid([u8; 16]);

impl Ulid {
    /// Build a ULID from its 16 raw bytes (48-bit timestamp, 80-bit randomness).
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Raw bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// Creation time in milliseconds since the Unix epoch.
    #[must_use]
    pub fn timestamp_ms(&self) -> u64 {
        let mut value: u64 = 0;
        for byte in &self.0[..TIME_BYTES] {
            value = (value << 8) | u64::from(*byte);
        }
        value
    }

    /// Parse a 26-character Crockford base32 ULID.
    ///
    /// # Errors
    /// Returns [`CoreError::InvalidUlid`] for a wrong length, an out-of-alphabet
    /// character, or an overflow in the first character (which would place the value
    /// beyond 128 bits).
    pub fn parse(value: &str) -> Result<Self, CoreError> {
        if value.len() != ENCODED_LEN {
            return Err(CoreError::InvalidUlid(value.to_string()));
        }
        let mut buffer: u128 = 0;
        for (index, ch) in value.bytes().enumerate() {
            let digit =
                decode_digit(ch).ok_or_else(|| CoreError::InvalidUlid(value.to_string()))?;
            if index == 0 && digit > 7 {
                // 26 base32 characters carry 130 bits; the leading character may only
                // contribute the 3 bits that fit in 128.
                return Err(CoreError::InvalidUlid(value.to_string()));
            }
            buffer = (buffer << 5) | u128::from(digit);
        }
        Ok(Self(buffer.to_be_bytes()))
    }

    /// Canonical 26-character representation.
    #[must_use]
    pub fn to_base32(&self) -> String {
        let mut out = [0u8; ENCODED_LEN];
        let value = u128::from_be_bytes(self.0);
        for index in (0..ENCODED_LEN).rev() {
            let digit = (value >> (5 * (ENCODED_LEN - 1 - index))) & 0x1F;
            out[index] = ALPHABET[digit as usize];
        }
        String::from_utf8(out.to_vec()).expect("alphabet is ASCII")
    }
}

impl fmt::Display for Ulid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_base32())
    }
}

impl core::str::FromStr for Ulid {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

fn decode_digit(ch: u8) -> Option<u8> {
    match ch {
        b'0'..=b'9' => Some(ch - b'0'),
        b'A'..=b'H' => Some(ch - b'A' + 10),
        b'J'..=b'K' => Some(ch - b'J' + 18),
        b'M'..=b'N' => Some(ch - b'M' + 20),
        b'P'..=b'T' => Some(ch - b'P' + 22),
        b'V'..=b'Z' => Some(ch - b'V' + 27),
        // Crockford's confusable aliases are accepted on input.
        b'a'..=b'h' => Some(ch - b'a' + 10),
        b'j'..=b'k' => Some(ch - b'j' + 18),
        b'm'..=b'n' => Some(ch - b'm' + 20),
        b'p'..=b't' => Some(ch - b'p' + 22),
        b'v'..=b'z' => Some(ch - b'v' + 27),
        _ => None,
    }
}

/// Monotonic ULID generator.
///
/// Within one millisecond the random component is incremented rather than redrawn, so
/// identifiers created by one generator are strictly increasing and safe as ordering
/// keys (DOMAIN.md §1.2 `sequence` discipline).
pub struct UlidGenerator<C: Clock = SystemClock, E: EntropySource = OsEntropy> {
    clock: C,
    entropy: E,
    last_ms: Option<u64>,
    previous: Option<[u8; 16]>,
}

impl UlidGenerator<SystemClock, OsEntropy> {
    /// Generator backed by the system clock and operating-system entropy.
    #[must_use]
    pub fn new() -> Self {
        Self {
            clock: SystemClock,
            entropy: OsEntropy,
            last_ms: None,
            previous: None,
        }
    }
}

impl Default for UlidGenerator<SystemClock, OsEntropy> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C: Clock, E: EntropySource> UlidGenerator<C, E> {
    /// Generator with injected clock and entropy (deterministic in tests).
    pub fn with_sources(clock: C, entropy: E) -> Self {
        Self {
            clock,
            entropy,
            last_ms: None,
            previous: None,
        }
    }

    /// Generate the next ULID.
    pub fn generate(&mut self) -> Ulid {
        let now = self.clock.now_ms();
        let mut bytes = [0u8; 16];
        bytes[..TIME_BYTES].copy_from_slice(&now.to_be_bytes()[2..]);
        match self.last_ms {
            Some(last) if now == last => {
                // Same millisecond: increment the previous random component.
                let mut previous = self.previous.expect("previous value recorded with last_ms");
                for index in (TIME_BYTES..16).rev() {
                    let (value, overflow) = previous[index].overflowing_add(1);
                    previous[index] = value;
                    if !overflow {
                        break;
                    }
                }
                bytes[TIME_BYTES..].copy_from_slice(&previous[TIME_BYTES..]);
                self.previous = Some(previous);
            }
            _ => {
                self.entropy.fill(&mut bytes[TIME_BYTES..]);
                self.previous = Some(bytes);
            }
        }
        self.last_ms = Some(now);
        Ulid(bytes)
    }

    /// Generate a ULID for an explicit timestamp (used by fixtures and replays).
    pub fn generate_at(&mut self, timestamp_ms: u64) -> Ulid {
        let mut bytes = [0u8; 16];
        bytes[..TIME_BYTES].copy_from_slice(&timestamp_ms.to_be_bytes()[2..]);
        self.entropy.fill(&mut bytes[TIME_BYTES..]);
        Ulid(bytes)
    }
}
