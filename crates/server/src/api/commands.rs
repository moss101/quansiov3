//! Mutating command dispatch (APP-001).
//!
//! The handler records the command in `commands`, calls the canonical owner, and keeps the owner's answer --
//! its result or its refusal -- as the command's outcome, which is what a resend of the same `command_id` gets.

use std::sync::Arc;

use super::{ApiError, ApiErrorCode, ApiState, CommandRequest, CommandResponse, TenantScope};
use crate::control::conversation::ConversationStore;
use crate::runtime::agents::AgentDelegationPort;
use crate::runtime::agents::AgentStore;
use crate::runtime::state_machine::{
    NewRun, Run, RunTriggerKind, RuntimeEngine, RuntimeError, RuntimeIdentity, TurnInput,
    TurnOutcome,
};
use crate::runtime::turn_loop::{
    QuestionAnswer, QuestionService, StoredProjectionProvider, ToolDispatchService,
};
use crate::runtime::verification::ContractVerifier;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use chrono::Utc;
use quansio_capability::layer::{Layer, ProjectionInput};
use quansio_capability::persistence::{stage_projected_event, ProjectionArgument, ProjectionRow};
use quansio_capability::projection::{CapabilityProjection, ProjectionSubject, SubjectKind};
use quansio_capability::Grant;
use quansio_core::{
    CanonicalId, CommandId, CorrelationId, Digest, Generation, Prefix, UlidGenerator,
};
use quansio_events::EventStore;
use serde_json::{json, Value};

/// Apply a catalog command.
pub async fn invoke(
    State(state): State<ApiState>,
    Path(command): Path<String>,
    headers: HeaderMap,
    Json(request): Json<CommandRequest>,
) -> Result<Json<CommandResponse>, ApiError> {
    // The command surface is where a service principal may act (OPS-001).
    let scope = TenantScope::resolve_actor(&state, &headers).await?;
    let command_id = TenantScope::command_id(&request)?;
    let correlation_id = CorrelationId::generate(&mut UlidGenerator::new());
    let correlation = correlation_id.to_string();
    if let Some(principal) = &scope.principal {
        // The principal's explicit scopes name the §14 command families it may run.
        let family = super::enterprise::family_of(&state, &command);
        if !family.is_some_and(|family| principal.holds(family)) {
            let reason = format!(
                "{command} is in the {} family, which this service principal does not hold",
                family.unwrap_or("unknown")
            );
            let _ = state
                .enterprise()
                .audit_principal_command(principal, &command, "refused", &reason, &correlation)
                .await;
            return Err(ApiError::new(
                ApiErrorCode::ScopeForbidden,
                reason,
                correlation,
            ));
        }
    }
    if !state.limits().allow(&scope.tenant_id) {
        return Err(ApiError::new(
            ApiErrorCode::RateLimited,
            "this tenant has exceeded the API rate limit",
            correlation,
        ));
    }

    // The command log keeps a digest of the params. A payment value is short enough to be recovered from its digest, so
    // the digest of `SupplyPaymentInput` (and of `ImportBrowserSession`, whose cookies are sessions) covers the params without
    // their values.
    let digested = if command.eq_ignore_ascii_case("SupplyPaymentInput") {
        super::payment_inputs::redacted_params(&request.params)
    } else if command.eq_ignore_ascii_case("ImportBrowserSession") {
        super::browser_imports::redacted_params(&request.params)
    } else {
        request.params.clone()
    };
    let params_digest = Digest::of_canonical_json(&digested.to_string())
        .as_str()
        .to_string();
    let known = state
        .catalog()
        .commands()
        .iter()
        .any(|name| name.eq_ignore_ascii_case(&command));
    if !known {
        return Err(ApiError::new(
            ApiErrorCode::NotFound,
            format!("{command:?} is not a command in the catalog"),
            correlation,
        ));
    }
    // A command names a workspace of the caller's tenant or none at all (QA-007 F-7). Foreign keys
    // alone would admit another tenant's workspace id, so the tenant boundary is checked here, once,
    // before any owner sees the params; a malformed id is left to the owner's own validation.
    if let Some(workspace_id) = request
        .params
        .get("workspace_id")
        .and_then(Value::as_str)
        .filter(|id| id.starts_with("ws_") && id.len() == 29)
    {
        let mut tx = state.tenant_tx(&scope.tenant_id, &correlation).await?;
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM workspaces WHERE tenant_id = $1 AND id = $2)",
        )
        .bind(&scope.tenant_id)
        .bind(workspace_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| {
            ApiError::new(
                ApiErrorCode::Internal,
                "the workspace could not be checked",
                correlation.clone(),
            )
        })?;
        tx.commit().await.map_err(|_| {
            ApiError::new(
                ApiErrorCode::Internal,
                "the workspace could not be checked",
                correlation.clone(),
            )
        })?;
        if !exists {
            return Err(ApiError::new(
                ApiErrorCode::NotFound,
                format!("{workspace_id} is not a workspace of this tenant"),
                correlation,
            ));
        }
    }

    // The record's id is minted here (DOMAIN.md §1.1). The client's command_id is unique only within its
    // tenant (§1.2), so as the key it would let one tenant's id collide with another's instead of replaying.
    let record_id = CanonicalId::generate(Prefix::Command, &mut UlidGenerator::new());
    // The record is committed before the owner is called, so a resend while it works is told to come back.
    let mut log = state.tenant_tx(&scope.tenant_id, &correlation).await?;
    let accepted_at: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
        "INSERT INTO commands (id, tenant_id, command_name, command_id, params_digest, status, correlation_id) \
         VALUES ($1, $2, $3, $4, $5, 'accepted', $6) \
         ON CONFLICT (tenant_id, command_id) DO NOTHING RETURNING created_at",
    )
    .bind(record_id.to_string())
    .bind(&scope.tenant_id)
    .bind(&command)
    .bind(command_id.as_canonical().to_string())
    .bind(&params_digest)
    .bind(&correlation)
    .fetch_optional(&mut *log)
    .await
    .map_err(database_error)?;
    log.commit().await.map_err(database_error)?;

    let Some(accepted_at) = accepted_at else {
        return replay(&state, &scope, &command_id, &params_digest, &correlation).await;
    };

    let identity = RuntimeIdentity::system(&scope.tenant_id, "api", correlation_id)
        .with_command_id(command_id);
    let dispatched = dispatch(
        &state,
        &scope,
        &command,
        &command_id,
        &identity,
        &request,
        &correlation,
    )
    .await;
    if let Some(principal) = &scope.principal {
        let (decision, reason) = match &dispatched {
            Ok(_) => (
                "applied",
                format!("command_id {}", command_id.as_canonical()),
            ),
            Err(refusal) => ("refused", format!("{}: {}", refusal.code, refusal.message)),
        };
        let _ = state
            .enterprise()
            .audit_principal_command(principal, &command, decision, &reason, &correlation)
            .await;
    }
    let result = match dispatched {
        Ok(result) => result,
        Err(refusal) => {
            record_refusal(&state, &scope, &command_id, &refusal).await;
            return Err(refusal);
        }
    };

    // A one-time value (a webhook signing secret) is returned once and never kept.
    let mut log = state.tenant_tx(&scope.tenant_id, &correlation).await?;
    sqlx::query(
        "UPDATE commands SET status = 'applied', result = $3 WHERE tenant_id = $1 AND command_id = $2",
    )
    .bind(&scope.tenant_id)
    .bind(command_id.as_canonical().to_string())
    .bind(super::webhooks::stored_result(&result))
    .execute(&mut *log)
    .await
    .map_err(database_error)?;
    log.commit().await.map_err(database_error)?;

    Ok(Json(CommandResponse {
        command_id: command_id.as_canonical().to_string().to_string(),
        accepted_at,
        replayed: false,
        result,
    }))
}

