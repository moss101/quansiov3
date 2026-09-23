//! Routines: definitions, the firing ledger and event triggers (APP-010, DOMAIN.md §13.1).
//!
//! Canonical owner of `routines` definitions and `routine_fires`. Timing — `next_due_at`
//! from a cron or interval trigger, and what a missed window means under the absence
//! policy — is CORE-008's `scheduler::RoutineScheduler`, which claims windows through
//! [`ledger::claim_in`]. Executing a fire is the runtime's interactive path, reached through
//! the scheduler's `RunDispatch` port, so a routine's run is governed exactly like a run a
//! person started.
//!
//! * [`RoutineStore`] — create, update, pause, resume, delete and trigger-now, each with its
//!   `routine.*` event; the trigger, template and teammate are validated before anything is
//!   written.
//! * [`ledger`] — one row per fire key; a window, an event or a trigger-now command claims
//!   exactly one fire, executing it is leased, and a failed execution is retried with
//!   exponential backoff before it is marked failed.
//! * [`triggers`] — event triggers: a consumer of the tenant's RuntimeEvents that claims a
//!   fire per matching event in the transaction that advances its cursor.

pub mod ledger;
mod store;
pub mod triggers;

pub use ledger::{
    backoff, Fire, FireLedger, FireSource, FireStatus, FIRE_LEASE_SECONDS, MAX_FIRE_ATTEMPTS,
};
pub use store::{
    NewRoutine, RoutineChange, RoutineError, RoutineStore, RoutineView, ROUTINES_OWNER,
};
pub use triggers::{event_matches, EventTriggers};
