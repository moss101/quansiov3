//! What a native computer action says about itself (ADP-002, DOMAIN.md §8.6.1 A and C).
//!
//! The central rule is that **unknown is not a value you may round**. `effect_verified` is
//! `Some(true)` only when the bridge observed the intended effect, `Some(false)` when it observed the
//! effect is absent or a background invariant was broken, and `None` when it could not observe at all.
//! Nothing here converts `None` into either of the others, and the runtime re-derives the cases a bridge
//! could get wrong (a background claim with a broken or missing measurement) rather than trusting the
//! flag it was sent.

use serde_json::{json, Map, Value};

/// How the bridge acted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptMethod {
    /// Read the accessibility tree.
    AccessibilityTree,
    /// Perform an accessibility action on a control.
    AccessibilityAction,
    /// Post an event to one process.
    ProcessEvent,
    /// Post an event into the system input stream.
    HidEvent,
    /// Read or write the pasteboard.
    Pasteboard,
}

impl ReceiptMethod {
    /// Canonical wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AccessibilityTree => "accessibility_tree",
            Self::AccessibilityAction => "accessibility_action",
            Self::ProcessEvent => "process_event",
            Self::HidEvent => "hid_event",
            Self::Pasteboard => "pasteboard",
        }
    }

    /// Parse the canonical wire name.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        [
            Self::AccessibilityTree,
            Self::AccessibilityAction,
            Self::ProcessEvent,
            Self::HidEvent,
            Self::Pasteboard,
        ]
        .into_iter()
        .find(|m| m.as_str() == value)
    }
}

/// Whether the action claims to leave the person's machine state alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMethod {
    /// Changes neither the frontmost application nor the real pointer.
    Background,
    /// May do either.
    Foreground,
}

impl ExecutionMethod {
    /// Canonical wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Foreground => "foreground",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "background" => Some(Self::Background),
            "foreground" => Some(Self::Foreground),
            _ => None,
        }
    }
}

/// The result, in words that cannot be mistaken for each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptOutcome {
    /// The intended effect was observed (`effect_verified = true`).
    Applied,
    /// A read returned data; there was no effect to verify (`null`).
    Observed,
    /// Input was handed to the system and its effect could not be observed (`null`).
    Unverified,
    /// Input was handed to the system and its effect was observed to be absent (`false`).
    NotApplied,
    /// A background claim was broken (`false`).
    InvariantViolated,
    /// Nothing was sent (`dispatched = false`, `false`).
    Refused,
}

impl ReceiptOutcome {
    /// Canonical wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Observed => "observed",
            Self::Unverified => "unverified",
            Self::NotApplied => "not_applied",
            Self::InvariantViolated => "invariant_violated",
            Self::Refused => "refused",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        [
            Self::Applied,
            Self::Observed,
            Self::Unverified,
            Self::NotApplied,
            Self::InvariantViolated,
            Self::Refused,
        ]
        .into_iter()
        .find(|o| o.as_str() == value)
    }
}

/// One application as the bridge saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontmostApp {
    /// Bundle identifier.
    pub bundle_id: String,
    /// Process id.
    pub pid: i32,
}

/// What the bridge measured around a background action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundInvariants {
    /// Any input arrived from the system input stream during the action window.
    pub physical_input: bool,
    /// The real pointer is not where it was.
    pub moved_system_cursor: bool,
    /// The frontmost application before the action.
    pub frontmost_before: FrontmostApp,
    /// The frontmost application after it.
    pub frontmost_after: FrontmostApp,
    /// The bridge's own statement that the frontmost application changed.
    pub frontmost_changed: bool,
}

impl BackgroundInvariants {
    /// Whether the measurement shows a broken claim. The bridge's flag can only add to this: the
    /// before and after applications are compared here as well.
    #[must_use]
    pub fn violated(&self) -> bool {
        self.physical_input
            || self.moved_system_cursor
            || self.frontmost_changed
            || self.frontmost_before != self.frontmost_after
    }

    /// The wire form.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "physical_input": self.physical_input,
            "moved_system_cursor": self.moved_system_cursor,
            "frontmost_before": app_json(&self.frontmost_before),
            "frontmost_after": app_json(&self.frontmost_after),
            "frontmost_changed": self.frontmost_changed,
        })
    }

    fn from_json(value: &Value) -> Result<Self, ReceiptError> {
        let object = exact(
            value,
            "invariants",
            &[
                "physical_input",
                "moved_system_cursor",
                "frontmost_before",
                "frontmost_after",
                "frontmost_changed",
            ],
        )?;
        Ok(Self {
            physical_input: boolean(object, "physical_input")?,
            moved_system_cursor: boolean(object, "moved_system_cursor")?,
            frontmost_before: app_from(object, "frontmost_before")?,
            frontmost_after: app_from(object, "frontmost_after")?,
            frontmost_changed: boolean(object, "frontmost_changed")?,
        })
    }
}

