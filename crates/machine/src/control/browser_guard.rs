//! The hardened browser's durable state and decisions (ADP-001, DOMAIN.md §8.4).
//!
//! `browser.rs` owns the session row; this module owns what the hardened driver adds to it, in the same
//! row and under the same transaction rules:
//!
//! * **the guard state** — the worker's last report of each tab's document revision, the dialog waiting
//!   for an answer, the certificate errors waiting for a user, the downloads, and the browser actions
//!   whose outcome is in doubt. The worker is not trusted to describe itself without bound, so a report
//!   is parsed strictly and sized;
//! * **user-only decisions** — resolving a certificate error and granting a media permission. They are
//!   recorded with the user's identity and return a [`DecisionReceipt`] that only this module can mint,
//!   and only for a person. Agent input has no path to one, and a refused attempt leaves the row exactly
//!   as it was;
//! * **secret placeholders** — resolving `{{secret:<name>.<field>}}` through the secret broker's handles.
//!   There is no store of its own: the fields and the origins they may be typed into are the sealed
//!   material of an ordinary handle, so a rotation fences every materialization of them.

use serde_json::{json, Value};
use sqlx::{Acquire, PgConnection};

use super::browser::{
    row_to_session, tabs_json, BrowserSession, BrowserSessionError, BrowserSessionStore, COLUMNS,
};

/// How many tabs, downloads and in-doubt actions one report may carry. A report is a worker's own
/// description of itself; bounding it keeps a misbehaving worker from growing the row without limit.
const GUARD_MAX_TABS: usize = 64;
const GUARD_MAX_ITEMS: usize = 64;
const GUARD_MAX_TEXT: usize = 4096;

/// A JavaScript dialog waiting for an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardDialog {
    /// The id an answer must quote exactly.
    pub id: String,
    /// `alert`, `confirm`, `prompt` or `beforeunload`.
    pub kind: String,
    /// The page's message. Page content: untrusted.
    pub message: String,
    /// The page that raised it.
    pub url: String,
}

/// A certificate error and what was decided about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardCertificate {
    /// The id a decision must quote.
    pub id: String,
    /// The origin whose certificate failed.
    pub origin: String,
    /// The browser's error.
    pub error_type: String,
    /// `pending`, `continued`, `cancelled` or `expired`.
    pub status: String,
    /// SHA-256 of the leaf certificate once the browser revealed it.
    pub certificate_sha256: Option<String>,
    /// The user who decided, when one did.
    pub decided_by: Option<String>,
}

/// A download, as the worker reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardDownload {
    /// `dl_…`.
    pub id: String,
    /// The sanitised name the server suggested. Page content: untrusted.
    pub suggested_filename: String,
    /// `in_progress`, `completed` or `cancelled`.
    pub state: String,
    /// Bytes received.
    pub received_bytes: u64,
    /// Why the driver cancelled it, when it did.
    pub failure: Option<String>,
}

/// What the worker reported about one tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardTab {
    /// The CDP target id.
    pub tab_id: String,
    /// The page it shows.
    pub url: String,
    /// The document revision.
    pub document_revision: String,
    /// The dialog waiting for an answer, if any.
    pub dialog: Option<GuardDialog>,
    /// Certificate errors and their decisions.
    pub certificates: Vec<GuardCertificate>,
    /// Downloads.
    pub downloads: Vec<GuardDownload>,
    /// How many popups were closed for lack of a genuine user gesture.
    pub popups_blocked: u64,
}

/// A browser action whose outcome could not be established. It is never retried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InDoubtAction {
    /// The ToolCall it belongs to.
    pub call_id: String,
    /// The action id.
    pub action_id: String,
    /// Why it is in doubt.
    pub reason: String,
}

/// What a user decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Continue past the certificate error, or grant the permission.
    Allow,
    /// Cancel, or deny.
    Deny,
}

impl Decision {
    /// The canonical spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
        }
    }
}

/// What a decision is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionKind {
    /// A certificate error.
    Certificate,
    /// A media permission.
    Permission,
}

impl DecisionKind {
    /// The canonical spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Certificate => "certificate",
            Self::Permission => "permission",
        }
    }
}

