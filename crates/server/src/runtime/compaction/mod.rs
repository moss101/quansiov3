//! Compaction epochs and the staleness rule (INT-008, DOMAIN.md §5.7, DOSSIER.md §8).
//!
//! A compaction epoch is a *versioned summary of a range of a thread's event history*: it names the
//! sequence range it stands in for, the artifact that holds the summary and the model route that
//! produced it. This module owns the epoch's lifecycle — create, install, reject — and nothing else.
//!
//! Two properties are the point of it, and both are enforced here rather than described:
//!
//! * **A fork or revert never installs compaction from abandoned history.** An epoch may only be
//!   installed while the position the caller holds still contains the range it summarises and while it
//!   belongs to the run the caller is working on. A revert moves the position *back*, so an epoch whose
//!   range ends beyond it is refused; a fork leaves the epoch belonging to a different run, so it is
//!   refused. Either refusal records `rejected_stale` on the row — the epoch is never deleted, because
//!   the refusal is itself the auditable fact (the schema's status column already says so).
//! * **The decision is made inside the installing transaction.** The row is read `FOR UPDATE` and the
//!   checks run against durable state in the same unit of work that writes the status, so a concurrent
//!   revert cannot land between the decision and the write.
//!
//! What this module deliberately does **not** do: read or write protocol state, and appear in any
//! recovery path. Exact protocol replay reads the event log and the protocol state, never a summary —
//! "exact protocol replay does not depend on summary text" is a property that is maintained by *not*
//! being here, and RUN-009's declared recovery read set is where that is asserted.
//!
//! Staleness is decided from what the schema records: the epoch's run, the run's generation and the
//! caller's position. The table has no authoring-generation column (adding one would be a CORE-001
//! schema change, outside this task's paths), and it does not need one: a generation is a *fence* the
//! caller holds, so a mismatched fence is refused as a stale controller, while abandoned history is
//! identified by the run the epoch belongs to and by the position that still contains its range.

use sqlx::postgres::PgConnection;
use sqlx::Row;

use crate::control::schema::{set_tenant_context_conn, SchemaError};

/// The lifecycle state of an epoch (the schema's `status` column).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpochStatus {
    /// Created, not yet installed.
    Pending,
    /// Installed: the summary may stand in for its range in a model's context.
    Installed,
    /// Refused because the history it summarises is no longer the lineage the caller holds.
    RejectedStale,
}

impl EpochStatus {
    /// Canonical column value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Installed => "installed",
            Self::RejectedStale => "rejected_stale",
        }
    }

    /// Parse the canonical column value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "installed" => Some(Self::Installed),
            "rejected_stale" => Some(Self::RejectedStale),
            _ => None,
        }
    }
}

/// One compaction epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionEpoch {
    /// Epoch id (`cep_…`).
    pub id: String,
    /// Thread whose history it summarises.
    pub thread_id: String,
    /// Run that produced it, when it was produced inside a run.
    pub run_id: Option<String>,
    /// Per-thread ordinal (the schema stores it as `INTEGER`).
    pub seq: i32,
    /// First event sequence the summary stands in for.
    pub source_from_sequence: i64,
    /// Last event sequence the summary stands in for.
    pub source_to_sequence: i64,
    /// Artifact holding the summary text, when it has been written.
    pub summary_artifact_id: Option<String>,
    /// Estimated tokens the summary costs.
    pub token_estimate: i64,
    /// Lifecycle state.
    pub status: EpochStatus,
    /// Route that produced the summary.
    pub created_by_model_route_id: Option<String>,
}

impl CompactionEpoch {
    /// Whether this epoch may still stand in for the history a caller holds.
    ///
    /// The check is pure: `position` is the sequence the caller has committed to, and a range that
    /// ends beyond it summarises history the caller's lineage does not contain (which is what a revert
    /// creates).
    #[must_use]
    pub const fn covers_position(&self, position: i64) -> bool {
        self.source_to_sequence <= position
    }
}