fn app_json(app: &FrontmostApp) -> Value {
    json!({ "bundle_id": app.bundle_id, "pid": app.pid })
}

fn app_from(object: &Map<String, Value>, key: &str) -> Result<FrontmostApp, ReceiptError> {
    let value = object
        .get(key)
        .ok_or_else(|| ReceiptError::Malformed(format!("{key} is required")))?;
    let app = exact(value, key, &["bundle_id", "pid"])?;
    let bundle_id = app
        .get("bundle_id")
        .and_then(Value::as_str)
        .ok_or_else(|| ReceiptError::Malformed(format!("{key}.bundle_id must be a string")))?;
    let pid = app
        .get("pid")
        .and_then(Value::as_i64)
        .and_then(|pid| i32::try_from(pid).ok())
        .ok_or_else(|| ReceiptError::Malformed(format!("{key}.pid must be an integer")))?;
    Ok(FrontmostApp {
        bundle_id: bundle_id.to_string(),
        pid,
    })
}

/// A receipt that cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReceiptError {
    /// Wrong shape: a missing key, an extra key, a wrong type or an unknown word.
    #[error("receipt is malformed: {0}")]
    Malformed(String),
    /// The fields contradict each other.
    #[error("receipt is incoherent: {0}")]
    Incoherent(String),
}

/// How the Effect Ledger settles the effect a receipt describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerOutcome {
    /// The effect is established.
    SettledSuccess,
    /// Nothing happened, so a later attempt is a new effect.
    SettledFailed,
    /// Input reached the machine and its consequence is not established: reconcile, never retry blind.
    OutcomeUnknown,
}

impl LedgerOutcome {
    /// The ledger's own name for the status (DOMAIN.md §7.2).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SettledSuccess => "SETTLED_SUCCESS",
            Self::SettledFailed => "SETTLED_FAILED",
            Self::OutcomeUnknown => "OUTCOME_UNKNOWN",
        }
    }
}

/// The receipt of one native computer action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionReceipt {
    /// How the bridge acted.
    pub method: ReceiptMethod,
    /// Whether it claims to have left the foreground and the pointer alone.
    pub execution_method: ExecutionMethod,
    /// Whether input was handed to the system.
    pub dispatched: bool,
    /// `Some(true)`, `Some(false)` or `None` (unknown). Never rounded.
    pub effect_verified: Option<bool>,
    /// The result in words.
    pub outcome: ReceiptOutcome,
    /// Whether a background claim was broken.
    pub invariant_violated: bool,
    /// What was measured around a background action.
    pub invariants: Option<BackgroundInvariants>,
}

impl ActionReceipt {
    /// A read that returned data.
    #[must_use]
    pub const fn observed(method: ReceiptMethod, execution_method: ExecutionMethod) -> Self {
        Self {
            method,
            execution_method,
            dispatched: true,
            effect_verified: None,
            outcome: ReceiptOutcome::Observed,
            invariant_violated: false,
            invariants: None,
        }
    }

    /// Input that was handed to the system and whose effect could not be observed.
    #[must_use]
    pub const fn unverified(method: ReceiptMethod, execution_method: ExecutionMethod) -> Self {
        Self {
            method,
            execution_method,
            dispatched: true,
            effect_verified: None,
            outcome: ReceiptOutcome::Unverified,
            invariant_violated: false,
            invariants: None,
        }
    }

    /// Input whose intended effect was observed.
    #[must_use]
    pub const fn applied(method: ReceiptMethod, execution_method: ExecutionMethod) -> Self {
        Self {
            method,
            execution_method,
            dispatched: true,
            effect_verified: Some(true),
            outcome: ReceiptOutcome::Applied,
            invariant_violated: false,
            invariants: None,
        }
    }

    /// Input whose intended effect was observed to be absent.
    #[must_use]
    pub const fn not_applied(method: ReceiptMethod, execution_method: ExecutionMethod) -> Self {
        Self {
            method,
            execution_method,
            dispatched: true,
            effect_verified: Some(false),
            outcome: ReceiptOutcome::NotApplied,
            invariant_violated: false,
            invariants: None,
        }
    }

