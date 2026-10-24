//! One tab's state, and the pure rules that keep it in step with what the browser reports (ADP-001).
//!
//! The browser is the authority on the page; this module is the driver's *memory* of it, and every
//! fact in it comes from an event. [`TabState::apply`] is a pure function of the state and one CDP
//! event: it changes the document revision when the page changes under the model, records a pending
//! dialog, a certificate error, a download or a blocked popup, and returns the follow-up work that needs
//! the connection (cancel a download, close a popup, time out a certificate decision). Keeping the rules
//! free of I/O is what makes them testable without a browser, and the driver tests them against one.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use quansio_core::Digest;
use serde_json::{json, Value};

use super::fence::{Capture, Snapshot};
use super::origin::{is_external_handoff, origin_of};
use super::CdpEvent;

/// How a download is bounded (DOMAIN.md §8.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadPolicy {
    /// The most bytes one download may receive before it is cancelled.
    pub max_bytes: u64,
    /// How long a started download may go without its first byte.
    pub start_timeout: Duration,
    /// How long a download may go without new bytes once it has begun.
    pub stall_timeout: Duration,
    /// The most downloads one tab may start; further ones are cancelled.
    pub max_per_tab: usize,
}

impl Default for DownloadPolicy {
    fn default() -> Self {
        Self {
            max_bytes: 50 * 1024 * 1024,
            start_timeout: Duration::from_secs(15),
            stall_timeout: Duration::from_secs(20),
            max_per_tab: 16,
        }
    }
}

/// A JavaScript dialog that is waiting for an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDialog {
    /// The id an answer must quote exactly.
    pub id: String,
    /// `alert`, `confirm`, `prompt` or `beforeunload`.
    pub kind: String,
    /// The page's message (page content: untrusted).
    pub message: String,
    /// The URL of the page that raised it.
    pub url: String,
    /// The prompt's default text.
    pub default_prompt: String,
    /// The document revision it was raised under.
    pub revision: String,
}

impl PendingDialog {
    /// The wire form.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "kind": self.kind,
            "message": self.message,
            "url": self.url,
            "default_prompt": self.default_prompt,
            "document_revision": self.revision,
        })
    }
}

/// Where a certificate decision stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertificateStatus {
    /// Waiting for a user.
    Pending,
    /// A user chose to continue.
    Continued,
    /// A user chose to cancel.
    Cancelled,
    /// Nobody decided in time; the request was cancelled.
    Expired,
}

impl CertificateStatus {
    /// The canonical spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Continued => "continued",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
        }
    }
}

/// A certificate error, and what was decided about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateRecord {
    /// The id a decision must quote.
    pub id: String,
    /// The browser's event id, which the answer is sent against.
    pub event_id: i64,
    /// The origin whose certificate failed.
    pub origin: String,
    /// The browser's error (`ERR_CERT_AUTHORITY_INVALID`, …).
    pub error_type: String,
    /// The URL that was being loaded.
    pub url: String,
    /// Where it stands.
    pub status: CertificateStatus,
    /// SHA-256 of the leaf certificate, once the browser revealed it (after a *continue*).
    pub certificate_sha256: Option<String>,
    /// Who decided.
    pub decided_by: Option<String>,
}

impl CertificateRecord {
    /// The wire form.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "origin": self.origin,
            "error_type": self.error_type,
            "url": self.url,
            "status": self.status.as_str(),
            "certificate_sha256": self.certificate_sha256,
            "decided_by": self.decided_by,
        })
    }
}

/// A download's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadState {
    /// Receiving.
    InProgress,
    /// Finished and kept.
    Completed,
    /// Cancelled by the driver or the page.
    Cancelled,
}

