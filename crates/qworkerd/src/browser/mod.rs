//! The managed browser session: DOM/CDP-first control, with vision as the fallback (EXEC-009,
//! DOMAIN.md §8.4).
//!
//! One browser is exposed to the agent and to the user, and it is driven through CDP rather than through
//! images. That ordering is the point of the module: understanding a page means reading its DOM, its
//! accessibility tree and its network metadata, and a screenshot is taken **only** when those say
//! nothing — a canvas, an image-only page — because a screenshot cannot say what a control *does*.
//!
//! Two invariants are enforced here rather than trusted:
//!
//! * **consequence is classified from page semantics, not from the fact that a click happened.** A click
//!   on a link, a click that submits a purchase and a click that deletes a record are three different
//!   effect classes at three different tiers, and the class is derived from the control's role, its
//!   accessible name and the form it belongs to. A consequential action therefore cannot slip into the
//!   ledger as an unclassified `browser.click`.
//! * **control is held by one party at a time.** A takeover moves control to the user and fences agent
//!   input; a handback returns it; no second session is created for a takeover (§8.4), which is why the
//!   rule lives on the session rather than in a new one.
//!
//! The session *row* is not written here. `crates/qworkerd` may not hold a Postgres client, so
//! `browser_sessions` is owned by machine control and this module carries the session state the worker
//! is given and reports back.

pub mod cdp;

use std::collections::BTreeMap;

use serde_json::{json, Value};

pub use cdp::{CdpConnection, CdpError, CdpEvent, PageTarget};

/// A default bound on the readable text an observation extracts.
pub const DEFAULT_MAX_TEXT_BYTES: usize = 262_144;

/// A default bound on the links an observation returns.
pub const DEFAULT_MAX_LINKS: usize = 512;

/// Below this much readable text, a page is treated as one whose meaning is visual and a screenshot is
/// taken as well. It is a threshold on *text*, not on size: a page can be large and say nothing.
pub const VISUAL_ONLY_TEXT_THRESHOLD: usize = 32;

/// Who is driving the session (the schema's `browser_sessions.control_holder`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlHolder {
    /// The agent.
    Agent,
    /// The user, after a takeover. Agent input is fenced while this holds.
    User,
    /// Nobody: the session is not being driven.
    None,
}

impl ControlHolder {
    /// Canonical column value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::User => "user",
            Self::None => "none",
        }
    }

    /// Parse the canonical column value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "agent" => Some(Self::Agent),
            "user" => Some(Self::User),
            "none" => Some(Self::None),
            _ => None,
        }
    }
}

/// A session's lifecycle (the schema's `browser_sessions.status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    /// Usable.
    Active,
    /// The user has taken over; agent input is fenced.
    PausedTakeover,
    /// Paused by policy rather than by a person.
    PausedPolicy,
    /// Closed.
    Closed,
}

impl SessionStatus {
    /// Canonical column value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::PausedTakeover => "paused_takeover",
            Self::PausedPolicy => "paused_policy",
            Self::Closed => "closed",
        }
    }

    /// Parse the canonical column value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "paused_takeover" => Some(Self::PausedTakeover),
            "paused_policy" => Some(Self::PausedPolicy),
            "closed" => Some(Self::Closed),
            _ => None,
        }
    }
}

/// One open tab, as the session records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tab {
    /// The CDP target id.
    pub target_id: String,
    /// Its url.
    pub url: String,
    /// Its title.
    pub title: String,
}

/// Whether the session's frames are being streamed, and under what reference.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Screencast {
    /// Whether it is on.
    pub enabled: bool,
    /// The stream reference a client reads frames from.
    pub stream_ref: Option<String>,
}

