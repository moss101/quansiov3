//! Collaboration: membership, ordering and revoke (APP-009).
//!
//! Concurrent messages keep stable seq/ids. A removed participant cannot receive
//! protected events.

use std::cmp::Ordering;

/// A message as ordered on a thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderedMessage {
    /// `msg_` id.
    pub id: String,
    /// Thread-monotonic sequence.
    pub seq: i64,
}

/// Total order: seq, then id. Concurrent inserts with distinct seq stay stable.
#[must_use]
pub fn order_messages(mut messages: Vec<OrderedMessage>) -> Vec<OrderedMessage> {
    messages.sort_by(|left, right| match left.seq.cmp(&right.seq) {
        Ordering::Equal => left.id.cmp(&right.id),
        other => other,
    });
    messages
}

/// Whether a participant may receive protected events.
#[must_use]
pub fn can_receive(status: &str) -> bool {
    status == "active"
}
