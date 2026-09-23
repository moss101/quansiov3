//! Collaboration commands and the protected thread projections (APP-009, DOMAIN.md §3.1).
//!
//! `CreateThread`, `AddParticipant`, `RemoveParticipant`, `ReactToMessage` and
//! `HandOffThread` go through the collaboration owner (`control::collaboration`), which
//! composes the conversation owner in one transaction. Every command and every thread
//! projection first asks [`thread_access`]: a protected thread answers `SCOPE_FORBIDDEN`
//! (`details.kind = "thread"`) to anyone who is not an active participant with an active
//! workspace membership, whatever their workspace role — an admin manages membership, it
//! does not read other people's conversations.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use quansio_core::{CanonicalId, CorrelationId, Prefix, UlidGenerator};
use serde::Deserialize;
use serde_json::{json, Value};

use super::rbac::Authorization;
use super::{ApiError, ApiErrorCode, ApiState, TenantScope};
use crate::control::collaboration::{
    participant_attention, thread_access, thread_attention_for, Access, CollaborationError,
    CollaborationStore, Participant, ParticipantKind, ParticipantRole,
};
use crate::control::conversation::{ConversationError, ConversationStore, Mention, Thread};
use crate::control::teammates::TeammateStore;
use crate::policy::rbac::ActionFamily;
use crate::runtime::agents::{AgentStore, NewMailboxItem};
use crate::runtime::state_machine::RuntimeIdentity;

/// Map a collaboration refusal onto DOMAIN.md §15.
pub(super) fn collaboration_error(error: CollaborationError, correlation: &str) -> ApiError {
    let code = match error.code() {
        "VALIDATION_SCHEMA" => ApiErrorCode::ValidationSchema,
        "VALIDATION_BOUNDS" => ApiErrorCode::ValidationBounds,
        "NOT_FOUND" => ApiErrorCode::NotFound,
        "CONFLICT_STATE" => ApiErrorCode::ConflictState,
        _ => ApiErrorCode::Internal,
    };
    let code = match &error {
        CollaborationError::Conversation(ConversationError::NotFound(_)) => ApiErrorCode::NotFound,
        _ => code,
    };
    ApiError::new(code, error.to_string(), correlation)
}

fn conversation_error(error: ConversationError, correlation: &str) -> ApiError {
    let code = match &error {
        ConversationError::NotFound(_) | ConversationError::MessageNotFound(_) => {
            ApiErrorCode::NotFound
        }
        ConversationError::Schema(_) => ApiErrorCode::ValidationSchema,
        ConversationError::Unchanged => ApiErrorCode::ConflictState,
        ConversationError::Event(_) | ConversationError::Database(_) => ApiErrorCode::Internal,
    };
    ApiError::new(code, error.to_string(), correlation)
}

fn internal(error: impl std::fmt::Display, correlation: &str) -> ApiError {
    ApiError::new(ApiErrorCode::Internal, error.to_string(), correlation)
}

fn projection_identity(scope: &TenantScope) -> RuntimeIdentity {
    RuntimeIdentity::system(
        &scope.tenant_id,
        "api",
        CorrelationId::generate(&mut UlidGenerator::new()),
    )
}

fn text<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(Value::as_str)
}

fn required<'a>(params: &'a Value, key: &str, correlation: &str) -> Result<&'a str, ApiError> {
    text(params, key).ok_or_else(|| {
        ApiError::new(
            ApiErrorCode::ValidationSchema,
            format!("{key} is required"),
            correlation,
        )
    })
}

fn forbidden_thread(thread_id: &str, correlation: &str) -> ApiError {
    ApiError::new(
        ApiErrorCode::ScopeForbidden,
        format!("thread {thread_id} is protected and this user is not one of its participants"),
        correlation,
    )
    .with_details(
        json!({ "kind": "thread", "thread_id": thread_id, "reason": "not_a_participant" }),
    )
}

