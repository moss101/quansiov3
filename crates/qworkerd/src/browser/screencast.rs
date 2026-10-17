//! The live view of a session's tab: Chromium's screencast, relayed as frames (APP-007 over EXEC-009).
//!
//! Event-driven, with no timer: Chromium emits a frame when the page changes, the pump acknowledges it (Chromium sends
//! no more until it is) and offers it to the sink. A sink that is full drops the frame instead of waiting, because a
//! viewer who is behind wants the newest picture, not every picture, and the pump must never hold up the connection the
//! agent's own actions use.

use std::sync::Arc;

use serde_json::json;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use super::cdp::CdpEvent;
use super::driver::{BrowserDriver, Tab};
use super::fault::BrowserFault;

/// One picture of a tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreencastFrame {
    /// The session it belongs to.
    pub session_id: String,
    /// The tab it shows.
    pub tab_id: String,
    /// The image's media type.
    pub media_type: &'static str,
    /// The image, base64.
    pub data: String,
    /// The page's URL when the frame was taken.
    pub url: String,
    /// The page's viewport width in CSS pixels (what a click's coordinates are in); `0` when the browser did not say.
    pub device_width: u32,
    /// The page's viewport height in CSS pixels; `0` when the browser did not say.
    pub device_height: u32,
    /// Counts up per pump, so a viewer can drop a frame that arrives after a newer one.
    pub sequence: u64,
}

/// A running screencast of one tab. Stopping it (or dropping it) ends the pump.
pub struct ScreencastHandle {
    task: JoinHandle<()>,
    tab: Arc<Tab>,
}

impl std::fmt::Debug for ScreencastHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScreencastHandle")
            .field("tab", &self.tab.id)
            .finish()
    }
}

impl ScreencastHandle {
    /// Stop the screencast and the pump.
    pub async fn stop(self) {
        self.task.abort();
        let _ = self.tab.conn.call("Page.stopScreencast", json!({})).await;
    }
}

impl Drop for ScreencastHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The next event, skipping the ones a slow pump missed rather than ending.
async fn next_event(events: &mut broadcast::Receiver<CdpEvent>) -> Option<CdpEvent> {
    loop {
        match events.recv().await {
            Ok(event) => return Some(event),
            Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(broadcast::error::RecvError::Closed) => return None,
        }
    }
}

impl BrowserDriver {
    /// Start relaying a tab's frames to `sink`.
    ///
    /// # Errors
    /// Returns [`BrowserFault`] when the tab does not exist or the browser refuses.
    pub async fn start_screencast(
        &self,
        tab_id: &str,
        sink: mpsc::Sender<ScreencastFrame>,
    ) -> Result<ScreencastHandle, BrowserFault> {
        let tab = self.tab(Some(tab_id))?;
        let mut events = tab.conn.subscribe();
        tab.conn
            .call(
                "Page.startScreencast",
                json!({ "format": "jpeg", "quality": 60, "maxWidth": 1280, "maxHeight": 800, "everyNthFrame": 1 }),
            )
            .await?;
        let pumped = Arc::clone(&tab);
        let session_id = self.session_id();
        let task = tokio::spawn(async move {
            let mut sequence = 0_u64;
            while let Some(event) = next_event(&mut events).await {
                if event.method != "Page.screencastFrame" {
                    continue;
                }
                if let Some(ack) = event.params.get("sessionId") {
                    let _ = pumped
                        .conn
                        .call("Page.screencastFrameAck", json!({ "sessionId": ack }))
                        .await;
                }
                let Some(data) = event.params.get("data").and_then(|data| data.as_str()) else {
                    continue;
                };
                sequence += 1;
                let dimension = |key: &str| {
                    event
                        .params
                        .pointer(&format!("/metadata/{key}"))
                        .and_then(serde_json::Value::as_f64)
                        .map_or(0, |value| value.round().clamp(0.0, 100_000.0) as u32)
                };
                let frame = ScreencastFrame {
                    session_id: session_id.clone(),
                    tab_id: pumped.id.clone(),
                    media_type: "image/jpeg",
                    data: data.to_string(),
                    url: pumped.state().url.clone(),
                    device_width: dimension("deviceWidth"),
                    device_height: dimension("deviceHeight"),
                    sequence,
                };
                if sink.is_closed() {
                    break;
                }
                let _ = sink.try_send(frame);
            }
        });
        Ok(ScreencastHandle { task, tab })
    }
}
