//! Computer-use: capability tiers, fail-closed app identity, and the takeover fence (EXEC-010).
//!
//! Native automation is the one place a model's action reaches a person's machine, so the rules live in
//! Rust and the native bridge only answers questions. Three of them, and each is enforced here:
//!
//! * **an app this runtime cannot name is refused, whatever the tier.** The identity of the foreground
//!   application is the thing the decision hangs on — "click" means nothing without knowing what is being
//!   clicked — so an unidentified foreground app is a refusal for every action, privileged or not. That is
//!   what "fails closed" has to mean: there is no tier for which guessing is safe.
//! * **a tier is a grant, not a request.** `computer.click`, `computer.type`, `computer.clipboard` and
//!   `computer.system_key` each need a grant at least as high as the action, so a run granted reading
//!   cannot type its way into a write.
//! * **a takeover cannot race with agent input.** [`store::ComputerControlStore`] serializes them in
//!   PostgreSQL: an action owns one exact ToolCall slot and a takeover only completes after it settles or
//!   is reconciled. The holder and generation survive process and worker restarts.
//!
//! The native side is `native/macos` (AX over Accessibility, input over `CGEvent`), which the Rust machine
//! module calls (DOSSIER.md §5). It is deliberately *not* in this crate: `#![forbid(unsafe_code)]` holds
//! here, and the privileged calls belong in the bridge that the operator grants the permission to.

use std::collections::BTreeSet;

pub mod store;

/// What an action does, in the order the tiers escalate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ComputerTier {
    /// Read the application's accessibility tree and its bounds. Observes only.
    Read,
    /// Post a click at a coordinate.
    Click,
    /// Type text into whatever holds focus.
    Type,
    /// Read or replace the clipboard, which is shared with every other application.
    Clipboard,
    /// Send a system key combination, which can do anything the keyboard can.
    SystemKey,
}

impl ComputerTier {
    /// Canonical name, as a grant selector and the audit log spell it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Click => "click",
            Self::Type => "type",
            Self::Clipboard => "clipboard",
            Self::SystemKey => "system_key",
        }
    }

    /// Parse the canonical name.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "read" => Some(Self::Read),
            "click" => Some(Self::Click),
            "type" => Some(Self::Type),
            "clipboard" => Some(Self::Clipboard),
            "system_key" => Some(Self::SystemKey),
            _ => None,
        }
    }

    /// The tool names that act at this tier.
    #[must_use]
    pub const fn tools(self) -> &'static [&'static str] {
        match self {
            Self::Read => &["computer.read"],
            Self::Click => &["computer.click"],
            Self::Type => &["computer.type"],
            Self::Clipboard => &["computer.clipboard"],
            Self::SystemKey => &["computer.system_key"],
        }
    }
}

/// The highest tier a run has been granted (`GrantComputerTier`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ComputerGrant {
    highest: ComputerTier,
}

impl ComputerGrant {
    /// A grant up to and including `highest`.
    #[must_use]
    pub const fn up_to(highest: ComputerTier) -> Self {
        Self { highest }
    }

    /// A grant that permits only observation.
    #[must_use]
    pub const fn read_only() -> Self {
        Self::up_to(ComputerTier::Read)
    }

    /// The highest tier granted.
    #[must_use]
    pub const fn highest(self) -> ComputerTier {
        self.highest
    }

    /// Whether this grant covers `tier`.
    #[must_use]
    pub const fn covers(self, tier: ComputerTier) -> bool {
        tier as u8 <= self.highest as u8
    }
}

/// What the system says an application is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppIdentity {
    /// The bundle identifier, which is what a known-app list matches on rather than a display name.
    pub bundle_id: String,
    /// The name a person would recognise.
    pub name: String,
    /// The process id it was observed at.
    pub pid: i32,
}

impl AppIdentity {
    /// An identity.
    #[must_use]
    pub fn new(bundle_id: impl Into<String>, name: impl Into<String>, pid: i32) -> Self {
        Self {
            bundle_id: bundle_id.into(),
            name: name.into(),
            pid,
        }
    }
}

