//! The durable half of the egress broker: the network policy a target is bound to, and the grants
//! issued against it.
//!
//! Two ownership boundaries are respected here. `policies` and `execution_targets` belong to RUN-006
//! and EXEC-001, so this store only ever *reads* them; `egress_grants` is EXEC-008's own table and is
//! the only thing written. A policy change is therefore not this module's to perform — what this
//! module guarantees is the consequence: the moment `policies.version` moves, every grant issued under
//! the previous revision is refused, because a grant records the revision it was issued under and the
//! decision compares it against the policy's current one.
//!
//! Every statement carries an explicit `tenant_id = $n` predicate as well as relying on the table's
//! forced row-level security, because the test DSN is a superuser and RLS is bypassed there: a missing
//! predicate would be invisible to the isolation suite otherwise.

use sqlx::{Acquire, PgConnection, Row};

use super::{Decision, DecisionRecord, EgressBroker, EgressRequest, NetworkPolicy};

/// A grant: a destination an execution target may reach, under one policy revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressGrant {
    /// The `network.egress.new_destination` effect that authorized it, which is also its identity.
    pub effect_id: String,
    /// The tenant that owns it.
    pub tenant_id: String,
    /// The target it applies to.
    pub target_id: String,
    /// The target generation it was issued under.
    pub target_generation: i64,
    /// The capability it is narrowed to, when it is.
    pub capability_id: Option<String>,
    /// The policy it was issued under.
    pub policy_id: String,
    /// The policy revision it was issued under; the fence.
    pub policy_version: i32,
    /// The canonical lowercase host.
    pub host: String,
    /// The port.
    pub port: u16,
    /// When it was issued (canonical ISO-8601 UTC).
    pub issued_at: String,
    /// When it stops being usable (canonical ISO-8601 UTC).
    pub expires_at: String,
    /// When it was revoked, if it was.
    pub revoked_at: Option<String>,
}

/// The grant a caller asks to issue. The policy and its revision are not part of the request: they are
/// read from the target's policy inside the issuing transaction, so a grant can never pin a revision
/// the policy was not at when the grant was written.
#[derive(Debug, Clone)]
pub struct NewGrant<'a> {
    /// The authorizing effect.
    pub effect_id: &'a str,
    /// The tenant.
    pub tenant_id: &'a str,
    /// The target.
    pub target_id: &'a str,
    /// The target generation.
    pub target_generation: i64,
    /// The capability, when narrowed.
    pub capability_id: Option<&'a str>,
    /// The destination host.
    pub host: &'a str,
    /// The destination port.
    pub port: u16,
    /// When the grant starts (canonical ISO-8601 UTC).
    pub issued_at: &'a str,
    /// When it stops (canonical ISO-8601 UTC).
    pub expires_at: &'a str,
}

/// What an issuing attempt did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOutcome {
    /// The grant was written.
    Installed(Box<EgressGrant>),
    /// The authorizing effect already had a grant, so nothing was written: the Effect Ledger's
    /// idempotency, not a second rule.
    AlreadyInstalled(Box<EgressGrant>),
}

impl InstallOutcome {
    /// The grant, however it got there.
    #[must_use]
    pub fn grant(&self) -> &EgressGrant {
        match self {
            Self::Installed(grant) | Self::AlreadyInstalled(grant) => grant,
        }
    }

    /// Whether this call wrote the row.
    #[must_use]
    pub const fn installed(&self) -> bool {
        matches!(self, Self::Installed(_))
    }
}