/// A thread the caller may read, and how: a protected thread admits its active
/// participants; an open one admits every workspace reader.
///
/// # Errors
/// `NOT_FOUND` for a thread that is not in this tenant, `SCOPE_FORBIDDEN` otherwise.
pub(super) async fn readable_thread(
    state: &ApiState,
    scope: &TenantScope,
    thread_id: &str,
    correlation: &str,
) -> Result<(Thread, Access), ApiError> {
    CanonicalId::parse_typed(thread_id, Prefix::Thread).map_err(|error| {
        ApiError::new(
            ApiErrorCode::ValidationSchema,
            format!("thread_id: {error}"),
            correlation,
        )
    })?;
    let access = thread_access(
        state.pool(),
        &scope.tenant_id,
        thread_id,
        scope.user_id.as_deref(),
    )
    .await
    .map_err(|error| internal(error, correlation))?;
    match access {
        Access::Missing => Err(ApiError::new(
            ApiErrorCode::NotFound,
            format!("thread {thread_id} was not found"),
            correlation,
        )),
        Access::Denied => Err(forbidden_thread(thread_id, correlation)),
        Access::Open | Access::Participant => {
            let store = ConversationStore::new(state.pool().clone(), projection_identity(scope))
                .map_err(|error| internal(error, correlation))?;
            let thread = store
                .get_thread(thread_id)
                .await
                .map_err(|error| conversation_error(error, correlation))?;
            if access == Access::Open {
                Authorization::check(
                    state.pool(),
                    scope,
                    Some(&thread.workspace_id),
                    ActionFamily::Read,
                    correlation,
                )
                .await?;
            }
            Ok((thread, access))
        }
    }
}

/// A thread whose membership the caller may manage: one they may read, or — for a
/// workspace admin — any thread of their workspace. An admin manages membership without
/// reading the conversation (DOMAIN.md §3.1).
///
/// # Errors
/// `NOT_FOUND` for a thread that is not in this tenant, `SCOPE_FORBIDDEN` otherwise.
pub(super) async fn managed_thread(
    state: &ApiState,
    scope: &TenantScope,
    thread_id: &str,
    correlation: &str,
) -> Result<Thread, ApiError> {
    match readable_thread(state, scope, thread_id, correlation).await {
        Ok((thread, _)) => Ok(thread),
        Err(refusal) if refusal.code == ApiErrorCode::ScopeForbidden.as_str() => {
            let thread = ConversationStore::new(state.pool().clone(), projection_identity(scope))
                .map_err(|error| internal(error, correlation))?
                .get_thread(thread_id)
                .await
                .map_err(|error| conversation_error(error, correlation))?;
            let admin = Authorization::resolve(state.pool(), scope, Some(&thread.workspace_id))
                .await?
                .require(ActionFamily::ManageWorkspace, correlation)
                .is_ok();
            if admin {
                Ok(thread)
            } else {
                Err(refusal)
            }
        }
        Err(refusal) => Err(refusal),
    }
}

/// Parse `participants[]` / a single participant reference: `{kind, id, role?}`.
fn participant_ref(
    value: &Value,
    correlation: &str,
) -> Result<(ParticipantKind, String, ParticipantRole), ApiError> {
    let kind = value
        .get("kind")
        .and_then(Value::as_str)
        .and_then(ParticipantKind::parse)
        .filter(|kind| *kind != ParticipantKind::Worker)
        .ok_or_else(|| {
            ApiError::new(
                ApiErrorCode::ValidationSchema,
                "a participant's kind is user or teammate",
                correlation,
            )
        })?;
    let id = value.get("id").and_then(Value::as_str).ok_or_else(|| {
        ApiError::new(
            ApiErrorCode::ValidationSchema,
            "a participant needs an id",
            correlation,
        )
    })?;
    let role = match value.get("role").and_then(Value::as_str) {
        None => ParticipantRole::Member,
        Some(role) => ParticipantRole::parse(role).ok_or_else(|| {
            ApiError::new(
                ApiErrorCode::ValidationSchema,
                "a participant's role is owner or member",
                correlation,
            )
        })?,
    };
    Ok((kind, id.to_string(), role))
}

/// Parse `mentions[]`: `[{kind: user|teammate, id}]`.
pub(super) fn mentions_of(params: &Value, correlation: &str) -> Result<Vec<Mention>, ApiError> {
    match params.get("mentions") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => {
            let mut mentions: Vec<Mention> = Vec::new();
            for item in items {
                let (kind, id, _) = participant_ref(item, correlation)?;
                CanonicalId::parse_typed(&id, kind.prefix()).map_err(|error| {
                    ApiError::new(
                        ApiErrorCode::ValidationSchema,
                        format!("mention {id}: {error}"),
                        correlation,
                    )
                })?;
                let mention = Mention {
                    kind: kind.as_str().to_string(),
                    id,
                };
                if !mentions.contains(&mention) {
                    mentions.push(mention);
                }
            }
            Ok(mentions)
        }
        Some(_) => Err(ApiError::new(
            ApiErrorCode::ValidationSchema,
            "mentions must be an array of {kind, id}",
            correlation,
        )),
    }
}

