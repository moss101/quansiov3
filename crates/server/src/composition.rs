//! The composition root: the modules in one deployable (APP-001).
//!
//! Composing is all this module does. It reads the environment, opens one pool, and hands each module the
//! pool and the identity it needs — so the deployment has one process, one pool and one place where the
//! wiring is written down, and no module reaches for another's store behind its back. It is deliberately
//! not an orchestration layer: a caller still reaches the authoritative module, and nothing here decides
//! what a command means.

use std::env;
use std::sync::Arc;
use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

use crate::api::{self, ApiState};
use crate::artifacts::{ObjectStore, S3Config, S3ObjectStore};

/// How the process is configured. Every value comes from the environment so the same binary runs in dev,
/// CI and a deployment.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// The database this deployment writes to.
    pub database_url: String,
    /// `QUANSIO_MIGRATION_DATABASE_URL`: when set, the schema is applied over this connection (the
    /// schema owner) and requests are served over `database_url`, which can then be a login role in
    /// `quansio_app` — never the owner, never a superuser, so row-level security always applies and
    /// the serving process cannot alter the schema. Unset, one DSN does both (development).
    pub migration_database_url: Option<String>,
    /// The address the API listens on.
    pub bind: String,
    /// How many connections the pool holds.
    pub max_connections: u32,
    /// Where attachments are written (CORE-007's object store), when the deployment has one.
    pub object_store: Option<S3Config>,
    /// Root directory of INT-004's derived search index (`/v1/search` content channel), when set.
    pub search_index_root: Option<String>,
    /// Directory of the built web app served under `/app/` (APP-012), when set.
    pub web_root: Option<String>,
    /// How often the background loop runs (APP-010); `None` when `QUANSIO_SCHEDULER=off`.
    pub background_interval: Option<Duration>,
    /// `QUANSIO_SECRETS_KEY_FILE`: the local master key (base64, 32 bytes) that seals secret
    /// handles in dev and self-host (DOSSIER.md §15); `QUANSIO_SECRETS_KEY_REF` names it.
    pub secrets_key_file: Option<String>,
    /// The key reference recorded on sealed secrets.
    pub secrets_key_ref: String,
    /// `QUANSIO_INTELLIGENCE_ENDPOINT`: the intelligence process (`http://host:port` or
    /// `unix:///path`) that builds each turn's context and fulfils its model calls (APP-001).
    /// Unset, turns are answered by the conformance stub, and startup says so.
    pub intelligence_endpoint: Option<String>,
}

impl ServerConfig {
    /// Read the configuration from the environment.
    ///
    /// # Errors
    /// Returns a message naming the variable that is missing, because a deployment that cannot say what it
    /// was configured with is a deployment whose failure cannot be diagnosed.
    pub fn from_env() -> Result<Self, String> {
        let database_url = env::var("QUANSIO_DATABASE_URL")
            .ok()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "QUANSIO_DATABASE_URL is not set".to_string())?;
        let migration_database_url = env::var("QUANSIO_MIGRATION_DATABASE_URL")
            .ok()
            .filter(|value| !value.is_empty());
        let bind = env::var("QUANSIO_BIND")
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "127.0.0.1:8080".to_string());
        let max_connections = env::var("QUANSIO_MAX_CONNECTIONS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(8);
        // The object store is opt-in: `QUANSIO_OBJECT_STORE=1` reads the `QUANSIO_TEST_MINIO_*`
        // boundary (CORE-007's contract, dev-stack defaults included). Without it uploads fail
        // closed with `TARGET_UNAVAILABLE` rather than silently pointing a deployment at the
        // dev MinIO.
        let object_store = if env::var("QUANSIO_OBJECT_STORE").as_deref() == Ok("1") {
            Some(S3Config::from_env().map_err(|error| format!("object store: {error}"))?)
        } else {
            None
        };
        let search_index_root = env::var("QUANSIO_SEARCH_INDEX_ROOT")
            .ok()
            .filter(|value| !value.is_empty());
        let web_root = env::var("QUANSIO_WEB_ROOT")
            .ok()
            .filter(|value| !value.is_empty());
        // The background loop fires routines, consumes events into routine triggers and
        // notifications, and delivers email. It is on unless a deployment turns it off.
        let background_interval = if env::var("QUANSIO_SCHEDULER").as_deref() == Ok("off") {
            None
        } else {
            let millis = env::var("QUANSIO_SCHEDULER_INTERVAL_MS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|millis| *millis >= 50)
                .unwrap_or(1000);
            Some(Duration::from_millis(millis))
        };
        let secrets_key_file = env::var("QUANSIO_SECRETS_KEY_FILE")
            .ok()
            .filter(|value| !value.is_empty());
        let secrets_key_ref = env::var("QUANSIO_SECRETS_KEY_REF")
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "local:master-v1".to_string());
        let intelligence_endpoint = env::var("QUANSIO_INTELLIGENCE_ENDPOINT")
            .ok()
            .filter(|value| !value.is_empty());
        Ok(Self {
            intelligence_endpoint,
            secrets_key_file,
            secrets_key_ref,
            database_url,
            migration_database_url,
            bind,
            max_connections,
            object_store,
            web_root,
            search_index_root,
            background_interval,
        })
    }
}

