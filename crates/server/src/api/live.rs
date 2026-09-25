//! In-process live-frame hub (APP-004, DOMAIN.md §9.3).
//!
//! The runtime publishes transient frames (`model.delta`, `progress`) through
//! [`LiveFramePort`]; `/v1/stream` sessions receive them per tenant and forward the ones on
//! channels they subscribed to. Frames are never persisted: a subscriber that falls behind
//! the broadcast capacity simply misses frames, which the contract allows, and the durable
//! events carry the truth.
//!
//! The operator's `stream.shed` switch (OPS-008) is a per-tenant signal: engaging it bumps a
//! generation every live session watches, so each one closes with `STREAM_BACKPRESSURE` (the
//! client reconnects with its cursor and loses nothing durable); while it is engaged a session
//! that lags is disconnected instead of silently missing frames.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use quansio_events::LiveFrame;
use serde_json::Value;
use tokio::sync::{broadcast, watch};

use crate::runtime::state_machine::LiveFramePort;

/// Frames buffered per tenant before a slow subscriber starts missing them.
pub const LIVE_CAPACITY: usize = 256;

/// A tenant's shed signal: how many times its sessions were shed, and whether shedding is on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ShedState {
    /// Bumped each time the operator sheds the tenant's sessions.
    pub generation: u64,
    /// While true, a lagging session is disconnected.
    pub shedding: bool,
}

/// One broadcast channel per tenant, created on first use.
#[derive(Debug, Default)]
pub struct LiveHub {
    tenants: Mutex<HashMap<String, broadcast::Sender<LiveFrame>>>,
    shed: Mutex<HashMap<String, watch::Sender<ShedState>>>,
}

impl LiveHub {
    /// A fresh hub.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn sender(&self, tenant_id: &str) -> broadcast::Sender<LiveFrame> {
        let mut tenants = self
            .tenants
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        tenants
            .entry(tenant_id.to_string())
            .or_insert_with(|| broadcast::channel(LIVE_CAPACITY).0)
            .clone()
    }

    /// Receive every live frame for `tenant_id` from now on.
    #[must_use]
    pub fn subscribe(&self, tenant_id: &str) -> broadcast::Receiver<LiveFrame> {
        self.sender(tenant_id).subscribe()
    }

    fn shed_sender(&self, tenant_id: &str) -> watch::Sender<ShedState> {
        let mut shed = self.shed.lock().unwrap_or_else(|error| error.into_inner());
        shed.entry(tenant_id.to_string())
            .or_insert_with(|| watch::channel(ShedState::default()).0)
            .clone()
    }

    /// Watch the tenant's shed signal.
    #[must_use]
    pub fn shed_signal(&self, tenant_id: &str) -> watch::Receiver<ShedState> {
        self.shed_sender(tenant_id).subscribe()
    }

    /// Disconnect every live session of `tenant_id` now and shed laggards until [`Self::resume`].
    pub fn shed(&self, tenant_id: &str) {
        self.shed_sender(tenant_id).send_modify(|state| {
            state.generation += 1;
            state.shedding = true;
        });
    }

    /// Stop shedding `tenant_id`'s laggards (the switch was released).
    pub fn resume(&self, tenant_id: &str) {
        self.shed_sender(tenant_id)
            .send_modify(|state| state.shedding = false);
    }
}

impl LiveFramePort for LiveHub {
    fn publish(&self, tenant_id: &str, channel: &str, frame: Value) {
        // No receiver is not an error: nobody is watching this tenant right now.
        let _ = self.sender(tenant_id).send(LiveFrame {
            channel: channel.to_string(),
            frame,
        });
    }
}
