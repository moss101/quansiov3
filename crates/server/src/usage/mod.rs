//! Usage, quota and entitlement projections derived from RuntimeEvents (OPS-004).
//!
//! Canonical owner: `crates/server/usage`. UsageRecords are a **projection** of the
//! event stream (DOMAIN.md §13.4): they are rebuildable, they are not runtime truth,
//! and billing reads them rather than the Effect Ledger. Hard quotas refuse *before*
//! an effect is reserved, so a quota failure cannot partially commit a new effect.

mod meters;
mod project;
mod quota;

pub use meters::UsageMeter;
pub use project::{project_event, UsageDelta, UsageProjection, UsageRecord};
pub use quota::{admit_effect, evaluate, BudgetAlert, QuotaDecision, QuotaLimit, QuotaPolicy};

/// Repository path of this module's canonical owner.
pub const USAGE_OWNER: &str = "crates/server/src/usage";

/// A usage/quota refusal.
#[derive(Debug, thiserror::Error)]
pub enum UsageError {
    /// Unknown meter spelling.
    #[error("unknown usage meter {meter}")]
    UnknownMeter {
        /// Offending name.
        meter: String,
    },
    /// Hard quota would be exceeded; no effect was reserved.
    #[error("quota exceeded on {meter}: {used} would exceed hard limit {hard}")]
    QuotaExceeded {
        /// Meter.
        meter: &'static str,
        /// Resulting total.
        used: f64,
        /// Hard limit.
        hard: f64,
    },
    /// The effect commit that follows admission failed; usage was not applied.
    #[error("effect commit failed after quota admission: {detail}")]
    Effect {
        /// Commit error.
        detail: String,
    },
}
