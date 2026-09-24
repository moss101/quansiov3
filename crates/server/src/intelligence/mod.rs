//! The runtime's client for the intelligence gateway (INT-001, DOSSIER.md §4.2).
//!
//! The Python intelligence plane runs as its own process (`python -m intelligence.server`) and
//! serves the generated `quansio.v1.intelligence.IntelligenceGateway` gRPC contract. This is the
//! Rust side of that boundary: it reaches the process over loopback TCP (`http://127.0.0.1:port`)
//! or a Unix domain socket (`unix:///path/to/socket`), and nothing is shared in-process.
//!
//! Every call carries a [`CallScope`] — tenant, workspace, correlation id, deadline and capability
//! projection id — as the `quansio-scope-bin` invocation metadata the gateway requires (it refuses
//! a call without one), and the deadline also bounds the call on this side. A failure comes back
//! typed: the gateway's DOMAIN.md §15 code rides in the `quansio-error-bin` trailer and the status,
//! and [`IntelligenceError`] keeps it.
//!
//! Python only proposes: every method here returns a proposal or a classification for the Rust
//! runtime to act on; none commits state.

use std::time::Duration;

use prost::Message as _;
use quansio_contracts::generated::intelligence::intelligence_gateway_client::IntelligenceGatewayClient;
use quansio_contracts::generated::intelligence::{
    ContextBuildRequest, ContextProjection, ModelCallRequest, ModelEvent, ScopeContext,
    TrustClassifyRequest, TrustClassifyResponse,
};
use tonic::metadata::MetadataValue;
use tonic::transport::{Channel, Endpoint, Uri};

/// The invocation metadata key the gateway reads the scope from.
pub const SCOPE_METADATA_KEY: &str = "quansio-scope-bin";
/// The trailer the gateway puts its typed error in.
pub const ERROR_TRAILER_KEY: &str = "quansio-error-bin";
/// The contract's schema version.
pub const SCHEMA_VERSION: &str = "v1";

/// The scope every gateway call is made in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallScope {
    /// Tenant.
    pub tenant_id: String,
    /// Workspace.
    pub workspace_id: String,
    /// Correlation id the gateway logs and returns in errors.
    pub correlation_id: String,
    /// How long the call may take.
    pub deadline: Duration,
    /// The capability projection the call is made under.
    pub capability_projection_id: String,
}

impl CallScope {
    fn context(&self) -> ScopeContext {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        ScopeContext {
            schema_version: SCHEMA_VERSION.to_string(),
            tenant_id: self.tenant_id.clone(),
            workspace_id: self.workspace_id.clone(),
            correlation_id: self.correlation_id.clone(),
            deadline_ms: u64::try_from((now + self.deadline).as_millis()).unwrap_or(u64::MAX),
            capability_projection_id: self.capability_projection_id.clone(),
        }
    }
}

/// Why a gateway call failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IntelligenceError {
    /// The endpoint is not one this client can reach.
    #[error("intelligence endpoint {0:?} is not http://host:port or unix:///path")]
    Endpoint(String),
    /// The process could not be reached.
    #[error("intelligence gateway unreachable: {0}")]
    Unreachable(String),
    /// The gateway refused or failed the call with a typed error.
    #[error("intelligence gateway {code}: {message}")]
    Refused {
        /// The DOMAIN.md §15 code (`ROUTE_UNAVAILABLE`, `AUTH_REQUIRED`, …) or the gRPC status.
        code: String,
        /// What the gateway said.
        message: String,
    },
    /// The call ran past its deadline.
    #[error("intelligence gateway call exceeded its deadline")]
    DeadlineExceeded,
}