/// The composed process.
#[derive(Clone)]
pub struct Composition {
    config: ServerConfig,
    pool: PgPool,
    api: ApiState,
}

impl Composition {
    /// Open the pool and build every module's state over it.
    ///
    /// # Errors
    /// Returns a message when the pool cannot be opened or a catalog is not the shape the contract
    /// defines; both are startup failures rather than per-request ones.
    pub async fn new(config: ServerConfig) -> Result<Self, String> {
        let pool = PgPoolOptions::new()
            .max_connections(config.max_connections)
            .acquire_timeout(Duration::from_secs(10))
            .connect(&config.database_url)
            .await
            .map_err(|error| format!("cannot open the database pool: {error}"))?;
        // The schema is applied here rather than assumed: a deployment that starts against a database
        // missing a table would otherwise fail later, per request, with a worse message. With a
        // migration DSN the owner applies it over its own short-lived connection, and the serving
        // pool never holds schema privileges.
        match config.migration_database_url.as_deref() {
            Some(owner_url) => {
                let owner = PgPoolOptions::new()
                    .max_connections(1)
                    .acquire_timeout(Duration::from_secs(10))
                    .connect(owner_url)
                    .await
                    .map_err(|error| format!("cannot open the migration connection: {error}"))?;
                let applied = sqlx::migrate!("../../migrations").run(&owner).await;
                owner.close().await;
                applied.map_err(|error| format!("cannot apply the schema: {error}"))?;
            }
            None => sqlx::migrate!("../../migrations")
                .run(&pool)
                .await
                .map_err(|error| format!("cannot apply the schema: {error}"))?,
        }
        // The deployable believes bearer sessions only; a development harness may opt into the
        // trusted tenant/user headers explicitly, and the choice is logged so it is never silent.
        let trust = if env::var("QUANSIO_TRUST_SCOPE_HEADERS").as_deref() == Ok("1") {
            crate::observability::notice(crate::observability::Level::Info, "QUANSIO_TRUST_SCOPE_HEADERS=1: x-quansio-tenant/x-quansio-user headers are trusted (development only)");
            crate::api::ScopeTrust::TrustedHeaders
        } else {
            crate::api::ScopeTrust::SessionOnly
        };
        let mut api = ApiState::new(pool.clone())
            .map_err(|error| error.message)?
            .with_scope_trust(trust);
        if let Some(store) = config.object_store.clone() {
            crate::observability::notice(
                crate::observability::Level::Info,
                &format!(
                    "QUANSIO_OBJECT_STORE=1: attachments go to bucket {:?} at {}",
                    store.bucket, store.endpoint
                ),
            );
            let objects: Arc<dyn ObjectStore> = Arc::new(
                S3ObjectStore::new(store).map_err(|error| format!("object store: {error}"))?,
            );
            api = api.with_object_store(objects);
        }
        if let Some(root) = config.search_index_root.as_deref() {
            // Derived files only: the index is rebuilt from the canonical tables and the
            // object store whenever the corpus changed, so losing this directory loses nothing.
            let index = quansio_indexer::SearchIndex::open(root)
                .map_err(|error| format!("search index at {root}: {error}"))?;
            crate::observability::notice(
                crate::observability::Level::Info,
                &format!("QUANSIO_SEARCH_INDEX_ROOT={root}: /v1/search indexes artifact content"),
            );
            api = api.with_search_index(Arc::new(index));
        }
        if let Some(path) = config.secrets_key_file.as_deref() {
            let keys = quansio_machine::secrets::LocalMasterKeyProvider::from_file(
                std::path::Path::new(path),
                config.secrets_key_ref.clone(),
            )
            .map_err(|error| format!("secret key provider: {error}"))?;
            crate::observability::notice(
                crate::observability::Level::Info,
                &format!(
                    "QUANSIO_SECRETS_KEY_FILE: secret handles are sealed under {}",
                    config.secrets_key_ref
                ),
            );
            api = api.with_key_provider(Arc::new(keys));
        }
        if let Ok(token) = env::var("QUANSIO_METRICS_TOKEN") {
            if !token.is_empty() {
                crate::observability::notice(
                    crate::observability::Level::Info,
                    "QUANSIO_METRICS_TOKEN is set: /metrics serves Prometheus text to its bearer",
                );
                api = api.with_metrics_token(&token);
            }
        }
        if let Some(endpoint) = config.intelligence_endpoint.as_deref() {
            let settings = crate::intelligence::model_source::ModelCallSettings::load()?;
            let client = crate::intelligence::IntelligenceClient::connect_lazy(endpoint)
                .map_err(|error| format!("QUANSIO_INTELLIGENCE_ENDPOINT: {error}"))?;
            let context = Arc::new(crate::runtime::context_bridge::GatewayContextBridge::new(
                client.clone(),
                settings.deadline,
            ));
            crate::observability::notice(crate::observability::Level::Info, &format!("QUANSIO_INTELLIGENCE_ENDPOINT={endpoint}: turns are answered through the intelligence gateway"));
            api = api
                .with_intelligence(client.clone())
                .with_model_source(Arc::new(
                    crate::intelligence::model_source::GatewayModelSource::new(
                        pool.clone(),
                        client,
                        context,
                        settings,
                    ),
                ));
        } else {
            crate::observability::notice(crate::observability::Level::Info, "QUANSIO_INTELLIGENCE_ENDPOINT is not set: turns are answered by the conformance stub, not a model");
        }
        if let Some(root) = config.web_root.as_deref() {
            let root = crate::api::web::WebRoot::new(root)?;
            crate::observability::notice(
                crate::observability::Level::Info,
                &format!(
                    "QUANSIO_WEB_ROOT={}: the web app is served under /app/",
                    root.path().display()
                ),
            );
            api = api.with_web_root(root);
        }
        Ok(Self { config, pool, api })
    }