/// Call the canonical owner of `command`. What it answers, a result or a refusal, is the command's outcome.
async fn dispatch(
    state: &ApiState,
    scope: &TenantScope,
    command: &str,
    command_id: &CommandId,
    identity: &RuntimeIdentity,
    request: &CommandRequest,
    correlation: &str,
) -> Result<Value, ApiError> {
    let user_id = scope.user_id.clone();
    let result = match command {
        "CreateWorkspace" | "UpdateWorkspace" | "UpdateUserSettings" => {
            identity_command(state, scope, command, &request.params, correlation).await?
        }
        "CancelRun" => cancel_run(state, identity, &request.params, correlation).await?,
        "PauseRun" => pause_run(state, identity, &request.params, correlation).await?,
        "ResumeRun" => resume_run(state, identity, &request.params, correlation).await?,
        "CreateThread" => {
            super::collaboration::create_thread(
                state,
                scope,
                identity,
                &request.params,
                correlation,
            )
            .await?
        }
        "AddParticipant" => {
            super::collaboration::add_participant(
                state,
                scope,
                identity,
                &request.params,
                correlation,
            )
            .await?
        }
        "RemoveParticipant" => {
            super::collaboration::remove_participant(
                state,
                scope,
                identity,
                &request.params,
                correlation,
            )
            .await?
        }
        "ReactToMessage" => {
            super::collaboration::react_to_message(
                state,
                scope,
                identity,
                &request.params,
                correlation,
            )
            .await?
        }
        "HandOffThread" => {
            super::collaboration::hand_off_thread(
                state,
                scope,
                identity,
                &request.params,
                correlation,
            )
            .await?
        }
        "PostMessage" => {
            post_message(
                state,
                scope,
                identity,
                &request.params,
                user_id.as_deref(),
                correlation,
            )
            .await?
        }
        "AnswerQuestion" => {
            answer_question(
                state,
                identity,
                &request.params,
                user_id.as_deref(),
                correlation,
            )
            .await?
        }
        "CancelTurn" => cancel_turn(state, identity, &request.params, correlation).await?,
        "ProposeGraphEdit" => {
            super::workgraph::propose_graph_edit(
                state,
                identity,
                user_id.as_deref(),
                &request.params,
                correlation,
            )
            .await?
        }
        "ApproveEffect" => {
            super::trust::approve_effect(
                state,
                identity,
                user_id.as_deref(),
                &request.params,
                correlation,
                chat_engine,
            )
            .await?
        }
        "DenyEffect" => {
            super::trust::deny_effect(
                state,
                identity,
                user_id.as_deref(),
                &request.params,
                correlation,
                chat_engine,
            )
            .await?
        }
        "CreateObjective" => {
            super::workgraph::create_objective(
                state,
                identity,
                user_id.as_deref(),
                &request.params,
                correlation,
            )
            .await?
        }
        "CreateArtifact" => {
            super::artifacts::create_artifact(
                state,
                &scope.tenant_id,
                user_id.as_deref(),
                &request.params,
                correlation,
            )
            .await?
        }
        "DeleteKnowledge" => {
            super::knowledge::delete_knowledge(
                state,
                identity,
                user_id.as_deref(),
                &request.params,
                correlation,
            )
            .await?
        }
        "SupersedeKnowledge" => {
            super::knowledge::supersede_knowledge(
                state,
                identity,
                user_id.as_deref(),
                &request.params,
                correlation,
            )
            .await?
        }
        "DeleteMemory" => {
            super::knowledge::delete_memory(
                state,
                identity,
                user_id.as_deref(),
                &request.params,
                correlation,
            )
            .await?
        }
        "InstallSkill" => {
            // Installing registers a skill in the tenant's registry: a tenant administrator's call,
            // like promotion (DOMAIN.md §11.5).
            super::rbac::Authorization::check(
                state.pool(),
                scope,
                None,
                crate::policy::rbac::ActionFamily::ManageTenant,
                correlation,
            )
            .await?;
            super::knowledge::install_skill(
                state,
                identity,
                user_id.as_deref(),
                &request.params,
                correlation,
            )
            .await?
        }
        // Pack installs (ADP-008): what a tenant may use changes for everyone, so both are a tenant
        // administrator's, like installing a skill.
        "InstallPack" => {
            super::rbac::Authorization::check(
                state.pool(),
                scope,
                None,
                crate::policy::rbac::ActionFamily::ManageTenant,
                correlation,
            )
            .await?;
            super::packs::install_pack(
                state,
                identity,
                command_id,
                user_id.as_deref(),
                &request.params,
                correlation,
            )
            .await?
        }
        "RollbackPack" => {
            super::rbac::Authorization::check(
                state.pool(),
                scope,
                None,
                crate::policy::rbac::ActionFamily::ManageTenant,
                correlation,
            )
            .await?;
            super::packs::rollback_pack(
                state,
                identity,
                user_id.as_deref(),
                &request.params,
                correlation,
            )
            .await?
        }
        "PromoteSkillVersion" => {
            // Promotion changes what enters production context: a tenant administrator's call.
            super::rbac::Authorization::check(
                state.pool(),
                scope,
                None,
                crate::policy::rbac::ActionFamily::ManageTenant,
                correlation,
            )
            .await?;
            super::knowledge::promote_skill_version(
                state,
                identity,
                user_id.as_deref(),
                &request.params,
                correlation,
            )
            .await?
        }
        "ProposeSkillCandidates" => {
            super::rbac::Authorization::check(
                state.pool(),
                scope,
                None,
                crate::policy::rbac::ActionFamily::ManageTenant,
                correlation,
            )
            .await?;
            super::knowledge::propose_skill_candidates(
                state,
                identity,
                user_id.as_deref(),
                &request.params,
                correlation,
            )
            .await?
        }
        "DisableSkill" => {
            // Disabling deprecates a version out of production context, irreversibly: a tenant administrator's call,
            // like promotion (DOMAIN.md §11.5).
            super::rbac::Authorization::check(
                state.pool(),
                scope,
                None,
                crate::policy::rbac::ActionFamily::ManageTenant,
                correlation,
            )
            .await?;
            super::knowledge::disable_skill(
                state,
                identity,
                user_id.as_deref(),
                &request.params,
                correlation,
            )
            .await?
        }
        // Skill recording (ADP-003): a person's commands. The handlers refuse a service principal and
        // build the owner fence from the authenticated session; the store checks it.
        "StartRecording" => {
            super::recordings::start_recording(state, scope, identity, &request.params, correlation)
                .await?
        }
        "FinishRecording" => {
            super::recordings::finish_recording(
                state,
                scope,
                identity,
                &request.params,
                correlation,
            )
            .await?
        }
        "CancelRecording" => {
            super::recordings::cancel_recording(
                state,
                scope,
                identity,
                &request.params,
                correlation,
            )
            .await?
        }
        "CompileRecording" => {
            super::recordings::compile_recording(
                state,
                scope,
                identity,
                &request.params,
                correlation,
            )
            .await?
        }
        "SubmitRecordingSkill" => {
            super::recordings::submit_recording_skill(
                state,
                scope,
                identity,
                &request.params,
                correlation,
            )
            .await?
        }
        "InviteMember" => {
            super::admin::invite_member(state, scope, identity, &request.params, correlation)
                .await?
        }
        "FreezeEffects" | "UnfreezeEffects" | "DisableProvider" | "EnableProvider"
        | "QuarantineWorker" | "DrainTarget" | "RevokeConnector" | "EngageKillSwitch"
        | "ReleaseKillSwitch" => {
            super::ops::command(
                state,
                scope,
                identity,
                command,
                &request.params,
                correlation,
            )
            .await?
        }
        "CreateServicePrincipal"
        | "RevokeServicePrincipal"
        | "ConfigureSso"
        | "SetGroupRoleMapping"
        | "RemoveGroupRoleMapping"
        | "RevokeUserSessions" => {
            super::enterprise::command(
                state,
                scope,
                identity,
                command,
                &request.params,
                correlation,
            )
            .await?
        }
        "UpdateMembership" => {
            super::admin::update_membership(state, scope, identity, &request.params, correlation)
                .await?
        }
        "RemoveMember" => {
            super::admin::remove_member(state, scope, identity, &request.params, correlation)
                .await?
        }
        "CreateTeammate" => {
            super::teammates::create_teammate(
                state,
                scope,
                identity,
                user_id.as_deref(),
                &request.params,
                correlation,
            )
            .await?
        }
        "UpdateTeammate" => {
            super::teammates::update_teammate(
                state,
                scope,
                identity,
                user_id.as_deref(),
                &request.params,
                correlation,
            )
            .await?
        }
        "ArchiveTeammate" => {
            super::teammates::archive_teammate(
                state,
                scope,
                identity,
                user_id.as_deref(),
                &request.params,
                correlation,
            )
            .await?
        }
        "CreateRoutine" => {
            super::routines::create_routine(state, scope, &request.params, correlation).await?
        }
        "UpdateRoutine" => {
            super::routines::update_routine(state, scope, &request.params, correlation).await?
        }
        "PauseRoutine" | "ResumeRoutine" | "DeleteRoutine" => {
            super::routines::transition_routine(state, scope, command, &request.params, correlation)
                .await?
        }
        "TriggerRoutineNow" => {
            super::routines::trigger_routine_now(
                state,
                scope,
                &command_id.as_canonical().to_string(),
                &request.params,
                correlation,
            )
            .await?
        }
        "DryRunRoutine" => {
            super::routines::dry_run_routine(state, scope, &request.params, correlation).await?
        }
        "RotateRoutineHook" => {
            super::routines::rotate_routine_hook(state, scope, &request.params, correlation).await?
        }
        "UpdateNotificationPreferences" => {
            super::routines::update_notification_preferences(
                state,
                scope,
                &request.params,
                correlation,
            )
            .await?
        }
        "MarkNotificationRead" => {
            super::routines::mark_notification_read(state, scope, &request.params, correlation)
                .await?
        }
        "RequestTakeover" => {
            super::live_sessions::request_takeover(
                state,
                scope,
                identity,
                user_id.as_deref(),
                &request.params,
                correlation,
            )
            .await?
        }
        // Remote control of a person's own computer (ADP-009, DOMAIN.md 8.6.2): a viewer asks, the person at the computer decides.
        "RequestRemoteControl" => {
            super::remote::request_remote_control(
                state,
                scope,
                identity,
                user_id.as_deref(),
                &request.params,
                correlation,
            )
            .await?
        }
        "GrantRemoteControl" => {
            super::remote::grant_remote_control(
                state,
                scope,
                identity,
                user_id.as_deref(),
                &request.params,
                correlation,
            )
            .await?
        }
        "DenyRemoteControl" => {
            super::remote::deny_remote_control(
                state,
                scope,
                identity,
                user_id.as_deref(),
                &request.params,
                correlation,
            )
            .await?
        }
        "StopRemoteControl" => {
            super::remote::stop_remote_control(
                state,
                scope,
                identity,
                user_id.as_deref(),
                &request.params,
                correlation,
            )
            .await?
        }
        // A person's answer to a certificate error or a camera/microphone request in the agent's browser (ADP-001).
        "ResolveBrowserCertificate" => {
            super::live_sessions::resolve_browser_certificate(
                state,
                scope,
                user_id.as_deref(),
                &request.params,
                correlation,
            )
            .await?
        }
        "DecideBrowserPermission" => {
            super::live_sessions::decide_browser_permission(
                state,
                scope,
                user_id.as_deref(),
                &request.params,
                correlation,
            )
            .await?
        }
        // A person's staged cookies for the agent's browser (ADP-009): sealed, never applied until browser.import_session is approved.
        "ImportBrowserSession" => {
            super::browser_imports::import_browser_session(
                state,
                scope,
                user_id.as_deref(),
                &request.params,
                correlation,
            )
            .await?
        }
        // A person's one-time payment values for one checkout (ADP-009): sealed, usable once, never returned.
        "SupplyPaymentInput" => {
            super::payment_inputs::supply_payment_input(
                state,
                scope,
                user_id.as_deref(),
                &request.params,
                correlation,
            )
            .await?
        }
        "Handback" => {
            super::live_sessions::handback(
                state,
                scope,
                identity,
                user_id.as_deref(),
                &request.params,
                correlation,
            )
            .await?
        }
        "CreateWebhookSubscription" => {
            super::webhooks::create_subscription(state, scope, &request.params, correlation).await?
        }
        "UpdateWebhookSubscription" => {
            super::webhooks::update_subscription(state, scope, &request.params, correlation).await?
        }
        "RedeliverWebhook" => {
            super::webhooks::redeliver(state, scope, &request.params, correlation).await?
        }
        "DeleteWebhookSubscription" => {
            super::webhooks::delete_subscription(state, scope, &request.params, correlation).await?
        }
        "SetBudget" => {
            super::admin::set_budget(
                state,
                scope,
                identity,
                user_id.as_deref(),
                &request.params,
                correlation,
            )
            .await?
        }
        "SetPolicy" => {
            super::admin::set_policy(
                state,
                scope,
                identity,
                user_id.as_deref(),
                &request.params,
                correlation,
            )
            .await?
        }
        other => {
            return Err(ApiError::new(
                ApiErrorCode::Internal,
                format!("{other} is in the catalog and has no handler in this build"),
                correlation,
            ));
        }
    };
    Ok(result)
}