/// Request to create an epoch.
#[derive(Debug, Clone)]
pub struct NewEpoch {
    /// Epoch id.
    pub id: String,
    /// Thread whose history it summarises.
    pub thread_id: String,
    /// Run that produced it.
    pub run_id: Option<String>,
    /// Per-thread ordinal (the schema stores it as `INTEGER`).
    pub seq: i32,
    /// First event sequence of the range.
    pub source_from_sequence: i64,
    /// Last event sequence of the range.
    pub source_to_sequence: i64,
    /// Artifact holding the summary.
    pub summary_artifact_id: Option<String>,
    /// Estimated tokens the summary costs.
    pub token_estimate: i64,
    /// Route that produced the summary.
    pub created_by_model_route_id: Option<String>,
}

/// What the installing caller holds: its run, its generation fence and its committed position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionFence {
    /// Run the caller is working on.
    pub run_id: String,
    /// Generation the caller holds; a newer controller has a higher one.
    pub generation: i64,
    /// Sequence the caller has committed to.
    pub position: i64,
}

/// The result of asking to install an epoch.
///
/// A stale refusal is deliberately **not** an [`CompactionError`]: a refusal about *history* is a decided
/// state the epoch is moved into (`rejected_stale`), and the schema has a status for it precisely because
/// the decision is durable. Returning it as an error would leave the caller's transaction to roll back —
/// discarding the very record of the refusal — so the caller receives the reason *and* the written epoch,
/// and commits.
///
/// The exception is a stale *generation*: a fenced-out caller is not the writer any more, so it writes
/// nothing at all and the epoch it handed back is unchanged. Writing a decision on the current
/// controller's behalf would be exactly the stale write fencing exists to prevent.
#[derive(Debug)]
pub enum InstallOutcome {
    /// The epoch was installed and may stand in for its range.
    Installed(CompactionEpoch),
    /// The epoch was refused because the history it summarises is not the caller's lineage.
    Rejected {
        /// The epoch, now carrying [`EpochStatus::RejectedStale`].
        epoch: CompactionEpoch,
        /// Why it was refused.
        refusal: CompactionError,
    },
}

/// Compaction errors.
#[derive(Debug, thiserror::Error)]
pub enum CompactionError {
    /// The database rejected the operation.
    #[error("compaction store: {0}")]
    Database(#[from] sqlx::Error),
    /// The tenant context could not be established.
    #[error("compaction store: {0}")]
    Schema(#[from] SchemaError),
    /// The epoch does not exist.
    #[error("compaction epoch {0} not found")]
    NotFound(String),
    /// The range is not a range.
    #[error("compaction epoch range {from}..{to} is not ordered")]
    RangeInvalid {
        /// First sequence.
        from: i64,
        /// Last sequence.
        to: i64,
    },
    /// Only a pending epoch may be installed.
    #[error("compaction epoch {id} is already {status}")]
    NotPending {
        /// Epoch id.
        id: String,
        /// Its current status.
        status: &'static str,
    },
    /// The epoch belongs to abandoned history: a fork left it on another run.
    #[error(
        "compaction epoch {epoch_id} belongs to run {epoch_run}, the caller works on {fence_run}"
    )]
    StaleRun {
        /// Epoch id.
        epoch_id: String,
        /// Run the epoch belongs to.
        epoch_run: String,
        /// Run the caller holds.
        fence_run: String,
    },
    /// The caller's fence is stale: another controller owns the run.
    #[error("run {run_id} is at generation {stored}, the caller holds {held}")]
    StaleGeneration {
        /// Run id.
        run_id: String,
        /// Generation recorded for the run.
        stored: i64,
        /// Generation the caller holds.
        held: i64,
    },
    /// The epoch summarises history beyond the position the caller's lineage contains.
    #[error(
        "compaction epoch {epoch_id} covers up to sequence {covers}, the caller holds {position}"
    )]
    StalePosition {
        /// Epoch id.
        epoch_id: String,
        /// Last sequence the epoch covers.
        covers: i64,
        /// Sequence the caller holds.
        position: i64,
    },
    /// The stored status is not a canonical status.
    #[error("unknown compaction epoch status: {0}")]
    UnknownStatus(String),
}

/// Durable compaction-epoch store.
pub struct CompactionStore;