impl DownloadState {
    /// The canonical spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// A download, as a typed record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadRecord {
    /// `dl_…`.
    pub id: String,
    /// The browser's identifier.
    pub guid: String,
    /// Where it came from (page content: untrusted).
    pub url: String,
    /// The name the server suggested, sanitised (page content: untrusted).
    pub suggested_filename: String,
    /// Where it stands.
    pub state: DownloadState,
    /// Bytes received.
    pub received_bytes: u64,
    /// Bytes expected, when the server said.
    pub total_bytes: u64,
    /// The file inside the session's download directory, once complete.
    pub file: Option<String>,
    /// SHA-256 of the file, once complete.
    pub sha256: Option<String>,
    /// Why the driver cancelled it (`too_large`, `start_timeout`, `stalled`, `too_many`).
    pub failure: Option<String>,
    /// When it began.
    pub started_at: Instant,
    /// When bytes last arrived.
    pub last_progress_at: Instant,
}

impl DownloadRecord {
    /// The wire form.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "url": self.url,
            "suggested_filename": self.suggested_filename,
            "state": self.state.as_str(),
            "received_bytes": self.received_bytes,
            "total_bytes": self.total_bytes,
            "file": self.file,
            "sha256": self.sha256,
            "failure": self.failure,
        })
    }
}

/// A popup the page tried to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PopupRecord {
    /// The URL it wanted (page content: untrusted).
    pub url: String,
    /// Whether it was closed.
    pub blocked: bool,
    /// Why.
    pub reason: String,
}

/// An external-application handoff the page tried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffRecord {
    /// The scheme only: the rest of the URL is not needed to know what was attempted.
    pub scheme: String,
    /// Whether it was stopped.
    pub blocked: bool,
    /// Why.
    pub reason: String,
}

/// One console message the page wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleEntry {
    /// `log`, `warning`, `error`, …
    pub level: String,
    /// The text (page content: untrusted), bounded.
    pub text: String,
}

/// Work the connection has to do after an event was applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FollowUp {
    /// A download began: supervise its start, stall and size.
    SuperviseDownload {
        /// The browser's identifier.
        guid: String,
    },
    /// Cancel a download now.
    CancelDownload {
        /// The browser's identifier.
        guid: String,
    },
    /// A certificate decision is pending: time it out.
    CertificatePending {
        /// The record id.
        id: String,
    },
    /// The page opened a window.
    Popup {
        /// The URL it asked for.
        url: String,
    },
    /// The page navigated toward another application.
    ExternalHandoff {
        /// The URL it asked for.
        url: String,
    },
    /// The leaf certificate of an origin is known now: bind it to its decision.
    CertificateObserved {
        /// The origin.
        origin: String,
        /// SHA-256 of the leaf, hex.
        sha256: String,
    },
    /// A response promised an attachment: if no download has begun for it by the start timeout, it is
    /// cancelled and recorded as one that never started.
    AttachmentPromised {
        /// The URL the response came from.
        url: String,
        /// The suggested file name, sanitised.
        filename: String,
    },
    /// Cancel a certificate error that is not a navigation, now.
    CancelCertificate {
        /// The browser's event id.
        event_id: i64,
    },
    /// The next download was refused for being over the per-tab limit.
    Noted,
}

/// One tab's state.
#[derive(Debug)]
pub struct TabState {
    tab_id: String,
    nonce: u64,
    counter: u64,
    /// The current document revision.
    pub revision: String,
    /// The main frame's id.
    pub main_frame_id: Option<String>,
    /// The main frame's current URL.
    pub url: String,
    /// The main frame's origin as the browser reports it (`null` for opaque).
    pub main_origin: String,
    /// Frames currently loading.
    loading: BTreeSet<String>,
    /// Snapshots still valid, by id.
    pub snapshots: BTreeMap<String, Snapshot>,
    /// Captures still valid, by id.
    pub captures: BTreeMap<String, Capture>,
    /// The dialog waiting for an answer, if any.
    pub dialog: Option<PendingDialog>,
    dialog_counter: u64,
    /// Certificate errors and decisions, oldest first.
    pub certificates: Vec<CertificateRecord>,
    certificate_counter: u64,
    /// Downloads, oldest first.
    pub downloads: Vec<DownloadRecord>,
    download_counter: u64,
    /// Popups the page tried to open.
    pub popups: Vec<PopupRecord>,
    /// External handoffs the page tried.
    pub handoffs: Vec<HandoffRecord>,
    /// The page's recent console output.
    pub console: VecDeque<ConsoleEntry>,
    /// Requests in flight, by id, so a response that carries only an id can be given its URL.
    requests: BTreeMap<String, String>,
    /// URLs the tab recently navigated to (without fragment): the documents a certificate error may
    /// ask a user about.
    navigations: Vec<String>,
    /// When a genuine user last gave input to this tab.
    pub last_user_gesture: Option<Instant>,
    /// Whether the tab's connection is gone.
    pub closed: bool,
}