/// A decision a user made, kept with the report because it must outlive the worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedDecision {
    /// What it was about.
    pub kind: DecisionKind,
    /// The tab.
    pub tab_id: String,
    /// The certificate id, or `<origin>|<permission>`.
    pub subject: String,
    /// What was decided.
    pub decision: Decision,
    /// The user.
    pub decided_by: String,
    /// When.
    pub decided_at: String,
}

impl RecordedDecision {
    /// This stored decision as the order a worker takes it in. The decision was minted by machine control for a user and
    /// recorded with the session; sending it again to a worker that was away is not a new decision.
    #[must_use]
    pub fn to_receipt_wire(&self, session_id: &str) -> ReceiptWire {
        ReceiptWire {
            session_id: session_id.to_string(),
            tab_id: self.tab_id.clone(),
            kind: self.kind.as_str().to_string(),
            subject: self.subject.clone(),
            decision: self.decision.as_str().to_string(),
            decided_by: self.decided_by.clone(),
            decided_at: self.decided_at.clone(),
        }
    }
}

/// The session's guard state: the worker's last report, and the user's decisions.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GuardState {
    /// When the worker last reported.
    pub reported_at: Option<String>,
    /// Per-tab state.
    pub tabs: Vec<GuardTab>,
    /// Actions in doubt.
    pub in_doubt: Vec<InDoubtAction>,
    /// The user's decisions, oldest first.
    pub decisions: Vec<RecordedDecision>,
}

fn s(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn opt(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

impl GuardState {
    /// Read what the row stores. Lenient: a missing or malformed part reads as empty, because the row
    /// is rewritten by the next report and a surface must still render.
    #[must_use]
    pub fn from_json(value: &Value) -> Self {
        let list = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        };
        Self {
            reported_at: opt(value, "reported_at"),
            tabs: list("tabs").iter().map(parse_tab).collect(),
            in_doubt: list("in_doubt").iter().map(parse_in_doubt).collect(),
            decisions: list("decisions")
                .iter()
                .filter_map(|item| {
                    Some(RecordedDecision {
                        kind: match s(item, "kind").as_str() {
                            "certificate" => DecisionKind::Certificate,
                            "permission" => DecisionKind::Permission,
                            _ => return None,
                        },
                        tab_id: s(item, "tab_id"),
                        subject: s(item, "subject"),
                        decision: match s(item, "decision").as_str() {
                            "allow" => Decision::Allow,
                            _ => Decision::Deny,
                        },
                        decided_by: s(item, "decided_by"),
                        decided_at: s(item, "decided_at"),
                    })
                })
                .collect(),
        }
    }

    /// The stored and projected form.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "reported_at": self.reported_at,
            "tabs": self.tabs.iter().map(tab_json).collect::<Vec<_>>(),
            "in_doubt": self.in_doubt.iter().map(|item| json!({
                "call_id": item.call_id,
                "action_id": item.action_id,
                "reason": item.reason,
            })).collect::<Vec<_>>(),
            "decisions": self.decisions.iter().map(|item| json!({
                "kind": item.kind.as_str(),
                "tab_id": item.tab_id,
                "subject": item.subject,
                "decision": item.decision.as_str(),
                "decided_by": item.decided_by,
                "decided_at": item.decided_at,
            })).collect::<Vec<_>>(),
        })
    }
}

fn parse_in_doubt(item: &Value) -> InDoubtAction {
    InDoubtAction {
        call_id: s(item, "call_id"),
        action_id: s(item, "action_id"),
        reason: s(item, "reason"),
    }
}