fn refused(status: &tonic::Status) -> IntelligenceError {
    // The canonical envelope's code (the trailer carries the §15 `Error` shape as JSON) when
    // the gateway attached one; the gRPC status otherwise.
    let code = status
        .metadata()
        .get_bin(ERROR_TRAILER_KEY)
        .and_then(|value| value.to_bytes().ok())
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(bytes.as_ref()).ok())
        .and_then(|error| {
            error
                .get("code")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .filter(|code| !code.is_empty())
        .unwrap_or_else(|| format!("{:?}", status.code()));
    IntelligenceError::Refused {
        code,
        message: status.message().to_string(),
    }
}

/// A connected client.
#[derive(Debug, Clone)]
pub struct IntelligenceClient {
    inner: IntelligenceGatewayClient<Channel>,
}

impl IntelligenceClient {
    /// Connect to `endpoint`: `http://127.0.0.1:port` or `unix:///absolute/path`.
    ///
    /// # Errors
    /// [`IntelligenceError::Endpoint`] for an endpoint of another shape,
    /// [`IntelligenceError::Unreachable`] when nothing answers.
    pub async fn connect(endpoint: &str) -> Result<Self, IntelligenceError> {
        let channel = if let Some(path) = endpoint.strip_prefix("unix://") {
            if !path.starts_with('/') {
                return Err(IntelligenceError::Endpoint(endpoint.to_string()));
            }
            let path = path.to_string();
            // The URI is ignored by the connector; the socket path is the address.
            Endpoint::try_from("http://[::1]:1")
                .map_err(|_| IntelligenceError::Endpoint(endpoint.to_string()))?
                .connect_with_connector(tower::service_fn(move |_: Uri| {
                    let path = path.clone();
                    async move {
                        let stream = tokio::net::UnixStream::connect(path).await?;
                        Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
                    }
                }))
                .await
                .map_err(|error| IntelligenceError::Unreachable(error.to_string()))?
        } else if endpoint.starts_with("http://") {
            Endpoint::from_shared(endpoint.to_string())
                .map_err(|_| IntelligenceError::Endpoint(endpoint.to_string()))?
                .connect()
                .await
                .map_err(|error| IntelligenceError::Unreachable(error.to_string()))?
        } else {
            return Err(IntelligenceError::Endpoint(endpoint.to_string()));
        };
        Ok(Self {
            inner: IntelligenceGatewayClient::new(channel),
        })
    }

    fn request<T>(scope: &CallScope, message: T) -> tonic::Request<T> {
        let mut request = tonic::Request::new(message);
        request.set_timeout(scope.deadline);
        request.metadata_mut().insert_bin(
            SCOPE_METADATA_KEY,
            MetadataValue::from_bytes(&scope.context().encode_to_vec()),
        );
        request
    }

    /// `ClassifyTrust`: the deterministic trust label and injection verdict for one segment.
    ///
    /// # Errors
    /// The typed refusal, or an unreachable or late gateway.
    pub async fn classify_trust(
        &mut self,
        scope: &CallScope,
        source_kind: &str,
        content: &str,
    ) -> Result<TrustClassifyResponse, IntelligenceError> {
        let request = Self::request(
            scope,
            TrustClassifyRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                scope: Some(scope.context()),
                source_kind: source_kind.to_string(),
                content: content.to_string(),
            },
        );
        let call = self.inner.classify_trust(request);
        match tokio::time::timeout(scope.deadline, call).await {
            Err(_) => Err(IntelligenceError::DeadlineExceeded),
            Ok(Err(status)) => Err(refused(&status)),
            Ok(Ok(response)) => Ok(response.into_inner()),
        }
    }

    /// `BuildContext`: the Context plane's projection for one turn and the messages it rendered.
    ///
    /// The request's scope is set from `scope`. What comes back is a proposal: the caller runs
    /// the runtime's boundary check (`runtime::context_bridge::validate_projection`) before use.
    ///
    /// # Errors
    /// The typed refusal, or an unreachable or late gateway.
    pub async fn build_context(
        &mut self,
        scope: &CallScope,
        mut request: ContextBuildRequest,
    ) -> Result<ContextProjection, IntelligenceError> {
        request.schema_version = SCHEMA_VERSION.to_string();
        request.scope = Some(scope.context());
        let call = self.inner.build_context(Self::request(scope, request));
        match tokio::time::timeout(scope.deadline, call).await {
            Err(_) => Err(IntelligenceError::DeadlineExceeded),
            Ok(Err(status)) => Err(refused(&status)),
            Ok(Ok(response)) => Ok(response.into_inner()),
        }
    }

    /// `FulfillModel`: the normalized event stream of one model call, collected to its end.
    ///
    /// # Errors
    /// The typed refusal before the stream starts, a transport failure mid-stream, or the
    /// deadline. A provider failure after the stream started arrives as a `KIND_ERROR` event.
    pub async fn fulfill_model(
        &mut self,
        scope: &CallScope,
        call: ModelCallRequest,
    ) -> Result<Vec<ModelEvent>, IntelligenceError> {
        let request = Self::request(scope, call);
        let collect = async {
            let mut stream = self
                .inner
                .fulfill_model(request)
                .await
                .map_err(|status| refused(&status))?
                .into_inner();
            let mut events = Vec::new();
            while let Some(event) = stream.message().await.map_err(|status| refused(&status))? {
                events.push(event);
            }
            Ok(events)
        };
        match tokio::time::timeout(scope.deadline, collect).await {
            Err(_) => Err(IntelligenceError::DeadlineExceeded),
            Ok(result) => result,
        }
    }
}
