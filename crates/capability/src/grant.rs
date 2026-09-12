//! Grants and their constraints (DOMAIN.md §6.1, §6.3, §7.1).
//!
//! A [`Grant`] is an effect class, the resource region it covers and the constraints
//! that bound it. Composition is **intersection** with constraints combined
//! most-restrictively; a constraint can only get tighter, never looser.

use core::fmt;
use core::str::FromStr;

use chrono::{DateTime, Utc};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::CapabilityError;
use crate::selector::ResourceSelector;

/// Highest consequence tier in DOMAIN.md §7.1.
pub const MAX_TIER: u8 = 4;

/// A validated effect class such as `message.send` (DOMAIN.md §7.1).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EffectClass(String);

impl EffectClass {
    /// Validate and build an effect class.
    ///
    /// # Errors
    /// Returns [`CapabilityError::MalformedGrant`] when the value is not a lowercase
    /// dotted identifier.
    pub fn parse(value: impl Into<String>) -> Result<Self, CapabilityError> {
        let value = value.into();
        let malformed =
            || CapabilityError::MalformedGrant(format!("invalid effect class '{value}'"));
        if value.is_empty()
            || value.starts_with('.')
            || value.ends_with('.')
            || value.contains("..")
        {
            return Err(malformed());
        }
        let valid = value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || byte == b'_'
                || byte == b'.'
                || byte == b'-'
        });
        if valid {
            Ok(Self(value))
        } else {
            Err(malformed())
        }
    }

    /// The canonical string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EffectClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for EffectClass {
    type Err = CapabilityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Serialize for EffectClass {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for EffectClass {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(D::Error::custom)
    }
}

/// A consequence tier 0–4 (DOMAIN.md §7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Tier(u8);

impl Tier {
    /// Build a tier.
    ///
    /// # Errors
    /// Returns [`CapabilityError::MalformedGrant`] when the value exceeds [`MAX_TIER`].
    pub fn new(value: u8) -> Result<Self, CapabilityError> {
        if value <= MAX_TIER {
            Ok(Self(value))
        } else {
            Err(CapabilityError::MalformedGrant(format!(
                "tier {value} exceeds the maximum {MAX_TIER}"
            )))
        }
    }

    /// The numeric tier.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// The tier used when a grant leaves `max_tier` unspecified: the least restrictive.
    #[must_use]
    pub const fn unbounded() -> Self {
        Self(MAX_TIER)
    }

    /// Rank an optional tier for most-restrictive combination (`None` is unbounded).
    #[must_use]
    pub const fn rank(tier: Option<Self>) -> u8 {
        match tier {
            Some(tier) => tier.0,
            None => MAX_TIER,
        }
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for Tier {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(self.0)
    }
}

impl<'de> Deserialize<'de> for Tier {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = u8::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

/// The approval a grant demands (DOMAIN.md §6.1, §6.3).
///
/// Order is `never < ask < always`: combination takes the most restrictive, so approval
/// only narrows toward `ask`/`never` from `always`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Approval {
    /// The effect requires an approval receipt.
    Ask,
    /// The effect may proceed under a valid standing rule.
    Always,
    /// The effect is refused.
    Never,
}

impl Approval {
    /// Restrictiveness rank; a smaller rank is more restrictive.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::Never => 0,
            Self::Ask => 1,
            Self::Always => 2,
        }
    }

    /// The canonical wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Always => "always",
            Self::Never => "never",
        }
    }

    /// The more restrictive of two approvals.
    #[must_use]
    pub const fn most_restrictive(self, other: Self) -> Self {
        if self.rank() <= other.rank() {
            self
        } else {
            other
        }
    }
}

impl Default for Approval {
    /// An unspecified approval is `ask`: fail closed, never silently `always`.
    fn default() -> Self {
        Self::Ask
    }
}

impl fmt::Display for Approval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A budget reference a grant is bounded by (DOMAIN.md §6.1, §13.2).
///
/// The canonical wire shape is the reference id string. `cap_units` is optional
/// resolution metadata supplied when a layer resolved the reference to its remaining
/// allowance; it lets "budget = smallest" be decided without inventing a second budget
/// store.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BudgetRef {
    /// The `bud_…` reference id.
    pub reference: String,
    /// Resolved remaining allowance, when the layer resolved the reference.
    pub cap_units: Option<u64>,
}

impl BudgetRef {
    /// A budget reference without a resolved allowance.
    #[must_use]
    pub fn new(reference: impl Into<String>) -> Self {
        Self {
            reference: reference.into(),
            cap_units: None,
        }
    }

    /// The more restrictive of two budget references: the smaller allowance wins, a
    /// resolved allowance beats an unresolved (unbounded) one, and equal allowances break
    /// the tie by reference id so the combination is deterministic and commutative.
    #[must_use]
    pub fn most_restrictive<'a>(first: &'a Self, second: &'a Self) -> &'a Self {
        match (first.cap_units, second.cap_units) {
            (Some(left), Some(right)) if left < right => first,
            (Some(left), Some(right)) if right < left => second,
            (Some(_), Some(_)) | (None, None) => {
                if first.reference <= second.reference {
                    first
                } else {
                    second
                }
            }
            (Some(_), None) => first,
            (None, Some(_)) => second,
        }
    }

    fn restrictiveness(&self) -> (u64, &str) {
        (self.cap_units.unwrap_or(u64::MAX), self.reference.as_str())
    }
}

