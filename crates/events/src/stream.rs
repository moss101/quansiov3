//! Resumable client event streaming (DOMAIN.md §9.3, CORE-009).
//!
//! `subscribe` opens a channel-scoped stream over the tenant RuntimeEvent log:
//!
//! * durable [`EventFrame`]s carry the event envelope and an opaque cursor;
//! * transient [`LiveFrame`]s (`model.delta`, `terminal.bytes`, `browser.frame`,
//!   `progress`) are never persisted and may be dropped;
//! * reconnecting with the last cursor replays exactly the missed events in order;
//! * a slow consumer is disconnected with a typed [`StreamError::Backpressure`]
//!   (`STREAM_BACKPRESSURE`) instead of buffering without bound.
//!
//! Channels are tenant-scoped: [`Channel`] carries the tenant and every read is scoped
//! to it, so a subscriber can never observe another tenant's or another channel's
//! events.

use std::sync::{Arc, Mutex};

use quansio_core::{CanonicalId, Cursor, Prefix, Sequence};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio::task::JoinHandle;

use crate::envelope::RuntimeEvent;
use crate::error::EventError;
use crate::store::EventStore;

/// Stream tuning. The bounds are documented because backpressure is defined by them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamConfig {
    /// Maximum durable event frames buffered for one subscriber. When the buffer is
    /// full the subscriber is disconnected with `STREAM_BACKPRESSURE`; this bounds
    /// server memory per stream (default 64).
    pub buffer_capacity: usize,
    /// Maximum transient live frames buffered for one subscriber. Live frames that do
    /// not fit are dropped, never persisted and never disconnect the client
    /// (default 64).
    pub live_capacity: usize,
    /// Events read from the store per catch-up batch (default 256).
    pub batch_size: i64,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            buffer_capacity: 64,
            live_capacity: 64,
            batch_size: 256,
        }
    }
}

/// A client stream channel kind (DOMAIN.md §9.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelKind {
    /// `workspace:<ws>`
    Workspace,
    /// `thread:<thr>`
    Thread,
    /// `run:<run>`
    Run,
    /// `target:<tgt>`
    Target,
}

impl ChannelKind {
    /// The canonical channel prefix.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Thread => "thread",
            Self::Run => "run",
            Self::Target => "target",
        }
    }

    /// Parse a canonical channel kind.
    ///
    /// # Errors
    /// Returns [`StreamError::MalformedChannel`] for an unknown kind.
    pub fn parse(value: &str) -> Result<Self, StreamError> {
        match value {
            "workspace" => Ok(Self::Workspace),
            "thread" => Ok(Self::Thread),
            "run" => Ok(Self::Run),
            "target" => Ok(Self::Target),
            _ => Err(StreamError::MalformedChannel(value.to_string())),
        }
    }

    const fn prefix(self) -> Prefix {
        match self {
            Self::Workspace => Prefix::Workspace,
            Self::Thread => Prefix::Thread,
            Self::Run => Prefix::Run,
            Self::Target => Prefix::ExecutionTarget,
        }
    }
}

/// A tenant-scoped subscription channel.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Channel {
    tenant_id: String,
    kind: ChannelKind,
    id: String,
}

impl Channel {
    /// Parse `<kind>:<id>` for `tenant_id`, validating both identifiers.
    ///
    /// # Errors
    /// Returns [`StreamError::InvalidTenantId`] for a non-canonical tenant and
    /// [`StreamError::MalformedChannel`] for an unknown kind or a non-canonical id.
    pub fn parse(tenant_id: &str, text: &str) -> Result<Self, StreamError> {
        validate_tenant_id(tenant_id)?;
        let (kind, id) = text
            .split_once(':')
            .ok_or_else(|| StreamError::MalformedChannel(text.to_string()))?;
        if id.is_empty() || id.contains(':') {
            return Err(StreamError::MalformedChannel(text.to_string()));
        }
        let kind = ChannelKind::parse(kind)?;
        CanonicalId::parse_typed(id, kind.prefix())
            .map_err(|_| StreamError::MalformedChannel(text.to_string()))?;
        Ok(Self {
            tenant_id: tenant_id.to_string(),
            kind,
            id: id.to_string(),
        })
    }

    /// The owning tenant.
    #[must_use]
    pub fn tenant_id(&self) -> &str {
        &self.tenant_id
    }

