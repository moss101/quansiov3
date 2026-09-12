//! Generation, revision, sequence and lease-fence primitives (DOMAIN.md §1.2, §8.3).
//!
//! These are the replay-safety counters the runtime uses to reject stale work: a
//! message from an old worker, an old turn or an old controller generation must never
//! mutate state or dispatch an effect.

use core::fmt;
use core::str::FromStr;

use crate::error::CoreError;
use crate::id::CanonicalId;

/// Monotonic fencing number for a Run, AgentThread or ExecutionTarget controller.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct Generation(u64);

impl Generation {
    /// The first generation of any entity.
    pub const INITIAL: Self = Self(1);

    /// Build a generation from a raw counter value. Generation `0` is not a valid
    /// state: every entity starts at [`Generation::INITIAL`].
    ///
    /// # Errors
    /// Returns [`CoreError::StaleGeneration`] when `value` is zero.
    pub fn new(value: u64) -> Result<Self, CoreError> {
        if value == 0 {
            return Err(CoreError::StaleGeneration {
                received: 0,
                current: Generation::INITIAL.0,
            });
        }
        Ok(Self(value))
    }

    /// The raw counter value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// The next generation.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }

    /// Reject a message whose generation is older than this one.
    ///
    /// # Errors
    /// Returns [`CoreError::StaleGeneration`] when `received` is behind `self`.
    pub fn accept(self, received: Self) -> Result<Self, CoreError> {
        if received.0 < self.0 {
            return Err(CoreError::StaleGeneration {
                received: received.0,
                current: self.0,
            });
        }
        Ok(received)
    }
}

impl fmt::Display for Generation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Compare-and-set revision of a graph aggregate (DOMAIN.md §1.2 `revision`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct Revision(u64);

impl Revision {
    /// The revision of a newly created aggregate.
    pub const INITIAL: Self = Self(1);

    /// Build a revision from a raw value.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// The raw value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Verify that a mutation was issued against this revision and return the next one.
    ///
    /// # Errors
    /// Returns [`CoreError::StaleGeneration`] when the caller's `expected` revision is
    /// not the current revision, which the API surfaces as `CONFLICT_REVISION`.
    pub fn check_and_bump(self, expected: Self) -> Result<Self, CoreError> {
        if expected != self {
            return Err(CoreError::StaleGeneration {
                received: expected.0,
                current: self.0,
            });
        }
        Ok(Self(self.0 + 1))
    }
}

impl fmt::Display for Revision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Tenant-monotonic ordering of `RuntimeEvent`s, assigned inside the commit
/// transaction (DOMAIN.md §1.2, §9.1).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct Sequence(i64);

impl Sequence {
    /// The first sequence in a tenant stream.
    pub const FIRST: Self = Self(1);

    /// Build a sequence from a raw value.
    ///
    /// # Errors
    /// Returns [`CoreError::StaleGeneration`] when `value` is not positive.
    pub fn new(value: i64) -> Result<Self, CoreError> {
        if value < 1 {
            return Err(CoreError::StaleGeneration {
                received: 0,
                current: Sequence::FIRST.0 as u64,
            });
        }
        Ok(Self(value))
    }

    /// The raw value.
    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }

    /// The next sequence.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl fmt::Display for Sequence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// `(lease_id, generation)` pair carried on every worker→server message. qworkerd
/// rejects any action whose fence does not match its current lease (DOMAIN.md §8.3).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FenceToken {
    lease_id: CanonicalId,
    generation: Generation,
}

impl FenceToken {
    /// Build a fence token.
    #[must_use]
    pub const fn new(lease_id: CanonicalId, generation: Generation) -> Self {
        Self {
            lease_id,
            generation,
        }
    }

    /// The lease this token fences.
    #[must_use]
    pub const fn lease_id(&self) -> &CanonicalId {
        &self.lease_id
    }

    /// The generation at which the lease was granted.
    #[must_use]
    pub const fn generation(&self) -> Generation {
        self.generation
    }

    /// Verify the token against the lease the runtime currently holds.
    ///
    /// The lease identity and the generation must both match exactly: a message from a
    /// previous lease holder, or from a newer generation the runtime has not granted,
    /// is rejected rather than silently accepted (DOMAIN.md §8.3).
    ///
    /// # Errors
    /// Returns [`CoreError::FenceMismatch`] when the lease or generation differs.
    pub fn accept(&self, lease_id: &CanonicalId, generation: Generation) -> Result<(), CoreError> {
        if &self.lease_id != lease_id || self.generation != generation {
            return Err(CoreError::FenceMismatch {
                token: self.to_string(),
                lease_id: lease_id.to_string(),
            });
        }
        Ok(())
    }
}

impl fmt::Display for FenceToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.lease_id, self.generation)
    }
}

impl FromStr for FenceToken {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (lease, generation) = s
            .rsplit_once(':')
            .ok_or_else(|| CoreError::InvalidFenceToken(s.to_string()))?;
        let lease_id = CanonicalId::parse_typed(lease, crate::id::Prefix::Lease)
            .map_err(|_| CoreError::InvalidFenceToken(s.to_string()))?;
        let value: u64 = generation
            .parse()
            .map_err(|_| CoreError::InvalidFenceToken(s.to_string()))?;
        let generation =
            Generation::new(value).map_err(|_| CoreError::InvalidFenceToken(s.to_string()))?;
        Ok(Self::new(lease_id, generation))
    }
}