/// Why a browser operation failed.
#[derive(Debug, thiserror::Error)]
pub enum BrowserError {
    /// The protocol call failed.
    #[error("{0}")]
    Cdp(#[from] CdpError),
    /// The session is not usable.
    #[error("browser session {id} is {status}")]
    SessionNotUsable {
        /// The session.
        id: String,
        /// Its status.
        status: &'static str,
    },
    /// The agent tried to drive a session the user holds.
    #[error("browser session {id} is held by {holder}; agent input is fenced")]
    InputFenced {
        /// The session.
        id: String,
        /// Who holds it.
        holder: &'static str,
    },
    /// Nothing on the page matched.
    #[error("no element matched {selector:?}")]
    NoSuchElement {
        /// What was looked for.
        selector: String,
    },
    /// The page's own script refused the operation.
    #[error("the page refused the operation: {0}")]
    Page(String),
}

/// A browser session's state (§8.4), as the worker is given it and reports it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserSession {
    /// `bsn_…` identity.
    pub id: String,
    /// The tenant that owns it.
    pub tenant_id: String,
    /// The execution target it runs on.
    pub target_id: String,
    /// The run it belongs to, when it belongs to one.
    pub run_id: Option<String>,
    /// The persistent browser profile it uses, when it has one.
    pub profile_ref: Option<String>,
    /// Who is driving.
    pub control_holder: ControlHolder,
    /// When control last changed hands.
    pub control_since: Option<String>,
    /// The page the session is on.
    pub current_url: Option<String>,
    /// Its open tabs.
    pub tabs: Vec<Tab>,
    /// Whether frames are streamed.
    pub screencast: Screencast,
    /// The checkpoint it was last saved from.
    pub checkpoint_id: Option<String>,
    /// Its lifecycle state.
    pub status: SessionStatus,
}

impl BrowserSession {
    /// Open a session on a target, with the agent holding control.
    #[must_use]
    pub fn open(
        id: impl Into<String>,
        tenant_id: impl Into<String>,
        target_id: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            tenant_id: tenant_id.into(),
            target_id: target_id.into(),
            run_id: None,
            profile_ref: None,
            control_holder: ControlHolder::Agent,
            control_since: None,
            current_url: None,
            tabs: Vec::new(),
            screencast: Screencast::default(),
            checkpoint_id: None,
            status: SessionStatus::Active,
        }
    }

    /// Whether the agent may drive the session right now.
    #[must_use]
    pub const fn agent_may_drive(&self) -> bool {
        matches!(self.status, SessionStatus::Active)
            && matches!(self.control_holder, ControlHolder::Agent)
    }

    /// Whether a user may take over: an active session that a person does not already hold.
    #[must_use]
    pub const fn takeover_allowed(&self) -> bool {
        matches!(self.status, SessionStatus::Active)
            && !matches!(self.control_holder, ControlHolder::User)
    }

    /// Take control as the user.
    ///
    /// The session is *paused*, not duplicated: §8.4 is explicit that a takeover fences agent input
    /// rather than creating a second session, so the tab, the profile and the page all stay.
    ///
    /// # Errors
    /// Returns [`BrowserError::SessionNotUsable`] for a session that is not active.
    pub fn request_takeover(&mut self, at: &str) -> Result<(), BrowserError> {
        if !matches!(self.status, SessionStatus::Active) {
            return Err(BrowserError::SessionNotUsable {
                id: self.id.clone(),
                status: self.status.as_str(),
            });
        }
        self.control_holder = ControlHolder::User;
        self.control_since = Some(at.to_string());
        self.status = SessionStatus::PausedTakeover;
        Ok(())
    }

    /// Return control to the agent.
    ///
    /// # Errors
    /// Returns [`BrowserError::InputFenced`] when the user does not hold it, so a handback cannot be used
    /// to resume a session that was paused by policy.
    pub fn handback(&mut self, at: &str) -> Result<(), BrowserError> {
        if !matches!(self.control_holder, ControlHolder::User) {
            return Err(BrowserError::InputFenced {
                id: self.id.clone(),
                holder: self.control_holder.as_str(),
            });
        }
        self.control_holder = ControlHolder::Agent;
        self.control_since = Some(at.to_string());
        self.status = SessionStatus::Active;
        Ok(())
    }

    /// Pause the session for a policy reason. No handback resumes this: policy decides.
    pub fn pause_for_policy(&mut self, at: &str) {
        self.control_holder = ControlHolder::None;
        self.control_since = Some(at.to_string());
        self.status = SessionStatus::PausedPolicy;
    }

    /// Close the session.
    pub fn close(&mut self, at: &str) {
        self.control_holder = ControlHolder::None;
        self.control_since = Some(at.to_string());
        self.screencast = Screencast::default();
        self.status = SessionStatus::Closed;
    }

    /// Record the page the session is on, and keep the tab list in step with it.
    pub fn observe_page(&mut self, target_id: &str, url: &str, title: &str) {
        self.current_url = Some(url.to_string());
        let tab = Tab {
            target_id: target_id.to_string(),
            url: url.to_string(),
            title: title.to_string(),
        };
        match self
            .tabs
            .iter_mut()
            .find(|open| open.target_id == target_id)
        {
            Some(open) => *open = tab,
            None => self.tabs.push(tab),
        }
    }

    /// Refuse to drive a session the agent does not hold.
    ///
    /// # Errors
    /// Returns [`BrowserError::InputFenced`] when a person holds the session, and
    /// [`BrowserError::SessionNotUsable`] when it is paused by policy or closed.
    pub fn require_agent_control(&self) -> Result<(), BrowserError> {
        if matches!(self.control_holder, ControlHolder::User) {
            return Err(BrowserError::InputFenced {
                id: self.id.clone(),
                holder: ControlHolder::User.as_str(),
            });
        }
        if self.agent_may_drive() {
            return Ok(());
        }
        Err(BrowserError::SessionNotUsable {
            id: self.id.clone(),
            status: self.status.as_str(),
        })
    }
}