fn parse_tab(tab: &Value) -> GuardTab {
    let items = |key: &str| {
        tab.get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    GuardTab {
        tab_id: s(tab, "tab_id"),
        url: s(tab, "url"),
        document_revision: s(tab, "document_revision"),
        dialog: tab
            .get("dialog")
            .filter(|dialog| dialog.is_object())
            .map(|dialog| GuardDialog {
                id: s(dialog, "id"),
                kind: s(dialog, "kind"),
                message: s(dialog, "message"),
                url: s(dialog, "url"),
            }),
        certificates: items("certificates")
            .iter()
            .map(|item| GuardCertificate {
                id: s(item, "id"),
                origin: s(item, "origin"),
                error_type: s(item, "error_type"),
                status: s(item, "status"),
                certificate_sha256: opt(item, "certificate_sha256"),
                decided_by: opt(item, "decided_by"),
            })
            .collect(),
        downloads: items("downloads")
            .iter()
            .map(|item| GuardDownload {
                id: s(item, "id"),
                suggested_filename: s(item, "suggested_filename"),
                state: s(item, "state"),
                received_bytes: item
                    .get("received_bytes")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                failure: opt(item, "failure"),
            })
            .collect(),
        popups_blocked: tab
            .get("popups_blocked")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    }
}

fn tab_json(tab: &GuardTab) -> Value {
    json!({
        "tab_id": tab.tab_id,
        "url": tab.url,
        "document_revision": tab.document_revision,
        "dialog": tab.dialog.as_ref().map(|dialog| json!({
            "id": dialog.id,
            "kind": dialog.kind,
            "message": dialog.message,
            "url": dialog.url,
        })),
        "certificates": tab.certificates.iter().map(|item| json!({
            "id": item.id,
            "origin": item.origin,
            "error_type": item.error_type,
            "status": item.status,
            "certificate_sha256": item.certificate_sha256,
            "decided_by": item.decided_by,
        })).collect::<Vec<_>>(),
        "downloads": tab.downloads.iter().map(|item| json!({
            "id": item.id,
            "suggested_filename": item.suggested_filename,
            "state": item.state,
            "received_bytes": item.received_bytes,
            "failure": item.failure,
        })).collect::<Vec<_>>(),
        "popups_blocked": tab.popups_blocked,
    })
}

/// A worker's report of its guard state: strict, bounded and typed, because it is the one input to the
/// row that comes from a process the control plane does not run.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GuardReport {
    /// Per-tab state.
    pub tabs: Vec<GuardTab>,
    /// Actions in doubt.
    pub in_doubt: Vec<InDoubtAction>,
}

impl GuardReport {
    /// Parse a report.
    ///
    /// # Errors
    /// Returns [`BrowserSessionError::GuardInvalid`] when the report is not an object, carries too many
    /// tabs or items, names a certificate status or download state the domain does not define, lacks a
    /// tab's identity or revision, or is over its size bound.
    pub fn from_json(value: &Value) -> Result<Self, BrowserSessionError> {
        let invalid = |detail: &str| BrowserSessionError::GuardInvalid {
            detail: detail.to_string(),
        };
        let object = value.as_object().ok_or_else(|| invalid("not an object"))?;
        let tabs = object
            .get("tabs")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid("tabs must be a list"))?;
        if tabs.len() > GUARD_MAX_TABS {
            return Err(invalid("too many tabs"));
        }
        let in_doubt = object
            .get("in_doubt")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if in_doubt.len() > GUARD_MAX_ITEMS {
            return Err(invalid("too many in-doubt actions"));
        }
        let parsed: Vec<GuardTab> = tabs.iter().map(parse_tab).collect();
        for tab in &parsed {
            if tab.tab_id.is_empty() || tab.document_revision.is_empty() {
                return Err(invalid("a tab needs tab_id and document_revision"));
            }
            if tab.certificates.len() > GUARD_MAX_ITEMS || tab.downloads.len() > GUARD_MAX_ITEMS {
                return Err(invalid("too many certificates or downloads on a tab"));
            }
            for certificate in &tab.certificates {
                if !["pending", "continued", "cancelled", "expired"]
                    .contains(&certificate.status.as_str())
                {
                    return Err(invalid(
                        "a certificate status is not one the domain defines",
                    ));
                }
            }
            for download in &tab.downloads {
                if !["in_progress", "completed", "cancelled"].contains(&download.state.as_str()) {
                    return Err(invalid("a download state is not one the domain defines"));
                }
            }
        }
        let report = Self {
            tabs: parsed,
            in_doubt: in_doubt.iter().map(parse_in_doubt).collect(),
        };
        let rendered = GuardState {
            tabs: report.tabs.clone(),
            in_doubt: report.in_doubt.clone(),
            ..GuardState::default()
        }
        .to_json()
        .to_string();
        if rendered.len() > GUARD_MAX_TEXT * GUARD_MAX_TABS {
            return Err(invalid("the report is over its size bound"));
        }
        Ok(report)
    }
}

