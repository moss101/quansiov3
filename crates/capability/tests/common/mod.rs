// Shared helpers for capability tests: deterministic sources plus database scratch setup.
#![allow(dead_code)]

//! Shared helpers for capability tests.
//!
//! Pure tests build projections through [`StaticSource`]; database-backed tests create
//! and drop their own scratch database, so a development database is never touched.
//! `QUANSIO_TEST_POSTGRES_URL` is the superuser DSN used to create the scratch
//! database; when it is unset the tests report `BLOCKED_EXTERNAL` and return, because
//! the local development stack (`scripts/dev/up`) is not running.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use quansio_capability::{
    Approval, BudgetRef, Constraints, EffectClass, Grant, InputUnavailableReason, Layer,
    LayerInput, ProjectionSource, ResourceSelector, Tier,
};
use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool};

/// A projection source built from fixed per-layer outcomes and a tier table.
#[derive(Debug, Clone, Default)]
pub struct StaticSource {
    /// Per-layer outcomes; a layer without an entry is `SourceUnavailable`.
    pub outcomes: BTreeMap<Layer, Result<LayerInput, InputUnavailableReason>>,
    /// Effect class → catalog tier.
    pub tiers: BTreeMap<String, u8>,
}

impl StaticSource {
    /// An empty source.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a successful layer fetch.
    #[must_use]
    pub fn with(mut self, layer: Layer, input: LayerInput) -> Self {
        self.outcomes.insert(layer, Ok(input));
        self
    }

    /// Record a failing layer fetch.
    #[must_use]
    pub fn failing(mut self, layer: Layer, reason: InputUnavailableReason) -> Self {
        self.outcomes.insert(layer, Err(reason));
        self
    }

    /// Record a catalog tier.
    #[must_use]
    pub fn tier(mut self, effect_class: &str, tier: u8) -> Self {
        self.tiers.insert(effect_class.to_string(), tier);
        self
    }

    /// Record a source-unavailable failure for a layer.
    #[must_use]
    pub fn unavailable(self, layer: Layer) -> Self {
        self.failing(
            layer,
            InputUnavailableReason::SourceUnavailable {
                detail: format!("{layer} source unreachable"),
            },
        )
    }
}

impl ProjectionSource for StaticSource {
    fn fetch(&self, layer: Layer) -> Result<LayerInput, InputUnavailableReason> {
        self.outcomes.get(&layer).cloned().unwrap_or_else(|| {
            Err(InputUnavailableReason::SourceUnavailable {
                detail: format!("no input recorded for {layer}"),
            })
        })
    }

    fn tier_of(&self, effect_class: &EffectClass) -> Option<Tier> {
        self.tiers
            .get(effect_class.as_str())
            .and_then(|tier| Tier::new(*tier).ok())
    }
}

/// A source with platform grants and every other layer empty.
#[must_use]
pub fn source_with_platform(platform_grants: Vec<Grant>, tiers: &[(&str, u8)]) -> StaticSource {
    let mut source = StaticSource::new().with(
        Layer::Platform,
        LayerInput::grants("platform/baseline", platform_grants),
    );
    for layer in Layer::all().into_iter().skip(1) {
        source = source.with(layer, LayerInput::empty(format!("{layer}/empty")));
    }
    for (effect_class, tier) in tiers {
        source = source.tier(effect_class, *tier);
    }
    source
}

/// Parse an effect class in a test.
#[must_use]
pub fn effect(effect_class: &str) -> EffectClass {
    EffectClass::parse(effect_class).expect("valid effect class")
}

/// An `fs` selector.
#[must_use]
pub fn fs(glob: &str) -> ResourceSelector {
    ResourceSelector::from_parts("fs", glob, &[]).expect("valid fs selector")
}

/// A `connector` selector.
#[must_use]
pub fn connector(connector_id: &str, resource_glob: &str) -> ResourceSelector {
    ResourceSelector::from_parts("connector", &format!("{connector_id}/{resource_glob}"), &[])
        .expect("valid connector selector")
}

