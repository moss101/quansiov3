//! Composition seams for the walking skeleton (APP-001).
//!
//! The model gateway's deterministic conformance stub is INT-002's offline provider.
//! This module is the in-process port the runtime already uses: it proposes the same
//! `fs.read` tool the stub's `tool_call` scenario uses, then finishes the turn. It is
//! not live-provider evidence.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;

use crate::policy::{ActorRoles, TenantRole, WorkspaceRole};
use crate::runtime::state_machine::{
    ModelCallRequest, ModelProposal, ModelProposalSource, ProposedToolCall, RuntimeError,
};
use crate::runtime::turn_loop::{
    ActingActor, HostDispatch, HostFailure, HostOutcome, RoleProvider, ToolHostPort,
};

/// Conformance-stub model: one `fs.read` proposal, then assistant text.
#[derive(Debug, Default)]
pub struct ConformanceStubModel {
    calls: Mutex<HashMap<String, u32>>,
}

impl ConformanceStubModel {
    /// A fresh stub.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ModelProposalSource for ConformanceStubModel {
    async fn propose(&self, request: ModelCallRequest) -> Result<ModelProposal, RuntimeError> {
        let mut calls = self.calls.lock().unwrap_or_else(|error| error.into_inner());
        let count = calls.entry(request.run_id.clone()).or_insert(0);
        *count += 1;
        if *count == 1 {
            Ok(ModelProposal {
                assistant_text: Some("working".to_string()),
                tool_calls: vec![ProposedToolCall::new(
                    "call_conformance",
                    "fs.read",
                    json!({ "path": "/work/root/notes.md" }),
                )],
                ..ModelProposal::default()
            })
        } else {
            Ok(ModelProposal {
                assistant_text: Some("done".to_string()),
                ..ModelProposal::default()
            })
        }
    }
}

/// Host that settles a dispatch without leaving the process.
#[derive(Debug, Default)]
pub struct ConformanceStubHost;

#[async_trait]
impl ToolHostPort for ConformanceStubHost {
    async fn execute(&self, dispatch: HostDispatch) -> Result<HostOutcome, HostFailure> {
        Ok(HostOutcome::Success {
            output: json!({
                "ok": true,
                "tool": dispatch.tool,
                "stub": "conformance",
            }),
            evidence_ids: Vec::new(),
            remote_ref: None,
        })
    }
}

/// Session identity until APP-002 owns it: a workspace member who can approve.
#[derive(Debug, Clone)]
pub struct TenantMemberRoles {
    user_id: Option<String>,
}

impl TenantMemberRoles {
    /// Roles for an optional user.
    #[must_use]
    pub fn new(user_id: Option<String>) -> Self {
        Self { user_id }
    }
}

#[async_trait]
impl RoleProvider for TenantMemberRoles {
    async fn actor_for_run(&self, _run_id: &str) -> Result<Option<ActingActor>, RuntimeError> {
        Ok(Some(ActingActor {
            roles: ActorRoles::new(Some(TenantRole::Member), Some(WorkspaceRole::Approver)),
            user_id: self.user_id.clone(),
        }))
    }
}

/// Shared walking-skeleton seams.
pub fn skeleton_seams(
    user_id: Option<String>,
) -> (
    Arc<dyn ModelProposalSource>,
    Arc<dyn ToolHostPort>,
    Arc<dyn RoleProvider>,
) {
    (
        Arc::new(ConformanceStubModel::new()),
        Arc::new(ConformanceStubHost),
        Arc::new(TenantMemberRoles::new(user_id)),
    )
}