/// How far a piece of page content may be trusted (DOMAIN.md §12). A page fetched from the web is
/// always `UNTRUSTED_EXTERNAL`: it may contain instructions, and they are data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustLabel {
    /// Content that arrived from outside the trust boundary.
    UntrustedExternal,
}

impl TrustLabel {
    /// Canonical label, as the trust classifier and the policy engine spell it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UntrustedExternal => "UNTRUSTED_EXTERNAL",
        }
    }
}

/// A link the page exposes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// Its text, when it has any.
    pub text: String,
    /// The href as written.
    pub href: String,
}

/// The result of a headless fetch: what the page said, bounded, labelled and digestible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebFetch {
    /// The url that was asked for.
    pub requested_url: String,
    /// The url the page ended up on, which differs when the request redirected.
    pub url: String,
    /// When it was retrieved (canonical ISO-8601 UTC).
    pub retrieved_at: String,
    /// The page title.
    pub title: String,
    /// The readable text, bounded.
    pub text: String,
    /// The links, bounded.
    pub links: Vec<Link>,
    /// Page metadata (description, canonical, …).
    pub metadata: BTreeMap<String, String>,
    /// SHA-256 of the extracted content, so a caller can tell a changed page from a re-fetch.
    pub content_digest: String,
    /// Whether the text was cut to the bound.
    pub truncated: bool,
    /// How much text the page had, whether or not it was kept.
    pub total_text_bytes: usize,
    /// How many links the page had, whether or not they were kept.
    pub total_links: usize,
    /// Always `UNTRUSTED_EXTERNAL` for a fetched page.
    pub trust: TrustLabel,
}

/// What the DOM said about a page, before any screenshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomUnderstanding {
    /// Bytes of readable text.
    pub text_bytes: usize,
    /// Elements visited.
    pub node_count: usize,
    /// Accessibility-tree nodes that name something. Chrome always emits structural nodes for the
    /// document and body, so the count of *named* nodes is what says whether the page has content.
    pub named_accessibility_nodes: usize,
    /// Links found.
    pub links: usize,
    /// Forms found.
    pub forms: usize,
}

/// Why a screenshot was taken as well as, or instead of, reading the DOM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenshotReason {
    /// The DOM said nothing useful, so the page's meaning is visual.
    DomUninformative,
    /// The caller asked for one.
    Requested,
}

/// How a page was understood.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Understanding {
    /// The DOM, accessibility tree and metadata were enough.
    Dom(DomUnderstanding),
    /// They were not, and a screenshot carries the meaning.
    Visual {
        /// What the DOM did say, for the record.
        dom: DomUnderstanding,
        /// Why the screenshot was needed.
        reason: ScreenshotReason,
    },
}

impl Understanding {
    /// Whether this understanding came from the DOM alone.
    #[must_use]
    pub const fn is_dom_first(&self) -> bool {
        matches!(self, Self::Dom(_))
    }
}

/// What was seen about a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageObservation {
    /// The url as it stands now.
    pub url: String,
    /// The title.
    pub title: String,
    /// The readable text.
    pub text: String,
    /// The links.
    pub links: Vec<Link>,
    /// How it was understood.
    pub understanding: Understanding,
    /// Screenshot bytes, when one was taken.
    pub screenshot: Option<Vec<u8>>,
    /// The label the content carries.
    pub trust: TrustLabel,
}

/// A control the agent wants to act on, as the DOM describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Control {
    /// Its ARIA role, or the tag it was inferred from (`button`, `link`, `tab`, …).
    pub role: String,
    /// Its accessible name: what a person would call it.
    pub name: String,
    /// The url it navigates to, for a link.
    pub href: Option<String>,
    /// The form it belongs to, when it belongs to one.
    pub form: Option<FormSemantics>,
}