    /// The channel kind.
    #[must_use]
    pub const fn kind(&self) -> ChannelKind {
        self.kind
    }

    /// The canonical aggregate identity the channel scopes to.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The canonical `kind:id` form.
    #[must_use]
    pub fn as_str(&self) -> String {
        format!("{}:{}", self.kind.as_str(), self.id)
    }

    /// Whether `event` belongs to this channel.
    ///
    /// A workspace channel carries every event in the workspace; the other channels
    /// carry the events of their aggregate plus events that reference it (`thread_id`,
    /// `run_id`, `target_id` payload fields).
    #[must_use]
    pub fn matches(&self, event: &RuntimeEvent) -> bool {
        match self.kind {
            ChannelKind::Workspace => event.workspace_id.as_deref() == Some(self.id.as_str()),
            ChannelKind::Thread => {
                ((event.aggregate_type == "thread" || event.aggregate_type == "message")
                    && event.aggregate_id == self.id)
                    || payload_str(event, "thread_id") == Some(self.id.as_str())
            }
            ChannelKind::Run => {
                (event.aggregate_type == "run" && event.aggregate_id == self.id)
                    || payload_str(event, "run_id") == Some(self.id.as_str())
            }
            ChannelKind::Target => {
                (event.aggregate_type == "target" && event.aggregate_id == self.id)
                    || payload_str(event, "target_id") == Some(self.id.as_str())
            }
        }
    }
}

/// A client subscription: tenant, consumer identity, channels and optional cursor.
#[derive(Debug, Clone)]
pub struct StreamSubscription {
    tenant_id: String,
    consumer: String,
    channels: Vec<Channel>,
    cursor: Option<String>,
}

impl StreamSubscription {
    /// Build a subscription over one or more channels of one tenant.
    ///
    /// # Errors
    /// Returns [`StreamError::EmptySubscription`] when no channel is requested and
    /// [`StreamError::ChannelTenantMismatch`] when a channel belongs to another tenant.
    pub fn new(
        tenant_id: &str,
        consumer: &str,
        channels: Vec<Channel>,
    ) -> Result<Self, StreamError> {
        validate_tenant_id(tenant_id)?;
        if channels.is_empty() {
            return Err(StreamError::EmptySubscription);
        }
        if consumer.is_empty() {
            return Err(StreamError::EmptySubscription);
        }
        for channel in &channels {
            if channel.tenant_id() != tenant_id {
                return Err(StreamError::ChannelTenantMismatch {
                    channel: channel.as_str(),
                    tenant_id: tenant_id.to_string(),
                });
            }
        }
        Ok(Self {
            tenant_id: tenant_id.to_string(),
            consumer: consumer.to_string(),
            channels,
            cursor: None,
        })
    }

    /// Resume from the last cursor the client received.
    #[must_use]
    pub fn with_cursor(mut self, cursor: impl Into<String>) -> Self {
        self.cursor = Some(cursor.into());
        self
    }

    /// The owning tenant.
    #[must_use]
    pub fn tenant_id(&self) -> &str {
        &self.tenant_id
    }

    /// The consumer identity whose delivery position is persisted.
    #[must_use]
    pub fn consumer(&self) -> &str {
        &self.consumer
    }

    /// The subscribed channels.
    #[must_use]
    pub fn channels(&self) -> &[Channel] {
        &self.channels
    }

    /// The opaque cursor presented by the client, if any.
    #[must_use]
    pub fn cursor(&self) -> Option<&str> {
        self.cursor.as_deref()
    }

    /// The subscription's cursor stream identity: its channels in request order.
    #[must_use]
    pub fn stream_id(&self) -> String {
        self.channels
            .iter()
            .map(Channel::as_str)
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Whether `event` belongs to any subscribed channel.
    #[must_use]
    pub fn matches(&self, event: &RuntimeEvent) -> bool {
        self.channels.iter().any(|channel| channel.matches(event))
    }
}

/// A durable, replayable event frame.
#[derive(Debug, Clone, PartialEq)]
pub struct EventFrame {
    /// Opaque cursor naming this event; present it to resume.
    pub cursor: String,
    /// The RuntimeEvent envelope.
    pub event: RuntimeEvent,
}

/// A transient, non-durable live frame.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveFrame {
    /// The channel the frame belongs to (`kind:id`).
    pub channel: String,
    /// The frame payload (`model.delta`, `terminal.bytes`, `browser.frame`, `progress`).
    pub frame: Value,
}

