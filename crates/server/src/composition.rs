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
        Ok(Self {
            database_url,
            bind,
            max_connections,
            object_store,
            web_root,
            search_index_root,
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
        // missing a table would otherwise fail later, per request, with a worse message.
        sqlx::migrate!("../../migrations")
            .run(&pool)
            .await
            .map_err(|error| format!("cannot apply the schema: {error}"))?;
        // The deployable believes bearer sessions only; a development harness may opt into the
        // trusted tenant/user headers explicitly, and the choice is logged so it is never silent.
        let trust = if env::var("QUANSIO_TRUST_SCOPE_HEADERS").as_deref() == Ok("1") {
            eprintln!("quansio-server: QUANSIO_TRUST_SCOPE_HEADERS=1: x-quansio-tenant/x-quansio-user headers are trusted (development only)");
            crate::api::ScopeTrust::TrustedHeaders
        } else {
            crate::api::ScopeTrust::SessionOnly
        };
        let mut api = ApiState::new(pool.clone())
            .map_err(|error| error.message)?
            .with_scope_trust(trust);
        if let Some(store) = config.object_store.clone() {
            eprintln!(
                "quansio-server: QUANSIO_OBJECT_STORE=1: attachments go to bucket {:?} at {}",
                store.bucket, store.endpoint
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
            eprintln!("quansio-server: QUANSIO_SEARCH_INDEX_ROOT={root}: /v1/search indexes artifact content");
            api = api.with_search_index(Arc::new(index));
        }
        if let Some(root) = config.web_root.as_deref() {
            let root = crate::api::web::WebRoot::new(root)?;
            eprintln!(
                "quansio-server: QUANSIO_WEB_ROOT={}: the web app is served under /app/",
                root.path().display()
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
        axum::serve(listener, self.router())
            .await
            .map_err(|error| format!("the server stopped: {error}"))
    }
}