/// Why an egress operation failed. Every database failure is mapped, so a constraint violation never
/// escapes as a raw driver error.
#[derive(Debug, thiserror::Error)]
pub enum EgressError {
    /// The database refused or was unreachable.
    #[error("egress store database error: {0}")]
    Database(#[from] sqlx::Error),
    /// The tenant has no such execution target.
    #[error("execution target {0} does not exist for this tenant")]
    TargetNotFound(String),
    /// The target is not bound to a network policy, or the policy is gone.
    #[error("execution target {0} has no network policy")]
    PolicyMissing(String),
    /// A network rule could not be understood.
    #[error("the network policy rule is not one the domain defines: {detail}")]
    PolicyRuleInvalid {
        /// What is wrong with it.
        detail: String,
    },
    /// The host is not a canonical host.
    #[error("{0:?} is not a canonical lowercase host")]
    HostInvalid(String),
    /// The grant window does not move forward.
    #[error("grant expiry {expires_at} is not after its issue {issued_at}")]
    WindowInvalid {
        /// The issue instant.
        issued_at: String,
        /// The expiry instant.
        expires_at: String,
    },
    /// The authorizing effect belongs to another tenant or target.
    #[error("effect {effect_id} is already a grant for a different tenant or target")]
    GrantConflict {
        /// The effect.
        effect_id: String,
    },
}

/// The egress broker's durable operations.
pub struct EgressStore;

impl EgressStore {
    /// Read the network policy an execution target is bound to.
    ///
    /// # Errors
    /// Returns [`EgressError::TargetNotFound`] when the tenant has no such target,
    /// [`EgressError::PolicyMissing`] when it is bound to none (or the policy is gone), and
    /// [`EgressError::PolicyRuleInvalid`] when its network rule cannot be understood.
    pub async fn policy_for(
        conn: &mut PgConnection,
        tenant_id: &str,
        target_id: &str,
    ) -> Result<NetworkPolicy, EgressError> {
        let target = sqlx::query(
            "SELECT network_policy_id FROM execution_targets WHERE id = $1 AND tenant_id = $2",
        )
        .bind(target_id)
        .bind(tenant_id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| EgressError::TargetNotFound(target_id.to_string()))?;
        let policy_id: Option<String> = target.try_get("network_policy_id")?;
        let policy_id =
            policy_id.ok_or_else(|| EgressError::PolicyMissing(target_id.to_string()))?;

        let row =
            sqlx::query("SELECT id, version, rules FROM policies WHERE id = $1 AND tenant_id = $2")
                .bind(&policy_id)
                .bind(tenant_id)
                .fetch_optional(&mut *conn)
                .await?
                .ok_or_else(|| EgressError::PolicyMissing(target_id.to_string()))?;
        let version: i32 = row.try_get("version")?;
        let rules: serde_json::Value = row.try_get("rules")?;
        NetworkPolicy::from_rules(&policy_id, version, &rules)
    }

    /// Decide whether a request may reach its destination, and return the log line with it.
    ///
    /// This is the shipped entry point: it reads the target's policy and grants and applies the
    /// broker's rules to them, so a caller cannot reach a destination by holding its own rules.
    ///
    /// # Errors
    /// Returns [`EgressError`] when the policy or the target's grants cannot be read. A destination the
    /// broker refuses is a successful `Decision::Deny`, not an error: the request was answered.
    pub async fn decide(
        conn: &mut PgConnection,
        request: &EgressRequest<'_>,
    ) -> Result<(Decision, DecisionRecord), EgressError> {
        let policy = Self::policy_for(conn, request.tenant_id, request.target_id).await?;
        let grants = Self::grants_for(conn, request.tenant_id, request.target_id).await?;
        let decision = match EgressBroker::decide(request, &policy, &grants) {
            Ok(decision) => decision,
            Err(malformed) => Decision::Deny(malformed),
        };
        let record = EgressBroker::record(request, &decision, request.at);
        Ok((decision, record))
    }

    /// The grants a target holds, including revoked and expired ones: the decision needs them to
    /// explain *why* it refused, and a caller that only saw live grants could not tell a destination
    /// nobody granted from one whose grant lapsed.
    ///
    /// # Errors
    /// Returns [`EgressError::Database`] when the read fails.
    pub async fn grants_for(
        conn: &mut PgConnection,
        tenant_id: &str,
        target_id: &str,
    ) -> Result<Vec<EgressGrant>, EgressError> {
        let rows = sqlx::query(
            "SELECT effect_id, tenant_id, target_id, target_generation, capability_id, policy_id, \
                    policy_version, host, port, \
                    to_char(issued_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS issued_at, \
                    to_char(expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS expires_at, \
                    to_char(revoked_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS revoked_at \
             FROM egress_grants WHERE tenant_id = $1 AND target_id = $2 ORDER BY effect_id",
        )
        .bind(tenant_id)
        .bind(target_id)
        .fetch_all(&mut *conn)
        .await?;
        rows.iter().map(row_to_grant).collect()
    }