    /// Nothing was sent.
    #[must_use]
    pub const fn refused(method: ReceiptMethod, execution_method: ExecutionMethod) -> Self {
        Self {
            method,
            execution_method,
            dispatched: false,
            effect_verified: Some(false),
            outcome: ReceiptOutcome::Refused,
            invariant_violated: false,
            invariants: None,
        }
    }

    /// The same receipt carrying a background measurement.
    #[must_use]
    pub fn with_invariants(mut self, invariants: BackgroundInvariants) -> Self {
        self.invariants = Some(invariants);
        self
    }

    /// Re-derive what the runtime does not take on trust, then require the fields to agree.
    ///
    /// * A background action whose measurement shows a broken claim, or whose bridge flagged one, is
    ///   `invariant_violated` with `effect_verified = false` whatever the bridge said about the effect.
    /// * A background action reported `applied` without a measurement is lowered to `unverified`
    ///   (`effect_verified = None`): the claim that nothing else was disturbed is unproven, and so is
    ///   the verification that rests on it.
    ///
    /// # Errors
    /// Returns [`ReceiptError::Incoherent`] when what remains is self-contradictory.
    pub fn normalized(mut self) -> Result<Self, ReceiptError> {
        if self.execution_method == ExecutionMethod::Background && self.dispatched {
            let measured = self.invariants.as_ref().map(BackgroundInvariants::violated);
            if self.invariant_violated || measured == Some(true) {
                self.invariant_violated = true;
                self.effect_verified = Some(false);
                self.outcome = ReceiptOutcome::InvariantViolated;
            } else if measured.is_none() && self.effect_verified == Some(true) {
                self.effect_verified = None;
                self.outcome = ReceiptOutcome::Unverified;
            }
        }
        self.check_coherence()?;
        Ok(self)
    }

    /// Require the fields to agree (DOMAIN.md §8.6.1 A).
    ///
    /// # Errors
    /// Returns [`ReceiptError::Incoherent`] naming the contradiction.
    pub fn check_coherence(&self) -> Result<(), ReceiptError> {
        let bad = |detail: &str| Err(ReceiptError::Incoherent(detail.to_string()));
        if !self.dispatched {
            if self.outcome != ReceiptOutcome::Refused || self.effect_verified != Some(false) {
                return bad(
                    "a receipt that dispatched nothing must be refused with effect_verified=false",
                );
            }
            if self.invariant_violated {
                return bad("an action that dispatched nothing cannot violate an invariant");
            }
            return Ok(());
        }
        if self.outcome == ReceiptOutcome::Refused {
            return bad("a refused receipt cannot say it dispatched");
        }
        if self.invariant_violated {
            if self.execution_method != ExecutionMethod::Background {
                return bad("only a background action can violate a background invariant");
            }
            if self.effect_verified != Some(false)
                || self.outcome != ReceiptOutcome::InvariantViolated
            {
                return bad("a violated invariant is effect_verified=false and invariant_violated");
            }
            return Ok(());
        }
        match (self.effect_verified, self.outcome) {
            (Some(true), ReceiptOutcome::Applied)
            | (Some(false), ReceiptOutcome::NotApplied)
            | (None, ReceiptOutcome::Observed | ReceiptOutcome::Unverified) => {}
            _ => {
                return bad("effect_verified and outcome disagree");
            }
        }
        if self.outcome == ReceiptOutcome::InvariantViolated {
            return bad("invariant_violated outcome without invariant_violated");
        }
        if self.effect_verified == Some(true)
            && self.execution_method == ExecutionMethod::Background
            && self.invariants.is_none()
        {
            return bad("a background action cannot be verified without a measurement");
        }
        Ok(())
    }

    /// How the Effect Ledger settles this effect (DOMAIN.md §8.6.1 A, "Settlement").
    #[must_use]
    pub const fn ledger_outcome(&self) -> LedgerOutcome {
        match self.outcome {
            ReceiptOutcome::Applied | ReceiptOutcome::Observed => LedgerOutcome::SettledSuccess,
            ReceiptOutcome::Refused => LedgerOutcome::SettledFailed,
            ReceiptOutcome::Unverified
            | ReceiptOutcome::NotApplied
            | ReceiptOutcome::InvariantViolated => LedgerOutcome::OutcomeUnknown,
        }
    }

