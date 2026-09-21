"""GENERATED from DOMAIN.md by scripts/ci/gen_contracts.py --sdk. Do not hand edit.

The public API v1 operation table (APP-013): every command the catalog names, with its
method and path, the read projections, and the §15 error codes.
"""

from __future__ import annotations

OPERATIONS: dict[str, dict[str, str]] = {
    "CreateWorkspace": {
        "method": "POST",
        "path": "/v1/commands/CreateWorkspace",
        "family": "Identity",
    },
    "UpdateWorkspace": {
        "method": "POST",
        "path": "/v1/commands/UpdateWorkspace",
        "family": "Identity",
    },
    "InviteMember": {
        "method": "POST",
        "path": "/v1/commands/InviteMember",
        "family": "Identity",
    },
    "UpdateMembership": {
        "method": "POST",
        "path": "/v1/commands/UpdateMembership",
        "family": "Identity",
    },
    "RemoveMember": {
        "method": "POST",
        "path": "/v1/commands/RemoveMember",
        "family": "Identity",
    },
    "CreateServicePrincipal": {
        "method": "POST",
        "path": "/v1/commands/CreateServicePrincipal",
        "family": "Identity",
    },
    "RevokeServicePrincipal": {
        "method": "POST",
        "path": "/v1/commands/RevokeServicePrincipal",
        "family": "Identity",
    },
    "UpdateUserSettings": {
        "method": "POST",
        "path": "/v1/commands/UpdateUserSettings",
        "family": "Identity",
    },
    "CreateTeammate": {
        "method": "POST",
        "path": "/v1/commands/CreateTeammate",
        "family": "Teammates",
    },
    "UpdateTeammate": {
        "method": "POST",
        "path": "/v1/commands/UpdateTeammate",
        "family": "Teammates",
    },
    "ArchiveTeammate": {
        "method": "POST",
        "path": "/v1/commands/ArchiveTeammate",
        "family": "Teammates",
    },
    "CreateThread": {
        "method": "POST",
        "path": "/v1/commands/CreateThread",
        "family": "Conversation",
    },
    "PostMessage": {
        "method": "POST",
        "path": "/v1/commands/PostMessage",
        "family": "Conversation",
    },
    "EditMessage": {
        "method": "POST",
        "path": "/v1/commands/EditMessage",
        "family": "Conversation",
    },
    "DeleteMessage": {
        "method": "POST",
        "path": "/v1/commands/DeleteMessage",
        "family": "Conversation",
    },
    "ReactToMessage": {
        "method": "POST",
        "path": "/v1/commands/ReactToMessage",
        "family": "Conversation",
    },
    "AddParticipant": {
        "method": "POST",
        "path": "/v1/commands/AddParticipant",
        "family": "Conversation",
    },
    "RemoveParticipant": {
        "method": "POST",
        "path": "/v1/commands/RemoveParticipant",
        "family": "Conversation",
    },
    "AnswerQuestion": {
        "method": "POST",
        "path": "/v1/commands/AnswerQuestion",
        "family": "Conversation",
    },
    "CancelTurn": {
        "method": "POST",
        "path": "/v1/commands/CancelTurn",
        "family": "Conversation",
    },
    "CreateObjective": {
        "method": "POST",
        "path": "/v1/commands/CreateObjective",
        "family": "Work",
    },
    "ProposeGraphEdit": {
        "method": "POST",
        "path": "/v1/commands/ProposeGraphEdit",
        "family": "Work",
    },
    "CancelRun": {
        "method": "POST",
        "path": "/v1/commands/CancelRun",
        "family": "Work",
    },
    "RetryRun": {
        "method": "POST",
        "path": "/v1/commands/RetryRun",
        "family": "Work",
    },
    "PauseRun": {
        "method": "POST",
        "path": "/v1/commands/PauseRun",
        "family": "Work",
    },
    "ResumeRun": {
        "method": "POST",
        "path": "/v1/commands/ResumeRun",
        "family": "Work",
    },
    "RequestVerification": {
        "method": "POST",
        "path": "/v1/commands/RequestVerification",
        "family": "Work",
    },
    "ApproveEffect": {
        "method": "POST",
        "path": "/v1/commands/ApproveEffect",
        "family": "Trust",
    },
    "DenyEffect": {
        "method": "POST",
        "path": "/v1/commands/DenyEffect",
        "family": "Trust",
    },
    "SetUserRule": {
        "method": "POST",
        "path": "/v1/commands/SetUserRule",
        "family": "Trust",
    },
    "RevokeUserRule": {
        "method": "POST",
        "path": "/v1/commands/RevokeUserRule",
        "family": "Trust",
    },
    "ProvisionTarget": {
        "method": "POST",
        "path": "/v1/commands/ProvisionTarget",
        "family": "Execution",
    },
    "StopTarget": {
        "method": "POST",
        "path": "/v1/commands/StopTarget",
        "family": "Execution",
    },
    "ResetTarget": {
        "method": "POST",
        "path": "/v1/commands/ResetTarget",
        "family": "Execution",
    },
    "CreateCheckpoint": {
        "method": "POST",
        "path": "/v1/commands/CreateCheckpoint",
        "family": "Execution",
    },
    "RestoreCheckpoint": {
        "method": "POST",
        "path": "/v1/commands/RestoreCheckpoint",
        "family": "Execution",
    },
    "RequestTakeover": {
        "method": "POST",
        "path": "/v1/commands/RequestTakeover",
        "family": "Browser/computer",
    },
    "Handback": {
        "method": "POST",
        "path": "/v1/commands/Handback",
        "family": "Browser/computer",
    },
    "ImportBrowserSession": {
        "method": "POST",
        "path": "/v1/commands/ImportBrowserSession",
        "family": "Browser/computer",
    },
    "GrantComputerTier": {
        "method": "POST",
        "path": "/v1/commands/GrantComputerTier",
        "family": "Browser/computer",
    },
    "CreateArtifact": {
        "method": "POST",
        "path": "/v1/commands/CreateArtifact",
        "family": "Artifacts",
    },
    "AddArtifactVersion": {
        "method": "POST",
        "path": "/v1/commands/AddArtifactVersion",
        "family": "Artifacts",
    },
    "GrantArtifactAccess": {
        "method": "POST",
        "path": "/v1/commands/GrantArtifactAccess",
        "family": "Artifacts",
    },
    "DeleteArtifact": {
        "method": "POST",
        "path": "/v1/commands/DeleteArtifact",
        "family": "Artifacts",
    },
    "IngestSource": {
        "method": "POST",
        "path": "/v1/commands/IngestSource",
        "family": "Knowledge & memory",
    },
    "DeleteKnowledge": {
        "method": "POST",
        "path": "/v1/commands/DeleteKnowledge",
        "family": "Knowledge & memory",
    },
    "SupersedeKnowledge": {
        "method": "POST",
        "path": "/v1/commands/SupersedeKnowledge",
        "family": "Knowledge & memory",
    },
    "CreateMemory": {
        "method": "POST",
        "path": "/v1/commands/CreateMemory",
        "family": "Knowledge & memory",
    },
    "DeleteMemory": {
        "method": "POST",
        "path": "/v1/commands/DeleteMemory",
        "family": "Knowledge & memory",
    },
    "InstallSkill": {
        "method": "POST",
        "path": "/v1/commands/InstallSkill",
        "family": "Skills & packs",
    },
    "PromoteSkillVersion": {
        "method": "POST",
        "path": "/v1/commands/PromoteSkillVersion",
        "family": "Skills & packs",
    },
    "DisableSkill": {
        "method": "POST",
        "path": "/v1/commands/DisableSkill",
        "family": "Skills & packs",
    },
    "SubmitPack": {
        "method": "POST",
        "path": "/v1/commands/SubmitPack",
        "family": "Skills & packs",
    },
    "QualifyPack": {
        "method": "POST",
        "path": "/v1/commands/QualifyPack",
        "family": "Skills & packs",
    },
    "PublishPack": {
        "method": "POST",
        "path": "/v1/commands/PublishPack",
        "family": "Skills & packs",
    },
    "RollbackPack": {
        "method": "POST",
        "path": "/v1/commands/RollbackPack",
        "family": "Skills & packs",
    },
    "CreateRoutine": {
        "method": "POST",
        "path": "/v1/commands/CreateRoutine",
        "family": "Routines",
    },
    "UpdateRoutine": {
        "method": "POST",
        "path": "/v1/commands/UpdateRoutine",
        "family": "Routines",
    },
    "PauseRoutine": {
        "method": "POST",
        "path": "/v1/commands/PauseRoutine",
        "family": "Routines",
    },
    "ResumeRoutine": {
        "method": "POST",
        "path": "/v1/commands/ResumeRoutine",
        "family": "Routines",
    },
    "TriggerRoutineNow": {
        "method": "POST",
        "path": "/v1/commands/TriggerRoutineNow",
        "family": "Routines",
    },
    "DeleteRoutine": {
        "method": "POST",
        "path": "/v1/commands/DeleteRoutine",
        "family": "Routines",
    },
    "BeginConnectorAuth": {
        "method": "POST",
        "path": "/v1/commands/BeginConnectorAuth",
        "family": "Connectors",
    },
    "CompleteConnectorAuth": {
        "method": "POST",
        "path": "/v1/commands/CompleteConnectorAuth",
        "family": "Connectors",
    },
    "RevokeConnector": {
        "method": "POST",
        "path": "/v1/commands/RevokeConnector",
        "family": "Connectors",
    },
    "CreateWebhookSubscription": {
        "method": "POST",
        "path": "/v1/commands/CreateWebhookSubscription",
        "family": "Connectors",
    },
    "DeleteWebhookSubscription": {
        "method": "POST",
        "path": "/v1/commands/DeleteWebhookSubscription",
        "family": "Connectors",
    },
    "UpdateNotificationPreferences": {
        "method": "POST",
        "path": "/v1/commands/UpdateNotificationPreferences",
        "family": "Notifications",
    },
    "MarkNotificationRead": {
        "method": "POST",
        "path": "/v1/commands/MarkNotificationRead",
        "family": "Notifications",
    },
    "SetPolicy": {
        "method": "POST",
        "path": "/v1/commands/SetPolicy",
        "family": "Admin/ops",
    },
    "FreezeEffects": {
        "method": "POST",
        "path": "/v1/commands/FreezeEffects",
        "family": "Admin/ops",
    },
    "UnfreezeEffects": {
        "method": "POST",
        "path": "/v1/commands/UnfreezeEffects",
        "family": "Admin/ops",
    },
    "DisableProvider": {
        "method": "POST",
        "path": "/v1/commands/DisableProvider",
        "family": "Admin/ops",
    },
    "EnableProvider": {
        "method": "POST",
        "path": "/v1/commands/EnableProvider",
        "family": "Admin/ops",
    },
    "QuarantineWorker": {
        "method": "POST",
        "path": "/v1/commands/QuarantineWorker",
        "family": "Admin/ops",
    },
    "DrainTarget": {
        "method": "POST",
        "path": "/v1/commands/DrainTarget",
        "family": "Admin/ops",
    },
    "ExportTenantData": {
        "method": "POST",
        "path": "/v1/commands/ExportTenantData",
        "family": "Admin/ops",
    },
    "DeleteUserData": {
        "method": "POST",
        "path": "/v1/commands/DeleteUserData",
        "family": "Admin/ops",
    },
}

