//! The hardened browser driver: tabs, observation and target resolution (ADP-001).
//!
//! [`BrowserDriver`] is one managed browser session as a model drives it. It sits on the CDP
//! connection and the low-level reads in `mod.rs`, and adds what a model needs to act on a page it
//! cannot see live:
//!
//! * a **document revision** per tab, advanced by every navigation and frame-tree change, so an action
//!   can say which page it was meant for;
//! * **snapshots** whose element refs are bound to their tab and revision and are re-verified against
//!   the live page — by backend node, by semantic fingerprint and by a hit test at the point that will
//!   be clicked — before anything is dispatched;
//! * **captures** of the viewport whose points are bound to the screenshot they were read from, with
//!   per-tile digests that flag pixels changing near a click;
//! * one **action queue** per session, so two tool calls never interleave their input;
//! * everything a page can do *to* the agent — raise a dialog, start a download, ask for a permission,
//!   present a bad certificate, open a window, hand off to another application — as typed state the
//!   model reads and the rules in `actions.rs` enforce.
//!
//! The actions themselves (navigate, click, type, press a key, answer a dialog) are in `actions.rs`;
//! the model-facing tool surface is in `tools.rs`.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use futures_util::future::join_all;
use quansio_core::Digest;
use serde_json::{json, Value};
use tokio::sync::{broadcast, watch, Mutex as AsyncMutex};

use super::cdp::{self, CdpConnection, CdpEvent, PageTarget};
use super::fault::{BrowserFault, FaultCode};
use super::fence::{Capture, ElementRef, Fingerprint, Snapshot, Target};
use super::journal::ActionJournal;
use super::origin::validate_navigation;
use super::secrets::{sensitivity_of, SecretPort};
use super::tab::{
    CertificateStatus, DownloadPolicy, DownloadState, FollowUp, HandoffRecord, PendingDialog,
    PopupRecord, TabState,
};
use super::{BrowserSession, Classified, Control, FormSemantics};

/// The roles an observation gives a ref. These are the accessibility roles a person can act on.
const INTERACTIVE_ROLES: [&str; 17] = [
    "button",
    "link",
    "textbox",
    "searchbox",
    "combobox",
    "checkbox",
    "radio",
    "switch",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "tab",
    "option",
    "slider",
    "spinbutton",
    "treeitem",
    "listbox",
];

/// The permissions that are denied the moment a tab is attached, and stay denied unless a user grants a
/// media permission through a decision receipt. The names are the Permissions API's.
const DENIED_PERMISSIONS: [&str; 16] = [
    "geolocation",
    "notifications",
    "midi",
    "camera",
    "microphone",
    "clipboard-read",
    "clipboard-write",
    "persistent-storage",
    "background-sync",
    "screen-wake-lock",
    "idle-detection",
    "payment-handler",
    "local-fonts",
    "window-management",
    "storage-access",
    "display-capture",
];

/// Reads what the page says about one node, run inside the node's own frame.
pub(super) const DESCRIBE_FN: &str = r#"function () {
  const el = this;
  const doc = el.ownerDocument;
  const win = doc ? doc.defaultView : null;
  let ancestors = [];
  try { ancestors = win && win.location.ancestorOrigins ? Array.from(win.location.ancestorOrigins) : []; } catch (e) {}
  const attr = (name) => (el.getAttribute ? el.getAttribute(name) : null) || "";
  const anchor = el.closest ? el.closest("a[href]") : null;
  const form = el.closest ? el.closest("form") : null;
  let formInfo = null;
  if (form) {
    let action = null;
    try { action = form.getAttribute("action") ? new URL(form.getAttribute("action"), doc.baseURI).href : null; } catch (e) {}
    formInfo = {
      action: action,
      method: form.getAttribute("method"),
      input_types: Array.from(form.querySelectorAll("input")).map((i) => (i.type || "text").toLowerCase()),
      field_names: Array.from(form.querySelectorAll("input,select,textarea")).map((i) => i.name || "").filter(Boolean)
    };
  }
  return {
    tag: (el.tagName || "").toLowerCase(),
    connected: !!el.isConnected,
    focused: doc ? doc.activeElement === el : false,
    origin: win ? win.location.origin : null,
    ancestor_origins: ancestors,
    type: (el.type || "").toLowerCase(),
    autocomplete: attr("autocomplete").toLowerCase(),
    name: attr("name"),
    id: el.id || "",
    placeholder: attr("placeholder"),
    aria_label: attr("aria-label"),
    editable: !!(el.isContentEditable || /^(input|textarea|select)$/i.test(el.tagName || "")),
    href: anchor ? anchor.href : null,
    form: formInfo
  };
}"#;

/// How a driver is configured.
#[derive(Debug, Clone)]
pub struct DriverConfig {
    /// The browser's DevTools port, on loopback.
    pub debugging_port: u16,
    /// Where the journal and the downloads of this session live.
    pub session_dir: PathBuf,
    /// How long one input event may go unacknowledged before the action's outcome is *in doubt*.
    pub action_timeout: Duration,
    /// How long a navigation may take before it is reported as still loading.
    pub navigation_timeout: Duration,
    /// How long to wait for a page to finish loading after an action.
    pub settle_timeout: Duration,
    /// Download bounds.
    pub download: DownloadPolicy,
    /// The side of a capture's digest tile, in CSS pixels.
    pub tile_px: u32,
    /// How recent a user's input must be to count as the gesture behind a popup or a handoff.
    pub gesture_window: Duration,
    /// How long a certificate error may wait for a user before it is cancelled.
    pub certificate_wait: Duration,
    /// Bound on the readable text of an observation.
    pub max_text_bytes: usize,
    /// Bound on the refs of one snapshot.
    pub max_refs: usize,
}

impl DriverConfig {
    /// The defaults, for a browser on `debugging_port` and a session directory.
    #[must_use]
    pub fn new(debugging_port: u16, session_dir: impl Into<PathBuf>) -> Self {
        Self {
            debugging_port,
            session_dir: session_dir.into(),
            action_timeout: Duration::from_secs(15),
            navigation_timeout: Duration::from_secs(30),
            settle_timeout: Duration::from_secs(3),
            download: DownloadPolicy::default(),
            tile_px: 200,
            gesture_window: Duration::from_secs(2),
            certificate_wait: Duration::from_secs(120),
            max_text_bytes: super::DEFAULT_MAX_TEXT_BYTES,
            max_refs: 300,
        }
    }
}

/// The identity of one tool call, as the runtime dispatches it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserCallContext {
    /// The run.
    pub run_id: String,
    /// The ToolCall (`tc_…`).
    pub tool_call_id: String,
    /// The action id: the effect's idempotency key, derived by the runtime and never by the model.
    pub action_id: String,
    /// The highest consequence tier the runtime approved for this call. `None` means no ceiling was
    /// stated; the driver then only reports the classification.
    pub approved_tier: Option<u8>,
}