/// How many console entries a tab keeps.
pub const CONSOLE_KEEP: usize = 50;
/// The longest console text kept, in bytes.
const CONSOLE_TEXT_MAX: usize = 512;

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn short(input: &str, max: usize) -> String {
    if input.len() <= max {
        return input.to_string();
    }
    let mut end = max;
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    input[..end].to_string()
}

/// A download name made safe to show and to join to a path: no separators, no control characters.
#[must_use]
pub fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '\0') {
                '_'
            } else {
                c
            }
        })
        .collect();
    short(cleaned.trim_matches('.'), 120)
}

impl TabState {
    /// A fresh state for a tab.
    #[must_use]
    pub fn new(tab_id: &str, nonce: u64) -> Self {
        let mut state = Self {
            tab_id: tab_id.to_string(),
            nonce,
            counter: 0,
            revision: String::new(),
            main_frame_id: None,
            url: String::new(),
            main_origin: "null".to_string(),
            loading: BTreeSet::new(),
            snapshots: BTreeMap::new(),
            captures: BTreeMap::new(),
            dialog: None,
            dialog_counter: 0,
            certificates: Vec::new(),
            certificate_counter: 0,
            downloads: Vec::new(),
            download_counter: 0,
            popups: Vec::new(),
            handoffs: Vec::new(),
            console: VecDeque::new(),
            requests: BTreeMap::new(),
            navigations: Vec::new(),
            last_user_gesture: None,
            closed: false,
        };
        state.bump();
        state
    }

    fn token(&self, kind: &str, counter: u64) -> String {
        let digest =
            Digest::of(format!("{kind}:{}:{counter}:{}", self.tab_id, self.nonce).as_bytes());
        digest.as_str()[..16].to_string()
    }

    /// Mint a new document revision and invalidate every snapshot and capture.
    ///
    /// Called when the page changes under the model: a navigation, a frame-tree change, or events the
    /// driver lost. It is also the moment a ref stops being usable, because a ref is bound to the
    /// revision it was minted under.
    pub fn bump(&mut self) {
        self.counter += 1;
        self.revision = format!("rev_{}", self.token("rev", self.counter));
        self.snapshots.clear();
        self.captures.clear();
    }

    /// Invalidate snapshots and captures without changing the revision: the model mutated the page
    /// itself, so what it last read is no longer a description of it.
    pub fn invalidate_observations(&mut self) {
        self.snapshots.clear();
        self.captures.clear();
    }

    /// Mint an id with a prefix, unique within this tab.
    pub fn mint(&mut self, prefix: &str) -> String {
        self.counter += 1;
        format!("{prefix}_{}", &self.token(prefix, self.counter)[..12])
    }

    /// Whether any frame is still loading.
    #[must_use]
    pub fn is_loading(&self) -> bool {
        !self.loading.is_empty()
    }

    /// Whether a genuine user gave input to this tab within `window`.
    #[must_use]
    pub fn user_gesture_within(&self, window: Duration) -> bool {
        self.last_user_gesture
            .is_some_and(|at| at.elapsed() <= window)
    }