/// One frame delivered to a client.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamFrame {
    /// Durable event frame. Boxed because the RuntimeEvent envelope is much larger
    /// than a live frame and most frames on a busy stream are live.
    Event(Box<EventFrame>),
    /// Transient live frame.
    Live(LiveFrame),
}

impl StreamFrame {
    /// The wire `kind`: `event` or `live`.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Event(_) => "event",
            Self::Live(_) => "live",
        }
    }

    /// The cursor of a durable frame.
    #[must_use]
    pub fn cursor(&self) -> Option<&str> {
        match self {
            Self::Event(frame) => Some(&frame.cursor),
            Self::Live(_) => None,
        }
    }

    /// The event of a durable frame.
    #[must_use]
    pub fn event(&self) -> Option<&RuntimeEvent> {
        match self {
            Self::Event(frame) => Some(&frame.event),
            Self::Live(_) => None,
        }
    }

    /// The canonical wire shape of DOMAIN.md §9.3.
    #[must_use]
    pub fn to_json_value(&self) -> Value {
        match self {
            Self::Event(frame) => serde_json::json!({
                "kind": "event",
                "cursor": frame.cursor,
                "event": frame.event.to_json_value(),
            }),
            Self::Live(frame) => serde_json::json!({
                "kind": "live",
                "channel": frame.channel,
                "frame": frame.frame,
            }),
        }
    }
}

/// Outcome of publishing a live frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveSendOutcome {
    /// The frame was buffered for delivery.
    Sent,
    /// The subscriber's live buffer was full; the frame was dropped.
    Dropped,
}

/// Errors returned by the stream API (DOMAIN.md §15).
#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    /// The channel string is not `<kind>:<canonical id>`.
    #[error("malformed channel {0:?}: expected workspace|thread|run|target:<canonical id>")]
    MalformedChannel(String),
    /// The tenant id is not a canonical `tn_` identifier.
    #[error("invalid tenant id {0:?}")]
    InvalidTenantId(String),
    /// A subscription named no channel or no consumer.
    #[error("subscription must name at least one channel and a consumer")]
    EmptySubscription,
    /// A channel belongs to another tenant.
    #[error("channel {channel:?} does not belong to tenant {tenant_id:?}")]
    ChannelTenantMismatch {
        /// The rejected channel.
        channel: String,
        /// The tenant the subscription is scoped to.
        tenant_id: String,
    },
    /// A live frame was published to a channel this subscription did not request.
    #[error("channel {0:?} is not part of this subscription")]
    ChannelNotSubscribed(String),
    /// A cursor token is malformed or belongs to another stream.
    #[error("cursor error: {0}")]
    Cursor(#[from] quansio_core::CoreError),
    /// The EventStore rejected the read.
    #[error("event store error: {0}")]
    Store(#[from] EventError),
    /// The client did not read fast enough; the bounded buffer was full.
    #[error(
        "slow consumer disconnected: STREAM_BACKPRESSURE (bounded buffer capacity {capacity})"
    )]
    Backpressure {
        /// The durable buffer capacity that was exceeded.
        capacity: usize,
    },
    /// The stream ended and nothing is left to deliver.
    #[error("stream closed")]
    Closed,
}

impl StreamError {
    /// The DOMAIN.md §15 error code for this failure.
    #[must_use]
    pub const fn error_code(&self) -> &'static str {
        match self {
            Self::Backpressure { .. } => "STREAM_BACKPRESSURE",
            Self::Cursor(_)
            | Self::MalformedChannel(_)
            | Self::InvalidTenantId(_)
            | Self::EmptySubscription
            | Self::ChannelTenantMismatch { .. }
            | Self::ChannelNotSubscribed(_) => "VALIDATION_SCHEMA",
            Self::Store(_) | Self::Closed => "INTERNAL",
        }
    }

    /// Whether reconnecting with a cursor can recover.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        matches!(self, Self::Backpressure { .. } | Self::Store(_))
    }
}

/// Publishing handle for transient live frames.
#[derive(Debug, Clone)]
pub struct LiveSender {
    tx: mpsc::Sender<StreamFrame>,
    channels: Arc<Vec<Channel>>,
    capacity: usize,
}

