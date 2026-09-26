//! Routine fires executed beside the scheduling pass (CORE-008).
//!
//! A pass claims due routine windows — quick bookkeeping — and wakes the executor; the executor
//! leases the claimed fires and runs each as its own task, at most [`DEFAULT_FIRE_EXECUTORS`] at
//! once per process. A fire's turn can take as long as a model takes, so executing it inside the
//! pass would hold back every window that falls due meanwhile (QA-009 measured a second of delay
//! before a claim). The fire ledger's lease keeps two executors — or two instances — from running
//! one fire, and an expired lease (a crashed process) makes the fire due again.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use sqlx::PgPool;
use tokio::sync::{Notify, Semaphore};

use super::dispatch::RunDispatch;
use super::routine::RoutineScheduler;
use crate::control::routines::FireLedger;

/// Fires one process executes at once.
pub const DEFAULT_FIRE_EXECUTORS: usize = 16;

/// Executes leased routine fires as bounded, concurrent tasks.
#[derive(Clone)]
pub struct FireExecutor {
    pool: PgPool,
    dispatch: Arc<dyn RunDispatch>,
    permits: Arc<Semaphore>,
    wake: Arc<Notify>,
}

impl FireExecutor {
    /// An executor over the pool, running fires through `dispatch`, `concurrency` at a time.
    #[must_use]
    pub fn new(pool: PgPool, dispatch: Arc<dyn RunDispatch>, concurrency: usize) -> Self {
        Self {
            pool,
            dispatch,
            permits: Arc::new(Semaphore::new(concurrency.max(1))),
            wake: Arc::new(Notify::new()),
        }
    }

    /// What a scheduling pass signals after it claims fires.
    #[must_use]
    pub fn waker(&self) -> Arc<Notify> {
        Arc::clone(&self.wake)
    }

    /// Run forever: start due fires whenever a pass wakes the executor, and at least every
    /// `interval` (for retries after backoff and leases that expired).
    pub async fn run(self, interval: Duration) {
        loop {
            tokio::select! {
                () = self.wake.notified() => {}
                () = tokio::time::sleep(interval) => {}
            }
            if let Err(message) = self.drain().await {
                crate::observability::notice(
                    crate::observability::Level::Warn,
                    &format!("routine fire executor: {message}"),
                );
            }
        }
    }

    /// Lease every due fire there is a free executor for, across tenants, and start each as a
    /// task. Returns how many started; the tasks finish on their own.
    ///
    /// # Errors
    /// Returns a message when tenants cannot be listed or fires cannot be leased.
    pub async fn drain(&self) -> Result<usize, String> {
        let tenants: Vec<String> = sqlx::query_scalar("SELECT background_tenants()")
            .fetch_all(&self.pool)
            .await
            .map_err(|error| format!("cannot list tenants: {error}"))?;
        let mut started = 0;
        for tenant in tenants {
            let free = self.permits.available_permits();
            if free == 0 {
                break;
            }
            let scheduler = RoutineScheduler::new(self.pool.clone(), tenant.clone())
                .map_err(|error| format!("scheduler for {tenant}: {error}"))?;
            let ledger = FireLedger::new(self.pool.clone(), tenant.clone());
            let fires = ledger
                .lease_due(Utc::now(), i64::try_from(free).unwrap_or(i64::MAX))
                .await
                .map_err(|error| format!("leasing fires for {tenant}: {error}"))?;
            for fire in fires {
                let Ok(permit) = Arc::clone(&self.permits).acquire_owned().await else {
                    return Ok(started);
                };
                let (scheduler, ledger, dispatch) = (
                    scheduler.clone(),
                    ledger.clone(),
                    Arc::clone(&self.dispatch),
                );
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(error) = scheduler
                        .execute_fire(&ledger, dispatch.as_ref(), fire, Utc::now())
                        .await
                    {
                        crate::observability::notice(
                            crate::observability::Level::Warn,
                            &format!("routine fire: {error}"),
                        );
                    }
                });
                started += 1;
            }
        }
        Ok(started)
    }
}