/// What a form does, derived from what it contains and what it is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormSemantics {
    /// The action the form posts to, when it names one.
    pub action: Option<String>,
    /// The method.
    pub method: Option<String>,
    /// The input types present (`password`, `file`, `email`, …).
    pub input_types: Vec<String>,
    /// The field names present.
    pub field_names: Vec<String>,
}

/// What an action would do to the world (DOMAIN.md §7.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classified {
    /// The effect class it settles, or `None` when it has no external consequence and therefore no
    /// `EffectRecord`. Nothing is recorded for a click that only changes the page's own state.
    pub effect_class: Option<&'static str>,
    /// Its consequence tier, when it has a class.
    pub tier: Option<u8>,
    /// Why, in the words the audit log will carry.
    pub reason: String,
}

impl Classified {
    /// Whether this action settles an effect.
    #[must_use]
    pub const fn is_consequential(&self) -> bool {
        self.effect_class.is_some()
    }

    fn none(reason: impl Into<String>) -> Self {
        Self {
            effect_class: None,
            tier: None,
            reason: reason.into(),
        }
    }

    fn of(effect_class: &'static str, tier: u8, reason: impl Into<String>) -> Self {
        Self {
            effect_class: Some(effect_class),
            tier: Some(tier),
            reason: reason.into(),
        }
    }
}

/// The words that make a control consequential, matched case-insensitively against its accessible name.
///
/// These are the page's own semantics rather than a guess: the accessible name is what the control tells
/// a screen reader it does, so it is the most faithful statement of intent a page offers.
const SENDS: [&str; 8] = [
    "send", "submit", "post", "reply", "comment", "publish", "share", "invite",
];
const DELETES: [&str; 6] = [
    "delete",
    "remove",
    "destroy",
    "revoke",
    "cancel subscription",
    "unsubscribe",
];
const BUYS: [&str; 6] = ["buy", "purchase", "pay", "checkout", "subscribe", "order"];
const SIGNS: [&str; 5] = ["sign in", "log in", "login", "authorize", "grant access"];

/// Classify a navigation.
#[must_use]
pub fn classify_navigation(scheme: &str, host: &str) -> Classified {
    if !matches!(scheme, "http" | "https") {
        return Classified::none(format!(
            "{scheme} is not a network destination the ledger records"
        ));
    }
    Classified::of(
        "network.egress.new_destination",
        2,
        format!("navigating to {host} is egress to a new destination"),
    )
}

/// Classify reading the page: a `web.fetch`, an extract or a screenshot.
#[must_use]
pub fn classify_read(kind: &str) -> Classified {
    Classified::of(
        "read.external",
        0,
        format!("{kind} reads the page and changes nothing outside it"),
    )
}

/// Classify typing without submitting: the page's own state changes and nothing leaves the target.
#[must_use]
pub fn classify_input() -> Classified {
    Classified::none("input changes the page's own state and sends nothing")
}

/// Classify acting on a control, from its role, its accessible name, its href and its form.
///
/// The order matters: a control inside a form is judged by what the form does and what the control says,
/// because a button labelled "Continue" inside a checkout submits a purchase.
#[must_use]
pub fn classify_control(control: &Control) -> Classified {
    let name = control.name.to_lowercase();
    let role = control.role.to_lowercase();

    // A link that navigates is egress, and is the page's own business only when it stays on the page.
    if role == "link" || role == "a" {
        match control.href.as_deref() {
            Some(href) if href.starts_with('#') => {
                return Classified::none("an in-page anchor navigates nowhere")
            }
            Some(href) => {
                if let Some((scheme, rest)) = href.split_once("://") {
                    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
                    return classify_navigation(&scheme.to_lowercase(), host);
                }
                return Classified::none("a relative link stays within the site being read");
            }
            None => return Classified::none("a link with no target navigates nowhere"),
        }
    }

    // Money, deletion and disclosure outrank the generic form case: they are what the tier ladder is for.
    if BUYS.iter().any(|word| name.contains(word)) {
        return Classified::of("payment.execute", 4, format!("{name:?} executes a payment"));
    }
    if DELETES.iter().any(|word| name.contains(word)) {
        return Classified::of("record.delete", 3, format!("{name:?} deletes a record"));
    }
    if SIGNS.iter().any(|word| name.contains(word)) {
        return Classified::of(
            "credential.access",
            4,
            format!("{name:?} authenticates or grants access"),
        );
    }
    if SENDS.iter().any(|word| name.contains(word)) {
        return Classified::of(
            "message.send",
            3,
            format!("{name:?} sends content outside the target"),
        );
    }

    match &control.form {
        Some(form) => {
            if form.input_types.iter().any(|kind| kind == "file") {
                return Classified::of(
                    "content.upload",
                    1,
                    "the form uploads a file from the target".to_string(),
                );
            }
            if form.input_types.iter().any(|kind| kind == "password") {
                return Classified::of(
                    "credential.access",
                    4,
                    "the form submits a credential".to_string(),
                );
            }
            Classified::of(
                "record.create",
                2,
                "the form writes to the site it is posted to",
            )
        }
        // A control that neither navigates nor belongs to a form changes the page and nothing else --
        // a tab, an accordion, a menu. There is no effect for the ledger to settle.
        None => Classified::none(format!("{name:?} acts on the page itself")),
    }
}