impl LiveSender {
    /// The bounded live capacity; frames beyond it are dropped.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Publish a transient frame on a subscribed channel.
    ///
    /// Live frames are never persisted. When the subscriber's live buffer is full the
    /// frame is dropped ([`LiveSendOutcome::Dropped`]) rather than blocking the producer
    /// or disconnecting the client.
    ///
    /// # Errors
    /// Returns [`StreamError::ChannelNotSubscribed`] for a channel outside the
    /// subscription and [`StreamError::Closed`] when the session has ended.
    pub fn send(&self, channel: &Channel, frame: Value) -> Result<LiveSendOutcome, StreamError> {
        if !self.channels.iter().any(|subscribed| subscribed == channel) {
            return Err(StreamError::ChannelNotSubscribed(channel.as_str()));
        }
        match self.tx.try_send(StreamFrame::Live(LiveFrame {
            channel: channel.as_str(),
            frame,
        })) {
            Ok(()) => Ok(LiveSendOutcome::Sent),
            Err(TrySendError::Full(_)) => Ok(LiveSendOutcome::Dropped),
            Err(TrySendError::Closed(_)) => Err(StreamError::Closed),
        }
    }
}

/// A live client subscription.
#[derive(Debug)]
pub struct StreamSession {
    stream_id: String,
    events: mpsc::Receiver<StreamFrame>,
    live: mpsc::Receiver<StreamFrame>,
    failure: Arc<Mutex<Option<StreamError>>>,
    events_open: bool,
    live_open: bool,
    handle: JoinHandle<()>,
}

impl StreamSession {
    /// The subscription's cursor stream identity.
    #[must_use]
    pub fn stream_id(&self) -> &str {
        &self.stream_id
    }

    /// The next frame, or the typed reason the stream ended.
    ///
    /// Durable frames are delivered first (they are the replayable ones); live frames
    /// fill the gaps. A slow consumer observes [`StreamError::Backpressure`] once the
    /// bounded durable buffer was exhausted.
    ///
    /// # Errors
    /// Returns [`StreamError::Backpressure`] for a slow consumer, [`StreamError::Store`]
    /// if the read failed, or [`StreamError::Closed`] when the stream has ended.
    pub async fn next_frame(&mut self) -> Result<StreamFrame, StreamError> {
        match self.recv_next().await {
            Some(frame) => Ok(frame),
            None => Err(self.take_failure().unwrap_or(StreamError::Closed)),
        }
    }

    /// Whether the reader recorded a terminal failure; the remaining buffered frames
    /// are still delivered and then [`StreamSession::next_frame`] returns it.
    #[must_use]
    pub fn is_failed(&self) -> bool {
        self.failed()
    }

    /// Stop the subscription and wait for the reader task to finish.
    pub async fn close(self) {
        let handle = self.handle;
        let _ = handle.await;
    }

    async fn recv_next(&mut self) -> Option<StreamFrame> {
        while self.events_open || self.live_open {
            if self.events_open && self.live_open {
                tokio::select! {
                    biased;
                    event = self.events.recv() => match event {
                        Some(frame) => return Some(frame),
                        None => {
                            self.events_open = false;
                            if self.failed() {
                                return None;
                            }
                        }
                    },
                    live = self.live.recv() => match live {
                        Some(frame) => return Some(frame),
                        None => self.live_open = false,
                    },
                }
            } else if self.events_open {
                match self.events.recv().await {
                    Some(frame) => return Some(frame),
                    None => {
                        self.events_open = false;
                        if self.failed() {
                            return None;
                        }
                    }
                }
            } else {
                match self.live.recv().await {
                    Some(frame) => return Some(frame),
                    None => self.live_open = false,
                }
            }
        }
        None
    }

    fn failed(&self) -> bool {
        self.failure
            .lock()
            .map(|slot| slot.is_some())
            .unwrap_or(false)
    }

    fn take_failure(&self) -> Option<StreamError> {
        self.failure.lock().ok().and_then(|mut slot| slot.take())
    }
}

