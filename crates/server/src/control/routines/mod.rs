//! Routine fire identity (APP-010).
//!
//! A restart must not duplicate a scheduled run: the fire key is
//! `(routine_id, scheduled_at)` and is the same path interactive work uses
//! (`cmd_` / Effect Ledger), not a second scheduler.

/// Idempotency key for one scheduled fire.
#[must_use]
pub fn fire_key(routine_id: &str, scheduled_at: &str) -> String {
    format!("{routine_id}@{scheduled_at}")
}

/// Whether a second delivery of the same fire should run.
#[must_use]
pub fn should_fire(seen: &[String], key: &str) -> bool {
    !seen.contains(&key.to_string())
}

/// Routines use the same command catalog as interactive work.
pub const ROUTINE_COMMAND: &str = "TriggerRoutineNow";