/// What an action or a read returned.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionReport {
    /// The structured output. Page-derived text in it is `UNTRUSTED_EXTERNAL`.
    pub output: Value,
    /// What the action settles on the Effect Ledger, when it has an external consequence.
    pub classified: Option<Classified>,
    /// Evidence bytes (a screenshot), when the call captured any.
    pub evidence: Option<Vec<u8>>,
    /// Set when the outcome could not be established. Never retried.
    pub in_doubt: Option<String>,
    /// Whether this answered a replay of an action that already ran.
    pub replayed: bool,
}

impl ActionReport {
    pub(super) fn of(output: Value) -> Self {
        Self {
            output,
            classified: None,
            evidence: None,
            in_doubt: None,
            replayed: false,
        }
    }
}

/// One tab: its connection and its state.
pub(super) struct Tab {
    pub(super) id: String,
    pub(super) conn: CdpConnection,
    state: Mutex<TabState>,
    pub(super) attention: watch::Sender<u64>,
    pub(super) denied: Mutex<Vec<String>>,
}

impl Tab {
    /// The tab's state.
    pub(super) fn state(&self) -> MutexGuard<'_, TabState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Wake whoever is waiting on a dialog, a certificate or a closure.
    pub(super) fn ping(&self) {
        self.attention.send_modify(|count| *count += 1);
    }
}

pub(super) struct DriverInner {
    pub(super) config: DriverConfig,
    pub(super) session: Mutex<BrowserSession>,
    pub(super) journal: ActionJournal,
    pub(super) queue: AsyncMutex<()>,
    pub(super) tabs: Mutex<Vec<Arc<Tab>>>,
    pub(super) secrets: Option<SecretPort>,
    nonce: u64,
    pub(super) ignored: Mutex<BTreeSet<String>>,
}

/// A managed browser session, driven as a model drives it.
#[derive(Clone)]
pub struct BrowserDriver {
    pub(super) inner: Arc<DriverInner>,
}

impl std::fmt::Debug for BrowserDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserDriver")
            .field("session", &self.session_id())
            .finish()
    }
}

static NONCE: AtomicU64 = AtomicU64::new(1);

/// The first number out of a CDP quad (`x1 y1 x2 y2 …`) list, as the centre of the quad.
fn quad_centre(quad: &[f64]) -> Option<(f64, f64)> {
    if quad.len() < 8 {
        return None;
    }
    let xs = [quad[0], quad[2], quad[4], quad[6]];
    let ys = [quad[1], quad[3], quad[5], quad[7]];
    // The shoelace area: a quad with no area cannot be clicked.
    let mut area = 0.0;
    for i in 0..4 {
        let j = (i + 1) % 4;
        area += xs[i] * ys[j] - xs[j] * ys[i];
    }
    if (area / 2.0).abs() < 1.0 {
        return None;
    }
    Some((xs.iter().sum::<f64>() / 4.0, ys.iter().sum::<f64>() / 4.0))
}

fn stale(reason: &str, details: Value) -> BrowserFault {
    let mut with_reason = json!({ "reason": reason });
    if let (Some(target), Some(extra)) = (with_reason.as_object_mut(), details.as_object()) {
        for (key, value) in extra {
            target.insert(key.clone(), value.clone());
        }
    }
    BrowserFault::new(
        FaultCode::StalePage,
        format!("the page is not the one the action was meant for ({reason}); observe it again"),
    )
    .with_details(with_reason)
}

/// Where an element is, as the page and the accessibility tree describe it right now.
pub(super) struct LiveNode {
    pub(super) object_id: String,
    pub(super) descriptor: Value,
    pub(super) role: String,
    pub(super) name: String,
}

impl LiveNode {
    pub(super) fn fingerprint(&self) -> Fingerprint {
        Fingerprint {
            role: self.role.clone(),
            name: self.name.clone(),
            tag: self
                .descriptor
                .get("tag")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            frame_origin: self
                .descriptor
                .get("origin")
                .and_then(Value::as_str)
                .unwrap_or("null")
                .to_string(),
        }
    }

    /// The control this node is, for classification of what clicking it would do.
    pub(super) fn control(&self) -> Control {
        let form = self
            .descriptor
            .get("form")
            .filter(|form| form.is_object())
            .map(|form| {
                let list = |key: &str| {
                    form.get(key)
                        .and_then(Value::as_array)
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default()
                };
                FormSemantics {
                    action: form
                        .get("action")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    method: form
                        .get("method")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    input_types: list("input_types"),
                    field_names: list("field_names"),
                }
            });
        Control {
            role: self.role.clone(),
            name: self.name.clone(),
            href: self
                .descriptor
                .get("href")
                .and_then(Value::as_str)
                .map(str::to_string),
            form,
        }
    }
}

/// A target the driver resolved to a point on the live page.
pub(super) struct Resolved {
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) backend_node_id: i64,
    /// What the snapshot said the element was, when the target was a ref.
    pub(super) expected: Option<Fingerprint>,
    pub(super) node: LiveNode,
    pub(super) warnings: Vec<String>,
    pub(super) hit: Value,
}

