//! Autonomy modes: how much an agent may do before a person is asked (RUN-006, DOMAIN.md §7.4).
//!
//! A mode is a *setting* (`autonomy_mode`, layered like the other security controls: platform, tenant, workspace, user, the
//! strictest layer wins) that the policy evaluator applies after the policy rules. It only ever narrows: it can deny what
//! policy would allow or ask where policy would allow, never the reverse, and it cannot loosen tier 3 or tier 4, which the
//! rules and receipts already guard.
//!
//! | mode | effect |
//! |---|---|
//! | `standard` | policy and user rules as written (the default) |
//! | `ask_for_consequences` | an effect of tier 2 or above asks, whatever a standing rule said |
//! | `ask_every_action` | every effect above a read asks, whatever a standing rule said |
//! | `read_only` | nothing above a read is allowed at all |

use serde_json::Value;
use sqlx::PgPool;

/// How much an agent may do before a person is asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum AutonomyMode {
    /// Policy and user rules as written.
    #[default]
    Standard,
    /// Effects of tier 2 and above ask.
    AskForConsequences,
    /// Every effect above a read asks.
    AskEveryAction,
    /// Reads only.
    ReadOnly,
}

impl AutonomyMode {
    /// Every mode, least restrictive first.
    pub const ALL: [Self; 4] = [
        Self::Standard,
        Self::AskForConsequences,
        Self::AskEveryAction,
        Self::ReadOnly,
    ];

    /// The setting's spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::AskForConsequences => "ask_for_consequences",
            Self::AskEveryAction => "ask_every_action",
            Self::ReadOnly => "read_only",
        }
    }

    /// Parse the setting's spelling.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.as_str() == text)
    }

    /// The more restrictive of two modes.
    #[must_use]
    pub fn strictest(self, other: Self) -> Self {
        self.max(other)
    }

    /// Whether an effect of this tier is refused outright.
    #[must_use]
    pub const fn denies(self, tier: u8) -> bool {
        matches!(self, Self::ReadOnly) && tier >= 1
    }

    /// Whether an effect of this tier asks, whatever policy and standing rules said.
    #[must_use]
    pub const fn asks(self, tier: u8) -> bool {
        match self {
            Self::Standard | Self::ReadOnly => false,
            Self::AskForConsequences => tier >= 2,
            Self::AskEveryAction => tier >= 1,
        }
    }
}

/// The mode in force for an actor in a workspace: the strictest `autonomy_mode` of the tenant, workspace and user layers.
///
/// A layer that holds something that is not a mode reads as `read_only`, not as `standard`: a damaged setting never widens
/// what an agent may do.
///
/// # Errors
/// The database error; the caller fails closed.
pub async fn effective_mode(
    pool: &PgPool,
    tenant_id: &str,
    workspace_id: &str,
    user_id: Option<&str>,
) -> Result<AutonomyMode, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('quansio.tenant_id', $1, true)")
        .bind(tenant_id)
        .execute(&mut *tx)
        .await?;
    let tenant: Value = sqlx::query_scalar("SELECT settings FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .fetch_one(&mut *tx)
        .await?;
    let workspace: Value =
        sqlx::query_scalar("SELECT settings FROM workspaces WHERE id = $1 AND tenant_id = $2")
            .bind(workspace_id)
            .bind(tenant_id)
            .fetch_one(&mut *tx)
            .await?;
    let user: Option<Value> = match user_id {
        Some(user_id) => {
            sqlx::query_scalar("SELECT settings FROM users WHERE id = $1")
                .bind(user_id)
                .fetch_optional(&mut *tx)
                .await?
        }
        None => None,
    };
    tx.commit().await?;
    Ok([Some(tenant), Some(workspace), user]
        .into_iter()
        .flatten()
        .fold(AutonomyMode::Standard, |mode, layer| {
            mode.strictest(mode_of(&layer))
        }))
}

/// What one settings layer says: `standard` when it says nothing, `read_only` when it says something that is not a mode.
#[must_use]
pub fn mode_of(layer: &Value) -> AutonomyMode {
    match layer.get("autonomy_mode") {
        None | Some(Value::Null) => AutonomyMode::Standard,
        Some(value) => value
            .as_str()
            .and_then(AutonomyMode::parse)
            .unwrap_or(AutonomyMode::ReadOnly),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn modes_only_narrow_and_the_strictest_layer_wins() {
        assert!(AutonomyMode::Standard < AutonomyMode::AskForConsequences);
        assert!(AutonomyMode::AskForConsequences < AutonomyMode::AskEveryAction);
        assert!(AutonomyMode::AskEveryAction < AutonomyMode::ReadOnly);
        assert_eq!(
            AutonomyMode::Standard.strictest(AutonomyMode::AskEveryAction),
            AutonomyMode::AskEveryAction
        );
        for tier in 0..=4 {
            assert!(!AutonomyMode::Standard.asks(tier) && !AutonomyMode::Standard.denies(tier));
            // A read is never asked about or refused, in any mode.
            if tier == 0 {
                assert!(AutonomyMode::ALL.iter().all(|m| !m.asks(0) && !m.denies(0)));
            }
        }
        assert!(
            !AutonomyMode::AskForConsequences.asks(1) && AutonomyMode::AskForConsequences.asks(2)
        );
        assert!(AutonomyMode::AskEveryAction.asks(1) && !AutonomyMode::AskEveryAction.denies(4));
        assert!(AutonomyMode::ReadOnly.denies(1) && AutonomyMode::ReadOnly.denies(4));
        for mode in AutonomyMode::ALL {
            assert_eq!(AutonomyMode::parse(mode.as_str()), Some(mode));
        }
    }

    #[test]
    fn a_damaged_layer_reads_as_the_strictest_mode_and_a_missing_one_as_standard() {
        assert_eq!(mode_of(&json!({})), AutonomyMode::Standard);
        assert_eq!(
            mode_of(&json!({"autonomy_mode": null})),
            AutonomyMode::Standard
        );
        assert_eq!(
            mode_of(&json!({"autonomy_mode": "ask_every_action"})),
            AutonomyMode::AskEveryAction
        );
        for damaged in [json!("anything"), json!(3), json!(true), json!({"a": 1})] {
            assert_eq!(
                mode_of(&json!({ "autonomy_mode": damaged })),
                AutonomyMode::ReadOnly
            );
        }
    }
}