/// The applications this runtime recognises, by bundle identifier.
///
/// An empty set recognises nothing, which is the honest default: a run that has been told nothing about
/// the machine it is on may not act on whatever happens to be in front.
#[derive(Debug, Clone, Default)]
pub struct KnownApps {
    bundle_ids: BTreeSet<String>,
}

impl KnownApps {
    /// No application is known.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// The given bundle identifiers are known.
    #[must_use]
    pub fn of(bundle_ids: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            bundle_ids: bundle_ids.into_iter().map(Into::into).collect(),
        }
    }

    /// Whether an application is recognised.
    #[must_use]
    pub fn recognises(&self, bundle_id: &str) -> bool {
        self.bundle_ids.contains(bundle_id)
    }

    /// How many are known.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bundle_ids.len()
    }

    /// Whether none are known.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bundle_ids.is_empty()
    }
}

/// Why a computer-use action was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    /// The foreground application is not one this runtime recognises.
    #[error("the foreground application {bundle_id:?} is not recognised, so no action may be taken on it")]
    UnknownApp {
        /// What the system reported, which is what an operator needs to add it.
        bundle_id: String,
    },
    /// The system reports no foreground application at all.
    #[error("there is no foreground application to act on")]
    NoForegroundApp,
    /// The run's grant does not reach the tier the action needs.
    #[error("the action needs {tier} and the run is granted {granted}")]
    TierNotGranted {
        /// The tier the action needs.
        tier: &'static str,
        /// The highest tier granted.
        granted: &'static str,
    },
}

/// An authorised action: enough to act, and to prove afterwards that it may.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authorized {
    /// The tier the action acts at.
    pub tier: ComputerTier,
    /// The application it may act on.
    pub app: AppIdentity,
    /// The fence generation it holds.
    pub generation: u64,
}

/// The policy: what may be done, to what, and by whom.
#[derive(Debug, Clone)]
pub struct ComputerUsePolicy {
    known: KnownApps,
    grant: ComputerGrant,
}

impl ComputerUsePolicy {
    /// A policy over a known-app set and a grant.
    #[must_use]
    pub const fn new(known: KnownApps, grant: ComputerGrant) -> Self {
        Self { known, grant }
    }

    /// The known-app set.
    #[must_use]
    pub const fn known(&self) -> &KnownApps {
        &self.known
    }

    /// The grant.
    #[must_use]
    pub const fn grant(&self) -> ComputerGrant {
        self.grant
    }

    /// Decide whether an action may be taken.
    ///
    /// The order is deliberate and fail-closed: there must be a foreground application this runtime
    /// recognises *before* the tier is considered, because the app is what the decision is about. A run
    /// granted every tier still may not act on an application nobody has vouched for.
    ///
    /// # Errors
    /// Returns [`Refusal::NoForegroundApp`] for a missing identity, [`Refusal::UnknownApp`] for one that is
    /// not recognised, and [`Refusal::TierNotGranted`] when the grant does not reach the tier.
    pub fn authorize(
        &self,
        tier: ComputerTier,
        foreground: Option<&AppIdentity>,
    ) -> Result<(), Refusal> {
        let app = foreground.ok_or(Refusal::NoForegroundApp)?;
        if !self.known.recognises(&app.bundle_id) {
            return Err(Refusal::UnknownApp {
                bundle_id: app.bundle_id.clone(),
            });
        }
        if !self.grant.covers(tier) {
            return Err(Refusal::TierNotGranted {
                tier: tier.as_str(),
                granted: self.grant.highest().as_str(),
            });
        }
        Ok(())
    }
}

/// Who holds the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineHolder {
    /// The agent.
    Agent,
    /// A person, after a takeover.
    User,
    /// Nobody may drive, for example while policy has paused the target.
    Nobody,
}

impl MachineHolder {
    /// Canonical name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::User => "user",
            Self::Nobody => "none",
        }
    }

    /// Parse the canonical persisted value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "agent" => Some(Self::Agent),
            "user" => Some(Self::User),
            "none" => Some(Self::Nobody),
            _ => None,
        }
    }
}