impl BrowserDriver {
    /// Open the driver for a session: the journal is recovered from the session directory, so an action
    /// a previous worker left unfinished is in doubt from the first call.
    ///
    /// # Errors
    /// Returns [`BrowserFault`] when the session directory or the journal cannot be used.
    pub fn open(
        config: DriverConfig,
        session: BrowserSession,
        secrets: Option<SecretPort>,
    ) -> Result<Self, BrowserFault> {
        std::fs::create_dir_all(config.session_dir.join("downloads")).map_err(|error| {
            BrowserFault::new(
                FaultCode::PageFailure,
                format!("the session directory is unusable: {error}"),
            )
        })?;
        let journal = ActionJournal::open(&config.session_dir.join("actions.jsonl"))?;
        Ok(Self {
            inner: Arc::new(DriverInner {
                config,
                session: Mutex::new(session),
                journal,
                queue: AsyncMutex::new(()),
                tabs: Mutex::new(Vec::new()),
                secrets,
                nonce: NONCE.fetch_add(1, Ordering::Relaxed)
                    ^ std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|elapsed| {
                            u64::try_from(elapsed.as_nanos() & 0xFFFF_FFFF_FFFF).unwrap_or(0)
                        })
                        .unwrap_or(0),
                ignored: Mutex::new(BTreeSet::new()),
            }),
        })
    }

    /// The session's id.
    #[must_use]
    pub fn session_id(&self) -> String {
        self.session().id.clone()
    }

    pub(super) fn session(&self) -> MutexGuard<'_, BrowserSession> {
        self.inner
            .session
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The action journal.
    #[must_use]
    pub fn journal(&self) -> &ActionJournal {
        &self.inner.journal
    }

    /// Give control to the user: agent input is fenced until a handback.
    ///
    /// # Errors
    /// Returns [`BrowserFault`] when the session is not active.
    pub fn request_takeover(&self, at: &str) -> Result<(), BrowserFault> {
        self.session()
            .request_takeover(at)
            .map_err(BrowserFault::from)
    }

    /// Return control to the agent.
    ///
    /// # Errors
    /// Returns [`BrowserFault`] when a user does not hold the session.
    pub fn handback(&self, at: &str) -> Result<(), BrowserFault> {
        self.session().handback(at).map_err(BrowserFault::from)
    }

    /// The tabs the driver holds, in the order they were opened.
    #[must_use]
    pub fn tab_ids(&self) -> Vec<String> {
        self.inner
            .tabs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|tab| tab.id.clone())
            .collect()
    }

    /// The tab a request names, or the first one when it names none.
    pub(super) fn tab(&self, tab_id: Option<&str>) -> Result<Arc<Tab>, BrowserFault> {
        let tabs = self
            .inner
            .tabs
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match tab_id {
            Some(id) => tabs.iter().find(|tab| tab.id == id).cloned(),
            None => tabs.first().cloned(),
        }
        .ok_or_else(|| {
            BrowserFault::new(
                FaultCode::TargetNotFound,
                format!("the session has no tab {}", tab_id.unwrap_or("(none)")),
            )
            .with_details(json!({ "reason": "tab_unknown" }))
        })
    }

    /// Open a blank tab and attach to it.
    ///
    /// # Errors
    /// Returns [`BrowserFault`] when the browser does not answer.
    pub async fn open_tab(&self) -> Result<String, BrowserFault> {
        let page = cdp::open_page(self.inner.config.debugging_port, "about:blank").await?;
        self.attach_page(page).await
    }

    /// Attach to a page the browser already has open.
    ///
    /// Everything the guard needs is armed here, before the page is used: its domains are enabled,
    /// certificate errors are routed to the driver instead of being decided by the browser, downloads
    /// are directed to the session's bounded directory, and every permission is denied.
    ///
    /// # Errors
    /// Returns [`BrowserFault`] when the connection or a required domain fails.
    pub async fn attach_page(&self, page: PageTarget) -> Result<String, BrowserFault> {
        let conn = cdp::connect(page.clone()).await?;
        let rx = conn.subscribe();
        let (attention, _) = watch::channel(0u64);
        let tab = Arc::new(Tab {
            id: page.id.clone(),
            conn,
            state: Mutex::new(TabState::new(&page.id, self.inner.nonce)),
            attention,
            denied: Mutex::new(Vec::new()),
        });
        tokio::spawn(pump(Arc::downgrade(&self.inner), Arc::clone(&tab), rx));

        for method in [
            "Page.enable",
            "DOM.enable",
            "Runtime.enable",
            "Accessibility.enable",
            "Network.enable",
            "Security.enable",
        ] {
            tab.conn.call(method, json!({})).await?;
        }
        tab.conn
            .call(
                "Security.setOverrideCertificateErrors",
                json!({ "override": true }),
            )
            .await?;
        let downloads = self.inner.config.session_dir.join("downloads");
        tab.conn
            .call(
                "Browser.setDownloadBehavior",
                json!({
                    "behavior": "allowAndName",
                    "downloadPath": downloads.to_string_lossy(),
                    "eventsEnabled": true,
                }),
            )
            .await?;
        for permission in DENIED_PERMISSIONS {
            // A browser that does not know a permission name is not a reason to leave the others open.
            let accepted = tab
                .conn
                .call(
                    "Browser.setPermission",
                    json!({ "permission": { "name": permission }, "setting": "denied" }),
                )
                .await
                .is_ok();
            if accepted {
                tab.denied
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(permission.to_string());
            }
        }
        let tree = tab.conn.call("Page.getFrameTree", json!({})).await?;
        if let Some(frame) = tree.pointer("/frameTree/frame") {
            let mut state = tab.state();
            state.main_frame_id = frame.get("id").and_then(Value::as_str).map(str::to_string);
            state.url = frame
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            state.main_origin = frame
                .get("securityOrigin")
                .and_then(Value::as_str)
                .unwrap_or("null")
                .to_string();
        }
        let id = tab.id.clone();
        self.inner
            .tabs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(tab);
        Ok(id)
    }

    /// Close a tab.
    ///
    /// # Errors
    /// Returns [`BrowserFault`] when the tab is unknown.
    pub async fn close_tab(&self, tab_id: &str) -> Result<(), BrowserFault> {
        let tab = self.tab(Some(tab_id))?;
        self.inner
            .ignored
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(tab.id.clone());
        let _ = cdp::close_page(self.inner.config.debugging_port, &tab.id).await;
        self.inner
            .tabs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|open| open.id != tab_id);
        Ok(())
    }

    // ------------------------------------------------------------------ fences

    /// Refuse to act while the page is waiting on something only someone else can resolve.
    pub(super) fn fence_pending(&self, tab: &Tab, allow_dialog: bool) -> Result<(), BrowserFault> {
        let state = tab.state();
        if state.closed {
            return Err(BrowserFault::new(
                FaultCode::TargetNotFound,
                "the tab's connection is closed",
            )
            .with_details(json!({ "reason": "tab_closed" })));
        }
        if !allow_dialog {
            if let Some(dialog) = &state.dialog {
                return Err(BrowserFault::new(
                    FaultCode::DialogPending,
                    "a dialog is waiting for an answer; only browser.handle_dialog can run",
                )
                .with_details(json!({ "dialog": dialog.to_json() })));
            }
        }
        if let Some(certificate) = state.pending_certificates().first() {
            return Err(BrowserFault::new(
                FaultCode::CertificateDecisionRequired,
                "a certificate error is waiting for a decision only a user can make",
            )
            .with_details(json!({ "certificate": certificate.to_json() })));
        }
        Ok(())
    }

    /// Refuse an action whose revision is not the tab's.
    pub(super) fn check_revision(&self, tab: &Tab, expected: &str) -> Result<(), BrowserFault> {
        let state = tab.state();
        if state.revision != expected {
            return Err(stale(
                "document_revision",
                json!({ "expected": expected, "actual": state.revision }),
            ));
        }
        Ok(())
    }

    /// Refuse agent input while a user holds the session.
    pub(super) fn require_control(&self) -> Result<(), BrowserFault> {
        self.session()
            .require_agent_control()
            .map_err(BrowserFault::from)
    }

    // ------------------------------------------------------------------ live node

    /// Resolve a backend node and describe it as the page does right now.
    pub(super) async fn live_node(
        &self,
        conn: &CdpConnection,
        backend_node_id: i64,
    ) -> Result<LiveNode, BrowserFault> {
        let gone = |why: &str| stale("node_gone", json!({ "detail": why }));
        let resolved = conn
            .call(
                "DOM.resolveNode",
                json!({ "backendNodeId": backend_node_id, "objectGroup": "quansio" }),
            )
            .await
            .map_err(|_| gone("the node no longer resolves"))?;
        let object_id = resolved
            .pointer("/object/objectId")
            .and_then(Value::as_str)
            .ok_or_else(|| gone("the node has no object"))?
            .to_string();
        let described = conn
            .call(
                "Runtime.callFunctionOn",
                json!({
                    "objectId": object_id,
                    "functionDeclaration": DESCRIBE_FN,
                    "returnByValue": true,
                }),
            )
            .await
            .map_err(|_| gone("the node cannot be described"))?;
        let descriptor = described
            .pointer("/result/value")
            .cloned()
            .filter(Value::is_object)
            .ok_or_else(|| gone("the node answered nothing"))?;
        if descriptor.get("connected").and_then(Value::as_bool) != Some(true) {
            return Err(gone("the node is no longer in the document"));
        }
        let (role, name) = self.ax_role_name(conn, backend_node_id).await;
        Ok(LiveNode {
            object_id,
            descriptor,
            role,
            name,
        })
    }

    async fn ax_role_name(&self, conn: &CdpConnection, backend_node_id: i64) -> (String, String) {
        let Ok(tree) = conn
            .call(
                "Accessibility.getPartialAXTree",
                json!({ "backendNodeId": backend_node_id, "fetchRelatives": false }),
            )
            .await
        else {
            return (String::new(), String::new());
        };
        let node = tree.pointer("/nodes/0");
        let text = |pointer: &str| {
            node.and_then(|node| node.pointer(pointer))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        (text("/role/value"), text("/name/value"))
    }

    // ------------------------------------------------------------------ observation

    /// Observe a tab: its readable text and a snapshot whose refs a model may act on.
    ///
    /// A tab that has a dialog or a certificate error pending is not read — the page cannot answer while
    /// it waits — and the observation says so and carries the pending state instead.
    ///
    /// # Errors
    /// Returns [`BrowserFault`] when the tab is unknown or the browser does not answer, and
    /// [`FaultCode::StalePage`] when the page would not hold still long enough to be read.
    pub async fn observe(
        &self,
        tab_id: Option<&str>,
        include_screenshot: bool,
    ) -> Result<ActionReport, BrowserFault> {
        let _turn = self.inner.queue.lock().await;
        let tab = self.tab(tab_id)?;
        let mut output = self.tab_status(&tab);
        {
            let state = tab.state();
            if state.dialog.is_some() || !state.pending_certificates().is_empty() {
                output["page_available"] = json!(false);
                return Ok(ActionReport::of(output));
            }
        }
        let mut last = None;
        for _ in 0..3 {
            let revision = tab.state().revision.clone();
            match self.build_snapshot(&tab, &revision).await {
                Ok(built) => {
                    if tab.state().revision == revision {
                        last = Some((revision, built));
                        break;
                    }
                }
                Err(error) => {
                    // A page that changed under the read is retried; any other failure is the answer.
                    if tab.state().revision == revision {
                        return Err(error);
                    }
                }
            }
        }
        let (revision, built) = last.ok_or_else(|| {
            stale(
                "page_unstable",
                json!({ "detail": "the page changed while it was being read" }),
            )
        })?;
        let snapshot_id = {
            let mut state = tab.state();
            let id = state.mint("snap");
            let snapshot = Snapshot {
                id: id.clone(),
                tab_id: tab.id.clone(),
                revision: revision.clone(),
                refs: built.refs.clone(),
            };
            state.snapshots.insert(id.clone(), snapshot);
            id
        };
        output["page_available"] = json!(true);
        output["snapshot_id"] = json!(snapshot_id);
        output["document_revision"] = json!(revision);
        output["title"] = json!(built.title);
        output["text"] = json!(built.text);
        output["text_truncated"] = json!(built.truncated);
        output["elements"] = Value::Array(built.elements);
        output["elements_truncated"] = json!(built.elements_truncated);
        output["frames_skipped"] = json!(built.frames_skipped);
        let mut evidence = None;
        if include_screenshot {
            let (capture, png) = self.take_capture(&tab).await?;
            output["capture"] = capture;
            evidence = Some(png);
        }
        let mut report = ActionReport::of(output);
        report.evidence = evidence;
        Ok(report)
    }

    /// The part of an observation that comes from the driver's state rather than from reading the page.
    pub(super) fn tab_status(&self, tab: &Tab) -> Value {
        let state = tab.state();
        json!({
            "tab_id": tab.id,
            "url": state.url,
            "document_revision": state.revision,
            "dialog": state.dialog.as_ref().map(PendingDialog::to_json),
            "certificates": state.certificates.iter().map(|c| c.to_json()).collect::<Vec<_>>(),
            "downloads": state.downloads.iter().map(|d| d.to_json()).collect::<Vec<_>>(),
            "popups_blocked": state.popups.iter().filter(|p| p.blocked).map(|p| json!({
                "url": p.url,
                "reason": p.reason,
            })).collect::<Vec<_>>(),
            "handoffs_blocked": state.handoffs.iter().filter(|h| h.blocked).map(|h| json!({
                "scheme": h.scheme,
                "reason": h.reason,
            })).collect::<Vec<_>>(),
            "console": state.console.iter().map(|entry| json!({
                "level": entry.level,
                "text": entry.text,
            })).collect::<Vec<_>>(),
            "permissions_denied": tab.denied.lock().unwrap_or_else(PoisonError::into_inner).clone(),
        })
    }

    async fn build_snapshot(
        &self,
        tab: &Tab,
        _revision: &str,
    ) -> Result<BuiltSnapshot, BrowserFault> {
        let extracted = super::extract(&tab.conn).await?;
        let title = extracted
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let (text, truncated, _, _, _) =
            super::bound(&extracted, self.inner.config.max_text_bytes, 0);

        let tree = tab.conn.call("Page.getFrameTree", json!({})).await?;
        let mut frames = Vec::new();
        if let Some(root) = tree.get("frameTree") {
            flatten_frames(root, &mut frames);
        }
        let mut candidates: Vec<(i64, String, String, String)> = Vec::new();
        let mut frames_skipped = 0u32;
        for frame_id in &frames {
            let ax = tab
                .conn
                .call(
                    "Accessibility.getFullAXTree",
                    json!({ "frameId": frame_id }),
                )
                .await;
            let Ok(ax) = ax else {
                frames_skipped += 1;
                continue;
            };
            for node in ax
                .get("nodes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if node
                    .get("ignored")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    continue;
                }
                let role = node
                    .pointer("/role/value")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !INTERACTIVE_ROLES.contains(&role) {
                    continue;
                }
                let Some(backend) = node.get("backendDOMNodeId").and_then(Value::as_i64) else {
                    continue;
                };
                let name = node
                    .pointer("/name/value")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                candidates.push((
                    backend,
                    frame_id.clone(),
                    role.to_string(),
                    name.to_string(),
                ));
            }
        }
        let elements_truncated = candidates.len() > self.inner.config.max_refs;
        candidates.truncate(self.inner.config.max_refs);

        let mut refs = std::collections::BTreeMap::new();
        let mut elements = Vec::new();
        for chunk in candidates.chunks(12) {
            let described = join_all(chunk.iter().map(|(backend, _, _, _)| async move {
                self.describe_only(&tab.conn, *backend).await
            }))
            .await;
            for ((backend, frame_id, role, name), descriptor) in chunk.iter().zip(described) {
                let Some(descriptor) = descriptor else {
                    continue;
                };
                let tag = descriptor
                    .get("tag")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let origin = descriptor
                    .get("origin")
                    .and_then(Value::as_str)
                    .unwrap_or("null")
                    .to_string();
                let sensitive =
                    sensitivity_of(&descriptor).map(super::secrets::SensitiveKind::as_str);
                let reference = format!("e{}", refs.len() + 1);
                let fingerprint = Fingerprint {
                    role: role.clone(),
                    name: name.clone(),
                    tag,
                    frame_origin: origin,
                };
                let mut element = fingerprint.to_json();
                element["ref"] = json!(reference);
                if let Some(kind) = sensitive {
                    element["sensitive"] = json!(kind);
                }
                elements.push(element);
                refs.insert(
                    reference.clone(),
                    ElementRef {
                        reference,
                        backend_node_id: *backend,
                        frame_id: frame_id.clone(),
                        fingerprint,
                        sensitive,
                    },
                );
            }
        }
        let _ = tab
            .conn
            .call(
                "Runtime.releaseObjectGroup",
                json!({ "objectGroup": "quansio" }),
            )
            .await;
        Ok(BuiltSnapshot {
            title,
            text,
            truncated,
            elements,
            elements_truncated,
            frames_skipped,
            refs,
        })
    }

    /// Describe a node for a snapshot, or `None` when it cannot be described (it is gone, or hidden).
    async fn describe_only(&self, conn: &CdpConnection, backend: i64) -> Option<Value> {
        let resolved = conn
            .call(
                "DOM.resolveNode",
                json!({ "backendNodeId": backend, "objectGroup": "quansio" }),
            )
            .await
            .ok()?;
        let object_id = resolved.pointer("/object/objectId")?.as_str()?.to_string();
        let described = conn
            .call(
                "Runtime.callFunctionOn",
                json!({
                    "objectId": object_id,
                    "functionDeclaration": DESCRIBE_FN,
                    "returnByValue": true,
                }),
            )
            .await
            .ok()?;
        let value = described.pointer("/result/value")?.clone();
        (value.get("connected").and_then(Value::as_bool) == Some(true)).then_some(value)
    }

    // ------------------------------------------------------------------ captures

    /// Capture the viewport as a PNG and record what a model needs to point at it.
    ///
    /// # Errors
    /// Returns [`BrowserFault`] when the browser does not answer.
    pub async fn screenshot(&self, tab_id: Option<&str>) -> Result<ActionReport, BrowserFault> {
        let _turn = self.inner.queue.lock().await;
        let tab = self.tab(tab_id)?;
        self.fence_pending(&tab, false)?;
        let (capture, png) = self.take_capture(&tab).await?;
        let mut report = ActionReport::of(capture);
        report.evidence = Some(png);
        Ok(report)
    }

    async fn viewport(&self, conn: &CdpConnection) -> Result<(u32, u32, i64, i64), BrowserFault> {
        let metrics = conn.call("Page.getLayoutMetrics", json!({})).await?;
        let viewport = metrics
            .get("cssVisualViewport")
            .or_else(|| metrics.get("cssLayoutViewport"))
            .cloned()
            .unwrap_or(Value::Null);
        let number = |key: &str| viewport.get(key).and_then(Value::as_f64).unwrap_or(0.0);
        // The viewport is a handful of thousand pixels at most; the casts cannot truncate.
        Ok((
            number("clientWidth").round().max(1.0) as u32,
            number("clientHeight").round().max(1.0) as u32,
            number("pageX").round() as i64,
            number("pageY").round() as i64,
        ))
    }

    async fn tile_digest(
        &self,
        conn: &CdpConnection,
        capture: &Capture,
        column: u32,
        row: u32,
    ) -> Result<String, BrowserFault> {
        let (x, y, width, height) = capture.tile_rect(column, row);
        let shot = conn
            .call(
                "Page.captureScreenshot",
                json!({
                    "format": "png",
                    "clip": {
                        "x": capture.scroll_x + i64::from(x),
                        "y": capture.scroll_y + i64::from(y),
                        "width": width.max(1),
                        "height": height.max(1),
                        "scale": 1,
                    },
                }),
            )
            .await?;
        let data = shot.get("data").and_then(Value::as_str).unwrap_or_default();
        Ok(Digest::of(data.as_bytes()).as_str().to_string())
    }

    async fn take_capture(&self, tab: &Tab) -> Result<(Value, Vec<u8>), BrowserFault> {
        let revision = tab.state().revision.clone();
        let (width, height, scroll_x, scroll_y) = self.viewport(&tab.conn).await?;
        let png = super::screenshot(&tab.conn).await?;
        let mut capture = Capture {
            id: String::new(),
            tab_id: tab.id.clone(),
            revision: revision.clone(),
            width,
            height,
            scroll_x,
            scroll_y,
            tile_px: self.inner.config.tile_px,
            tiles: std::collections::BTreeMap::new(),
        };
        let columns = width.div_ceil(capture.tile_px.max(1));
        let rows = height.div_ceil(capture.tile_px.max(1));
        for row in 0..rows {
            for column in 0..columns {
                let digest = self.tile_digest(&tab.conn, &capture, column, row).await?;
                capture.tiles.insert((column, row), digest);
            }
        }
        let id = {
            let mut state = tab.state();
            if state.revision != revision {
                return Err(stale(
                    "page_unstable",
                    json!({ "detail": "the page changed while it was being captured" }),
                ));
            }
            let id = state.mint("cap");
            capture.id = id.clone();
            state.captures.insert(id.clone(), capture);
            id
        };
        Ok((
            json!({
                "tab_id": tab.id,
                "capture_id": id,
                "document_revision": revision,
                "width": width,
                "height": height,
                "scroll_x": scroll_x,
                "scroll_y": scroll_y,
                "tile_px": self.inner.config.tile_px,
                "png_bytes": png.len(),
            }),
            png,
        ))
    }

    // ------------------------------------------------------------------ target resolution

    /// Resolve a target to a point on the live page, or refuse because the page is not the one the
    /// model saw. Nothing here changes the page except scrolling the element into view.
    pub(super) async fn resolve_target(
        &self,
        tab: &Tab,
        target: &Target,
        expected_revision: &str,
    ) -> Result<Resolved, BrowserFault> {
        self.check_revision(tab, expected_revision)?;
        match target {
            Target::Ref {
                snapshot_id,
                reference,
            } => {
                let element = {
                    let state = tab.state();
                    let snapshot = state.snapshots.get(snapshot_id).ok_or_else(|| {
                        stale(
                            "snapshot_unknown_or_invalidated",
                            json!({ "snapshot_id": snapshot_id }),
                        )
                    })?;
                    if snapshot.tab_id != tab.id || snapshot.revision != expected_revision {
                        return Err(stale(
                            "snapshot_not_for_this_page",
                            json!({ "snapshot_id": snapshot_id }),
                        ));
                    }
                    snapshot.refs.get(reference).cloned().ok_or_else(|| {
                        BrowserFault::new(
                            FaultCode::TargetNotFound,
                            format!("snapshot {snapshot_id} has no element {reference}"),
                        )
                        .with_details(json!({ "reason": "ref_unknown" }))
                    })?
                };
                let node = self.live_node(&tab.conn, element.backend_node_id).await?;
                if let Some(field) = element.fingerprint.difference(&node.fingerprint()) {
                    return Err(stale(
                        "fingerprint_changed",
                        json!({ "ref": reference, "field": field }),
                    ));
                }
                let _ = tab
                    .conn
                    .call(
                        "DOM.scrollIntoViewIfNeeded",
                        json!({ "backendNodeId": element.backend_node_id }),
                    )
                    .await;
                let quads = tab
                    .conn
                    .call(
                        "DOM.getContentQuads",
                        json!({ "backendNodeId": element.backend_node_id }),
                    )
                    .await
                    .map_err(|_| stale("not_visible", json!({ "ref": reference })))?;
                let (x, y) = quads
                    .get("quads")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|quad| {
                        let numbers: Vec<f64> =
                            quad.as_array()?.iter().filter_map(Value::as_f64).collect();
                        quad_centre(&numbers)
                    })
                    .next()
                    .ok_or_else(|| stale("not_visible", json!({ "ref": reference })))?;
                // The point must land on the element: a modal, a banner or an overlay in front of it
                // means the click would go to something the model did not choose.
                let hit = self.hit_test(tab, x, y).await?;
                let contains = self
                    .contains_node(&tab.conn, &node.object_id, hit.backend_node_id)
                    .await?;
                if !contains {
                    return Err(stale(
                        "hit_test_mismatch",
                        json!({ "ref": reference, "covered_by": hit.summary }),
                    ));
                }
                Ok(Resolved {
                    x,
                    y,
                    backend_node_id: element.backend_node_id,
                    expected: Some(element.fingerprint.clone()),
                    node,
                    warnings: Vec::new(),
                    hit: hit.summary,
                })
            }
            Target::Point { capture_id, x, y } => {
                let capture = {
                    let state = tab.state();
                    state.captures.get(capture_id).cloned().ok_or_else(|| {
                        stale(
                            "capture_unknown_or_invalidated",
                            json!({ "capture_id": capture_id }),
                        )
                    })?
                };
                if capture.tab_id != tab.id || capture.revision != expected_revision {
                    return Err(stale(
                        "capture_not_for_this_page",
                        json!({ "capture_id": capture_id }),
                    ));
                }
                if *x < 0.0
                    || *y < 0.0
                    || *x >= f64::from(capture.width)
                    || *y >= f64::from(capture.height)
                {
                    return Err(BrowserFault::new(
                        FaultCode::InvalidRequest,
                        "the point is outside the capture",
                    ));
                }
                let (_, _, scroll_x, scroll_y) = self.viewport(&tab.conn).await?;
                if scroll_x != capture.scroll_x || scroll_y != capture.scroll_y {
                    return Err(stale(
                        "scrolled",
                        json!({
                            "capture_id": capture_id,
                            "captured_at": [capture.scroll_x, capture.scroll_y],
                            "now": [scroll_x, scroll_y],
                        }),
                    ));
                }
                let hit = self.hit_test(tab, *x, *y).await?;
                let node = self.live_node(&tab.conn, hit.backend_node_id).await?;
                let mut warnings = Vec::new();
                let mut changed = 0usize;
                for (column, row) in capture.neighbourhood(*x, *y) {
                    let now = self.tile_digest(&tab.conn, &capture, column, row).await?;
                    if capture.tiles.get(&(column, row)) != Some(&now) {
                        changed += 1;
                    }
                }
                if changed > 0 {
                    warnings.push(format!(
                        "pixels_changed_near_point: {changed} tile(s) around the point differ from the capture"
                    ));
                }
                Ok(Resolved {
                    x: *x,
                    y: *y,
                    backend_node_id: hit.backend_node_id,
                    expected: None,
                    node,
                    warnings,
                    hit: hit.summary,
                })
            }
        }
    }

    async fn hit_test(&self, tab: &Tab, x: f64, y: f64) -> Result<Hit, BrowserFault> {
        let hit = tab
            .conn
            .call(
                "DOM.getNodeForLocation",
                json!({
                    "x": x.round() as i64,
                    "y": y.round() as i64,
                    "includeUserAgentShadowDOM": false,
                }),
            )
            .await
            .map_err(|_| stale("nothing_at_point", json!({ "x": x, "y": y })))?;
        let backend_node_id = hit
            .get("backendNodeId")
            .and_then(Value::as_i64)
            .ok_or_else(|| stale("nothing_at_point", json!({ "x": x, "y": y })))?;
        let summary = match self.live_node(&tab.conn, backend_node_id).await {
            Ok(node) => json!({
                "tag": node.descriptor.get("tag"),
                "role": node.role,
                "name": node.name,
                "frame_origin": node.descriptor.get("origin"),
            }),
            Err(_) => json!({ "tag": null }),
        };
        Ok(Hit {
            backend_node_id,
            summary,
        })
    }

    async fn contains_node(
        &self,
        conn: &CdpConnection,
        target_object: &str,
        hit_backend: i64,
    ) -> Result<bool, BrowserFault> {
        let resolved = conn
            .call(
                "DOM.resolveNode",
                json!({ "backendNodeId": hit_backend, "objectGroup": "quansio" }),
            )
            .await
            .map_err(|_| stale("hit_node_gone", json!({})))?;
        let hit_object = resolved
            .pointer("/object/objectId")
            .and_then(Value::as_str)
            .ok_or_else(|| stale("hit_node_gone", json!({})))?
            .to_string();
        let result = conn
            .call(
                "Runtime.callFunctionOn",
                json!({
                    "objectId": target_object,
                    "functionDeclaration": "function (hit) { return this === hit || this.contains(hit); }",
                    "arguments": [{ "objectId": hit_object }],
                    "returnByValue": true,
                }),
            )
            .await?;
        Ok(result.pointer("/result/value").and_then(Value::as_bool) == Some(true))
    }

    /// Reads one element's text, HTML or JSON, for `browser.extract`.
    ///
    /// # Errors
    /// Returns [`BrowserFault`] when the tab is unknown, a dialog or certificate error is pending, or
    /// the selector matches nothing.
    pub async fn extract(
        &self,
        tab_id: Option<&str>,
        selector: &str,
        format: &str,
    ) -> Result<ActionReport, BrowserFault> {
        let _turn = self.inner.queue.lock().await;
        let tab = self.tab(tab_id)?;
        self.fence_pending(&tab, false)?;
        let read = match format {
            "html" => "el.outerHTML",
            "json" => "JSON.stringify({text: el.innerText || el.textContent || '', tag: el.tagName.toLowerCase(), attributes: Object.fromEntries(Array.from(el.attributes).map((a) => [a.name, a.value]))})",
            _ => "el.innerText || el.textContent || ''",
        };
        let expression = format!(
            "(() => {{ const el = document.querySelector({}); if (!el) return null; return String({read}); }})()",
            Value::String(selector.to_string())
        );
        let result = tab
            .conn
            .call(
                "Runtime.evaluate",
                json!({ "expression": expression, "returnByValue": true }),
            )
            .await?;
        let content = result
            .pointer("/result/value")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BrowserFault::new(
                    FaultCode::TargetNotFound,
                    format!("no element matched {selector:?}"),
                )
                .with_details(json!({ "reason": "selector_unmatched" }))
            })?;
        let mut cut = content.len().min(self.inner.config.max_text_bytes);
        while cut > 0 && !content.is_char_boundary(cut) {
            cut -= 1;
        }
        Ok(ActionReport::of(json!({
            "tab_id": tab.id,
            "document_revision": tab.state().revision,
            "content": &content[..cut],
            "truncated": cut < content.len(),
        })))
    }

    // ------------------------------------------------------------------ reporting

    /// The driver's state as the server's guard report spells it
    /// (`quansio_machine::control::browser::GuardReport`).
    #[must_use]
    pub fn guard_report(&self) -> Value {
        let tabs: Vec<Arc<Tab>> = self
            .inner
            .tabs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let reported: Vec<Value> = tabs
            .iter()
            .map(|tab| {
                let state = tab.state();
                json!({
                    "tab_id": tab.id,
                    "url": state.url,
                    "document_revision": state.revision,
                    "dialog": state.dialog.as_ref().map(|dialog| json!({
                        "id": dialog.id,
                        "kind": dialog.kind,
                        "message": dialog.message,
                        "url": dialog.url,
                    })),
                    "certificates": state.certificates.iter().map(|c| c.to_json()).collect::<Vec<_>>(),
                    "downloads": state.downloads.iter().map(|d| json!({
                        "id": d.id,
                        "suggested_filename": d.suggested_filename,
                        "state": d.state.as_str(),
                        "received_bytes": d.received_bytes,
                        "failure": d.failure,
                    })).collect::<Vec<_>>(),
                    "popups_blocked": state.popups.iter().filter(|p| p.blocked).count(),
                })
            })
            .collect();
        let in_doubt: Vec<Value> = self
            .inner
            .journal
            .in_doubt()
            .into_iter()
            .map(|(key, reason)| {
                json!({
                    "call_id": key.call_id,
                    "action_id": key.action_id,
                    "reason": reason,
                })
            })
            .collect();
        json!({ "tabs": reported, "in_doubt": in_doubt })
    }

    /// Validate a navigation target before anything uses it.
    ///
    /// # Errors
    /// Returns [`FaultCode::NavigationDenied`].
    pub fn validate_destination(url: &str) -> Result<(), BrowserFault> {
        validate_navigation(url)
    }
}

