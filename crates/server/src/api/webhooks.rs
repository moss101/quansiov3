//! Public webhook subscription surface (APP-015).
//!
//! Persistence stays on `webhook_subscriptions`. This module is the HTTP/API path.

pub use crate::notify::webhooks::{already_delivered, may_deliver, redact, WebhookSubscription};