/// The script an observation runs in the page to read its DOM, accessibility tree and metadata.
///
/// It is a `Runtime.evaluate` with `returnByValue`, so the browser does the reading and the worker gets
/// data — no second parser and no separate scraper runtime, which is what "the same browser stack" means.
const EXTRACT_SCRIPT: &str = r#"
(() => {
  const absolute = (href) => { try { return new URL(href, document.baseURI).href; } catch (e) { return href; } };
  const text = (document.body ? document.body.innerText : "") || "";
  const links = Array.from(document.querySelectorAll("a[href]")).slice(0, 5000).map((a) => ({
    text: (a.innerText || a.textContent || "").trim().slice(0, 512),
    href: absolute(a.getAttribute("href")),
  }));
  const formEls = Array.from(document.querySelectorAll("form"));
  const forms = formEls.map((f) => ({
    action: f.getAttribute("action") ? absolute(f.getAttribute("action")) : null,
    method: f.getAttribute("method"),
    input_types: Array.from(f.querySelectorAll("input")).map((i) => (i.type || "text").toLowerCase()),
    field_names: Array.from(f.querySelectorAll("input,select,textarea")).map((i) => i.name || "").filter(Boolean),
  }));
  const controls = Array.from(document.querySelectorAll("a[href],button,input[type=submit],input[type=button],[role=button],[role=link]"))
    .slice(0, 2000)
    .map((el) => {
      const form = el.closest("form");
      const index = form ? formEls.indexOf(form) : -1;
      return {
        role: (el.getAttribute("role") || el.tagName || "").toLowerCase(),
        name: (el.getAttribute("aria-label") || el.innerText || el.textContent || el.value || el.getAttribute("title") || "").trim().slice(0, 256),
        href: el.getAttribute("href") ? absolute(el.getAttribute("href")) : null,
        form_index: index,
      };
    });
  const meta = {};
  document.querySelectorAll("meta[name],meta[property]").forEach((m) => {
    const key = m.getAttribute("name") || m.getAttribute("property");
    if (key) meta[key] = (m.getAttribute("content") || "").slice(0, 1024);
  });
  const canonical = document.querySelector("link[rel=canonical]");
  return {
    url: document.location.href,
    title: document.title || "",
    text,
    links,
    forms,
    controls,
    metadata: meta,
    canonical: canonical ? absolute(canonical.getAttribute("href")) : null,
    ready_state: document.readyState,
  };
})()
"#;

/// What the extraction returned, before it is typed.
async fn extract(connection: &CdpConnection) -> Result<Value, BrowserError> {
    let result = connection
        .call(
            "Runtime.evaluate",
            json!({
                "expression": EXTRACT_SCRIPT,
                "returnByValue": true,
                "awaitPromise": true,
            }),
        )
        .await?;
    // A page that threw still answered, and its exception is a page failure rather than a protocol one.
    if let Some(details) = result.get("exceptionDetails") {
        return Err(BrowserError::Page(
            details
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("the page threw while being read")
                .to_string(),
        ));
    }
    result
        .get("result")
        .and_then(|inner| inner.get("value"))
        .cloned()
        .ok_or_else(|| BrowserError::Page("the page returned no value".to_string()))
}

