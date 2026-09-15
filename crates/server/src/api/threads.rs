//! Thread read/command facade path named by APP-004 (`crates/server/api/threads/`).
//!
//! Persistence is owned by [`crate::control::conversation`]. This module is the HTTP
//! projection of that owner, not a second store.

pub use crate::api::projections::{messages, thread};
pub use crate::control::conversation::{ConversationStore, Message, PostedMessage, Thread};