READ_PROJECTIONS: tuple[str, ...] = (
    "/v1/threads/{id}",
    "/v1/messages",
    "/v1/workgraph",
    "/v1/runs/{id}",
    "/v1/runs/{id}/timeline",
    "/v1/effects",
    "/v1/approvals",
    "/v1/questions",
    "/v1/artifacts",
    "/v1/artifacts/{id}/versions",
    "/v1/artifacts/{id}/versions/{vid}/content",
    "/v1/evidence/{id}",
    "/v1/targets",
    "/v1/browser-sessions/{id}",
    "/v1/knowledge",
    "/v1/memory",
    "/v1/skills",
    "/v1/packs",
    "/v1/routines",
    "/v1/connectors",
    "/v1/members",
    "/v1/policies",
    "/v1/teammates",
    "/v1/notifications",
    "/v1/usage",
    "/v1/audit",
    "/v1/search?q=",
    "/v1/stream",
)

ERROR_CODES: tuple[str, ...] = (
    "AUTH_REQUIRED",
    "AUTH_INVALID_TOKEN",
    "AUTH_EXPIRED",
    "SCOPE_FORBIDDEN",
    "TENANT_MISMATCH",
    "VALIDATION_SCHEMA",
    "VALIDATION_UNKNOWN_FIELD",
    "VALIDATION_BOUNDS",
    "CONFLICT_REVISION",
    "CONFLICT_IDEMPOTENCY_MISMATCH",
    "CONFLICT_STATE",
    "CAPABILITY_DENIED",
    "CAPABILITY_INPUTS_UNAVAILABLE",
    "POLICY_DENIED",
    "APPROVAL_REQUIRED",
    "APPROVAL_INVALID",
    "APPROVAL_EXPIRED",
    "APPROVAL_SUPERSEDED",
    "RUNTIME_ILLEGAL_TRANSITION",
    "FENCED_STALE_GENERATION",
    "LEASE_LOST",
    "EFFECT_UNKNOWN_PENDING_RECONCILIATION",
    "BUDGET_EXHAUSTED",
    "EFFECTS_FROZEN",
    "TARGET_UNAVAILABLE",
    "TARGET_PROVISION_FAILED",
    "TOOL_TIMEOUT",
    "TOOL_OUTPUT_TRUNCATED",
    "EGRESS_DENIED",
    "PROVIDER_UNAVAILABLE",
    "PROVIDER_RATE_LIMITED",
    "PROVIDER_REFUSAL",
    "ROUTE_UNAVAILABLE",
    "DLP_DENIED",
    "NOT_FOUND",
    "RATE_LIMITED",
    "STREAM_BACKPRESSURE",
    "INTERNAL",
)