/// A page's controls, as the DOM described them, with their forms resolved.
fn controls_from(extracted: &Value) -> Vec<Control> {
    let forms: Vec<FormSemantics> = extracted
        .get("forms")
        .and_then(Value::as_array)
        .map(|forms| {
            forms
                .iter()
                .map(|form| FormSemantics {
                    action: form
                        .get("action")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    method: form
                        .get("method")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    input_types: strings(form.get("input_types")),
                    field_names: strings(form.get("field_names")),
                })
                .collect()
        })
        .unwrap_or_default();
    extracted
        .get("controls")
        .and_then(Value::as_array)
        .map(|controls| {
            controls
                .iter()
                .map(|control| {
                    let index = control
                        .get("form_index")
                        .and_then(Value::as_i64)
                        .unwrap_or(-1);
                    Control {
                        role: control
                            .get("role")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        name: control
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        href: control
                            .get("href")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        form: usize::try_from(index)
                            .ok()
                            .and_then(|index| forms.get(index).cloned()),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// How many accessibility nodes name something a reader could act on.
///
/// Structural roles carry no meaning: a document, a generic container and an ignored node are scaffolding
/// rather than content. A node counts when the browser gave it a non-empty accessible name.
fn named_accessibility_nodes(tree: &Value) -> usize {
    const STRUCTURAL: [&str; 4] = ["RootWebArea", "none", "generic", "Ignored"];
    tree.get("nodes")
        .and_then(Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .filter(|node| {
                    if node
                        .get("ignored")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                    {
                        return false;
                    }
                    let role = node
                        .get("role")
                        .and_then(|role| role.get("value"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if STRUCTURAL.contains(&role) {
                        return false;
                    }
                    node.get("name")
                        .and_then(|name| name.get("value"))
                        .and_then(Value::as_str)
                        .map(|name| !name.trim().is_empty())
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0)
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn links_from(extracted: &Value) -> Vec<Link> {
    extracted
        .get("links")
        .and_then(Value::as_array)
        .map(|links| {
            links
                .iter()
                .map(|link| Link {
                    text: link
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    href: link
                        .get("href")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Bounds the session's readable text and links, digesting what it returns.
fn bound(
    extracted: &Value,
    max_text_bytes: usize,
    max_links: usize,
) -> (String, bool, usize, Vec<Link>, usize) {
    let text = extracted
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let total_text_bytes = text.len();
    let truncated = total_text_bytes > max_text_bytes;
    let kept = if truncated {
        // Cut on a character boundary: a byte slice could split a multi-byte character and make the
        // output invalid UTF-8.
        let mut end = max_text_bytes;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        text[..end].to_string()
    } else {
        text
    };
    let all_links = links_from(extracted);
    let total_links = all_links.len();
    let links: Vec<Link> = all_links.into_iter().take(max_links).collect();
    (kept, truncated, total_text_bytes, links, total_links)
}

/// Read a page's DOM, accessibility tree and metadata, and decide whether a screenshot is needed.
///
/// # Errors
/// Returns [`BrowserError::Cdp`] when the browser does not answer, and [`BrowserError::Page`] when the
/// page refuses to be read.
pub async fn observe(
    connection: &CdpConnection,
    max_text_bytes: usize,
    max_links: usize,
    force_screenshot: bool,
) -> Result<PageObservation, BrowserError> {
    let extracted = extract(connection).await?;
    let url = extracted
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or(connection.current_url())
        .to_string();
    let title = extracted
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let (text, _, total_text_bytes, links, total_links) =
        bound(&extracted, max_text_bytes, max_links);
    let controls = controls_from(&extracted);

    // The accessibility tree is the second DOM-first signal: a page whose text is empty but whose tree is
    // rich is still understood without an image, and a page with neither is visual.
    let named_accessibility_nodes = match connection
        .call("Accessibility.getFullAXTree", json!({}))
        .await
    {
        Ok(tree) => named_accessibility_nodes(&tree),
        // An accessibility tree that cannot be read is not a reason to fail an observation.
        Err(_) => 0,
    };

    let dom = DomUnderstanding {
        text_bytes: total_text_bytes,
        node_count: controls.len(),
        named_accessibility_nodes,
        links: total_links,
        forms: extracted
            .get("forms")
            .and_then(Value::as_array)
            .map(|forms| forms.len())
            .unwrap_or(0),
    };
    // A page is visual when it says too little in text *and* names nothing: an image-only page has
    // structural nodes but no named ones, and a page with a heading or a link does not.
    let uninformative =
        total_text_bytes < VISUAL_ONLY_TEXT_THRESHOLD && named_accessibility_nodes == 0;
    let (understanding, screenshot) = if force_screenshot || uninformative {
        let reason = if force_screenshot {
            ScreenshotReason::Requested
        } else {
            ScreenshotReason::DomUninformative
        };
        let bytes = screenshot(connection).await?;
        (Understanding::Visual { dom, reason }, Some(bytes))
    } else {
        (Understanding::Dom(dom), None)
    };

    Ok(PageObservation {
        url,
        title,
        text,
        links,
        understanding,
        screenshot,
        trust: TrustLabel::UntrustedExternal,
    })
}

/// Capture the page as a PNG.
///
/// # Errors
/// Returns [`BrowserError::Cdp`] when the browser does not answer.
pub async fn screenshot(connection: &CdpConnection) -> Result<Vec<u8>, BrowserError> {
    let result = connection
        .call("Page.captureScreenshot", json!({ "format": "png" }))
        .await?;
    let encoded = result
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| BrowserError::Page("the screenshot came back empty".to_string()))?;
    decode_base64(encoded)
}

/// Decode base64 without a dependency: CDP returns screenshots that way.
fn decode_base64(input: &str) -> Result<Vec<u8>, BrowserError> {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut lookup = [255u8; 256];
    for (index, byte) in alphabet.iter().enumerate() {
        lookup[*byte as usize] = index as u8;
    }
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for byte in input.bytes() {
        if byte == b'=' || byte == b'\n' || byte == b'\r' {
            continue;
        }
        let value = lookup[byte as usize];
        if value == 255 {
            return Err(BrowserError::Page(
                "the screenshot was not base64".to_string(),
            ));
        }
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    Ok(out)
}

/// Navigate the page and wait for it to settle.
///
/// # Errors
/// Returns [`BrowserError::Cdp`] when the browser does not answer.
pub async fn navigate(connection: &CdpConnection, url: &str) -> Result<(), BrowserError> {
    connection.call("Page.enable", json!({})).await?;
    connection
        .call("Page.navigate", json!({ "url": url }))
        .await?;
    // A page that never fires the load event is settled by the deadline rather than by waiting forever.
    let _ = connection
        .wait_for_event("Page.loadEventFired", cdp::CALL_TIMEOUT)
        .await;
    Ok(())
}

/// Everything one action needs to be classified: the control it targets, on the page as it is now.
///
/// # Errors
/// Returns [`BrowserError::Cdp`] when the browser does not answer, and [`BrowserError::NoSuchElement`]
/// when nothing matches.
pub async fn classify_click(
    connection: &CdpConnection,
    selector: &str,
) -> Result<(Classified, Control), BrowserError> {
    let extracted = extract(connection).await?;
    let controls = controls_from(&extracted);
    let index = find_control_index(connection, selector).await?;
    let control = controls.get(index).cloned().unwrap_or(Control {
        role: String::new(),
        name: String::new(),
        href: None,
        form: None,
    });
    Ok((classify_control(&control), control))
}

/// Which control in the extracted list a selector refers to, by asking the page for its index.
async fn find_control_index(
    connection: &CdpConnection,
    selector: &str,
) -> Result<usize, BrowserError> {
    let result = connection
        .call(
            "Runtime.evaluate",
            json!({
                "expression": format!(
                    "(() => {{ const all = Array.from(document.querySelectorAll(\"a[href],button,input[type=submit],input[type=button],[role=button],[role=link]\")); \
                     const el = document.querySelector({}); if (!el) return -1; return all.indexOf(el); }})()",
                    serde_json::Value::String(selector.to_string())
                ),
                "returnByValue": true,
            }),
        )
        .await?;
    let index = result
        .get("result")
        .and_then(|inner| inner.get("value"))
        .and_then(Value::as_i64)
        .unwrap_or(-1);
    usize::try_from(index).map_err(|_| BrowserError::NoSuchElement {
        selector: selector.to_string(),
    })
}

/// Click the element a selector names, and report what the click was classified as.
///
/// The classification is taken **before** the click, from the page as it is, so the effect the runtime
/// reserves is the effect the page's own semantics describe rather than one inferred afterwards.
///
/// # Errors
/// Returns [`BrowserError::NoSuchElement`] when nothing matches, and [`BrowserError::Page`] when the page
/// refuses.
pub async fn click(connection: &CdpConnection, selector: &str) -> Result<Classified, BrowserError> {
    let (classified, _control) = classify_click(connection, selector).await?;
    let result = connection
        .call(
            "Runtime.evaluate",
            json!({
                "expression": format!(
                    "(() => {{ const el = document.querySelector({}); if (!el) return false; el.click(); return true; }})()",
                    serde_json::Value::String(selector.to_string())
                ),
                "returnByValue": true,
            }),
        )
        .await?;
    match result
        .get("result")
        .and_then(|inner| inner.get("value"))
        .and_then(Value::as_bool)
    {
        Some(true) => Ok(classified),
        _ => Err(BrowserError::NoSuchElement {
            selector: selector.to_string(),
        }),
    }
}

/// Type into the element a selector names, replacing what is there.
///
/// # Errors
/// Returns [`BrowserError::NoSuchElement`] when nothing matches, and [`BrowserError::Page`] when the page
/// refuses.
pub async fn type_text(
    connection: &CdpConnection,
    selector: &str,
    text: &str,
) -> Result<Classified, BrowserError> {
    let result = connection
        .call(
            "Runtime.evaluate",
            json!({
                "expression": format!(
                    "(() => {{ const el = document.querySelector({}); if (!el) return false; \
                     el.focus(); el.value = {}; el.dispatchEvent(new Event('input', {{ bubbles: true }})); \
                     el.dispatchEvent(new Event('change', {{ bubbles: true }})); return true; }})()",
                    serde_json::Value::String(selector.to_string()),
                    serde_json::Value::String(text.to_string())
                ),
                "returnByValue": true,
            }),
        )
        .await?;
    match result
        .get("result")
        .and_then(|inner| inner.get("value"))
        .and_then(Value::as_bool)
    {
        Some(true) => Ok(classify_input()),
        _ => Err(BrowserError::NoSuchElement {
            selector: selector.to_string(),
        }),
    }
}

/// Fetch a page headlessly and return its readable content, bounded, labelled and digested.
///
/// This is the same browser stack the agent drives for actions: the page is loaded by the same browser
/// and read by the same extraction, so research and action cannot disagree about what a page said. There
/// is deliberately no separate scraper.
///
/// # Errors
/// Returns [`BrowserError::Cdp`] when the browser does not answer, and [`BrowserError::Page`] when the
/// page cannot be read.
pub async fn web_fetch(
    connection: &CdpConnection,
    requested_url: &str,
    retrieved_at: &str,
    max_text_bytes: usize,
    max_links: usize,
) -> Result<WebFetch, BrowserError> {
    navigate(connection, requested_url).await?;
    let extracted = extract(connection).await?;
    let url = extracted
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or(requested_url)
        .to_string();
    let title = extracted
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let (text, truncated, total_text_bytes, links, total_links) =
        bound(&extracted, max_text_bytes, max_links);
    let mut metadata = BTreeMap::new();
    if let Some(entries) = extracted.get("metadata").and_then(Value::as_object) {
        for (key, value) in entries {
            if let Some(value) = value.as_str() {
                metadata.insert(key.clone(), value.to_string());
            }
        }
    }
    if let Some(canonical) = extracted.get("canonical").and_then(Value::as_str) {
        metadata.insert("canonical".to_string(), canonical.to_string());
    }
    Ok(WebFetch {
        requested_url: requested_url.to_string(),
        url,
        retrieved_at: retrieved_at.to_string(),
        title,
        content_digest: quansio_core::Digest::of(text.as_bytes())
            .as_str()
            .to_string(),
        text,
        links,
        metadata,
        truncated,
        total_text_bytes,
        total_links,
        trust: TrustLabel::UntrustedExternal,
    })
}

/// Start streaming frames, so a user watching a takeover sees the page.
///
/// # Errors
/// Returns [`BrowserError::Cdp`] when the browser does not answer.
pub async fn start_screencast(
    connection: &CdpConnection,
    session: &mut BrowserSession,
) -> Result<(), BrowserError> {
    connection
        .call(
            "Page.startScreencast",
            json!({ "format": "jpeg", "quality": 60, "everyNthFrame": 1 }),
        )
        .await?;
    session.screencast = Screencast {
        enabled: true,
        stream_ref: Some(format!("{}:screencast", session.id)),
    };
    Ok(())
}

/// Stop streaming frames.
///
/// # Errors
/// Returns [`BrowserError::Cdp`] when the browser does not answer.
pub async fn stop_screencast(
    connection: &CdpConnection,
    session: &mut BrowserSession,
) -> Result<(), BrowserError> {
    connection.call("Page.stopScreencast", json!({})).await?;
    session.screencast = Screencast::default();
    Ok(())
}