    /// The API state, for a caller that wants to mount it elsewhere.
    #[must_use]
    pub const fn api(&self) -> &ApiState {
        &self.api
    }

    /// The pool.
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// The configuration it was built from.
    #[must_use]
    pub const fn config(&self) -> &ServerConfig {
        &self.config
    }

    /// The deployed surface.
    pub fn router(&self) -> axum::Router {
        api::router(self.api.clone())
    }

    /// Serve until the process is stopped.
    ///
    /// # Errors
    /// Returns a message when the address cannot be bound or the server fails.
    pub async fn serve(self) -> Result<(), String> {
        let listener = tokio::net::TcpListener::bind(&self.config.bind)
            .await
            .map_err(|error| format!("cannot bind {}: {error}", self.config.bind))?;
        if let Some(interval) = self.config.background_interval {
            let background = Background::new(self.pool.clone(), self.api.clone());
            crate::observability::notice(
                crate::observability::Level::Info,
                &format!(
                    "background loop every {} ms (routines, event triggers, notifications, email{}, webhooks{})",
                    interval.as_millis(),
                    if background.email.is_some() { "" } else { " unavailable: QUANSIO_EMAIL_ENDPOINT is not set" },
                    if background.keys.is_some() { "" } else { " unsigned and so not sent: QUANSIO_SECRETS_KEY_FILE is not set" }
                ),
            );
            tokio::spawn(background.clone().run_webhooks(interval));
            tokio::spawn(background.run(interval));
        }
        axum::serve(listener, self.router())
            .await
            .map_err(|error| format!("the server stopped: {error}"))
    }
}

/// The deployable's background loop (APP-010): for every tenant, one CORE-008 scheduler tick
/// (timers, routine windows and their fires, queued runs), one pass of the routine event
/// triggers, the notifier, and email delivery. Each pass is bounded; a failing tenant is
/// logged and the others go on.
#[derive(Clone)]
pub struct Background {
    pool: PgPool,
    keys: Option<Arc<dyn quansio_machine::secrets::KeyProvider>>,
    dispatch: Arc<dyn crate::scheduler::RunDispatch>,
    email: Option<Arc<dyn crate::notify::EmailTransport>>,
    instance: String,
}