    /// The certificate record with this id.
    #[must_use]
    pub fn certificate(&self, id: &str) -> Option<&CertificateRecord> {
        self.certificates.iter().find(|record| record.id == id)
    }

    /// Record a user's decision on certificate error `subject` and on every other error still waiting
    /// for the same origin and the same fault in this tab, and return the browser event ids to answer
    /// (the subject's first). The browser raises one error per connection it opens for a navigation,
    /// so a single page load can leave two waiting; the person saw one origin and decided about it, and
    /// the others would only hold the tab until their wait ran out.
    pub fn settle_certificates(
        &mut self,
        subject: &str,
        allow: bool,
        decided_by: &str,
    ) -> Vec<i64> {
        let Some((origin, error_type)) = self
            .certificate(subject)
            .map(|record| (record.origin.clone(), record.error_type.clone()))
        else {
            return Vec::new();
        };
        let mut subject_event = None;
        let mut siblings = Vec::new();
        for record in &mut self.certificates {
            let this = record.id == subject;
            let sibling = record.status == CertificateStatus::Pending
                && record.origin == origin
                && record.error_type == error_type;
            if !this && !sibling {
                continue;
            }
            record.status = if allow {
                CertificateStatus::Continued
            } else {
                CertificateStatus::Cancelled
            };
            record.decided_by = Some(decided_by.to_string());
            if this {
                subject_event = Some(record.event_id);
            } else {
                siblings.push(record.event_id);
            }
        }
        subject_event.into_iter().chain(siblings).collect()
    }

    /// Certificate errors still waiting for a user.
    #[must_use]
    pub fn pending_certificates(&self) -> Vec<&CertificateRecord> {
        self.certificates
            .iter()
            .filter(|record| record.status == CertificateStatus::Pending)
            .collect()
    }