impl EventStore {
    /// Open a resumable, channel-scoped client stream.
    ///
    /// The start position is the client cursor when present, otherwise the persisted
    /// position for `(stream_id, consumer)`, otherwise the beginning of the tenant
    /// stream. Reading and live delivery then run on a bounded buffer; when the buffer
    /// fills, the session ends with [`StreamError::Backpressure`].
    ///
    /// # Errors
    /// Returns a [`StreamError`] for a malformed cursor, a cursor from another stream or
    /// a database failure.
    pub async fn subscribe(
        &self,
        subscription: StreamSubscription,
        config: StreamConfig,
    ) -> Result<(StreamSession, LiveSender), StreamError> {
        let config = StreamConfig {
            buffer_capacity: config.buffer_capacity.max(1),
            live_capacity: config.live_capacity.max(1),
            batch_size: config.batch_size.max(1),
        };
        let stream_id = subscription.stream_id();
        let start = match subscription.cursor() {
            Some(token) => {
                let decoded = Cursor::decode(token)?;
                if decoded.stream_id() != stream_id {
                    return Err(StreamError::Store(EventError::CursorStreamMismatch {
                        expected: stream_id,
                        found: decoded.stream_id().to_string(),
                    }));
                }
                decoded.sequence().get()
            }
            None => self
                .load_cursor(
                    subscription.tenant_id(),
                    &stream_id,
                    subscription.consumer(),
                )
                .await?
                .map_or(0, |cursor| cursor.sequence().get()),
        };

        let channels = Arc::new(subscription.channels().to_vec());
        let (event_tx, event_rx) = mpsc::channel(config.buffer_capacity);
        let (live_tx, live_rx) = mpsc::channel(config.live_capacity);
        let failure = Arc::new(Mutex::new(None));
        let handle = tokio::spawn(pump_events(
            self.clone(),
            subscription,
            event_tx,
            Arc::clone(&failure),
            config,
            start,
        ));
        let session = StreamSession {
            stream_id,
            events: event_rx,
            live: live_rx,
            failure,
            events_open: true,
            live_open: true,
            handle,
        };
        let live = LiveSender {
            tx: live_tx,
            channels,
            capacity: config.live_capacity,
        };
        Ok((session, live))
    }
}

fn validate_tenant_id(tenant_id: &str) -> Result<(), StreamError> {
    CanonicalId::parse_typed(tenant_id, Prefix::Tenant)
        .map(|_| ())
        .map_err(|_| StreamError::InvalidTenantId(tenant_id.to_string()))
}

fn payload_str<'a>(event: &'a RuntimeEvent, key: &str) -> Option<&'a str> {
    event.payload.get(key).and_then(Value::as_str)
}

fn record_failure(failure: &Mutex<Option<StreamError>>, error: StreamError) {
    if let Ok(mut slot) = failure.lock() {
        *slot = Some(error);
    }
}

/// Read the tenant stream after `start` and push matching frames onto the bounded
/// buffer until the reader is caught up, the client is gone, or the buffer is full.
async fn pump_events(
    store: EventStore,
    subscription: StreamSubscription,
    tx: mpsc::Sender<StreamFrame>,
    failure: Arc<Mutex<Option<StreamError>>>,
    config: StreamConfig,
    start: i64,
) {
    let tenant_id = subscription.tenant_id().to_string();
    let stream_id = subscription.stream_id();
    let consumer = subscription.consumer().to_string();
    let batch = config.batch_size;
    let mut position = Sequence::new(start).ok();
    loop {
        let events = match store.read_events_after(&tenant_id, position, batch).await {
            Ok(events) => events,
            Err(error) => {
                record_failure(&failure, StreamError::Store(error));
                return;
            }
        };
        if events.is_empty() {
            return;
        }
        let read = i64::try_from(events.len()).unwrap_or(batch);
        let mut delivered: Option<Sequence> = None;
        for event in events {
            position = Some(event.sequence);
            if !subscription.matches(&event) {
                continue;
            }
            let frame = StreamFrame::Event(Box::new(EventFrame {
                cursor: Cursor::new(stream_id.clone(), event.sequence).encode(),
                event,
            }));
            match tx.try_send(frame) {
                Ok(()) => delivered = position,
                Err(TrySendError::Full(_)) => {
                    record_failure(
                        &failure,
                        StreamError::Backpressure {
                            capacity: config.buffer_capacity,
                        },
                    );
                    return;
                }
                Err(TrySendError::Closed(_)) => return,
            }
        }
        if let Some(sequence) = delivered {
            let cursor = Cursor::new(stream_id.clone(), sequence);
            let _ = store
                .save_cursor(&tenant_id, &stream_id, &consumer, &cursor)
                .await;
        }
        if read < batch {
            return;
        }
    }
}