/// Keep an owner's refusal as the command's outcome, so a resend is answered with it (DOMAIN.md §1.2).
///
/// A retryable refusal is kept as well. The owners are not idempotent by `command_id`, and some commit part
/// of their work before they can still fail (`PostMessage` stores the message before its turn runs), so
/// running a resend again could apply that part twice. Retrying such a refusal is a new command.
async fn record_refusal(
    state: &ApiState,
    scope: &TenantScope,
    command_id: &CommandId,
    refusal: &ApiError,
) {
    let recorded = async {
        let mut log = state
            .tenant_tx(&scope.tenant_id, &refusal.correlation_id)
            .await?;
        sqlx::query(
            "UPDATE commands SET status = 'rejected', error = $3 WHERE tenant_id = $1 AND command_id = $2",
        )
        .bind(&scope.tenant_id)
        .bind(command_id.as_canonical().to_string())
        .bind(json!(refusal))
        .execute(&mut *log)
        .await
        .map_err(database_error)?;
        log.commit().await.map_err(database_error)
    }
    .await;
    // The caller is still told the refusal. The record stays accepted, and a resend is told the outcome is
    // not known rather than that the command succeeded.
    if let Err(error) = recorded {
        eprintln!(
            "quansio-server: command {} was refused ({}) and the refusal was not recorded: {}",
            command_id.as_canonical(),
            refusal.code,
            error.message
        );
    }
}