    /// The wire form. `effect_verified` is always present, `null` when unknown.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "method": self.method.as_str(),
            "execution_method": self.execution_method.as_str(),
            "dispatched": self.dispatched,
            "effect_verified": self.effect_verified,
            "outcome": self.outcome.as_str(),
            "invariant_violated": self.invariant_violated,
            "invariants": self.invariants.as_ref().map(BackgroundInvariants::to_json),
        })
    }

    /// Parse the wire form strictly.
    ///
    /// Every key must be present (an absent `effect_verified` is *not* read as unknown: a receipt that
    /// does not say is malformed) and no other key may be.
    ///
    /// # Errors
    /// Returns [`ReceiptError::Malformed`] for a wrong shape or an unknown word.
    pub fn from_json(value: &Value) -> Result<Self, ReceiptError> {
        let object = exact(
            value,
            "receipt",
            &[
                "method",
                "execution_method",
                "dispatched",
                "effect_verified",
                "outcome",
                "invariant_violated",
                "invariants",
            ],
        )?;
        let word = |key: &str| -> Result<&str, ReceiptError> {
            object
                .get(key)
                .and_then(Value::as_str)
                .ok_or_else(|| ReceiptError::Malformed(format!("{key} must be a string")))
        };
        let method = ReceiptMethod::parse(word("method")?)
            .ok_or_else(|| ReceiptError::Malformed("unknown method".into()))?;
        let execution_method = ExecutionMethod::parse(word("execution_method")?)
            .ok_or_else(|| ReceiptError::Malformed("unknown execution_method".into()))?;
        let outcome = ReceiptOutcome::parse(word("outcome")?)
            .ok_or_else(|| ReceiptError::Malformed("unknown outcome".into()))?;
        let effect_verified = match object.get("effect_verified") {
            Some(Value::Bool(flag)) => Some(*flag),
            Some(Value::Null) => None,
            _ => {
                return Err(ReceiptError::Malformed(
                    "effect_verified must be true, false or null".into(),
                ))
            }
        };
        let invariants = match object.get("invariants") {
            Some(Value::Null) => None,
            Some(value) => Some(BackgroundInvariants::from_json(value)?),
            None => None,
        };
        Ok(Self {
            method,
            execution_method,
            dispatched: boolean(object, "dispatched")?,
            effect_verified,
            outcome,
            invariant_violated: boolean(object, "invariant_violated")?,
            invariants,
        })
    }
}

fn boolean(object: &Map<String, Value>, key: &str) -> Result<bool, ReceiptError> {
    object
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| ReceiptError::Malformed(format!("{key} must be a boolean")))
}

/// An object with exactly `keys`.
fn exact<'a>(
    value: &'a Value,
    what: &str,
    keys: &[&str],
) -> Result<&'a Map<String, Value>, ReceiptError> {
    let object = value
        .as_object()
        .ok_or_else(|| ReceiptError::Malformed(format!("{what} must be an object")))?;
    for key in keys {
        if !object.contains_key(*key) {
            return Err(ReceiptError::Malformed(format!("{what}.{key} is required")));
        }
    }
    if let Some(extra) = object.keys().find(|key| !keys.contains(&key.as_str())) {
        return Err(ReceiptError::Malformed(format!(
            "{what} has an unknown field {extra:?}"
        )));
    }
    Ok(object)
}

/// A bridge's answer to one action: the tool output and the receipt that goes with it.
#[derive(Debug, Clone, PartialEq)]
pub struct BridgeReport {
    /// The Tool's own output fields.
    pub output: Value,
    /// What the bridge says it did.
    pub receipt: ActionReceipt,
}

/// A settled action as the runtime hands it back: output, a receipt it re-derived, and what the ledger
/// should record.
#[derive(Debug, Clone, PartialEq)]
pub struct ComputerActionResult {
    /// The Tool's own output fields.
    pub output: Value,
    /// The receipt after the runtime's re-derivation.
    pub receipt: ActionReceipt,
}

impl ComputerActionResult {
    /// How the Effect Ledger settles this action.
    #[must_use]
    pub const fn ledger_outcome(&self) -> LedgerOutcome {
        self.receipt.ledger_outcome()
    }

    /// The Tool result a model is shown: the output object with the receipt added as `receipt`.
    ///
    /// # Errors
    /// Returns [`ReceiptError::Malformed`] when the output is not an object, or already has a `receipt`
    /// key (a bridge may not write the runtime's field).
    pub fn tool_result(&self) -> Result<Value, ReceiptError> {
        let mut object = self
            .output
            .as_object()
            .cloned()
            .ok_or_else(|| ReceiptError::Malformed("a tool output must be an object".into()))?;
        if object.contains_key("receipt") {
            return Err(ReceiptError::Malformed(
                "a tool output may not carry its own receipt".into(),
            ));
        }
        object.insert("receipt".to_string(), self.receipt.to_json());
        Ok(Value::Object(object))
    }
}
