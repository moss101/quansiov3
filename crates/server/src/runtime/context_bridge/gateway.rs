//! The production context bridge: the Context plane reached over INT-001's typed RPC.

use std::time::Duration;

use async_trait::async_trait;
use quansio_contracts::generated::intelligence::ContextBuildRequest;

use super::port::{ContextBridgePort, ProjectionRequest, ReceivedProjection, RPC_BOUNDARY_OWNER};
use super::ContextBridgeError;
use crate::intelligence::{CallScope, IntelligenceClient, IntelligenceError};

/// [`ContextBridgePort`] over the intelligence gateway's `BuildContext`.
///
/// It carries the runtime's segment requests across and hands back what the plane built,
/// unjudged: the caller's `validate_projection` is the boundary check. A plane refusal keeps its
/// typed code; an unreachable or late plane fails closed as unavailable.
#[derive(Debug, Clone)]
pub struct GatewayContextBridge {
    client: IntelligenceClient,
    deadline: Duration,
}

impl GatewayContextBridge {
    /// A bridge over a connected client; each projection may take `deadline`.
    #[must_use]
    pub const fn new(client: IntelligenceClient, deadline: Duration) -> Self {
        Self { client, deadline }
    }
}

#[async_trait]
impl ContextBridgePort for GatewayContextBridge {
    async fn project(
        &self,
        request: ProjectionRequest,
    ) -> Result<ReceivedProjection, ContextBridgeError> {
        let scope = CallScope {
            tenant_id: request.tenant_id.clone(),
            workspace_id: request.workspace_id.clone(),
            correlation_id: format!("{}:{}", request.run_id, request.turn_id),
            deadline: self.deadline,
            capability_projection_id: request.capability_projection_id.clone(),
        };
        let build = ContextBuildRequest {
            run_id: request.run_id,
            turn_id: request.turn_id,
            snapshot_id: request.snapshot_id,
            token_budget: u64::from(request.token_budget),
            policy_snapshot_digest: request.policy_snapshot_digest,
            segment_requests_json: request.segments.iter().map(|s| s.to_json()).collect(),
            ..ContextBuildRequest::default()
        };
        let mut client = self.client.clone();
        match client.build_context(&scope, build).await {
            Ok(projection) => Ok(ReceivedProjection::from(projection)),
            Err(IntelligenceError::Refused { code, message }) => Err(ContextBridgeError::Refused {
                code: known_code(&code),
                detail: message,
            }),
            Err(other) => Err(ContextBridgeError::Unavailable {
                owner: RPC_BOUNDARY_OWNER,
                detail: other.to_string(),
            }),
        }
    }
}

/// The plane's code, when it is one a context build can give; `INTERNAL` otherwise.
fn known_code(code: &str) -> &'static str {
    match code {
        "VALIDATION_SCHEMA" => "VALIDATION_SCHEMA",
        "VALIDATION_BOUNDS" => "VALIDATION_BOUNDS",
        "CONFLICT_STATE" => "CONFLICT_STATE",
        "AUTH_REQUIRED" => "AUTH_REQUIRED",
        _ => "INTERNAL",
    }
}