/// Who is making a decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecisionActor {
    /// A person, through the authenticated command surface.
    User {
        /// `usr_…`.
        user_id: String,
    },
    /// An agent. It can never decide.
    Agent {
        /// Any identity an agent presents.
        identity: String,
    },
}

impl DecisionActor {
    fn describe(&self) -> String {
        match self {
            Self::User { user_id } => format!("user {user_id}"),
            Self::Agent { identity } => format!("agent {identity}"),
        }
    }
}

/// Proof that a user made a decision.
///
/// It can be minted only by this module, and only for a [`DecisionActor::User`]: the worker takes a
/// receipt, not a flag, so the only way to resolve a certificate error or grant a permission is for the
/// control plane to have recorded a person doing it. Agent input has no path to one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionReceipt {
    session_id: String,
    tab_id: String,
    kind: DecisionKind,
    subject: String,
    decision: Decision,
    decided_by: String,
    decided_at: String,
}

impl DecisionReceipt {
    /// The session the decision was made in.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
    /// The tab.
    #[must_use]
    pub fn tab_id(&self) -> &str {
        &self.tab_id
    }
    /// What it was about.
    #[must_use]
    pub const fn kind(&self) -> DecisionKind {
        self.kind
    }
    /// The certificate id, or `<origin>|<permission>`.
    #[must_use]
    pub fn subject(&self) -> &str {
        &self.subject
    }
    /// What was decided.
    #[must_use]
    pub const fn decision(&self) -> Decision {
        self.decision
    }
    /// The user.
    #[must_use]
    pub fn decided_by(&self) -> &str {
        &self.decided_by
    }
    /// When.
    #[must_use]
    pub fn decided_at(&self) -> &str {
        &self.decided_at
    }
}

/// A receipt as it crosses the worker control channel (EXEC-009 composition).
///
/// The control plane mints a [`DecisionReceipt`] for a user and hands the worker this description of it over the
/// channel only the lease holder can write to. It is data: the worker turns it back into a receipt with
/// [`DecisionReceipt::from_control_channel`], and nothing else in the worker may.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReceiptWire {
    /// The session the decision was made in.
    pub session_id: String,
    /// The tab.
    pub tab_id: String,
    /// `certificate` or `permission`.
    pub kind: String,
    /// The certificate id, or `<origin>|<permission>`.
    pub subject: String,
    /// `allow` or `deny`.
    pub decision: String,
    /// The user.
    pub decided_by: String,
    /// When.
    pub decided_at: String,
}

impl DecisionReceipt {
    /// This receipt as it travels to the worker.
    #[must_use]
    pub fn to_wire(&self) -> ReceiptWire {
        ReceiptWire {
            session_id: self.session_id.clone(),
            tab_id: self.tab_id.clone(),
            kind: self.kind.as_str().to_string(),
            subject: self.subject.clone(),
            decision: self.decision.as_str().to_string(),
            decided_by: self.decided_by.clone(),
            decided_at: self.decided_at.clone(),
        }
    }

    /// The receipt a worker received from the control plane over its control channel.
    ///
    /// This is the one way a receipt exists outside the control plane's store functions, and only the worker's
    /// control-channel reader (`qworkerd::daemon`) calls it; a test pins both. The trust it carries is the
    /// channel's: only the holder of the target's lease can write to it.
    ///
    /// # Errors
    /// Returns the field that is not a value the domain defines.
    pub fn from_control_channel(wire: &ReceiptWire) -> Result<Self, String> {
        let kind = match wire.kind.as_str() {
            "certificate" => DecisionKind::Certificate,
            "permission" => DecisionKind::Permission,
            other => return Err(format!("{other:?} is not a decision kind")),
        };
        let decision = match wire.decision.as_str() {
            "allow" => Decision::Allow,
            "deny" => Decision::Deny,
            other => return Err(format!("{other:?} is not a decision")),
        };
        for (field, value) in [
            ("session_id", &wire.session_id),
            ("tab_id", &wire.tab_id),
            ("subject", &wire.subject),
            ("decided_by", &wire.decided_by),
            ("decided_at", &wire.decided_at),
        ] {
            if value.trim().is_empty() {
                return Err(format!("{field} is required"));
            }
        }
        Ok(Self {
            session_id: wire.session_id.clone(),
            tab_id: wire.tab_id.clone(),
            kind,
            subject: wire.subject.clone(),
            decision,
            decided_by: wire.decided_by.clone(),
            decided_at: wire.decided_at.clone(),
        })
    }
}

