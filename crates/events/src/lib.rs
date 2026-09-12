//! RuntimeEvent store, transactional outbox and resumable projections.
//!
//! Canonical owner (DOSSIER.md §17): `crates/events`. This crate owns the durable
//! RuntimeEvent stream (DOMAIN.md §9): the envelope, the tenant-monotonic `sequence`,
//! the transactional outbox that relays events to NATS JetStream, and durable consumer
//! cursors. It is not a second runtime or effect path: mutations and effects belong to
//! their own owners and are recorded here.
#![forbid(unsafe_code)]

/// Repository path of this crate's canonical owner, used by the workspace
/// conformance check to prove one owner maps to exactly one package.
pub const CANONICAL_OWNER: &str = "crates/events";

pub mod cursor;
pub mod envelope;
pub mod error;
pub mod event_type;
pub mod outbox;
pub mod projection;
pub mod store;
pub mod stream;

pub use cursor::{CursorState, ResumeBatch, PROJECTION_CONSUMER};
pub use envelope::{Actor, ActorKind, EventDraft, RuntimeEvent, SCHEMA_VERSION_V1};
pub use error::{EventError, TransportError};
pub use event_type::{parse_family, EventFamily, EventType, EVENT_FAMILIES};
pub use outbox::{EventTransport, OutboxPublisher, PublishReport, UnavailableTransport};
pub use projection::{
    ApplyOutcome, CatchUpReport, Projection, ProjectionRunner, RunStatusProjection,
    WorkNodeStatusProjection,
};
pub use store::{BoxEventFuture, EventBatch, EventStore};
pub use stream::{
    Channel, ChannelKind, EventFrame, LiveFrame, LiveSendOutcome, LiveSender, StreamConfig,
    StreamError, StreamFrame, StreamSession, StreamSubscription,
};