impl CompactionStore {
    /// Record a new pending epoch.
    ///
    /// # Errors
    /// Returns [`CompactionError::RangeInvalid`] when the range is not ordered, and a database error
    /// when the write fails (including the schema's `UNIQUE (thread_id, seq)` conflict).
    pub async fn create(
        conn: &mut PgConnection,
        tenant_id: &str,
        epoch: &NewEpoch,
    ) -> Result<(), CompactionError> {
        if epoch.source_to_sequence < epoch.source_from_sequence {
            return Err(CompactionError::RangeInvalid {
                from: epoch.source_from_sequence,
                to: epoch.source_to_sequence,
            });
        }
        set_tenant_context_conn(conn, tenant_id).await?;
        sqlx::query(
            "INSERT INTO compaction_epochs (id, tenant_id, thread_id, run_id, seq, \
             source_from_sequence, source_to_sequence, summary_artifact_id, token_estimate, status, \
             created_by_model_route_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'pending', $10)",
        )
        .bind(&epoch.id)
        .bind(tenant_id)
        .bind(&epoch.thread_id)
        .bind(&epoch.run_id)
        .bind(epoch.seq)
        .bind(epoch.source_from_sequence)
        .bind(epoch.source_to_sequence)
        .bind(&epoch.summary_artifact_id)
        .bind(epoch.token_estimate)
        .bind(&epoch.created_by_model_route_id)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    /// Load one epoch.
    ///
    /// # Errors
    /// Returns [`CompactionError::NotFound`] when the tenant has no such epoch.
    pub async fn load(
        conn: &mut PgConnection,
        tenant_id: &str,
        epoch_id: &str,
    ) -> Result<CompactionEpoch, CompactionError> {
        set_tenant_context_conn(conn, tenant_id).await?;
        let row = sqlx::query(
            "SELECT id, thread_id, run_id, seq, source_from_sequence, source_to_sequence, \
             summary_artifact_id, token_estimate, status, created_by_model_route_id \
             FROM compaction_epochs WHERE id = $1",
        )
        .bind(epoch_id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| CompactionError::NotFound(epoch_id.to_string()))?;
        Self::row_to_epoch(&row)
    }

    /// Every epoch of a thread, oldest first.
    ///
    /// # Errors
    /// Returns a database error when the query fails or a stored status is unknown.
    pub async fn list(
        conn: &mut PgConnection,
        tenant_id: &str,
        thread_id: &str,
    ) -> Result<Vec<CompactionEpoch>, CompactionError> {
        set_tenant_context_conn(conn, tenant_id).await?;
        let rows = sqlx::query(
            "SELECT id, thread_id, run_id, seq, source_from_sequence, source_to_sequence, \
             summary_artifact_id, token_estimate, status, created_by_model_route_id \
             FROM compaction_epochs WHERE thread_id = $1 ORDER BY seq ASC",
        )
        .bind(thread_id)
        .fetch_all(&mut *conn)
        .await?;
        rows.iter().map(Self::row_to_epoch).collect()
    }

    /// Install an epoch, or record why it may not be.
    ///
    /// The row is read `FOR UPDATE` so the decision and the status write share one unit of work. Every
    /// refusal that means "this history is not the caller's lineage" writes `rejected_stale` before
    /// returning: the epoch is retained, because the refusal is the auditable fact.
    ///
    /// # Errors
    /// Returns [`CompactionError::NotPending`] when the epoch was already decided (a caller asked twice),
    /// and [`CompactionError::NotFound`] when it does not exist. A **stale refusal is not an error**: it
    /// is returned as [`InstallOutcome::Rejected`] with the epoch already moved to
    /// [`EpochStatus::RejectedStale`], so a caller that commits keeps the record of the refusal.
    pub async fn install(
        conn: &mut PgConnection,
        tenant_id: &str,
        epoch_id: &str,
        fence: &CompactionFence,
    ) -> Result<InstallOutcome, CompactionError> {
        set_tenant_context_conn(conn, tenant_id).await?;
        let row = sqlx::query(
            "SELECT id, thread_id, run_id, seq, source_from_sequence, source_to_sequence, \
             summary_artifact_id, token_estimate, status, created_by_model_route_id \
             FROM compaction_epochs WHERE id = $1 FOR UPDATE",
        )
        .bind(epoch_id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| CompactionError::NotFound(epoch_id.to_string()))?;
        let epoch = Self::row_to_epoch(&row)?;

        if epoch.status != EpochStatus::Pending {
            return Err(CompactionError::NotPending {
                id: epoch.id,
                status: epoch.status.as_str(),
            });
        }

        // A fork leaves the epoch on the run it was produced under; the caller works on another one.
        if let Some(epoch_run) = &epoch.run_id {
            if epoch_run != &fence.run_id {
                return Self::reject(
                    conn,
                    &epoch,
                    CompactionError::StaleRun {
                        epoch_id: epoch.id.clone(),
                        epoch_run: epoch_run.clone(),
                        fence_run: fence.run_id.clone(),
                    },
                )
                .await;
            }
        }

        // A generation is a fence: a controller holding an older one is no longer the writer, and a
        // run that has gone entirely is abandoned history.
        let stored_generation: Option<i64> =
            sqlx::query("SELECT generation FROM runs WHERE id = $1")
                .bind(&fence.run_id)
                .fetch_optional(&mut *conn)
                .await?
                .map(|row| row.try_get("generation"))
                .transpose()?;
        match stored_generation {
            None => {
                return Self::reject(
                    conn,
                    &epoch,
                    CompactionError::StaleRun {
                        epoch_id: epoch.id.clone(),
                        epoch_run: epoch.run_id.clone().unwrap_or_default(),
                        fence_run: fence.run_id.clone(),
                    },
                )
                .await;
            }
            Some(stored) if stored != fence.generation => {
                // Fenced out: the caller is no longer the writer, so it must not write a decision
                // either. The epoch is handed back untouched and stays pending for the current
                // controller to decide.
                return Ok(InstallOutcome::Rejected {
                    epoch,
                    refusal: CompactionError::StaleGeneration {
                        run_id: fence.run_id.clone(),
                        stored,
                        held: fence.generation,
                    },
                });
            }
            Some(_) => {}
        }

        // A revert moves the position back: a range ending beyond it summarises history the caller's
        // lineage no longer contains.
        if !epoch.covers_position(fence.position) {
            return Self::reject(
                conn,
                &epoch,
                CompactionError::StalePosition {
                    epoch_id: epoch.id.clone(),
                    covers: epoch.source_to_sequence,
                    position: fence.position,
                },
            )
            .await;
        }

        sqlx::query("UPDATE compaction_epochs SET status = 'installed' WHERE id = $1")
            .bind(&epoch.id)
            .execute(&mut *conn)
            .await?;
        Ok(InstallOutcome::Installed(CompactionEpoch {
            status: EpochStatus::Installed,
            ..epoch
        }))
    }

    /// Record that an epoch was refused because its history is not the caller's lineage.
    ///
    /// The epoch is retained with `rejected_stale` and handed back with the reason: the refusal is the
    /// auditable fact, and deleting the row would destroy it.
    async fn reject(
        conn: &mut PgConnection,
        epoch: &CompactionEpoch,
        refusal: CompactionError,
    ) -> Result<InstallOutcome, CompactionError> {
        sqlx::query("UPDATE compaction_epochs SET status = 'rejected_stale' WHERE id = $1")
            .bind(&epoch.id)
            .execute(&mut *conn)
            .await?;
        Ok(InstallOutcome::Rejected {
            epoch: CompactionEpoch {
                status: EpochStatus::RejectedStale,
                ..epoch.clone()
            },
            refusal,
        })
    }

    fn row_to_epoch(row: &sqlx::postgres::PgRow) -> Result<CompactionEpoch, CompactionError> {
        let status: String = row.try_get("status")?;
        Ok(CompactionEpoch {
            id: row.try_get("id")?,
            thread_id: row.try_get("thread_id")?,
            run_id: row.try_get("run_id")?,
            seq: row.try_get("seq")?,
            source_from_sequence: row.try_get("source_from_sequence")?,
            source_to_sequence: row.try_get("source_to_sequence")?,
            summary_artifact_id: row.try_get("summary_artifact_id")?,
            token_estimate: row.try_get("token_estimate")?,
            status: EpochStatus::parse(&status).ok_or(CompactionError::UnknownStatus(status))?,
            created_by_model_route_id: row.try_get("created_by_model_route_id")?,
        })
    }
}