async fn replay(
    state: &ApiState,
    scope: &TenantScope,
    command_id: &CommandId,
    params_digest: &str,
    correlation: &str,
) -> Result<Json<CommandResponse>, ApiError> {
    let mut log = state.tenant_tx(&scope.tenant_id, correlation).await?;
    let record = sqlx::query_as::<
        _,
        (
            String,
            String,
            Option<Value>,
            Option<Value>,
            chrono::DateTime<chrono::Utc>,
        ),
    >(
        "SELECT params_digest, status, result, error, created_at FROM commands \
         WHERE tenant_id = $1 AND command_id = $2",
    )
    .bind(&scope.tenant_id)
    .bind(command_id.as_canonical().to_string())
    .fetch_optional(&mut *log)
    .await
    .map_err(database_error)?;
    log.commit().await.map_err(database_error)?;
    let (digest, status, result, error, accepted_at) = record.ok_or_else(|| {
        ApiError::new(
            ApiErrorCode::Internal,
            "a command conflicted but cannot be read back",
            correlation.to_string(),
        )
    })?;
    if digest != params_digest {
        return Err(ApiError::new(
            ApiErrorCode::ConflictIdempotencyMismatch,
            "this command_id was already used with different parameters",
            correlation,
        ));
    }
    match status.as_str() {
        "applied" => Ok(Json(CommandResponse {
            command_id: command_id.as_canonical().to_string(),
            accepted_at,
            replayed: true,
            result: result.unwrap_or(Value::Null),
        })),
        // The original refusal, with its own status and correlation id.
        "rejected" => Err(error
            .and_then(|error| serde_json::from_value::<ApiError>(error).ok())
            .unwrap_or_else(|| {
                ApiError::new(
                    ApiErrorCode::Internal,
                    "a refused command's refusal cannot be read back",
                    correlation,
                )
            })),
        // Still accepted: the first submission has not recorded an outcome. Resending later returns it, so
        // this refusal is retryable though its code is not.
        _ => {
            let mut pending = ApiError::new(
                ApiErrorCode::ConflictState,
                "this command_id is still being applied; resend it later for its outcome",
                correlation,
            )
            .with_details(json!({ "command_status": status }));
            pending.retryable = true;
            Err(pending)
        }
    }
}

async fn cancel_run(
    state: &ApiState,
    identity: &RuntimeIdentity,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let (run_id, generation) = run_ref(params, correlation)?;
    // A native computer sequence in flight ends before it asks for another action (DOMAIN.md 8.6.1 B).
    state.computer_stops().stop(&run_id.to_string());
    let engine = RuntimeEngine::new(state.pool().clone(), identity.clone())
        .map_err(|error| internal(error, correlation))?;
    let run = engine
        .cancel(&run_id, generation)
        .await
        .map_err(|error| runtime_error(error, correlation))?;
    Ok(run_json(&run))
}

async fn pause_run(
    state: &ApiState,
    identity: &RuntimeIdentity,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let (run_id, generation) = run_ref(params, correlation)?;
    state.computer_stops().stop(&run_id.to_string());
    let engine = RuntimeEngine::new(state.pool().clone(), identity.clone())
        .map_err(|error| internal(error, correlation))?;
    let run = engine
        .suspend(&run_id, generation, "api")
        .await
        .map_err(|error| runtime_error(error, correlation))?;
    Ok(run_json(&run))
}

async fn resume_run(
    state: &ApiState,
    identity: &RuntimeIdentity,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let (run_id, generation) = run_ref(params, correlation)?;
    let engine = RuntimeEngine::new(state.pool().clone(), identity.clone())
        .map_err(|error| internal(error, correlation))?;
    let run = engine
        .resume(&run_id, generation)
        .await
        .map_err(|error| runtime_error(error, correlation))?;
    Ok(run_json(&run))
}