    /// Apply one event. Pure: it changes the state and names the follow-up work, and does no I/O.
    pub fn apply(&mut self, event: &CdpEvent, policy: &DownloadPolicy) -> Vec<FollowUp> {
        let params = &event.params;
        let mut follow = Vec::new();
        match event.method.as_str() {
            "Page.frameNavigated" => {
                let frame = params.get("frame").cloned().unwrap_or(Value::Null);
                if frame.get("parentId").is_none() {
                    self.main_frame_id =
                        frame.get("id").and_then(Value::as_str).map(str::to_string);
                    self.url = text(&frame, "url");
                    self.main_origin = frame
                        .get("securityOrigin")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or_else(|| origin_of(&self.url))
                        .unwrap_or_else(|| "null".to_string());
                }
                self.bump();
            }
            "Page.navigatedWithinDocument" => {
                if params.get("frameId").and_then(Value::as_str) == self.main_frame_id.as_deref() {
                    self.url = text(params, "url");
                }
                self.bump();
            }
            "Page.frameAttached" | "Page.frameDetached" | "Page.documentOpened" => self.bump(),
            "Page.frameStartedLoading" => {
                self.loading.insert(text(params, "frameId"));
            }
            "Page.frameStoppedLoading" => {
                self.loading.remove(&text(params, "frameId"));
            }
            "Page.javascriptDialogOpening" => {
                self.dialog_counter += 1;
                let id = format!("dlg_{}", &self.token("dlg", self.dialog_counter)[..10]);
                self.dialog = Some(PendingDialog {
                    id,
                    kind: text(params, "type"),
                    message: short(&text(params, "message"), 2048),
                    url: text(params, "url"),
                    default_prompt: short(&text(params, "defaultPrompt"), 512),
                    revision: self.revision.clone(),
                });
            }
            "Page.javascriptDialogClosed" => {
                self.dialog = None;
            }
            "Page.downloadWillBegin" => {
                let guid = text(params, "guid");
                if self.downloads.iter().any(|download| download.guid == guid) {
                    return follow;
                }
                self.download_counter += 1;
                let id = format!("dl_{}", &self.token("dl", self.download_counter)[..10]);
                let over_limit = self.downloads.len() >= policy.max_per_tab;
                let now = Instant::now();
                self.downloads.push(DownloadRecord {
                    id,
                    guid: guid.clone(),
                    url: short(&text(params, "url"), 2048),
                    suggested_filename: sanitize_filename(&text(params, "suggestedFilename")),
                    state: if over_limit {
                        DownloadState::Cancelled
                    } else {
                        DownloadState::InProgress
                    },
                    received_bytes: 0,
                    total_bytes: 0,
                    file: None,
                    sha256: None,
                    failure: over_limit.then(|| "too_many".to_string()),
                    started_at: now,
                    last_progress_at: now,
                });
                follow.push(if over_limit {
                    FollowUp::CancelDownload { guid }
                } else {
                    FollowUp::SuperviseDownload { guid }
                });
            }
            "Page.downloadProgress" | "Browser.downloadProgress" => {
                let guid = text(params, "guid");
                let received = params
                    .get("receivedBytes")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let total = params
                    .get("totalBytes")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let state = text(params, "state");
                let file = params
                    .get("filePath")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                if let Some(download) = self.downloads.iter_mut().find(|d| d.guid == guid) {
                    if received > download.received_bytes {
                        download.last_progress_at = Instant::now();
                    }
                    download.received_bytes = download.received_bytes.max(received);
                    download.total_bytes = download.total_bytes.max(total);
                    if download.state == DownloadState::InProgress {
                        if download.received_bytes > policy.max_bytes
                            || (total > policy.max_bytes && total > 0)
                        {
                            download.state = DownloadState::Cancelled;
                            download.failure = Some("too_large".to_string());
                            follow.push(FollowUp::CancelDownload { guid: guid.clone() });
                        } else if state == "completed" {
                            download.state = DownloadState::Completed;
                        } else if state == "canceled" {
                            download.state = DownloadState::Cancelled;
                            download
                                .failure
                                .get_or_insert_with(|| "cancelled_by_browser".to_string());
                        }
                    }
                    if let Some(file) = file {
                        if download.state == DownloadState::Completed {
                            download.file = Some(file);
                        }
                    }
                }
            }
            "Network.requestWillBeSent" => {
                let request_id = text(params, "requestId");
                let url = short(
                    &text(params.get("request").unwrap_or(&Value::Null), "url"),
                    2048,
                );
                if !request_id.is_empty() {
                    if self.requests.len() >= 256 {
                        self.requests.clear();
                    }
                    self.requests.insert(request_id, url);
                }
            }
            "Network.responseReceived" | "Network.responseReceivedExtraInfo" => {
                // A browser that is handed an attachment's headers and then no body never starts a
                // download, so the only evidence is the response itself. The extra-info event carries
                // the headers and the request id; the URL comes from the request that id names.
                let headers = params
                    .pointer("/response/headers")
                    .or_else(|| params.get("headers"))
                    .and_then(Value::as_object);
                let disposition = headers
                    .and_then(|headers| {
                        headers
                            .iter()
                            .find(|(name, _)| name.eq_ignore_ascii_case("content-disposition"))
                            .and_then(|(_, value)| value.as_str())
                    })
                    .unwrap_or_default();
                if disposition
                    .trim_start()
                    .to_ascii_lowercase()
                    .starts_with("attachment")
                {
                    let filename = disposition
                        .split(';')
                        .filter_map(|part| part.trim().strip_prefix("filename="))
                        .map(|name| name.trim_matches('"'))
                        .next()
                        .unwrap_or_default();
                    let url = params
                        .pointer("/response/url")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or_else(|| self.requests.get(&text(params, "requestId")).cloned());
                    if let Some(url) = url {
                        follow.push(FollowUp::AttachmentPromised {
                            url,
                            filename: sanitize_filename(filename),
                        });
                    }
                }
            }
            "Page.windowOpen" => {
                follow.push(FollowUp::Popup {
                    url: text(params, "url"),
                });
            }
            "Security.certificateError" => {
                self.certificate_counter += 1;
                let id = format!("crt_{}", &self.token("crt", self.certificate_counter)[..10]);
                let url = short(&text(params, "requestURL"), 2048);
                let origin = origin_of(&url).unwrap_or_else(|| "null".to_string());
                let event_id = params.get("eventId").and_then(Value::as_i64).unwrap_or(-1);
                // Only a *navigation* — a document the user would see — waits for a decision. The
                // browser also raises the error for requests it makes on its own (a favicon) and for
                // subresources, and a decision nobody can see the point of is one nobody can make:
                // those are cancelled, which fails closed and costs a broken icon at worst.
                let without_fragment = url.split('#').next().unwrap_or_default().to_string();
                let navigating = self
                    .navigations
                    .iter()
                    .any(|seen| seen == &without_fragment);
                self.certificates.push(CertificateRecord {
                    id: id.clone(),
                    event_id,
                    origin,
                    error_type: text(params, "errorType"),
                    url,
                    status: if navigating {
                        CertificateStatus::Pending
                    } else {
                        CertificateStatus::Cancelled
                    },
                    certificate_sha256: None,
                    decided_by: (!navigating).then(|| "driver: not a navigation".to_string()),
                });
                follow.push(if navigating {
                    FollowUp::CertificatePending { id }
                } else {
                    FollowUp::CancelCertificate { event_id }
                });
            }
            "Page.frameStartedNavigating" | "Page.frameRequestedNavigation" => {
                let url = text(params, "url");
                let without_fragment = url.split('#').next().unwrap_or_default().to_string();
                if !without_fragment.is_empty() {
                    if self.navigations.len() >= 16 {
                        self.navigations.remove(0);
                    }
                    self.navigations.push(without_fragment);
                }
                if event.method == "Page.frameRequestedNavigation" && is_external_handoff(&url) {
                    follow.push(FollowUp::ExternalHandoff { url });
                }
            }
            "Security.visibleSecurityStateChanged" => {
                let leaf = params
                    .pointer("/visibleSecurityState/certificateSecurityState/certificate/0")
                    .and_then(Value::as_str);
                if let Some(leaf) = leaf {
                    if let Some(bytes) = super::decode_base64_bytes(leaf) {
                        follow.push(FollowUp::CertificateObserved {
                            origin: origin_of(&self.url)
                                .unwrap_or_else(|| self.main_origin.clone()),
                            sha256: Digest::of(&bytes).as_str().to_string(),
                        });
                    }
                }
            }
            "Runtime.consoleAPICalled" => {
                let rendered: Vec<String> = params
                    .get("args")
                    .and_then(Value::as_array)
                    .map(|args| {
                        args.iter()
                            .map(|arg| {
                                arg.get("value")
                                    .map(|value| match value {
                                        Value::String(text) => text.clone(),
                                        other => other.to_string(),
                                    })
                                    .or_else(|| {
                                        arg.get("description")
                                            .and_then(Value::as_str)
                                            .map(str::to_string)
                                    })
                                    .unwrap_or_default()
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                self.push_console(&text(params, "type"), &rendered.join(" "));
            }
            "Runtime.exceptionThrown" => {
                let description = params
                    .pointer("/exceptionDetails/exception/description")
                    .and_then(Value::as_str)
                    .or_else(|| {
                        params
                            .pointer("/exceptionDetails/text")
                            .and_then(Value::as_str)
                    })
                    .unwrap_or("uncaught exception");
                self.push_console("exception", description);
            }
            "Inspector.detached" | "Target.targetCrashed" | "Inspector.targetCrashed" => {
                self.closed = true;
            }
            _ => {}
        }
        follow
    }

    fn push_console(&mut self, level: &str, body: &str) {
        if self.console.len() >= CONSOLE_KEEP {
            self.console.pop_front();
        }
        self.console.push_back(ConsoleEntry {
            level: level.to_string(),
            text: short(body, CONSOLE_TEXT_MAX),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(method: &str, params: Value) -> CdpEvent {
        CdpEvent {
            method: method.to_string(),
            params,
        }
    }

    #[test]
    fn a_navigation_changes_the_revision_and_drops_observations() {
        let mut state = TabState::new("TAB", 7);
        let first = state.revision.clone();
        state.snapshots.insert(
            "snap_1".into(),
            Snapshot {
                id: "snap_1".into(),
                tab_id: "TAB".into(),
                revision: first.clone(),
                refs: BTreeMap::new(),
            },
        );
        state.apply(
            &event(
                "Page.frameNavigated",
                json!({"frame": {"id": "F", "url": "http://a.example/x", "securityOrigin": "http://a.example"}}),
            ),
            &DownloadPolicy::default(),
        );
        assert_ne!(state.revision, first);
        assert!(state.snapshots.is_empty());
        assert_eq!(state.main_origin, "http://a.example");
        assert_eq!(state.url, "http://a.example/x");
    }

    #[test]
    fn a_dialog_is_pending_until_it_closes() {
        let mut state = TabState::new("TAB", 7);
        state.apply(
            &event(
                "Page.javascriptDialogOpening",
                json!({"type": "confirm", "message": "Sure?", "url": "http://a.example/"}),
            ),
            &DownloadPolicy::default(),
        );
        let dialog = state.dialog.clone().expect("pending");
        assert_eq!(dialog.kind, "confirm");
        assert_eq!(dialog.revision, state.revision);
        state.apply(
            &event("Page.javascriptDialogClosed", json!({"result": true})),
            &DownloadPolicy::default(),
        );
        assert!(state.dialog.is_none());
    }

    #[test]
    fn a_download_over_the_cap_is_cancelled_and_named_so() {
        let policy = DownloadPolicy {
            max_bytes: 100,
            ..DownloadPolicy::default()
        };
        let mut state = TabState::new("TAB", 7);
        let follow = state.apply(
            &event(
                "Page.downloadWillBegin",
                json!({"guid": "g1", "url": "http://a.example/f", "suggestedFilename": "../../etc/passwd"}),
            ),
            &policy,
        );
        assert_eq!(
            follow,
            vec![FollowUp::SuperviseDownload { guid: "g1".into() }]
        );
        assert!(!state.downloads[0].suggested_filename.contains('/'));
        let follow = state.apply(
            &event(
                "Page.downloadProgress",
                json!({"guid": "g1", "receivedBytes": 500, "totalBytes": 0, "state": "inProgress"}),
            ),
            &policy,
        );
        assert_eq!(follow, vec![FollowUp::CancelDownload { guid: "g1".into() }]);
        assert_eq!(state.downloads[0].failure.as_deref(), Some("too_large"));
    }

    #[test]
    fn a_certificate_error_on_a_navigation_is_pending_and_one_on_a_subresource_is_cancelled() {
        let mut state = TabState::new("TAB", 7);
        // The browser's own favicon request is not a navigation: nobody can decide it, so it is cancelled.
        let follow = state.apply(
            &event(
                "Security.certificateError",
                json!({"eventId": 1, "errorType": "ERR_CERT_AUTHORITY_INVALID", "requestURL": "https://127.0.0.1:8443/favicon.ico"}),
            ),
            &DownloadPolicy::default(),
        );
        assert_eq!(follow, vec![FollowUp::CancelCertificate { event_id: 1 }]);
        assert_eq!(state.certificates[0].status, CertificateStatus::Cancelled);
        assert!(state.pending_certificates().is_empty());
        // A navigation to the same origin does wait.
        state.apply(
            &event(
                "Page.frameStartedNavigating",
                json!({"frameId": "F", "url": "https://127.0.0.1:8443/#top"}),
            ),
            &DownloadPolicy::default(),
        );
        let follow = state.apply(
            &event(
                "Security.certificateError",
                json!({"eventId": 3, "errorType": "ERR_CERT_AUTHORITY_INVALID", "requestURL": "https://127.0.0.1:8443/"}),
            ),
            &DownloadPolicy::default(),
        );
        assert!(matches!(follow[0], FollowUp::CertificatePending { .. }));
        let record = &state.certificates[1];
        assert_eq!(record.origin, "https://127.0.0.1:8443");
        assert_eq!(record.status, CertificateStatus::Pending);
        assert_eq!(state.pending_certificates().len(), 1);
    }

    #[test]
    fn a_decision_covers_the_other_errors_waiting_for_the_same_origin_and_fault() {
        let mut state = TabState::new("TAB", 7);
        state.apply(
            &event(
                "Page.frameStartedNavigating",
                json!({"frameId": "F", "url": "https://127.0.0.1:8443/"}),
            ),
            &DownloadPolicy::default(),
        );
        state.apply(
            &event(
                "Page.frameStartedNavigating",
                json!({"frameId": "F", "url": "https://other.example/"}),
            ),
            &DownloadPolicy::default(),
        );
        for (event_id, url, error) in [
            (10, "https://127.0.0.1:8443/", "ERR_CERT_AUTHORITY_INVALID"),
            (11, "https://127.0.0.1:8443/", "ERR_CERT_AUTHORITY_INVALID"),
            (12, "https://127.0.0.1:8443/", "ERR_CERT_DATE_INVALID"),
            (13, "https://other.example/", "ERR_CERT_AUTHORITY_INVALID"),
        ] {
            state.apply(
                &event(
                    "Security.certificateError",
                    json!({"eventId": event_id, "errorType": error, "requestURL": url}),
                ),
                &DownloadPolicy::default(),
            );
        }
        assert_eq!(state.pending_certificates().len(), 4);
        let first = state.certificates[0].id.clone();
        assert_eq!(
            state.settle_certificates(&first, true, "usr_X"),
            vec![10, 11]
        );
        let statuses: Vec<_> = state.certificates.iter().map(|r| r.status).collect();
        assert_eq!(
            statuses,
            vec![
                CertificateStatus::Continued,
                CertificateStatus::Continued,
                CertificateStatus::Pending,
                CertificateStatus::Pending,
            ],
            "another fault and another origin keep waiting"
        );
        assert_eq!(state.certificates[1].decided_by.as_deref(), Some("usr_X"));
        assert!(state
            .settle_certificates("crt_unknown", true, "usr_X")
            .is_empty());
    }

    #[test]
    fn a_window_and_an_application_handoff_are_follow_ups() {
        let mut state = TabState::new("TAB", 7);
        let follow = state.apply(
            &event("Page.windowOpen", json!({"url": "http://a.example/two"})),
            &DownloadPolicy::default(),
        );
        assert_eq!(
            follow,
            vec![FollowUp::Popup {
                url: "http://a.example/two".into()
            }]
        );
        let follow = state.apply(
            &event(
                "Page.frameRequestedNavigation",
                json!({"url": "mailto:a@b.c", "reason": "anchorClick"}),
            ),
            &DownloadPolicy::default(),
        );
        assert!(matches!(follow[0], FollowUp::ExternalHandoff { .. }));
        let none = state.apply(
            &event(
                "Page.frameRequestedNavigation",
                json!({"url": "https://a.example/", "reason": "anchorClick"}),
            ),
            &DownloadPolicy::default(),
        );
        assert!(none.is_empty());
    }

    #[test]
    fn filenames_lose_their_separators() {
        assert_eq!(sanitize_filename("../../a/b\\c.txt"), "_.._a_b_c.txt");
        assert!(!sanitize_filename("a\nb").contains('\n'));
    }
}