/// Refuse a mention of someone who may not see the thread: in a protected thread a person
/// must be an active participant; anywhere, a person needs an active workspace membership
/// and a teammate must be active in the workspace.
pub(super) async fn check_mentions(
    collaboration: &CollaborationStore,
    thread: Option<&Thread>,
    workspace_id: &str,
    mentions: &[Mention],
    correlation: &str,
) -> Result<(), ApiError> {
    let participants = match thread {
        Some(thread) => collaboration
            .participants(&thread.id)
            .await
            .map_err(|error| collaboration_error(error, correlation))?,
        None => Vec::new(),
    };
    let protected =
        thread.is_some_and(|thread| thread.kind != "objective") && !participants.is_empty();
    for mention in mentions {
        let kind = ParticipantKind::parse(&mention.kind).unwrap_or(ParticipantKind::User);
        if kind == ParticipantKind::User && protected {
            let present = participants.iter().any(|participant| {
                participant.is_active()
                    && participant.kind == "user"
                    && participant.principal_id == mention.id
            });
            if !present {
                return Err(ApiError::new(
                    ApiErrorCode::ConflictState,
                    format!(
                        "{} is not a participant of this thread; add them before mentioning them",
                        mention.id
                    ),
                    correlation,
                ));
            }
        }
        collaboration
            .check_principal(workspace_id, kind, &mention.id)
            .await
            .map_err(|error| collaboration_error(error, correlation))?;
    }
    Ok(())
}

fn participants_json(participants: &[Participant]) -> Vec<Value> {
    participants.iter().map(Participant::to_json).collect()
}

// ------------------------------------------------------------------ commands

/// `CreateThread {workspace_id, kind?: direct|group|objective, title?, participants?[]}`:
/// the caller becomes the owner; the other participants join in the same transaction.
pub async fn create_thread(
    state: &ApiState,
    scope: &TenantScope,
    identity: &RuntimeIdentity,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let workspace_id = required(params, "workspace_id", correlation)?;
    Authorization::check(
        state.pool(),
        scope,
        Some(workspace_id),
        ActionFamily::AuthorWork,
        correlation,
    )
    .await?;
    let kind = text(params, "kind").unwrap_or("direct");
    if !matches!(kind, "direct" | "group" | "objective") {
        return Err(ApiError::new(
            ApiErrorCode::ValidationSchema,
            "kind is direct, group or objective",
            correlation,
        ));
    }
    let others = match params.get("participants") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| participant_ref(item, correlation))
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => {
            return Err(ApiError::new(
                ApiErrorCode::ValidationSchema,
                "participants must be an array of {kind, id, role?}",
                correlation,
            ))
        }
    };
    let store = CollaborationStore::new(state.pool().clone(), identity.clone());
    let (thread, participants) = store
        .create_thread(
            workspace_id,
            kind,
            text(params, "title").map(str::to_string),
            scope.user_id.as_deref(),
            others,
        )
        .await
        .map_err(|error| collaboration_error(error, correlation))?;
    Ok(json!({
        "thread_id": thread.id,
        "workspace_id": thread.workspace_id,
        "kind": thread.kind,
        "title": thread.title,
        "participants": participants_json(&participants),
    }))
}

/// Whether the caller may manage other people's membership in this thread: a thread owner
/// or a workspace admin.
async fn may_manage(
    state: &ApiState,
    scope: &TenantScope,
    thread: &Thread,
    participants: &[Participant],
    correlation: &str,
) -> Result<bool, ApiError> {
    let owner = scope.user_id.as_deref().is_some_and(|user| {
        participants.iter().any(|participant| {
            participant.is_active()
                && participant.kind == "user"
                && participant.principal_id == user
                && participant.role == "owner"
        })
    });
    if owner {
        return Ok(true);
    }
    Ok(
        Authorization::resolve(state.pool(), scope, Some(&thread.workspace_id))
            .await?
            .require(ActionFamily::ManageWorkspace, correlation)
            .is_ok(),
    )
}

