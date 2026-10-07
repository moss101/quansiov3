//! The clock a preview scope's lifetime is measured on.
//!
//! Scopes expire on wall-clock time. The clock is a port so a test can move time exactly, instead
//! of sleeping through a five-minute lifetime or trusting a shortened one.

use std::sync::atomic::{AtomicU64, Ordering};

/// Milliseconds since the Unix epoch.
pub trait Clock: Send + Sync {
    /// Now.
    fn now_ms(&self) -> u64;
}

/// The system clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0)
    }
}

/// A clock that moves only when told to.
#[derive(Debug, Default)]
pub struct ManualClock {
    now: AtomicU64,
}

impl ManualClock {
    /// A clock showing `now_ms`.
    #[must_use]
    pub fn new(now_ms: u64) -> Self {
        Self {
            now: AtomicU64::new(now_ms),
        }
    }

    /// Move time forward.
    pub fn advance(&self, ms: u64) {
        self.now.fetch_add(ms, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }
}