    /// Issue a grant for a destination, pinning the target's current policy revision.
    ///
    /// The policy row is locked `FOR UPDATE` while the grant is written, so a concurrent policy change
    /// either lands before the grant (which then pins the new revision) or after it (which fences the
    /// grant). There is no interleaving in which a grant is live under a revision the policy was not
    /// at when the grant was written.
    ///
    /// # Errors
    /// Returns [`EgressError::HostInvalid`] or [`EgressError::WindowInvalid`] for a request the schema
    /// would reject — refused here with a named rule rather than as a constraint violation — and
    /// [`EgressError::GrantConflict`] when the authorizing effect already carries a grant for another
    /// tenant or target.
    pub async fn issue(
        conn: &mut PgConnection,
        grant: &NewGrant<'_>,
    ) -> Result<InstallOutcome, EgressError> {
        if grant.host.trim().is_empty() || grant.host != grant.host.to_lowercase() {
            return Err(EgressError::HostInvalid(grant.host.to_string()));
        }
        if grant.expires_at <= grant.issued_at {
            return Err(EgressError::WindowInvalid {
                issued_at: grant.issued_at.to_string(),
                expires_at: grant.expires_at.to_string(),
            });
        }

        let mut tx = conn.begin().await?;

        // The authorizing effect is the grant's identity, so this is checked before the target is even
        // looked at: reusing a settled effect for another target is a conflict whatever policy the new
        // target has, and an existing grant is returned rather than rewritten, because a settled effect
        // does not acquire a second meaning.
        if let Some(existing) =
            Self::grant_for_effect(&mut tx, grant.tenant_id, grant.effect_id).await?
        {
            let outcome = if existing.target_id == grant.target_id
                && existing.target_generation == grant.target_generation
            {
                InstallOutcome::AlreadyInstalled(Box::new(existing))
            } else {
                return Err(EgressError::GrantConflict {
                    effect_id: grant.effect_id.to_string(),
                });
            };
            tx.commit().await?;
            return Ok(outcome);
        }

        // The policy and its revision, locked so the pin cannot race a change.
        let target = sqlx::query(
            "SELECT network_policy_id FROM execution_targets WHERE id = $1 AND tenant_id = $2 FOR UPDATE",
        )
        .bind(grant.target_id)
        .bind(grant.tenant_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| EgressError::TargetNotFound(grant.target_id.to_string()))?;
        let policy_id: Option<String> = target.try_get("network_policy_id")?;
        let policy_id =
            policy_id.ok_or_else(|| EgressError::PolicyMissing(grant.target_id.to_string()))?;
        let policy_row =
            sqlx::query("SELECT version FROM policies WHERE id = $1 AND tenant_id = $2 FOR UPDATE")
                .bind(&policy_id)
                .bind(grant.tenant_id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(|| EgressError::PolicyMissing(grant.target_id.to_string()))?;
        let policy_version: i32 = policy_row.try_get("version")?;

        sqlx::query(
            "INSERT INTO egress_grants (effect_id, tenant_id, target_id, target_generation, \
                                        capability_id, policy_id, policy_version, host, port, \
                                        issued_at, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10::timestamptz, $11::timestamptz)",
        )
        .bind(grant.effect_id)
        .bind(grant.tenant_id)
        .bind(grant.target_id)
        .bind(grant.target_generation)
        .bind(grant.capability_id)
        .bind(&policy_id)
        .bind(policy_version)
        .bind(grant.host)
        .bind(i32::from(grant.port))
        .bind(grant.issued_at)
        .bind(grant.expires_at)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            // The effect id is the primary key, so a duplicate means the effect is already a grant --
            // possibly another tenant's, which the row-level-security-scoped lookup above could not
            // see. A unique violation is that conflict, not an opaque driver error.
            if is_unique_violation(&error) {
                EgressError::GrantConflict {
                    effect_id: grant.effect_id.to_string(),
                }
            } else {
                EgressError::Database(error)
            }
        })?;