#[cfg(feature = "test-support")]
impl DecisionReceipt {
    /// A receipt without a database, for another crate's tests of the worker half of a decision.
    /// Absent from every build that does not enable `test-support`, which no production target does.
    #[must_use]
    pub fn for_tests(
        session_id: &str,
        tab_id: &str,
        kind: DecisionKind,
        subject: &str,
        decision: Decision,
        decided_by: &str,
    ) -> Self {
        Self {
            session_id: session_id.to_string(),
            tab_id: tab_id.to_string(),
            kind,
            subject: subject.to_string(),
            decision,
            decided_by: decided_by.to_string(),
            decided_at: "2026-10-03T00:00:00Z".to_string(),
        }
    }
}

/// The permissions a decision can grant. Everything else is denied by default and stays denied.
pub const GRANTABLE_PERMISSIONS: [&str; 2] = ["camera", "microphone"];

impl BrowserSessionStore {
    /// Record what the worker reports about its tabs and actions.
    ///
    /// The report replaces the previous one, with one exception: a certificate a user has already
    /// decided is never put back to `pending` by a report that raced the decision. Each tab's document
    /// revision is also written into the session's `tabs`, so a surface and the run can tell the page
    /// changed.
    ///
    /// # Errors
    /// Returns [`BrowserSessionError::NotFound`] for an unknown session.
    pub async fn record_guard_report(
        conn: &mut PgConnection,
        tenant_id: &str,
        session_id: &str,
        report: &GuardReport,
        at: &str,
    ) -> Result<BrowserSession, BrowserSessionError> {
        let mut tx = conn.begin().await?;
        let mut session = lock_for_update(&mut tx, tenant_id, session_id).await?;
        let mut tabs = report.tabs.clone();
        for tab in &mut tabs {
            let stored = session
                .guard
                .tabs
                .iter()
                .find(|stored| stored.tab_id == tab.tab_id);
            for certificate in &mut tab.certificates {
                if certificate.status != "pending" {
                    continue;
                }
                if let Some(decided) = stored
                    .and_then(|stored| stored.certificates.iter().find(|c| c.id == certificate.id))
                    .filter(|stored| stored.status != "pending")
                {
                    certificate.status = decided.status.clone();
                    certificate.decided_by = decided.decided_by.clone();
                }
            }
        }
        session.guard.tabs = tabs;
        session.guard.in_doubt = report.in_doubt.clone();
        session.guard.reported_at = Some(at.to_string());
        let reported = session.guard.tabs.clone();
        for tab in &mut session.tabs {
            if let Some(reported) = reported.iter().find(|r| r.tab_id == tab.target_id) {
                tab.document_revision = Some(reported.document_revision.clone());
            }
        }
        write_guard(&mut tx, tenant_id, session_id, &session).await?;
        tx.commit().await?;
        Ok(session)
    }