async fn post_message(
    state: &ApiState,
    scope: &TenantScope,
    identity: &RuntimeIdentity,
    params: &Value,
    user_id: Option<&str>,
    correlation: &str,
) -> Result<Value, ApiError> {
    let workspace_id = required_str(params, "workspace_id", correlation)?;
    let content = required_str(params, "content", correlation)?;
    let thread_id = params.get("thread_id").and_then(Value::as_str);
    let author = user_id
        .or_else(|| params.get("author_id").and_then(Value::as_str))
        .unwrap_or("usr_api");
    let attachments = match params.get("attachments") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str().map(str::to_string).ok_or_else(|| {
                    ApiError::new(
                        ApiErrorCode::ValidationSchema,
                        "attachments must be artifact ids",
                        correlation,
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => {
            return Err(ApiError::new(
                ApiErrorCode::ValidationSchema,
                "attachments must be an array of artifact ids",
                correlation,
            ))
        }
    };
    let mentions = super::collaboration::mentions_of(params, correlation)?;
    // Posting is authoring (APP-009): a viewer reads, an editor or admin writes, and in a
    // protected thread only its active participants post.
    super::rbac::Authorization::check(
        state.pool(),
        scope,
        Some(workspace_id),
        crate::policy::rbac::ActionFamily::AuthorWork,
        correlation,
    )
    .await?;
    let store = ConversationStore::new(state.pool().clone(), identity.clone())
        .map_err(|error| internal(error, correlation))?;
    store
        .check_attachments(workspace_id, &attachments)
        .await
        .map_err(|error| conversation_error(error, correlation))?;
    let collaboration = crate::control::collaboration::CollaborationStore::new(
        state.pool().clone(),
        identity.clone(),
    );
    let existing = match thread_id {
        Some(id) => {
            let (thread, _) =
                super::collaboration::readable_thread(state, scope, id, correlation).await?;
            if thread.workspace_id != workspace_id {
                return Err(ApiError::new(
                    ApiErrorCode::ValidationBounds,
                    format!("thread {id} is not in workspace {workspace_id}"),
                    correlation,
                ));
            }
            Some(thread)
        }
        None => None,
    };
    super::collaboration::check_mentions(
        &collaboration,
        existing.as_ref(),
        workspace_id,
        &mentions,
        correlation,
    )
    .await?;
    // Who the message is for: the teammate named, else the first teammate mentioned, else
    // the thread's responsible teammate, else the workspace default (APP-014).
    let responsible = match &existing {
        Some(thread) => collaboration
            .responsible_teammate(&thread.id)
            .await
            .map_err(|error| super::collaboration::collaboration_error(error, correlation))?,
        None => None,
    };
    let addressed = params
        .get("teammate_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            mentions
                .iter()
                .find(|mention| mention.kind == "teammate")
                .map(|mention| mention.id.clone())
        })
        .or(responsible);
    let work_node = load_work_node(state, &identity.tenant_id, workspace_id, correlation).await?;
    let (agent_thread, teammate) = route_to_teammate(
        state,
        identity,
        workspace_id,
        &work_node,
        addressed.as_deref(),
        correlation,
    )
    .await?;
    let thread = match existing {
        Some(thread) => thread,
        None => {
            collaboration
                .create_thread(workspace_id, "direct", None, user_id, Vec::new())
                .await
                .map_err(|error| super::collaboration::collaboration_error(error, correlation))?
                .0
        }
    };
    let mut new = crate::control::conversation::NewMessage::user_text(author, content);
    new.attachments = attachments;
    new.mentions = mentions;
    let (thread, message, _) = collaboration
        .post(
            &thread.id,
            new,
            teammate.as_ref().map(|teammate| teammate.id.to_string()),
        )
        .await
        .map_err(|error| super::collaboration::collaboration_error(error, correlation))?;
    let posted = crate::control::conversation::PostedMessage { thread, message };

    let mut new_run = NewRun::new(
        workspace_id.to_string(),
        work_node,
        agent_thread,
        RunTriggerKind::Message,
    );
    new_run.trigger_ref = Some(posted.message.id.clone());
    // The teammate's default execution target binds its runs; the workspace's first target
    // is the fallback the walking skeleton always had.
    let preferred = teammate
        .as_ref()
        .and_then(|teammate| teammate.default_execution_target_id.clone());
    if let Some(target) = preferred {
        new_run.execution_target_id = Some(target);
    } else if let Some(target) = load_target(state, &identity.tenant_id, workspace_id).await? {
        new_run.execution_target_id = Some(target);
    }
    let engine =
        chat_engine(state, identity.clone()).map_err(|error| internal(error, correlation))?;
    let run = engine
        .create_run(new_run)
        .await
        .map_err(|error| runtime_error(error, correlation))?;
    seed_projection(state, identity, workspace_id, &run)
        .await
        .map_err(|error| internal(error, correlation))?;
    let teammate_json = teammate.as_ref().map(|teammate| {
        json!({ "id": teammate.id.to_string(), "name": teammate.name, "version": teammate.version })
    });
    let run = engine
        .enqueue(&run.id, run.generation)
        .await
        .map_err(|error| runtime_error(error, correlation))?;
    let run = engine
        .start(&run.id, run.generation)
        .await
        .map_err(|error| runtime_error(error, correlation))?;
    let turn = {
        let (state, identity, correlation) =
            (state.clone(), identity.clone(), correlation.to_string());
        let (run_id, generation) = (run.id, run.generation);
        let (message_id, thread_id) = (posted.message.id.clone(), posted.thread.id.clone());
        let agent = agent_thread.to_string();
        async move {
            let outcome = engine
                .run_turn(
                    &run_id,
                    generation,
                    TurnInput::new(RunTriggerKind::Message).with_reference(message_id),
                )
                .await
                .map_err(|error| runtime_error(error, &correlation))?;
            let store = ConversationStore::new(state.pool().clone(), identity)
                .map_err(|error| internal(error, &correlation))?;
            let reply = post_agent_reply(
                &store,
                &thread_id,
                &agent,
                &run_id.to_string(),
                &outcome,
                &correlation,
            )
            .await?;
            Ok((outcome, reply))
        }
    };
    let mut result = json!({
        "thread_id": posted.thread.id,
        "message_id": posted.message.id,
        "run_id": run.id.to_string(),
        "run_status": run.status.as_db_str(),
        "teammate": teammate_json,
    });
    if let Some((outcome, reply)) = continue_run(state, turn).await? {
        result["turn"] = turn_json(&outcome);
        result["reply_message_id"] = json!(reply);
    }
    Ok(result)
}

/// Continue a run with its next turn and post the teammate's reply. Served, the command answers
/// once it is accepted and the turn runs after it ([`ApiState::with_background_turns`]); its
/// progress, reply, and any question or approval it raises reach clients as events and live
/// frames, and a failure is the run's own (logged here). Otherwise the command waits for the turn
/// and reports its outcome and reply.
pub(super) async fn continue_run<F>(
    state: &ApiState,
    turn: F,
) -> Result<Option<(TurnOutcome, Option<String>)>, ApiError>
where
    F: std::future::Future<Output = Result<(TurnOutcome, Option<String>), ApiError>>
        + Send
        + 'static,
{
    if !state.background_turns() {
        return turn.await.map(Some);
    }
    tokio::spawn(async move {
        if let Err(error) = turn.await {
            crate::observability::notice(
                crate::observability::Level::Warn,
                &format!(
                    "a turn after its accepted command failed: {} ({})",
                    error.message, error.code
                ),
            );
        }
    });
    Ok(None)
}

/// The assistant text of a completed turn becomes the agent's durable message in the thread.
pub(super) async fn post_agent_reply(
    store: &ConversationStore,
    thread_id: &str,
    agent_id: &str,
    run_id: &str,
    outcome: &TurnOutcome,
    correlation: &str,
) -> Result<Option<String>, ApiError> {
    // A verified completion keeps what the model said alongside its claim as the reply.
    let (turn_id, text) = match outcome {
        TurnOutcome::Completed {
            turn_id,
            assistant_text: Some(text),
        }
        | TurnOutcome::Succeeded {
            turn_id,
            assistant_text: Some(text),
            ..
        } => (turn_id, text),
        _ => return Ok(None),
    };
    if text.trim().is_empty() {
        return Ok(None);
    }
    let message = store
        .post_agent_message(thread_id, agent_id, text, run_id, turn_id)
        .await
        .map_err(|error| conversation_error(error, correlation))?;
    Ok(Some(message.id))
}

/// `AnswerQuestion {question_id, value}`: record the answer, release the parked run and
/// run its next turn (RUN-011 resume) so the reply follows the answer.
async fn answer_question(
    state: &ApiState,
    identity: &RuntimeIdentity,
    params: &Value,
    user_id: Option<&str>,
    correlation: &str,
) -> Result<Value, ApiError> {
    let question_id = required_str(params, "question_id", correlation)?;
    let value = params.get("value").cloned().ok_or_else(|| {
        ApiError::new(
            ApiErrorCode::ValidationSchema,
            "AnswerQuestion needs a value",
            correlation,
        )
    })?;
    let answering_user = user_id
        .or_else(|| params.get("user_id").and_then(Value::as_str))
        .unwrap_or("usr_api")
        .to_string();
    let questions = QuestionService::new(state.pool().clone(), identity.clone())
        .map_err(|error| internal(error, correlation))?;
    let answered = questions
        .answer(
            question_id,
            QuestionAnswer {
                user_id: answering_user,
                value,
            },
        )
        .await
        .map_err(|error| runtime_error(error, correlation))?;
    let mut result = json!({
        "question_id": answered.question.id,
        "run_id": answered.question.run_id,
        "status": answered.question.status.as_str(),
        "resumed_run_id": answered.resumed_run_id,
    });
    let Some(resumed) = answered.resumed_run_id.as_deref() else {
        return Ok(result);
    };
    let run_id = CanonicalId::parse_typed(resumed, Prefix::Run)
        .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), correlation))?;
    let engine =
        chat_engine(state, identity.clone()).map_err(|error| internal(error, correlation))?;
    let run = engine
        .store()
        .load_run(&run_id)
        .await
        .map_err(|error| runtime_error(error, correlation))?;
    let thread_id = thread_of_run(state, &identity.tenant_id, resumed).await?;
    if let Some(thread_id) = &thread_id {
        result["thread_id"] = json!(thread_id);
    }
    let turn = {
        let (state, identity, correlation) =
            (state.clone(), identity.clone(), correlation.to_string());
        let (question_id, resumed) = (question_id.to_string(), resumed.to_string());
        async move {
            let outcome = engine
                .run_turn(
                    &run.id,
                    run.generation,
                    TurnInput::new(RunTriggerKind::Wake).with_reference(question_id),
                )
                .await
                .map_err(|error| runtime_error(error, &correlation))?;
            let reply = match thread_id {
                Some(thread_id) => {
                    let store = ConversationStore::new(state.pool().clone(), identity)
                        .map_err(|error| internal(error, &correlation))?;
                    post_agent_reply(
                        &store,
                        &thread_id,
                        &run.agent_thread_id.to_string(),
                        &resumed,
                        &outcome,
                        &correlation,
                    )
                    .await?
                }
                None => None,
            };
            Ok((outcome, reply))
        }
    };
    if let Some((outcome, reply)) = continue_run(state, turn).await? {
        result["turn"] = turn_json(&outcome);
        if result.get("thread_id").is_some() {
            result["reply_message_id"] = json!(reply);
        }
    }
    Ok(result)
}