impl Background {
    /// A loop over the pool with the API's interactive dispatch and the environment's mail
    /// transport.
    #[must_use]
    pub fn new(pool: PgPool, api: ApiState) -> Self {
        Self {
            pool,
            keys: api.key_provider(),
            dispatch: crate::api::routines::interactive_dispatch(&api),
            email: crate::notify::HttpEmailTransport::from_env(),
            instance: format!(
                "scheduler:{}",
                quansio_core::UlidGenerator::new().generate()
            ),
        }
    }

    /// Run forever, one pass per `interval`.
    pub async fn run(self, interval: Duration) {
        loop {
            tokio::time::sleep(interval).await;
            if let Err(message) = self.pass().await {
                crate::observability::notice(
                    crate::observability::Level::Warn,
                    &format!("background pass: {message}"),
                );
            }
        }
    }

    /// Run the webhook fan-out and deliveries forever, one pass per `interval`. They run beside
    /// the main loop rather than in it, so a slow destination never delays a routine.
    pub async fn run_webhooks(self, interval: Duration) {
        loop {
            tokio::time::sleep(interval).await;
            if let Err(message) = self.webhook_pass().await {
                crate::observability::notice(
                    crate::observability::Level::Warn,
                    &format!("webhook pass: {message}"),
                );
            }
        }
    }

    /// One webhook pass over every tenant: enqueue new events, then attempt due deliveries.
    ///
    /// # Errors
    /// Returns a message when the tenants cannot be listed.
    pub async fn webhook_pass(&self) -> Result<(), String> {
        let tenants: Vec<String> = sqlx::query_scalar("SELECT background_tenants()")
            .fetch_all(&self.pool)
            .await
            .map_err(|error| format!("cannot list tenants: {error}"))?;
        for tenant in tenants {
            let fanout = crate::notify::WebhookFanout::new(self.pool.clone(), tenant.as_str());
            if let Err(error) = fanout.scan(512).await {
                crate::observability::notice(
                    crate::observability::Level::Warn,
                    &format!("webhook fan-out for {tenant}: {error}"),
                );
                continue;
            }
            let deliverer = crate::notify::WebhookDeliverer::new(
                self.pool.clone(),
                tenant.as_str(),
                self.keys.clone(),
            );
            if let Err(error) = deliverer.deliver_due(chrono::Utc::now(), 32).await {
                crate::observability::notice(
                    crate::observability::Level::Warn,
                    &format!("webhook delivery for {tenant}: {error}"),
                );
            }
        }
        Ok(())
    }

    /// One pass over every tenant.
    ///
    /// # Errors
    /// Returns a message when the tenants cannot be listed.
    pub async fn pass(&self) -> Result<(), String> {
        let tenants: Vec<String> = sqlx::query_scalar("SELECT background_tenants()")
            .fetch_all(&self.pool)
            .await
            .map_err(|error| format!("cannot list tenants: {error}"))?;
        for tenant in tenants {
            if let Err(message) = self.tenant_pass(&tenant).await {
                crate::observability::notice(
                    crate::observability::Level::Warn,
                    &format!("background pass for {tenant}: {message}"),
                );
            }
        }
        Ok(())
    }

    async fn tenant_pass(&self, tenant: &str) -> Result<(), String> {
        let triggers = crate::control::routines::EventTriggers::new(self.pool.clone(), tenant);
        triggers
            .scan(512)
            .await
            .map_err(|error| format!("routine triggers: {error}"))?;
        let scheduler = crate::scheduler::Scheduler::new(
            self.pool.clone(),
            tenant,
            crate::scheduler::SchedulerConfig::new(self.instance.clone()),
            Arc::clone(&self.dispatch),
        )
        .map_err(|error| format!("scheduler: {error}"))?;
        scheduler
            .tick()
            .await
            .map_err(|error| format!("scheduler tick: {error}"))?;
        crate::notify::Notifier::new(self.pool.clone(), tenant)
            .scan(512)
            .await
            .map_err(|error| format!("notifier: {error}"))?;
        crate::notify::EmailDeliveries::new(self.pool.clone(), tenant, self.email.clone())
            .deliver_due(chrono::Utc::now(), 64)
            .await
            .map_err(|error| format!("email: {error}"))?;
        Ok(())
    }
}