impl fmt::Display for BudgetRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.reference)
    }
}

impl Serialize for BudgetRef {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.reference)
    }
}

impl<'de> Deserialize<'de> for BudgetRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Reference(String),
            Detailed {
                reference: String,
                #[serde(default)]
                cap_units: Option<u64>,
            },
        }
        match Wire::deserialize(deserializer)? {
            Wire::Reference(reference) => Ok(Self {
                reference,
                cap_units: None,
            }),
            Wire::Detailed {
                reference,
                cap_units,
            } => Ok(Self {
                reference,
                cap_units,
            }),
        }
    }
}

/// The constraints bounding a grant (DOMAIN.md §6.1).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct Constraints {
    /// Highest consequence tier the grant covers; `None` means unconstrained.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tier: Option<Tier>,
    /// The approval the grant demands.
    pub approval: Approval,
    /// Instant after which the grant is void.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// The budget the grant draws on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_ref: Option<BudgetRef>,
}

impl Default for Constraints {
    fn default() -> Self {
        Self {
            max_tier: None,
            approval: Approval::Ask,
            expires_at: None,
            budget_ref: None,
        }
    }
}

impl Constraints {
    /// Combine two constraint sets most-restrictively (DOMAIN.md §6.3):
    /// `max_tier = min`, approval toward `ask`/`never`, expiry = earliest, budget =
    /// smallest.
    #[must_use]
    pub fn combine(first: &Self, second: &Self) -> Self {
        Self {
            max_tier: min_tier(first.max_tier, second.max_tier),
            approval: first.approval.most_restrictive(second.approval),
            expires_at: earliest(first.expires_at, second.expires_at),
            budget_ref: match (&first.budget_ref, &second.budget_ref) {
                (Some(left), Some(right)) => Some(BudgetRef::most_restrictive(left, right).clone()),
                (Some(left), None) => Some(left.clone()),
                (None, Some(right)) => Some(right.clone()),
                (None, None) => None,
            },
        }
    }

    /// Whether `self` is at least as restrictive as `other` on every dimension.
    #[must_use]
    pub fn is_at_least_as_restrictive_as(&self, other: &Self) -> bool {
        Tier::rank(self.max_tier) <= Tier::rank(other.max_tier)
            && self.approval.rank() <= other.approval.rank()
            && expiry_rank(self.expires_at) <= expiry_rank(other.expires_at)
            && budget_rank(self.budget_ref.as_ref()) <= budget_rank(other.budget_ref.as_ref())
    }
}

fn min_tier(first: Option<Tier>, second: Option<Tier>) -> Option<Tier> {
    match (first, second) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(tier), None) | (None, Some(tier)) => Some(tier),
        (None, None) => None,
    }
}

fn earliest(first: Option<DateTime<Utc>>, second: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    match (first, second) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn expiry_rank(expires_at: Option<DateTime<Utc>>) -> (u8, i64) {
    match expires_at {
        Some(instant) => (0, instant.timestamp_millis()),
        None => (1, 0),
    }
}

fn budget_rank(budget: Option<&BudgetRef>) -> (u8, u64, &str) {
    match budget {
        Some(budget) => {
            let (cap, reference) = budget.restrictiveness();
            (0, cap, reference)
        }
        None => (1, 0, ""),
    }
}

/// A grant: effect class + resource selector + constraints (DOMAIN.md §6.1).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Grant {
    /// The semantic operation this grant authorizes.
    pub effect_class: EffectClass,
    /// The resource region it covers.
    pub resource: ResourceSelector,
    /// The constraints bounding it.
    pub constraints: Constraints,
}

impl Grant {
    /// Build a grant.
    #[must_use]
    pub fn new(
        effect_class: EffectClass,
        resource: ResourceSelector,
        constraints: Constraints,
    ) -> Self {
        Self {
            effect_class,
            resource,
            constraints,
        }
    }

    /// Parse a grant from its canonical JSON shape (DOMAIN.md §6.1).
    ///
    /// # Errors
    /// Returns [`CapabilityError::MalformedGrant`] when the value is not a canonical
    /// grant; a caller resolves this fail-closed rather than skipping the grant.
    pub fn from_json(value: &serde_json::Value) -> Result<Self, CapabilityError> {
        serde_json::from_value(value.clone())
            .map_err(|error| CapabilityError::MalformedGrant(error.to_string()))
    }

    /// The canonical JSON shape.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("a grant is JSON-serializable")
    }

    /// Whether `self` is at most as permissive as `other`: same effect class, a resource
    /// region contained by `other`, and constraints no looser than `other`'s.
    #[must_use]
    pub fn is_narrowing_of(&self, other: &Self) -> bool {
        self.effect_class == other.effect_class
            && other.resource.covers(&self.resource)
            && self
                .constraints
                .is_at_least_as_restrictive_as(&other.constraints)
    }

    /// The intersection of two grants: the narrower resource with constraints combined
    /// most-restrictively, or `None` when neither resource contains the other.
    #[must_use]
    pub fn intersect(&self, other: &Self) -> Option<Self> {
        if self.effect_class != other.effect_class {
            return None;
        }
        self.resource
            .intersect(&other.resource)
            .map(|resource| Self {
                effect_class: self.effect_class.clone(),
                resource,
                constraints: Constraints::combine(&self.constraints, &other.constraints),
            })
    }
}

impl fmt::Display for Grant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.effect_class, self.resource)
    }
}