/// `CancelTurn {run_id, generation?}`: cancel the run the turn belongs to and withdraw its
/// open questions; the durable `run.cancelled` event tells every client the turn stopped.
async fn cancel_turn(
    state: &ApiState,
    identity: &RuntimeIdentity,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let (run_id, generation) = run_ref(params, correlation)?;
    let engine = RuntimeEngine::new(state.pool().clone(), identity.clone())
        .map_err(|error| internal(error, correlation))?;
    let run = engine
        .cancel(&run_id, generation)
        .await
        .map_err(|error| runtime_error(error, correlation))?;
    let questions = QuestionService::new(state.pool().clone(), identity.clone())
        .map_err(|error| internal(error, correlation))?;
    let cancelled = questions
        .cancel_for_run(&run_id.to_string())
        .await
        .map_err(|error| runtime_error(error, correlation))?;
    let mut result = run_json(&run);
    result["cancelled_questions"] = json!(cancelled);
    Ok(result)
}

/// The conversation thread a run was triggered from, when its trigger was a message.
pub(super) async fn thread_of_run(
    state: &ApiState,
    tenant_id: &str,
    run_id: &str,
) -> Result<Option<String>, ApiError> {
    let mut tx = state.tenant_tx(tenant_id, "database").await?;
    let thread = sqlx::query_scalar(
        "SELECT m.thread_id FROM runs r JOIN messages m ON m.id = r.trigger_ref AND m.tenant_id = r.tenant_id \
         WHERE r.tenant_id = $1 AND r.id = $2",
    )
    .bind(tenant_id)
    .bind(run_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(database_error)?;
    tx.commit().await.map_err(database_error)?;
    Ok(thread)
}

pub(super) fn chat_engine(
    state: &ApiState,
    identity: RuntimeIdentity,
) -> Result<RuntimeEngine, RuntimeError> {
    let delegation: Arc<dyn crate::runtime::state_machine::DelegationPort> = Arc::new(
        AgentDelegationPort::with_structural_check(state.pool().clone(), identity.clone())?,
    );
    let projections = StoredProjectionProvider::new(state.pool().clone(), identity.clone())?;
    let mut dispatcher = ToolDispatchService::new(
        state.pool().clone(),
        identity.clone(),
        Arc::new(projections),
        state.roles(),
        state.host(),
        Arc::clone(&delegation),
    )?
    .with_plan_port(Arc::new(crate::runtime::planning::RunPlanApplication::new(
        state.pool().clone(),
        identity.clone(),
    )?))
    // A call's derived trust comes from what its turn's model calls were shown (INT-012).
    .with_proposal_trust(Arc::new(crate::policy::StoredProposalTrust::new(
        state.pool().clone(),
        &identity,
    )));
    // memory.propose reaches INT-007's gated owner through the intelligence plane; without
    // one the seam stays unavailable and the call is refused, never silently dropped.
    if let Some(client) = state.intelligence() {
        let deadline = crate::intelligence::model_source::ModelCallSettings::load()
            .map_err(RuntimeError::ProtocolState)?
            .deadline;
        dispatcher = dispatcher.with_memory_port(Arc::new(
            crate::intelligence::memory::GatewayMemoryProposals::new(
                state.pool().clone(),
                identity.clone(),
                client.clone(),
                deadline,
            )?,
        ));
    }
    let questions = QuestionService::new(state.pool().clone(), identity.clone())?;
    let live: Arc<dyn crate::runtime::state_machine::LiveFramePort> = state.live().clone();
    let budgets =
        crate::runtime::budgets::RuntimeBudgets::new(state.pool().clone(), identity.clone())?;
    // A completion claim reaches SUCCEEDED only through the contract verifier (RUN-008), with
    // the plane's independent semantic verifier when the deployment has one.
    let verifier = ContractVerifier::new(state.pool().clone(), identity.clone())?
        .with_semantic_verifier(state.semantic_verifier());
    Ok(RuntimeEngine::new(state.pool().clone(), identity)?
        .with_budgets(Arc::new(budgets))
        .with_verification(Arc::new(verifier))
        .with_model_source(state.model())
        .with_tool_dispatch(Arc::new(dispatcher))
        .with_delegation(delegation)
        .with_question_port(Arc::new(questions))
        .with_live_frames(live))
}

/// The AgentThread a message runs under: the named teammate's (`teammate_id`), or the
/// workspace's default teammate's — the oldest active, available definition, provisioned
/// from the `general-assistant` template when the workspace has none (configuration, not
/// code). A teammate that is archived or away refuses new runs; a pre-definition
/// teammate thread (the walking skeleton's) is kept for a workspace that still has one and
/// no definition.
pub(super) async fn route_to_teammate(
    state: &ApiState,
    identity: &RuntimeIdentity,
    workspace_id: &str,
    work_node: &CanonicalId,
    addressed: Option<&str>,
    correlation: &str,
) -> Result<(CanonicalId, Option<crate::control::teammates::Teammate>), ApiError> {
    let store = crate::control::teammates::TeammateStore::new(state.pool().clone());
    let teammate_error = |error: crate::control::teammates::TeammateError| {
        let code = match error.code() {
            "NOT_FOUND" => ApiErrorCode::NotFound,
            "CONFLICT_STATE" => ApiErrorCode::ConflictState,
            "VALIDATION_SCHEMA" => ApiErrorCode::ValidationSchema,
            _ => ApiErrorCode::Internal,
        };
        ApiError::new(code, error.to_string(), correlation)
    };
    let teammate = match addressed {
        Some(id) => {
            let id = CanonicalId::parse_typed(id, Prefix::Teammate).map_err(|error| {
                ApiError::new(
                    ApiErrorCode::ValidationSchema,
                    format!("teammate_id: {error}"),
                    correlation,
                )
            })?;
            let teammate = store
                .get(&identity.tenant_id, &id)
                .await
                .map_err(teammate_error)?;
            if teammate.workspace_id != workspace_id {
                return Err(ApiError::new(
                    ApiErrorCode::NotFound,
                    format!("teammate {id} is not in workspace {workspace_id}"),
                    correlation,
                ));
            }
            Some(teammate)
        }
        None => {
            let default = store
                .default_for(&identity.tenant_id, workspace_id)
                .await
                .map_err(teammate_error)?;
            match default {
                Some(teammate) => Some(teammate),
                None => {
                    let mut tx = state.tenant_tx(&identity.tenant_id, correlation).await?;
                    let legacy: Option<String> = sqlx::query_scalar(
                        "SELECT id FROM agent_threads WHERE tenant_id = $1 AND workspace_id = $2 \
                         AND agent_kind = 'teammate' AND definition_id IS NULL \
                         AND status = 'ACTIVE' ORDER BY created_at LIMIT 1",
                    )
                    .bind(&identity.tenant_id)
                    .bind(workspace_id)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(database_error)?;
                    tx.commit().await.map_err(database_error)?;
                    if let Some(id) = legacy {
                        let id = CanonicalId::parse_typed(&id, Prefix::AgentThread).map_err(
                            |error| {
                                ApiError::new(
                                    ApiErrorCode::Internal,
                                    error.to_string(),
                                    correlation,
                                )
                            },
                        )?;
                        return Ok((id, None));
                    }
                    let admin = crate::control::teammates::TeammateAdmin {
                        tenant_id: identity.tenant_id.clone(),
                        actor: identity.actor.clone(),
                        correlation_id: identity.correlation_id,
                    };
                    let (teammate, thread) = store
                        .create(
                            &admin,
                            identity,
                            workspace_id,
                            crate::control::teammates::NewTeammate {
                                template_id: Some("general-assistant".to_string()),
                                ..Default::default()
                            },
                        )
                        .await
                        .map_err(teammate_error)?;
                    let _ = work_node;
                    return Ok((thread.id, Some(teammate)));
                }
            }
        }
    };
    let Some(teammate) = teammate else {
        unreachable!("every arm above returns or names a teammate");
    };
    if teammate.status != "active" {
        return Err(ApiError::new(
            ApiErrorCode::ConflictState,
            format!(
                "teammate {} ({}) is archived and takes no new message",
                teammate.name, teammate.id
            ),
            correlation,
        ));
    }
    if teammate.availability != "available" {
        return Err(ApiError::new(
            ApiErrorCode::ConflictState,
            format!(
                "teammate {} ({}) is {}",
                teammate.name, teammate.id, teammate.availability
            ),
            correlation,
        ));
    }
    let thread = store
        .provision_thread(identity, &teammate)
        .await
        .map_err(teammate_error)?;
    let agents = AgentStore::new(state.pool().clone(), identity.clone())
        .map_err(|error| internal(error, correlation))?;
    // A suspended thread of an active teammate (the definition was archived and recreated,
    // or the runtime suspended it) resumes; the definition decides. Two messages routed to the
    // teammate at once both find it inactive, and the second finds it already activated.
    let thread = agents
        .ensure_active(thread)
        .await
        .map_err(|error| runtime_error(error, correlation))?;
    Ok((thread.id, Some(teammate)))
}

async fn load_work_node(
    state: &ApiState,
    tenant_id: &str,
    workspace_id: &str,
    correlation: &str,
) -> Result<CanonicalId, ApiError> {
    let mut tx = state.tenant_tx(tenant_id, correlation).await?;
    let id: Option<String> = sqlx::query_scalar(
        "SELECT id FROM work_nodes WHERE tenant_id = $1 AND workspace_id = $2 ORDER BY created_at LIMIT 1",
    )
    .bind(tenant_id)
    .bind(workspace_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(database_error)?;
    tx.commit().await.map_err(database_error)?;
    let id = id.ok_or_else(|| {
        ApiError::new(
            ApiErrorCode::ValidationBounds,
            "the workspace has no work node",
            correlation,
        )
    })?;
    CanonicalId::parse_typed(&id, Prefix::WorkNode)
        .map_err(|error| ApiError::new(ApiErrorCode::Internal, error.to_string(), correlation))
}

pub(super) async fn load_target(
    state: &ApiState,
    tenant_id: &str,
    workspace_id: &str,
) -> Result<Option<String>, ApiError> {
    let mut tx = state.tenant_tx(tenant_id, "database").await?;
    let target = sqlx::query_scalar(
        "SELECT id FROM execution_targets WHERE tenant_id = $1 AND workspace_id = $2 \
         ORDER BY created_at LIMIT 1",
    )
    .bind(tenant_id)
    .bind(workspace_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(database_error)?;
    tx.commit().await.map_err(database_error)?;
    Ok(target)
}

pub(super) async fn seed_projection(
    state: &ApiState,
    identity: &RuntimeIdentity,
    workspace_id: &str,
    run: &Run,
) -> Result<(), String> {
    // The run's projection covers what the workspace's policies allow or ask for: policy
    // still decides per call (allow → dispatch, ask → approval), but a class no policy
    // mentions is not even in the projection. The skeleton's read grant stays as the floor.
    let mut grants: Vec<Grant> = vec![Grant::from_json(&json!({
        "effect_class": "read.internal",
        "resource": { "kind": "fs", "selector": "**" },
        "constraints": {},
    }))
    .expect("grant")];
    // The inputs are recorded (APP-011): which policies and which active skill versions
    // the run's authority was resolved from, one input per layer in the fixed order, so
    // the projection says why the run may do what it may. Skills are an input that never
    // contributes a grant (DOMAIN.md §11.5).
    let mut inputs: Vec<ProjectionInput> = Vec::new();
    if let Ok(policies) = crate::policy::PolicyStore::new(state.pool().clone(), identity.clone()) {
        if let Ok(set) = policies.load_policy_set(workspace_id).await {
            for policy in set.in_order() {
                let layer = match policy.scope {
                    crate::policy::PolicyScope::Tenant => Layer::TenantPolicy,
                    crate::policy::PolicyScope::Workspace => Layer::WorkspacePolicy,
                };
                inputs.push(ProjectionInput::new(
                    layer,
                    format!("{}@{}", policy.id, policy.version),
                    digest_of(&json!(policy.rules)),
                ));
            }
            for rule in set.in_order().iter().flat_map(|policy| policy.rules.iter()) {
                if matches!(
                    rule.decision,
                    quansio_capability::PolicyDecision::Allow
                        | quansio_capability::PolicyDecision::Ask
                ) {
                    let grant = Grant::new(
                        rule.effect_class.clone(),
                        rule.resource_selector.clone(),
                        quansio_capability::Constraints::default(),
                    );
                    if !grants.contains(&grant) {
                        grants.push(grant);
                    }
                }
            }
        }
    }
    // The teammate definition behind the run's thread (APP-014): recorded as the
    // `agent_definition` input, and its needs narrow the grants — never widen them. The
    // definition's text (persona, standing instructions) is not read here at all.
    let definition = crate::control::teammates::TeammateStore::new(state.pool().clone())
        .turn_definition(&identity.tenant_id, &run.agent_thread_id.to_string())
        .await
        .ok()
        .flatten();
    if let Some(definition) = definition {
        let store = crate::control::teammates::TeammateStore::new(state.pool().clone());
        if let Ok(id) = CanonicalId::parse_typed(&definition.teammate_id, Prefix::Teammate) {
            if let Ok(teammate) = store.get(&identity.tenant_id, &id).await {
                inputs.push(ProjectionInput::new(
                    Layer::AgentDefinition,
                    format!("{}@v{}", teammate.id, teammate.version),
                    teammate.digest(),
                ));
                grants = crate::control::teammates::narrow_by_needs(
                    grants,
                    &teammate.default_capability_needs,
                );
            }
        }
    }
    if let Ok(active) = crate::control::skills::SkillStore::new(state.pool().clone())
        .active_versions(&identity.tenant_id)
        .await
    {
        if !active.is_empty() {
            let versions: Vec<Value> = active
                .iter()
                .map(|(skill, version)| {
                    json!({
                        "skill_id": skill.id.to_string(),
                        "skill_version_id": version.id.to_string(),
                        "semver": version.semver,
                        "manifest_digest": digest_of(&version.manifest).to_string(),
                    })
                })
                .collect();
            let reference = active
                .iter()
                .map(|(_, version)| version.id.to_string())
                .collect::<Vec<_>>()
                .join(",");
            inputs.push(ProjectionInput::new(
                Layer::ActiveSkills,
                reference,
                digest_of(&json!(versions)),
            ));
        }
    }
    let mut generator = UlidGenerator::new();
    let projection = CapabilityProjection::from_parts(
        CanonicalId::generate(Prefix::CapabilityProjection, &mut generator),
        ProjectionSubject::new(SubjectKind::Run, run.id.to_string()),
        inputs,
        grants,
        Utc::now(),
        Some(Utc::now() + chrono::Duration::hours(1)),
    )
    .map_err(|error| error.to_string())?;
    let row = ProjectionRow::from_projection(
        &projection,
        identity.tenant_id.clone(),
        Some(workspace_id.to_string()),
    );
    let store = EventStore::new(state.pool().clone());
    let actor = identity.actor.clone();
    let correlation = identity.correlation_id;
    store
        .commit_mutation_tx(&identity.tenant_id, move |tx, batch| {
            Box::pin(async move {
                let mut query = sqlx::query(ProjectionRow::INSERT_SQL);
                for argument in row.arguments() {
                    query = match argument {
                        ProjectionArgument::Text(value) => query.bind(value),
                        ProjectionArgument::OptionalText(value) => query.bind(value),
                        ProjectionArgument::Json(value) => query.bind(value),
                        ProjectionArgument::Timestamptz(value) => query.bind(value),
                        ProjectionArgument::OptionalTimestamptz(value) => query.bind(value),
                    };
                }
                query.execute(&mut **tx).await?;
                stage_projected_event(batch, &row, correlation, actor);
                Ok(())
            })
        })
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// SHA-256 over a value's canonical JSON (serde_json objects serialize with sorted keys).
fn digest_of(value: &Value) -> Digest {
    Digest::of_canonical_json(&value.to_string())
}

fn run_ref(params: &Value, correlation: &str) -> Result<(CanonicalId, Generation), ApiError> {
    let run_id =
        CanonicalId::parse_typed(required_str(params, "run_id", correlation)?, Prefix::Run)
            .map_err(|error| {
                ApiError::new(
                    ApiErrorCode::ValidationBounds,
                    error.to_string(),
                    correlation,
                )
            })?;
    let generation = Generation::new(
        params
            .get("generation")
            .and_then(Value::as_u64)
            .unwrap_or(Generation::INITIAL.get()),
    )
    .map_err(|error| {
        ApiError::new(
            ApiErrorCode::ValidationBounds,
            error.to_string(),
            correlation,
        )
    })?;
    Ok((run_id, generation))
}

fn required_str<'a>(params: &'a Value, key: &str, correlation: &str) -> Result<&'a str, ApiError> {
    params.get(key).and_then(Value::as_str).ok_or_else(|| {
        ApiError::new(
            ApiErrorCode::ValidationSchema,
            format!("{key} is required"),
            correlation,
        )
    })
}

fn run_json(run: &Run) -> Value {
    json!({ "run_id": run.id.to_string(), "status": run.status.as_db_str() })
}

pub(super) fn turn_json(outcome: &TurnOutcome) -> Value {
    match outcome {
        TurnOutcome::Completed {
            turn_id,
            assistant_text,
        } => json!({
            "kind": "completed",
            "turn_id": turn_id,
            "assistant_text": assistant_text,
        }),
        TurnOutcome::Succeeded {
            run_id,
            turn_id,
            evidence_ids,
            assistant_text,
        } => json!({
            "kind": "succeeded",
            "run_id": run_id,
            "turn_id": turn_id,
            "evidence_ids": evidence_ids,
            "assistant_text": assistant_text,
        }),
        TurnOutcome::Parked {
            run_id,
            turn_id,
            state,
            wait_key,
        } => json!({
            "kind": "parked",
            "run_id": run_id,
            "turn_id": turn_id,
            "state": state.as_db_str(),
            "wait_key": wait_key,
        }),
        TurnOutcome::BudgetExhausted {
            turn_id,
            steps_used,
            max_steps,
        } => json!({
            "kind": "budget_exhausted",
            "turn_id": turn_id,
            "steps_used": steps_used,
            "max_steps": max_steps,
        }),
        TurnOutcome::Suspended {
            run_id,
            turn_id,
            reason,
        } => json!({
            "kind": "suspended",
            "run_id": run_id,
            "turn_id": turn_id,
            "reason": reason,
        }),
        other => json!({ "kind": "other", "debug": format!("{other:?}") }),
    }
}

pub(super) fn database_error(error: sqlx::Error) -> ApiError {
    ApiError::new(ApiErrorCode::Internal, error.to_string(), "database")
}

pub(super) fn internal(error: impl std::fmt::Display, correlation: &str) -> ApiError {
    ApiError::new(ApiErrorCode::Internal, error.to_string(), correlation)
}

fn conversation_error(
    error: crate::control::conversation::ConversationError,
    correlation: &str,
) -> ApiError {
    let code = if matches!(
        error,
        crate::control::conversation::ConversationError::NotFound(_)
    ) {
        ApiErrorCode::NotFound
    } else {
        ApiErrorCode::ValidationBounds
    };
    ApiError::new(code, error.to_string(), correlation)
}

pub(super) fn runtime_error(error: RuntimeError, correlation: &str) -> ApiError {
    // The runtime's codes are the catalog's spellings (`RuntimeError::code`), so a model or
    // context failure keeps its code (`PROVIDER_UNAVAILABLE`, `ROUTE_UNAVAILABLE`, …); a code
    // the catalog does not know is an internal fault, not a client error.
    let code = match error.code() {
        "RUNTIME_LEASE_LOST" => ApiErrorCode::LeaseLost,
        spelled => ApiErrorCode::ALL
            .into_iter()
            .find(|code| code.as_str() == spelled)
            .unwrap_or(ApiErrorCode::Internal),
    };
    ApiError::new(code, error.to_string(), correlation)
}

/// §14 identity commands: `CreateWorkspace`, `UpdateWorkspace`, `UpdateUserSettings` (APP-002).
///
/// Every one of them needs an acting user, so a trusted-header request without
/// `x-quansio-user` and a bearer-less request are both refused with `AUTH_REQUIRED`.
async fn identity_command(
    state: &ApiState,
    scope: &TenantScope,
    command: &str,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let Some(user_id) = scope.user_id.as_deref() else {
        return Err(ApiError::new(
            ApiErrorCode::AuthRequired,
            format!("{command} needs an acting user"),
            correlation,
        ));
    };
    let identity = state.auth().identity();
    let text = |key: &str| params.get(key).and_then(Value::as_str);
    let schema =
        |message: String| ApiError::new(ApiErrorCode::ValidationSchema, message, correlation);
    // The owner's refusal keeps its own code; a database or event-store failure is `INTERNAL`
    // and never echoes the database's message (table names, policy names) to the caller.
    let refused = |error: crate::control::identity::IdentityError| {
        use crate::control::identity::IdentityError;
        match error {
            IdentityError::Schema(message) => schema(message),
            IdentityError::NotFound(message) => {
                ApiError::new(ApiErrorCode::NotFound, message, correlation)
            }
            IdentityError::Conflict(message) => {
                ApiError::new(ApiErrorCode::ConflictState, message, correlation)
            }
            IdentityError::Event(_) | IdentityError::Database(_) => ApiError::new(
                ApiErrorCode::Internal,
                format!("{command} could not be completed"),
                correlation,
            ),
        }
    };
    let outcome = match command {
        "CreateWorkspace" => {
            let name =
                text("name").ok_or_else(|| schema("CreateWorkspace needs `name`".to_string()))?;
            let workspace_id = identity
                .create_workspace(&scope.tenant_id, user_id, name)
                .await
                .map_err(refused)?;
            json!({ "workspace_id": workspace_id, "tenant_id": scope.tenant_id, "name": name.trim() })
        }
        "UpdateWorkspace" => {
            let workspace_id = text("workspace_id")
                .ok_or_else(|| schema("UpdateWorkspace needs `workspace_id`".to_string()))?;
            let change = identity
                .update_workspace(
                    &scope.tenant_id,
                    user_id,
                    workspace_id,
                    text("name"),
                    params.get("settings"),
                )
                .await
                .map_err(refused)?;
            json!({
                "workspace_id": workspace_id,
                "effective_settings": change.effective,
                "effects": change.effects.iter().map(|effect| effect.to_json()).collect::<Vec<_>>(),
            })
        }
        _ => {
            let workspace_id = text("workspace_id").ok_or_else(|| {
                schema(
                    "UpdateUserSettings needs `workspace_id` to report effective settings"
                        .to_string(),
                )
            })?;
            let settings = params
                .get("settings")
                .ok_or_else(|| schema("UpdateUserSettings needs `settings`".to_string()))?;
            let change = identity
                .update_user_settings(&scope.tenant_id, workspace_id, user_id, settings)
                .await
                .map_err(refused)?;
            json!({
                "user_id": user_id,
                "workspace_id": workspace_id,
                "effective_settings": change.effective,
                "effects": change.effects.iter().map(|effect| effect.to_json()).collect::<Vec<_>>(),
            })
        }
    };
    Ok(outcome)
}