        let installed = Self::grant_for_effect(&mut tx, grant.tenant_id, grant.effect_id)
            .await?
            .ok_or_else(|| EgressError::TargetNotFound(grant.effect_id.to_string()))?;
        tx.commit().await?;
        Ok(InstallOutcome::Installed(Box::new(installed)))
    }

    /// Revoke a grant. Revocation is recorded rather than deleted, so a later request is refused with
    /// the reason and the audit trail survives.
    ///
    /// # Errors
    /// Returns [`EgressError::Database`] when the write fails. Revoking a grant that does not exist or
    /// is already revoked is a no-op, not an error: the caller asked for a safe state and it holds.
    pub async fn revoke(
        conn: &mut PgConnection,
        tenant_id: &str,
        effect_id: &str,
        at: &str,
    ) -> Result<(), EgressError> {
        sqlx::query(
            "UPDATE egress_grants SET revoked_at = $3::timestamptz \
             WHERE tenant_id = $1 AND effect_id = $2 AND revoked_at IS NULL",
        )
        .bind(tenant_id)
        .bind(effect_id)
        .bind(at)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    /// Revoke every grant a target holds, in one statement.
    ///
    /// This is the tool a policy owner or an operator reaches for when it wants grants gone *now*
    /// rather than fenced by a revision bump; it returns how many were revoked so a caller can report
    /// what it changed, and it leaves the rows in place for audit.
    ///
    /// # Errors
    /// Returns [`EgressError::Database`] when the write fails.
    pub async fn revoke_for_target(
        conn: &mut PgConnection,
        tenant_id: &str,
        target_id: &str,
        at: &str,
    ) -> Result<u64, EgressError> {
        let revoked = sqlx::query(
            "UPDATE egress_grants SET revoked_at = $3::timestamptz \
             WHERE tenant_id = $1 AND target_id = $2 AND revoked_at IS NULL",
        )
        .bind(tenant_id)
        .bind(target_id)
        .bind(at)
        .execute(&mut *conn)
        .await?
        .rows_affected();
        Ok(revoked)
    }

    async fn grant_for_effect(
        conn: &mut PgConnection,
        tenant_id: &str,
        effect_id: &str,
    ) -> Result<Option<EgressGrant>, EgressError> {
        let row = sqlx::query(
            "SELECT effect_id, tenant_id, target_id, target_generation, capability_id, policy_id, \
                    policy_version, host, port, \
                    to_char(issued_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS issued_at, \
                    to_char(expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS expires_at, \
                    to_char(revoked_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS revoked_at \
             FROM egress_grants WHERE tenant_id = $1 AND effect_id = $2",
        )
        .bind(tenant_id)
        .bind(effect_id)
        .fetch_optional(&mut *conn)
        .await?;
        row.as_ref().map(row_to_grant).transpose()
    }
}

/// Whether a database error is a unique-constraint violation (SQLSTATE 23505).
fn is_unique_violation(error: &sqlx::Error) -> bool {
    matches!(
        error,
        sqlx::Error::Database(database) if database.code().as_deref() == Some("23505")
    )
}

fn row_to_grant(row: &sqlx::postgres::PgRow) -> Result<EgressGrant, EgressError> {
    let port: i32 = row.try_get("port")?;
    Ok(EgressGrant {
        effect_id: row.try_get("effect_id")?,
        tenant_id: row.try_get("tenant_id")?,
        target_id: row.try_get("target_id")?,
        target_generation: row.try_get("target_generation")?,
        capability_id: row.try_get("capability_id")?,
        policy_id: row.try_get("policy_id")?,
        policy_version: row.try_get("policy_version")?,
        host: row.try_get("host")?,
        port: u16::try_from(port).map_err(|_| EgressError::HostInvalid(format!("port {port}")))?,
        issued_at: row.try_get("issued_at")?,
        expires_at: row.try_get("expires_at")?,
        revoked_at: row.try_get("revoked_at")?,
    })
}
