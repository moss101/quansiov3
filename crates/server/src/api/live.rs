//! In-process live-frame hub (APP-004, DOMAIN.md §9.3).
//!
//! The runtime publishes transient frames (`model.delta`, `progress`) through
//! [`LiveFramePort`]; `/v1/stream` sessions receive them per tenant and forward the ones on
//! channels they subscribed to. Frames are never persisted: a subscriber that falls behind
//! the broadcast capacity simply misses frames, which the contract allows, and the durable
//! events carry the truth.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use quansio_events::LiveFrame;
use serde_json::Value;
use tokio::sync::broadcast;

use crate::runtime::state_machine::LiveFramePort;

/// Frames buffered per tenant before a slow subscriber starts missing them.
pub const LIVE_CAPACITY: usize = 256;

/// One broadcast channel per tenant, created on first use.
#[derive(Debug, Default)]
pub struct LiveHub {
    tenants: Mutex<HashMap<String, broadcast::Sender<LiveFrame>>>,
}

impl LiveHub {
    /// A fresh hub.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn sender(&self, tenant_id: &str) -> broadcast::Sender<LiveFrame> {
        let mut tenants = self.tenants.lock().unwrap_or_else(|error| error.into_inner());
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
