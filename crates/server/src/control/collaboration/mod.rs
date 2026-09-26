//! Collaboration: thread membership, protection, attention and handoff (APP-009,
//! DOMAIN.md §3.1).
//!
//! Canonical owner of `thread_participants`. The conversation owner
//! (`control::conversation`) keeps `threads` and `messages`; this module composes its
//! in-transaction functions so a thread is created with its owner, a message lands with the
//! teammate it addresses, and a handoff notice commits with the membership change — each
//! owner writing only its own table, in one transaction.
//!
//! * [`CollaborationStore`] — participants (add, leave, remove, the last-owner guard), the
//!   thread's responsible teammate and [`CollaborationStore::hand_off`].
//! * [`access`] — who may read a thread and receive its events: [`thread_access`] for a
//!   projection or a command, [`ThreadGate`] for the stream, which filters `thread.*`,
//!   `question.*` and a thread's live frames per subscriber.
//! * [`attention`] — derived attention: mentions after a person's own last message, open
//!   questions, a teammate's run in the thread.

pub mod access;
pub mod attention;
mod store;

pub use access::{thread_access, thread_access_many, Access, ThreadGate};
pub use attention::{participant_attention, thread_attention_for, ParticipantAttention};
pub use store::{
    CollaborationError, CollaborationStore, HandOff, Participant, ParticipantKind, ParticipantRole,
    COLLABORATION_OWNER,
};