    /// A user's decision on a certificate error.
    ///
    /// Only a [`DecisionActor::User`] can decide; an agent is refused before anything is read, and the
    /// row is left exactly as it was. The decision is written with the user's identity, and the receipt
    /// it returns is what the worker needs to act on it.
    ///
    /// # Errors
    /// Returns [`BrowserSessionError::UserDecisionRequired`] for a non-user and
    /// [`BrowserSessionError::DecisionSubjectNotFound`] when the certificate error is not pending.
    #[allow(clippy::too_many_arguments)]
    pub async fn decide_certificate(
        conn: &mut PgConnection,
        tenant_id: &str,
        session_id: &str,
        tab_id: &str,
        certificate_id: &str,
        decision: Decision,
        actor: &DecisionActor,
        at: &str,
    ) -> Result<DecisionReceipt, BrowserSessionError> {
        let user_id = require_user(actor, "a certificate decision")?;
        let mut tx = conn.begin().await?;
        let mut session = lock_for_update(&mut tx, tenant_id, session_id).await?;
        let not_found = || BrowserSessionError::DecisionSubjectNotFound(certificate_id.to_string());
        let tab = session
            .guard
            .tabs
            .iter_mut()
            .find(|tab| tab.tab_id == tab_id)
            .ok_or_else(not_found)?;
        let certificate = tab
            .certificates
            .iter_mut()
            .find(|c| c.id == certificate_id && c.status == "pending")
            .ok_or_else(not_found)?;
        certificate.status = match decision {
            Decision::Allow => "continued",
            Decision::Deny => "cancelled",
        }
        .to_string();
        certificate.decided_by = Some(user_id.clone());
        session.guard.decisions.push(RecordedDecision {
            kind: DecisionKind::Certificate,
            tab_id: tab_id.to_string(),
            subject: certificate_id.to_string(),
            decision,
            decided_by: user_id.clone(),
            decided_at: at.to_string(),
        });
        write_guard(&mut tx, tenant_id, session_id, &session).await?;
        tx.commit().await?;
        Ok(DecisionReceipt {
            session_id: session_id.to_string(),
            tab_id: tab_id.to_string(),
            kind: DecisionKind::Certificate,
            subject: certificate_id.to_string(),
            decision,
            decided_by: user_id,
            decided_at: at.to_string(),
        })
    }

    /// A user's decision on a media permission for one origin.
    ///
    /// Only camera and microphone can be granted at all, and only by a user, through this one path.
    /// Every other permission is denied by default and has no decision to make.
    ///
    /// # Errors
    /// Returns [`BrowserSessionError::UserDecisionRequired`] for a non-user and
    /// [`BrowserSessionError::DecisionNotGrantable`] for any permission but camera or microphone, or an
    /// origin that is not `http(s)://host[:port]`.
    #[allow(clippy::too_many_arguments)]
    pub async fn decide_permission(
        conn: &mut PgConnection,
        tenant_id: &str,
        session_id: &str,
        tab_id: &str,
        origin: &str,
        permission: &str,
        decision: Decision,
        actor: &DecisionActor,
        at: &str,
    ) -> Result<DecisionReceipt, BrowserSessionError> {
        let user_id = require_user(actor, "a permission decision")?;
        if !GRANTABLE_PERMISSIONS.contains(&permission) {
            return Err(BrowserSessionError::DecisionNotGrantable(
                permission.to_string(),
            ));
        }
        let well_formed = ["http://", "https://"].iter().any(|scheme| {
            origin
                .strip_prefix(scheme)
                .is_some_and(|rest| !rest.is_empty() && !rest.contains(['/', '@', '?', '#']))
        });
        if !well_formed {
            return Err(BrowserSessionError::DecisionNotGrantable(format!(
                "{permission} for {origin}"
            )));
        }
        let mut tx = conn.begin().await?;
        let mut session = lock_for_update(&mut tx, tenant_id, session_id).await?;
        let subject = format!("{origin}|{permission}");
        session.guard.decisions.push(RecordedDecision {
            kind: DecisionKind::Permission,
            tab_id: tab_id.to_string(),
            subject: subject.clone(),
            decision,
            decided_by: user_id.clone(),
            decided_at: at.to_string(),
        });
        write_guard(&mut tx, tenant_id, session_id, &session).await?;
        tx.commit().await?;
        Ok(DecisionReceipt {
            session_id: session_id.to_string(),
            tab_id: tab_id.to_string(),
            kind: DecisionKind::Permission,
            subject,
            decision,
            decided_by: user_id,
            decided_at: at.to_string(),
        })
    }
}

fn require_user(actor: &DecisionActor, subject: &str) -> Result<String, BrowserSessionError> {
    match actor {
        DecisionActor::User { user_id } if user_id.starts_with("usr_") => Ok(user_id.clone()),
        other => Err(BrowserSessionError::UserDecisionRequired {
            subject: subject.to_string(),
            actor: other.describe(),
        }),
    }
}

