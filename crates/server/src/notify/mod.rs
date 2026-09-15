//! Notification dispatch: in-app, desktop, email via secret handles (APP-010, OPS-003).
//! Outbound webhooks: APP-015.

pub mod webhooks;

pub use webhooks::{already_delivered, may_deliver, redact, WebhookSubscription};