/// `AddParticipant {thread_id, kind: user|teammate, id, role?: owner|member}`.
pub async fn add_participant(
    state: &ApiState,
    scope: &TenantScope,
    identity: &RuntimeIdentity,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let thread_id = required(params, "thread_id", correlation)?;
    let (kind, principal, role) = participant_ref(params, correlation)?;
    let thread = managed_thread(state, scope, thread_id, correlation).await?;
    Authorization::check(
        state.pool(),
        scope,
        Some(&thread.workspace_id),
        ActionFamily::AuthorWork,
        correlation,
    )
    .await?;
    let store = CollaborationStore::new(state.pool().clone(), identity.clone());
    let participants = store
        .participants(&thread.id)
        .await
        .map_err(|error| collaboration_error(error, correlation))?;
    if role == ParticipantRole::Owner
        && !may_manage(state, scope, &thread, &participants, correlation).await?
    {
        return Err(ApiError::new(
            ApiErrorCode::ScopeForbidden,
            "only a thread owner or a workspace admin may make someone an owner",
            correlation,
        )
        .with_details(
            json!({ "kind": "thread", "thread_id": thread.id, "reason": "not_an_owner" }),
        ));
    }
    store
        .check_principal(&thread.workspace_id, kind, &principal)
        .await
        .map_err(|error| collaboration_error(error, correlation))?;
    let (participant, changed) = store
        .add_participant(&thread.id, kind, &principal, role)
        .await
        .map_err(|error| collaboration_error(error, correlation))?;
    Ok(json!({
        "thread_id": thread.id,
        "participant": participant.to_json(),
        "changed": changed,
    }))
}

/// `RemoveParticipant {thread_id, kind, id}`: anyone may leave; removing someone else needs
/// the thread owner role or the workspace admin role.
pub async fn remove_participant(
    state: &ApiState,
    scope: &TenantScope,
    identity: &RuntimeIdentity,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let thread_id = required(params, "thread_id", correlation)?;
    let (kind, principal, _) = participant_ref(params, correlation)?;
    let store = CollaborationStore::new(state.pool().clone(), identity.clone());
    let leaving =
        kind == ParticipantKind::User && scope.user_id.as_deref() == Some(principal.as_str());
    let thread = if leaving {
        // Leaving needs no read access beyond being the participant who leaves.
        ConversationStore::new(state.pool().clone(), identity.clone())
            .map_err(|error| internal(error, correlation))?
            .get_thread(thread_id)
            .await
            .map_err(|error| conversation_error(error, correlation))?
    } else {
        let thread = managed_thread(state, scope, thread_id, correlation).await?;
        let participants = store
            .participants(&thread.id)
            .await
            .map_err(|error| collaboration_error(error, correlation))?;
        if !may_manage(state, scope, &thread, &participants, correlation).await? {
            return Err(ApiError::new(
                ApiErrorCode::ScopeForbidden,
                "only a thread owner or a workspace admin may remove another participant",
                correlation,
            )
            .with_details(
                json!({ "kind": "thread", "thread_id": thread.id, "reason": "not_an_owner" }),
            ));
        }
        thread
    };
    let participant = store
        .remove_participant(
            &thread.id,
            kind,
            &principal,
            if leaving { "left" } else { "removed" },
        )
        .await
        .map_err(|error| collaboration_error(error, correlation))?;
    Ok(json!({
        "thread_id": thread.id,
        "participant": participant.to_json(),
    }))
}

/// `ReactToMessage {message_id, emoji, remove?}`: idempotent per user and emoji.
pub async fn react_to_message(
    state: &ApiState,
    scope: &TenantScope,
    identity: &RuntimeIdentity,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let message_id = required(params, "message_id", correlation)?;
    let emoji = required(params, "emoji", correlation)?;
    let remove = params
        .get("remove")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let Some(user_id) = scope.user_id.as_deref() else {
        return Err(ApiError::new(
            ApiErrorCode::AuthRequired,
            "a reaction belongs to a signed-in person",
            correlation,
        ));
    };
    let conversation = ConversationStore::new(state.pool().clone(), identity.clone())
        .map_err(|error| internal(error, correlation))?;
    let message = conversation
        .load_message(message_id)
        .await
        .map_err(|error| conversation_error(error, correlation))?;
    readable_thread(state, scope, &message.thread_id, correlation).await?;
    let (message, changed) = conversation
        .react(message_id, user_id, emoji, remove)
        .await
        .map_err(|error| conversation_error(error, correlation))?;
    Ok(json!({
        "message_id": message.id,
        "thread_id": message.thread_id,
        "reactions": message.reactions,
        "changed": changed,
    }))
}