async fn lock_for_update(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: &str,
    session_id: &str,
) -> Result<BrowserSession, BrowserSessionError> {
    let sql = format!(
        "SELECT {COLUMNS} FROM browser_sessions WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
    );
    let row = sqlx::query(&sql)
        .bind(session_id)
        .bind(tenant_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| BrowserSessionError::NotFound(session_id.to_string()))?;
    row_to_session(&row)
}

async fn write_guard(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: &str,
    session_id: &str,
    session: &BrowserSession,
) -> Result<(), BrowserSessionError> {
    sqlx::query(
        "UPDATE browser_sessions SET guard_state = $3, tabs = $4 WHERE id = $1 AND tenant_id = $2",
    )
    .bind(session_id)
    .bind(tenant_id)
    .bind(session.guard.to_json())
    .bind(tabs_json(&session.tabs))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

// ------------------------------------------------------------------ secret placeholders

/// The tool a browser secret is materialized for. A browser secret is typed, so its materialization is
/// scoped to the tool that types it and cannot be presented for a connector or a target.
pub const BROWSER_SECRET_TOOL: &str = "browser.type";

/// Why a browser secret could not be resolved.
#[derive(Debug, thiserror::Error)]
pub enum BrowserSecretError {
    /// The broker refused or the database failed.
    #[error("{0}")]
    Broker(#[from] crate::secrets::SecretError),
    /// The tenant has no active handle by this name.
    #[error("no active secret is named {0:?}")]
    NotFound(String),
    /// More than one active handle carries the name, so the placeholder would be a guess.
    #[error("more than one active secret is named {0:?}")]
    Ambiguous(String),
    /// The secret has no such field.
    #[error("secret {name:?} has no field {field:?}")]
    FieldMissing {
        /// The secret.
        name: String,
        /// The field.
        field: String,
    },
    /// The sealed material is not the browser secret shape.
    #[error("the secret material is not a browser secret: {0}")]
    Malformed(String),
}

/// What a browser secret is made of: named fields, and the origins they may be typed into.
///
/// It is the plaintext that gets sealed by the secret broker as a handle's material — there is no store
/// of its own. The allow-list travels *inside* the sealed material, so the origin a secret is bound to
/// cannot be edited without the handle being rotated, and a rotation fences every materialization.
pub struct BrowserSecretMaterial {
    fields: std::collections::BTreeMap<String, String>,
    allowed_origins: Vec<String>,
}

impl std::fmt::Debug for BrowserSecretMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserSecretMaterial")
            .field("fields", &self.fields.keys().collect::<Vec<_>>())
            .field("allowed_origins", &self.allowed_origins)
            .finish()
    }
}

impl BrowserSecretMaterial {
    /// Build material from fields and the origins they may be used on.
    #[must_use]
    pub fn new(
        fields: impl IntoIterator<Item = (String, String)>,
        allowed_origins: Vec<String>,
    ) -> Self {
        Self {
            fields: fields.into_iter().collect(),
            allowed_origins,
        }
    }

    /// The bytes to hand to `SecretBroker::register` or `rotate`.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        json!({ "fields": self.fields, "allowed_origins": self.allowed_origins })
            .to_string()
            .into_bytes()
    }

    /// Read sealed material.
    ///
    /// # Errors
    /// Returns [`BrowserSecretError::Malformed`] when it is not the browser secret shape.
    pub fn parse(bytes: &[u8]) -> Result<Self, BrowserSecretError> {
        let value: Value = serde_json::from_slice(bytes)
            .map_err(|_| BrowserSecretError::Malformed("not JSON".to_string()))?;
        let fields = value
            .get("fields")
            .and_then(Value::as_object)
            .ok_or_else(|| BrowserSecretError::Malformed("no fields".to_string()))?;
        let mut parsed = std::collections::BTreeMap::new();
        for (name, field) in fields {
            let text = field
                .as_str()
                .ok_or_else(|| BrowserSecretError::Malformed("a field is not text".to_string()))?;
            parsed.insert(name.clone(), text.to_string());
        }
        let allowed_origins = value
            .get("allowed_origins")
            .and_then(Value::as_array)
            .map(|origins| {
                origins
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Ok(Self {
            fields: parsed,
            allowed_origins,
        })
    }

    /// One field's value.
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields.get(name).map(String::as_str)
    }

    /// The origins the secret may be typed into. Empty means nowhere.
    #[must_use]
    pub fn allowed_origins(&self) -> &[String] {
        &self.allowed_origins
    }
}

