//! Per-tenant API rate limits (APP-001, DOMAIN.md §15 `RATE_LIMITED`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Sliding-window limiter keyed by tenant.
#[derive(Clone, Debug)]
pub struct RateLimiter {
    inner: Arc<Mutex<HashMap<String, Window>>>,
    limit: u32,
    window: Duration,
}

#[derive(Debug, Clone)]
struct Window {
    started: Instant,
    count: u32,
}

impl RateLimiter {
    /// `limit` requests per `window` for each tenant.
    #[must_use]
    pub fn new(limit: u32, window: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            limit: limit.max(1),
            window,
        }
    }

    /// Default: 120 mutating calls per minute per tenant.
    #[must_use]
    pub fn from_env() -> Self {
        let limit = std::env::var("QUANSIO_API_RATE_LIMIT")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(120);
        Self::new(limit, Duration::from_secs(60))
    }

    /// Record one call. `false` means the tenant is over the limit.
    #[must_use]
    pub fn allow(&self, tenant_id: &str) -> bool {
        let mut map = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        let now = Instant::now();
        let entry = map.entry(tenant_id.to_string()).or_insert(Window {
            started: now,
            count: 0,
        });
        if now.duration_since(entry.started) >= self.window {
            entry.started = now;
            entry.count = 0;
        }
        if entry.count >= self.limit {
            return false;
        }
        entry.count += 1;
        true
    }
}