/// A `domain` selector.
#[must_use]
pub fn domain(host_glob: &str, ports: &[u16]) -> ResourceSelector {
    ResourceSelector::from_parts("domain", host_glob, ports).expect("valid domain selector")
}

/// A grant with explicit constraints.
#[must_use]
pub fn grant_with(
    effect_class: &str,
    resource: ResourceSelector,
    constraints: Constraints,
) -> Grant {
    Grant::new(effect(effect_class), resource, constraints)
}

/// A grant with default constraints.
#[must_use]
pub fn grant(effect_class: &str, resource: ResourceSelector) -> Grant {
    grant_with(effect_class, resource, Constraints::default())
}

/// Constraints for tests.
#[must_use]
pub fn constraints(
    max_tier: Option<u8>,
    approval: Approval,
    expires_at: Option<DateTime<Utc>>,
    budget_ref: Option<BudgetRef>,
) -> Constraints {
    Constraints {
        max_tier: max_tier.map(|tier| Tier::new(tier).expect("valid tier")),
        approval,
        expires_at,
        budget_ref,
    }
}

/// Parse an RFC3339 instant in a test.
#[must_use]
pub fn at(value: &str) -> DateTime<Utc> {
    value.parse().expect("valid RFC3339 instant")
}

/// Superuser DSN, or `None` when the environment does not provide one.
#[must_use]
pub fn admin_url() -> Option<String> {
    std::env::var("QUANSIO_TEST_POSTGRES_URL")
        .ok()
        .filter(|value| !value.is_empty())
}

/// Report the standard blocked marker once.
pub fn blocked_marker() {
    eprintln!("BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set; dev stack not running");
}

/// Unique scratch database name for a test.
#[must_use]
pub fn scratch_name(prefix: &str) -> String {
    format!("quansio_cap_{prefix}_{}", std::process::id())
}

fn scratch_url(admin: &str, database: &str) -> String {
    match admin.rsplit_once('/') {
        Some((base, _)) => format!("{base}/{database}"),
        None => format!("{admin}/{database}"),
    }
}

/// Create an empty scratch database, apply the canonical migrations and return a pool.
pub async fn fresh_migrated_database(name: &str) -> Option<PgPool> {
    let admin = admin_url()?;
    let admin_pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&admin)
        .await
        .expect("connect to the admin database");
    admin_pool
        .execute(format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)").as_str())
        .await
        .expect("drop scratch database");
    admin_pool
        .execute(format!("CREATE DATABASE {name}").as_str())
        .await
        .expect("create scratch database");
    let url = scratch_url(&admin, name);
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .expect("connect to the scratch database");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("apply canonical migrations");
    Some(pool)
}

/// Close a pool and drop its scratch database.
pub async fn drop_pool(pool: &PgPool, name: &str) {
    let Some(admin) = admin_url() else {
        return;
    };
    pool.close().await;
    if let Ok(admin_pool) = PgPoolOptions::new()
        .max_connections(2)
        .connect(&admin)
        .await
    {
        let _ = admin_pool
            .execute(format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)").as_str())
            .await;
    }
}

/// Seed one tenant and one workspace inside it.
pub async fn seed_tenant(pool: &PgPool, tenant: &str, workspace: &str) {
    let mut tx = pool.begin().await.expect("begin");
    sqlx::query("INSERT INTO tenants (id, name) VALUES ($1, 'seed')")
        .bind(tenant)
        .execute(&mut *tx)
        .await
        .expect("insert tenant");
    sqlx::query("INSERT INTO workspaces (id, tenant_id, name) VALUES ($1, $2, 'seed')")
        .bind(workspace)
        .bind(tenant)
        .execute(&mut *tx)
        .await
        .expect("insert workspace");
    tx.commit().await.expect("commit");
}