/// One placeholder to resolve.
#[derive(Debug)]
pub struct BrowserSecretRequest<'a> {
    /// The tenant.
    pub tenant_id: &'a str,
    /// The workspace asking.
    pub workspace_id: Option<&'a str>,
    /// The placeholder's name: the handle's label.
    pub name: &'a str,
    /// The placeholder's field.
    pub field: &'a str,
    /// Who is asking (the run's identity).
    pub actor: &'a str,
    /// The `credential.access` effect this materialization settles.
    pub effect_id: &'a str,
    /// The instant (canonical ISO-8601 UTC).
    pub at: &'a str,
    /// When the materialization lapses.
    pub expires_at: &'a str,
}

/// A resolved field, ready to be handed to the insertion boundary: no `Display`, no `Clone`, no
/// `Serialize`, and a redacted `Debug`.
pub struct BrowserSecretValue {
    value: String,
    allowed_origins: Vec<String>,
}

impl BrowserSecretValue {
    /// The characters. The caller is at the insertion boundary; nothing else should be.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.value
    }

    /// The origins the secret may be typed into.
    #[must_use]
    pub fn allowed_origins(&self) -> &[String] {
        &self.allowed_origins
    }
}

impl std::fmt::Debug for BrowserSecretValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "BrowserSecretValue(<{} bytes redacted>, origins: {:?})",
            self.value.len(),
            self.allowed_origins
        )
    }
}

/// Resolves `{{secret:<name>.<field>}}` placeholders through the secret broker.
pub struct BrowserSecrets;

impl BrowserSecrets {
    /// Resolve one placeholder: find the active handle by label, materialize it for `browser.type`,
    /// and read the field. The materialization is the broker's — fenced by generation, scoped to the
    /// tool, short-lived — and the access it returns is what the audit owner persists.
    ///
    /// # Errors
    /// Returns [`BrowserSecretError`]; every refusal names the rule and none carries the value.
    pub async fn resolve(
        conn: &mut PgConnection,
        provider: &dyn crate::secrets::KeyProvider,
        request: &BrowserSecretRequest<'_>,
    ) -> Result<(BrowserSecretValue, crate::secrets::SecretAccess), BrowserSecretError> {
        use crate::secrets::{MaterializationScope, MaterializeRequest, SecretBroker};
        let handles = SecretBroker::list(conn, request.tenant_id).await?;
        let mut matching = handles.into_iter().filter(|handle| {
            handle.label == request.name
                && handle.usable_at(request.at).is_ok()
                && match (&handle.workspace_id, request.workspace_id) {
                    (Some(owner), Some(asked)) => owner == asked,
                    _ => true,
                }
        });
        let handle = matching
            .next()
            .ok_or_else(|| BrowserSecretError::NotFound(request.name.to_string()))?;
        if matching.next().is_some() {
            return Err(BrowserSecretError::Ambiguous(request.name.to_string()));
        }
        let (materialization, access) = SecretBroker::materialize(
            conn,
            provider,
            &MaterializeRequest {
                tenant_id: request.tenant_id,
                workspace_id: request.workspace_id,
                handle_id: &handle.id,
                actor: request.actor,
                effect_id: request.effect_id,
                scope: MaterializationScope::Tool {
                    tool: BROWSER_SECRET_TOOL.to_string(),
                },
                expires_at: request.expires_at,
                at: request.at,
            },
        )
        .await?;
        let material = BrowserSecretMaterial::parse(materialization.material.expose())?;
        let value = material
            .field(request.field)
            .ok_or_else(|| BrowserSecretError::FieldMissing {
                name: request.name.to_string(),
                field: request.field.to_string(),
            })?
            .to_string();
        Ok((
            BrowserSecretValue {
                value,
                allowed_origins: material.allowed_origins().to_vec(),
            },
            access,
        ))
    }
}