/// `HandOffThread {thread_id, to_teammate_id}` (DOMAIN.md §3.1).
pub async fn hand_off_thread(
    state: &ApiState,
    scope: &TenantScope,
    identity: &RuntimeIdentity,
    params: &Value,
    correlation: &str,
) -> Result<Value, ApiError> {
    let thread_id = required(params, "thread_id", correlation)?;
    let to = required(params, "to_teammate_id", correlation)?;
    let to_id = CanonicalId::parse_typed(to, Prefix::Teammate).map_err(|error| {
        ApiError::new(
            ApiErrorCode::ValidationSchema,
            format!("to_teammate_id: {error}"),
            correlation,
        )
    })?;
    let (thread, _) = readable_thread(state, scope, thread_id, correlation).await?;
    Authorization::check(
        state.pool(),
        scope,
        Some(&thread.workspace_id),
        ActionFamily::AuthorWork,
        correlation,
    )
    .await?;
    let store = CollaborationStore::new(state.pool().clone(), identity.clone());
    store
        .check_principal(&thread.workspace_id, ParticipantKind::Teammate, to)
        .await
        .map_err(|error| collaboration_error(error, correlation))?;
    let teammates = TeammateStore::new(state.pool().clone());
    let teammate_error =
        |error: crate::control::teammates::TeammateError| internal(error, correlation);
    let target = teammates
        .get(&identity.tenant_id, &to_id)
        .await
        .map_err(teammate_error)?;
    let from_name = match store
        .responsible_teammate(&thread.id)
        .await
        .map_err(|error| collaboration_error(error, correlation))?
    {
        Some(from) => match CanonicalId::parse_typed(&from, Prefix::Teammate) {
            Ok(from) => teammates
                .get(&identity.tenant_id, &from)
                .await
                .ok()
                .map(|teammate| teammate.name),
            Err(_) => None,
        },
        None => None,
    };
    let handoff = store
        .hand_off(&thread.id, to, &target.name, from_name.as_deref())
        .await
        .map_err(|error| collaboration_error(error, correlation))?;
    // RUN-002 carries the handoff into the target's own work: a durable mailbox item keyed
    // by the notice, so a retried command delivers it once.
    let mailbox = match teammates
        .thread_of(identity, &target)
        .await
        .map_err(teammate_error)?
    {
        Some(agent_thread) => {
            let evidence = store
                .last_run_evidence(&thread.id, &handoff.from.principal_id)
                .await
                .map_err(|error| collaboration_error(error, correlation))?;
            let item = AgentStore::new(state.pool().clone(), identity.clone())
                .map_err(|error| internal(error, correlation))?
                .deliver(
                    &agent_thread.id,
                    NewMailboxItem::new(format!("thread-handoff:{}", handoff.notice.id))
                        .with_payload(json!({
                            "kind": "thread_handoff",
                            "thread_id": thread.id,
                            "from_teammate_id": handoff.from.principal_id,
                            "to_teammate_id": handoff.to.principal_id,
                            "notice_message_id": handoff.notice.id,
                            "last_run_id": evidence.as_ref().map(|(run, _)| run.clone()),
                            "evidence_ids": evidence.map(|(_, ids)| ids).unwrap_or_default(),
                        })),
                )
                .await
                .map_err(|error| internal(error, correlation))?;
            Some(json!({ "agent_thread_id": agent_thread.id.to_string(), "seq": item.seq }))
        }
        None => None,
    };
    Ok(json!({
        "thread_id": thread.id,
        "from": handoff.from.to_json(),
        "to": handoff.to.to_json(),
        "notice_message_id": handoff.notice.id,
        "mailbox": mailbox,
    }))
}

// ------------------------------------------------------------------ projections

