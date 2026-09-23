//! Notifications: preferences, the notifier and channel delivery (APP-010, DOMAIN.md §13.3).
//! Outbound webhooks: APP-015.
//!
//! * [`store`] — notifications and per user × workspace × kind preferences; every change is
//!   a user-scoped `notification.*` event.
//! * [`notifier`] — the event-log consumer that makes notifications (mentions, questions,
//!   approvals, routine outcomes), once per event, user and kind.
//! * [`email`] — server-side email delivery with retry and backoff (OD-009).
//!
//! In-app is the notification row itself; desktop is the desktop client showing a native
//! notification when `notification.created` says `desktop: true`.

pub mod email;
pub mod notifier;
pub mod store;
pub mod webhooks;

pub use email::{
    email_backoff, DeliveryReport, EmailDeliveries, EmailMessage, EmailTransport,
    HttpEmailTransport, MAX_EMAIL_ATTEMPTS,
};
pub use notifier::{Notifier, NOTIFIER_CONSUMER, NOTIFIER_STREAM};
pub use store::{
    channels_for, create_in, validate_preference, NewNotification, Notification, NotificationStore,
    NotifyError, DEFAULT_CHANNELS, NOTIFICATION_CHANNELS, NOTIFICATION_KINDS,
};
pub use webhooks::{already_delivered, may_deliver, redact, WebhookSubscription};