struct BuiltSnapshot {
    title: String,
    text: String,
    truncated: bool,
    elements: Vec<Value>,
    elements_truncated: bool,
    frames_skipped: u32,
    refs: std::collections::BTreeMap<String, ElementRef>,
}

struct Hit {
    backend_node_id: i64,
    summary: Value,
}

fn flatten_frames(node: &Value, out: &mut Vec<String>) {
    if let Some(id) = node.pointer("/frame/id").and_then(Value::as_str) {
        out.push(id.to_string());
    }
    for child in node
        .get("childFrames")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        flatten_frames(child, out);
    }
}

// ---------------------------------------------------------------------- event pump

/// Whether an event changes something an in-flight action may be waiting on.
fn wakes_waiters(method: &str) -> bool {
    matches!(
        method,
        "Page.javascriptDialogOpening"
            | "Page.javascriptDialogClosed"
            | "Security.certificateError"
            | "Page.frameNavigated"
            | "Inspector.detached"
            | "Target.targetCrashed"
            | "Inspector.targetCrashed"
    )
}

/// Keeps a tab's state in step with the browser: one task per tab, reading every event.
async fn pump(inner: Weak<DriverInner>, tab: Arc<Tab>, mut rx: broadcast::Receiver<CdpEvent>) {
    loop {
        match rx.recv().await {
            Ok(event) => {
                let Some(driver) = inner.upgrade() else {
                    break;
                };
                let follow = tab.state().apply(&event, &driver.config.download);
                if wakes_waiters(&event.method) {
                    tab.ping();
                }
                for work in follow {
                    follow_up(&driver, &tab, work);
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {
                // Events were lost, so what the driver knows about the page is not to be trusted.
                tab.state().bump();
                tab.ping();
            }
            Err(broadcast::error::RecvError::Closed) => {
                tab.state().closed = true;
                tab.ping();
                break;
            }
        }
    }
}

fn follow_up(driver: &Arc<DriverInner>, tab: &Arc<Tab>, work: FollowUp) {
    match work {
        FollowUp::SuperviseDownload { guid } => {
            tokio::spawn(supervise_download(
                Arc::clone(driver),
                Arc::clone(tab),
                guid,
            ));
        }
        FollowUp::CancelDownload { guid } => {
            let tab = Arc::clone(tab);
            tokio::spawn(async move {
                let _ = tab
                    .conn
                    .call("Browser.cancelDownload", json!({ "guid": guid }))
                    .await;
            });
        }
        FollowUp::CancelCertificate { event_id } => {
            let tab = Arc::clone(tab);
            tokio::spawn(async move {
                let _ = tab
                    .conn
                    .call(
                        "Security.handleCertificateError",
                        json!({ "eventId": event_id, "action": "cancel" }),
                    )
                    .await;
            });
        }
        FollowUp::CertificatePending { id } => {
            tokio::spawn(certificate_timeout(Arc::clone(driver), Arc::clone(tab), id));
        }
        FollowUp::Popup { url } => {
            tokio::spawn(handle_popup(Arc::clone(driver), Arc::clone(tab), url));
        }
        FollowUp::ExternalHandoff { url } => {
            tokio::spawn(handle_handoff(Arc::clone(driver), Arc::clone(tab), url));
        }
        FollowUp::CertificateObserved { origin, sha256 } => {
            let mut state = tab.state();
            if let Some(record) = state.certificates.iter_mut().rev().find(|record| {
                record.origin == origin
                    && record.status == CertificateStatus::Continued
                    && record.certificate_sha256.is_none()
            }) {
                record.certificate_sha256 = Some(sha256);
            }
        }
        FollowUp::AttachmentPromised { url, filename } => {
            tokio::spawn(attachment_start_timeout(
                Arc::clone(driver),
                Arc::clone(tab),
                url,
                filename,
            ));
        }
        FollowUp::Noted => {}
    }
}

/// A response promised an attachment. If the browser has not begun a download for that URL within the
/// start timeout, stop the load and record a download that never started.
async fn attachment_start_timeout(
    driver: Arc<DriverInner>,
    tab: Arc<Tab>,
    url: String,
    filename: String,
) {
    tokio::time::sleep(driver.config.download.start_timeout).await;
    {
        let mut state = tab.state();
        if state.downloads.iter().any(|download| download.url == url) {
            return;
        }
        let now = std::time::Instant::now();
        let id = state.mint("dl");
        state.downloads.push(super::tab::DownloadRecord {
            id,
            guid: String::new(),
            url,
            suggested_filename: filename,
            state: DownloadState::Cancelled,
            received_bytes: 0,
            total_bytes: 0,
            file: None,
            sha256: None,
            failure: Some("start_timeout".to_string()),
            started_at: now,
            last_progress_at: now,
        });
    }
    let _ = tab.conn.call("Page.stopLoading", json!({})).await;
}

/// Cancel a download that never starts, stalls or grows past its bound; hash it when it completes.
async fn supervise_download(driver: Arc<DriverInner>, tab: Arc<Tab>, guid: String) {
    let policy = driver.config.download;
    // How many polls a completed download may wait for the browser to name its file.
    let mut waiting_for_file = 0u32;
    loop {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let verdict = {
            let state = tab.state();
            let Some(download) = state.downloads.iter().find(|d| d.guid == guid) else {
                return;
            };
            match download.state {
                DownloadState::InProgress => {
                    if download.received_bytes == 0
                        && download.started_at.elapsed() > policy.start_timeout
                    {
                        Some("start_timeout")
                    } else if download.received_bytes > 0
                        && download.last_progress_at.elapsed() > policy.stall_timeout
                    {
                        Some("stalled")
                    } else {
                        None
                    }
                }
                DownloadState::Completed => {
                    let finished = download.file.clone();
                    drop(state);
                    match finished {
                        Some(path) => {
                            record_download_file(&driver, &tab, &guid, &path);
                            return;
                        }
                        // The completion event can arrive before the one that names the file.
                        None if waiting_for_file < 40 => {
                            waiting_for_file += 1;
                            continue;
                        }
                        None => return,
                    }
                }
                DownloadState::Cancelled => return,
            }
        };
        if let Some(reason) = verdict {
            {
                let mut state = tab.state();
                if let Some(download) = state.downloads.iter_mut().find(|d| d.guid == guid) {
                    if download.state == DownloadState::InProgress {
                        download.state = DownloadState::Cancelled;
                        download.failure = Some(reason.to_string());
                    }
                }
            }
            let _ = tab
                .conn
                .call("Browser.cancelDownload", json!({ "guid": guid }))
                .await;
            return;
        }
    }
}

/// Record where a finished download is (relative to the session directory) and its SHA-256.
fn record_download_file(driver: &DriverInner, tab: &Tab, guid: &str, path: &str) {
    let root = driver.config.session_dir.join("downloads");
    let file = std::path::Path::new(path);
    let relative = file
        .strip_prefix(&root)
        .map(|rest| format!("downloads/{}", rest.display()))
        .unwrap_or_else(|_| format!("downloads/{guid}"));
    let digest = std::fs::read(file)
        .ok()
        .map(|bytes| Digest::of(&bytes).as_str().to_string());
    let mut state = tab.state();
    if let Some(download) = state.downloads.iter_mut().find(|d| d.guid == guid) {
        download.file = Some(relative);
        download.sha256 = digest;
    }
}

/// Cancel a certificate error nobody decided in time. Failing closed is the only safe default.
async fn certificate_timeout(driver: Arc<DriverInner>, tab: Arc<Tab>, id: String) {
    tokio::time::sleep(driver.config.certificate_wait).await;
    let event_id = {
        let mut state = tab.state();
        let Some(record) = state.certificates.iter_mut().find(|record| record.id == id) else {
            return;
        };
        if record.status != CertificateStatus::Pending {
            return;
        }
        record.status = CertificateStatus::Expired;
        record.event_id
    };
    let _ = tab
        .conn
        .call(
            "Security.handleCertificateError",
            json!({ "eventId": event_id, "action": "cancel" }),
        )
        .await;
    tab.ping();
}

/// A page opened a window. Without a genuine user gesture it is closed; with one it becomes a tab.
async fn handle_popup(driver: Arc<DriverInner>, tab: Arc<Tab>, url: String) {
    tokio::time::sleep(Duration::from_millis(200)).await;
    let Ok(pages) = cdp::pages(driver.config.debugging_port).await else {
        return;
    };
    let known: BTreeSet<String> = driver
        .tabs
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .map(|open| open.id.clone())
        .chain(
            driver
                .ignored
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .cloned(),
        )
        .collect();
    let genuine = tab
        .state()
        .user_gesture_within(driver.config.gesture_window);
    for page in pages.into_iter().filter(|page| !known.contains(&page.id)) {
        if genuine {
            tab.state().popups.push(PopupRecord {
                url: url.clone(),
                blocked: false,
                reason: "opened from a genuine user gesture".to_string(),
            });
            // A popup a user opened is a tab the session now has; the next call can name it.
            let driver_handle = BrowserDriver {
                inner: Arc::clone(&driver),
            };
            let _ = driver_handle.attach_page(page).await;
        } else {
            driver
                .ignored
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(page.id.clone());
            let _ = cdp::close_page(driver.config.debugging_port, &page.id).await;
            tab.state().popups.push(PopupRecord {
                url: url.clone(),
                blocked: true,
                reason: "agent input is not a user gesture".to_string(),
            });
        }
    }
}

/// A page tried to hand off to another application. Without a genuine user gesture it is stopped.
async fn handle_handoff(driver: Arc<DriverInner>, tab: Arc<Tab>, url: String) {
    let genuine = tab
        .state()
        .user_gesture_within(driver.config.gesture_window);
    let scheme = url
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !genuine {
        let _ = tab.conn.call("Page.stopLoading", json!({})).await;
    }
    tab.state().handoffs.push(HandoffRecord {
        scheme,
        blocked: !genuine,
        reason: if genuine {
            "from a genuine user gesture".to_string()
        } else {
            "agent input is not a user gesture".to_string()
        },
    });
}

/// Classify what an action would do, for the ledger, from the node it lands on.
pub(super) fn classify_node(node: &LiveNode) -> Classified {
    super::classify_control(&node.control())
}