/// `GET /v1/threads/{id}`: the thread, its participants (active and past), each active
/// participant's derived attention, and the responsible teammate.
pub async fn thread(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    let (thread, access) = readable_thread(&state, &scope, &id, "projection").await?;
    let store = CollaborationStore::new(state.pool().clone(), projection_identity(&scope));
    let participants = store
        .participants(&thread.id)
        .await
        .map_err(|error| collaboration_error(error, "projection"))?;
    let attention =
        participant_attention(state.pool(), &scope.tenant_id, &thread.id, &participants)
            .await
            .map_err(|error| internal(error, "projection"))?;
    let responsible = store
        .responsible_teammate(&thread.id)
        .await
        .map_err(|error| collaboration_error(error, "projection"))?;
    let me = scope.user_id.as_deref().and_then(|user| {
        participants
            .iter()
            .find(|participant| {
                participant.is_active()
                    && participant.kind == "user"
                    && participant.principal_id == user
            })
            .map(|participant| participant.role.clone())
    });
    Ok(Json(json!({
        "id": thread.id,
        "workspace_id": thread.workspace_id,
        "kind": thread.kind,
        "title": thread.title,
        "protected": access == Access::Participant,
        "participants": participants_json(&participants),
        "attention": attention.iter().map(|entry| entry.to_json()).collect::<Vec<_>>(),
        "responsible_teammate_id": responsible,
        "my_role": me,
    })))
}

/// Query string for `GET /v1/messages`.
#[derive(Debug, Deserialize)]
pub struct MessageQuery {
    /// Thread to list.
    pub thread_id: Option<String>,
}

/// `GET /v1/messages?thread_id=`: refused (`SCOPE_FORBIDDEN`) to a non-participant of a
/// protected thread.
pub async fn messages(
    State(state): State<ApiState>,
    Query(query): Query<MessageQuery>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    let thread_id = query.thread_id.ok_or_else(|| {
        ApiError::new(
            ApiErrorCode::ValidationSchema,
            "messages requires thread_id",
            "projection",
        )
    })?;
    let (thread, _) = readable_thread(&state, &scope, &thread_id, "projection").await?;
    let store = ConversationStore::new(state.pool().clone(), projection_identity(&scope))
        .map_err(|error| internal(error, "projection"))?;
    let messages = store
        .list_messages(&thread.id)
        .await
        .map_err(|error| conversation_error(error, "projection"))?;
    Ok(Json(
        json!({ "messages": messages.iter().map(|message| json!({
        "id": message.id,
        "thread_id": message.thread_id,
        "seq": message.seq,
        "author_kind": message.author_kind,
        "author_id": message.author_id,
        "run_id": message.run_id,
        "turn_id": message.turn_id,
        "content_blocks": message.content_blocks,
        "attachments": message.attachments,
        "mentions": message.mentions,
        "reactions": message.reactions,
        "created_at": message.created_at,
    })).collect::<Vec<_>>() }),
    ))
}

/// `?workspace_id=` on the thread list.
#[derive(Debug, Deserialize)]
pub struct WorkspaceQuery {
    workspace_id: String,
}

/// `GET /v1/threads?workspace_id=`: the threads this reader may see, most recently active
/// first, each with the reader's attention in it.
pub async fn list_threads(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
) -> Result<Json<Value>, ApiError> {
    let scope = TenantScope::resolve(&state, &headers).await?;
    let store = ConversationStore::new(state.pool().clone(), projection_identity(&scope))
        .map_err(|error| internal(error, "projection"))?;
    let threads = store
        .list_threads(&query.workspace_id)
        .await
        .map_err(|error| {
            ApiError::new(
                ApiErrorCode::ValidationBounds,
                error.to_string(),
                "projection",
            )
        })?;
    let attention = match scope.user_id.as_deref() {
        Some(user) => {
            thread_attention_for(state.pool(), &scope.tenant_id, &query.workspace_id, user)
                .await
                .map_err(|error| internal(error, "projection"))?
        }
        None => std::collections::HashMap::new(),
    };
    let mut visible = Vec::new();
    for thread in &threads {
        let access = thread_access(
            state.pool(),
            &scope.tenant_id,
            &thread.id,
            scope.user_id.as_deref(),
        )
        .await
        .map_err(|error| internal(error, "projection"))?;
        if !access.admits() {
            continue;
        }
        let (mentions_pending, open_questions) =
            attention.get(&thread.id).copied().unwrap_or((0, 0));
        visible.push(json!({
            "id": thread.id,
            "workspace_id": thread.workspace_id,
            "kind": thread.kind,
            "title": thread.title,
            "last_activity_at": thread.last_activity_at,
            "message_count": thread.message_count,
            "protected": access == Access::Participant,
            "attention": {
                "mentions_pending": mentions_pending,
                "open_questions": open_questions,
                "needs_you": mentions_pending + open_questions > 0,
            },
        }));
    }
    Ok(Json(json!({ "threads": visible })))
}
