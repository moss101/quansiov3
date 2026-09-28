# QUANSIO V8.1 — CANONICAL DOMAIN MODEL

**Version:** 8.1 (revision 2)
**Status:** IMPLEMENTATION AUTHORITY — normative for names, identities, fields, states and transitions
**Purpose:** Give every implementation agent one shared vocabulary and one shared shape for every canonical entity so that tasks implemented independently compose without drift.

This file is authority for *what the entities are*. `DOSSIER.md` is authority for *who owns them and why*. `registries/tasks.json` is authority for *when they are built*. Generated contracts (`schemas/`, Protobuf, OpenAPI) MUST be derived from this document; if a generated contract and this document disagree, this document wins and the contract is fixed in the same commit.

Field lists below are the **minimum canonical fields**. Implementations may add fields; they may not rename, retype or drop the ones listed. Every persisted entity additionally carries `created_at`, `updated_at` and, where it is tenant data, `tenant_id` and (when workspace-scoped) `workspace_id`.

---

## 0. Glossary (Quansio-native terminology)

Use these names in code, contracts, events, UI copy and tests. Do not introduce synonyms.

| Term | Meaning |
|---|---|
| **Tenant** | Isolation, billing and policy boundary. An organization or a personal account. |
| **Workspace** | Collaboration unit inside a tenant: members, teammates, threads, work, knowledge, connectors, policies, one Persistent Workspace Computer. |
| **Member** | A user's membership in a tenant or workspace, with a role. |
| **Teammate** | A persistent agent definition (persona, standing instructions, default capabilities, memory scope) that participates in threads over time. |
| **Worker** | An ephemeral agent instance spawned to perform scoped delegated work. Same runtime primitives as a teammate, different lifecycle policy. |
| **AgentThread** | The runtime identity of one agent participation (teammate or worker) with mailbox, lineage and capability projection. |
| **Thread** | A conversation container with participants and messages. |
| **Message** | A durable authored unit in a thread (user, agent or system). |
| **Attachment** | A file bound to a message, stored as an Artifact. |
| **Objective** | A long-running goal; the root WorkNode of a WorkGraph created from a thread. |
| **WorkGraph / WorkNode / WorkEdge** | Definition of work: nodes with acceptance and dependencies. |
| **AgentGraph** | Delegation lineage between AgentThreads and their assigned WorkNodes. |
| **StateGraph** | Observed runtime state (runs, steps, attempts, waits) linked to WorkNodes and AgentThreads. |
| **GraphTransaction** | The only way to mutate WorkGraph/AgentGraph/StateGraph: atomic, revision-checked, event-emitting. |
| **Run** | One execution of a WorkNode by an AgentThread under a generation. |
| **Turn** | One bounded model-driven loop within a Run, started by an input (message, wake, child result, approval, answer). |
| **Step** | One unit of work in a Turn: model call, tool call, delegation, wait, verification or checkpoint. |
| **Attempt** | One try of a Step. Retries create new Attempts, never overwrite. |
| **Command** | A validated intent submitted to the server (by client, routine or agent) with idempotency. |
| **RuntimeEvent** | Durable, ordered record of a committed state transition. |
| **LiveFrame** | Transient, non-durable stream data (model token deltas, screencast frames, terminal bytes). Never authority. |
| **ProtocolState** | Durable per-Run record of in-flight tool/model/approval/question/wait/browser/terminal lifecycle needed for exact resume. |
| **Checkpoint** | Recoverable snapshot metadata of workspace files, browser session or terminal state for an execution target. |
| **CompactionEpoch** | Versioned compressed conversational history bound to a source event range. |
| **CapabilityProjection** | The exact authority available to a Run/AgentThread/ToolCall, computed by narrowing only. |
| **Policy** | Tenant/workspace rules evaluated against a proposed action (RBAC, privacy, sequence, approval requirements). |
| **UserRule** | A user's Ask/Always/Never preference for an effect class + resource pattern, within policy limits. |
| **ApprovalRequest / ApprovalReceipt** | A request for human authorization of an exact action, and the bound receipt that authorizes it. |
| **EffectRecord** | The Universal Effect Ledger entry for one consequential action: reservation → dispatch → settlement/reconciliation. |
| **Effect class** | Semantic category of consequence, independent of mechanism (API vs click). |
| **Tool** | A typed, registered capability the runtime can dispatch (file, terminal, browser, connector, artifact, work, user). |
| **ToolCall** | One proposed and validated invocation of a Tool inside a Step. |
| **ExecutionTarget** | A managed execution environment (persistent workspace computer or isolated task runtime) on a substrate. |
| **ComputerControl** | Durable native-computer input ownership and fencing state for one ExecutionTarget. |
| **Substrate** | How a target is realized: cloud microVM, macOS local capsule, Windows local capsule, Windows native VM, customer private worker. |
| **Lease** | Time-bounded exclusive control of an ExecutionTarget by one controller generation. |
| **Generation** | Monotonic fencing number for a Run/AgentThread/Target controller; stale generations are rejected. |
| **qworkerd** | The Rust worker daemon inside every execution target. |
| **Artifact** | A durable user-facing file/document with versions, stored as bytes in object storage and metadata in PostgreSQL. |
| **Evidence** | Immutable execution proof (tool output, screenshot, log, test result, digest) attached to steps/effects/completions. |
| **EvidenceBundle** | A set of evidence descriptors linked to claims in a research/verification output. |
| **ContextProjection** | The bounded, provenance-tagged, trust-labelled content visible to a model for one call. |
| **SearchProgram** | Typed retrieval IR executed by the Context plane. |
| **KnowledgeEntry** | Durable evaluated knowledge with provenance, scope, confidence and lifecycle. |
| **MemoryEntry** | Semantic user/workspace/teammate memory with provenance. Not recovery. |
| **Skill** | Versioned procedural asset (instructions, examples, tool needs, evals). Cannot grant permissions. |
| **Business Capability Pack** | Versioned bundle of knowledge, skills, tools, connectors, policy, approvals, templates and evals for an enterprise capability. |
| **CompletionContract** | The verifiable definition of done for a WorkNode. |
| **PlanProposal** | Model-proposed WorkGraph mutation awaiting validation. |
| **Routine** | Scheduled or event-triggered definition that creates Objectives/Runs. |
| **Notification** | Delivered attention item for a user (in-app, desktop, email, webhook). |
| **Connector** | Configured integration with an external system, with opaque credential handles. |
| **SecretHandle** | Opaque reference to a credential; resolved only at an approved execution boundary. |
| **ModelRoute** | Deterministic resolution of which provider/model/config fulfils a model call. |
| **UsageRecord** | Metered consumption (tokens, cost, machine time, storage, connector calls) attributed to scopes. |
| **AuditEntry** | Append-only record of admin/access/policy/effect decisions. |

---

## 1. Identity scheme

### 1.1 Canonical IDs
- All canonical IDs are **typed, prefixed, time-sortable strings**: `<prefix>_<ULID>` (ULID: 26 chars, Crockford base32, uppercase). Example: `run_01J8Z3K6F1N8VQ2X5W9Y0ABCDE`.
- IDs are generated by the Rust authority that owns the entity, never by clients or models. Clients supply `command_id`s (also ULIDs) for idempotency.
- Stored as `TEXT` with a check constraint on prefix; indexed. No integer surrogate keys for canonical entities.

| Entity | Prefix | | Entity | Prefix |
|---|---|---|---|---|
| Tenant | `tn_` | | ToolCall | `tc_` |
| Workspace | `ws_` | | EffectRecord | `eff_` |
| User | `usr_` | | ApprovalRequest | `apr_` |
| ServicePrincipal | `sp_` | | ApprovalReceipt | `rcp_` |
| Teammate (AgentDefinition) | `agt_` | | UserRule | `rule_` |
| AgentThread | `ath_` | | CapabilityProjection | `cap_` |
| Thread | `thr_` | | Policy | `pol_` |
| Message | `msg_` | | ExecutionTarget | `tgt_` |
| WorkNode | `wn_` | | Lease | `lse_` |
| WorkEdge | `we_` | | Checkpoint | `ckp_` |
| Run | `run_` | | CompactionEpoch | `cep_` |
| Turn | `trn_` | | Artifact | `art_` |
| Step | `stp_` | | ArtifactVersion | `artv_` |
| Attempt | `att_` | | Evidence | `evd_` |
| Command | `cmd_` | | EvidenceBundle | `evb_` |
| RuntimeEvent | `evt_` | | KnowledgeEntry | `kn_` |
| Question | `q_` | | MemoryEntry | `mem_` |
| Skill / SkillVersion | `skl_` / `sklv_` | | Routine | `rtn_` |
| CapabilityPack / Version | `pck_` / `pckv_` | | Notification | `ntf_` |
| Connector | `cnx_` | | SecretHandle | `sec_` |
| ModelRoute | `mr_` | | UsageRecord | `use_` |
| AuditEntry | `aud_` | | Webhook subscription | `whk_` |
| TerminalSession | `tsn_` | | BrowserSession | `bsn_` |
| Session (auth) | `ses_` | | Webhook delivery | `whd_` |
| ContextProjection | `ctx_` | | DirectoryGroup | `dgr_` |
| KillSwitch (engagement) | `ksw_` | | Budget | `bdg_` |

### 1.2 Replay-safety primitives (CORE-002)
- `command_id`: client-generated ULID; unique per tenant; duplicate submission returns the original result (idempotent).
- `idempotency_key`: derived per Tool/effect class from stable inputs (see §7); unique per (tenant, effect_class, key).
- `generation`: `u64`, monotonic per (Run | AgentThread | ExecutionTarget). Every mutation and dispatch carries the generation it was issued under; a mutation with `generation < current` is rejected with `FENCED_STALE_GENERATION`.
- `fence_token`: `(lease_id, generation)` pair carried on every worker→server message.
- `revision`: `u64` per graph aggregate; compare-and-set on GraphTransaction.
- `sequence`: `i64` tenant-monotonic ordering of RuntimeEvents (assigned inside the commit transaction).
- `cursor`: opaque base64 string encoding `(stream_id, sequence)`; clients resume with it.
- `correlation_id`: ULID shared by everything caused by one external input (user message, webhook, timer).
- `causation_id`: the `event_id` or `command_id` that directly caused this event.

---

## 2. Tenancy, identity and roles

```text
Tenant (organization or personal)
 ├── Users (via TenantMembership: owner | admin | billing | member)
 ├── ServicePrincipals (tenant-scoped, explicit scopes)
 ├── Policies (tenant level)
 └── Workspaces
      ├── WorkspaceMembership (admin | editor | approver | viewer)
      ├── Teammates
      ├── Threads → Messages
      ├── WorkGraph (Objectives …)
      ├── ExecutionTargets (one Persistent Workspace Computer by default)
      ├── Connectors, Routines, Artifacts, Knowledge, Memory (workspace scope)
      └── Policies (workspace level; can only narrow tenant policy)
```

- Every self-serve user receives a **personal tenant** with one default workspace, so single-user and enterprise share one model.
- `User` fields: `id, primary_email, display_name, status (active|suspended|deleted), auth_methods[], created_at`.
- `TenantMembership`: `tenant_id, user_id, role, status (invited|active|suspended|removed), invited_by`.
- `WorkspaceMembership`: `workspace_id, user_id, role, status`.
- `ServicePrincipal`: `id, tenant_id, name, scopes[] (explicit command families), key_hash, status, last_used_at`.
- Enterprise identity (OPS-001): `SsoConnection` (one per tenant): `tenant_id, protocol (oidc), issuer, client_id, client_secret_handle?, groups_claim, default_workspace_id?, enforced, status (active|disabled)`. `GroupRoleMapping`: `tenant_id, external_group, scope ('' for the tenant | ws_ id), tenant_role? (admin|billing|member), workspace_role? (admin|editor|approver|viewer)`. `ExternalIdentity`: `tenant_id, user_id, issuer?, subject?, external_id?, origin (provisioned|linked), sso_groups[]`. `DirectoryGroup`: `id, tenant_id, display_name, external_id?, members[] (usr_)`.
- Role semantics: `approver` can approve effects within the workspace; `editor` can create work and messages; `viewer` reads; `admin` manages members, policies, connectors, teammates and packs; tenant `owner/admin` inherit workspace `admin` on all workspaces; `billing` sees usage only.
- Baseline authentication (self-serve): passwordless email link + OAuth sign-in (Google, Microsoft, GitHub). Sessions: short-lived access token (ES256 JWT, ≤15 min) + opaque rotating refresh token (stored in OS keychain on desktop, httpOnly cookie on web). Enterprise SSO/SCIM is OPS-001. API keys for service principals are hashed at rest (argon2id) and prefixed `qsk_`.
- Session endpoints (public API v1; the two marked *bearer* need an access token, the rest establish one): `POST /v1/auth/magic-link/start`, `POST /v1/auth/magic-link/complete`, `GET /v1/auth/oauth/{provider}/start`, `POST /v1/auth/oauth/{provider}/complete`, `POST /v1/auth/refresh`, `POST /v1/auth/sign-out` (bearer), `GET /v1/me` (bearer). A refresh token presented after it was rotated is a replay and revokes its whole session family.
- Enterprise sign-in (OPS-001): `GET /v1/auth/sso/{tenant_id}/start?redirect_uri=` and `POST /v1/auth/sso/{tenant_id}/complete {code, state, redirect_uri}` run the OIDC authorization-code flow with PKCE S256, `state` and `nonce` (stored hashed, single use). The id_token is accepted only when RS256/ES256-signed by a key of the issuer's discovered JWKS, with `iss` exact, `aud` containing the client id (`azp` when there are several), `exp`/`nbf`/`iat` within the configured skew, the request's `nonce`, a subject, and no `email_verified: false`. Roles come only from `GroupRoleMapping` rows over the configured groups claim (highest wins: tenant admin > billing > member; workspace admin > approver > editor > viewer); roles a client sends and every other claim are ignored, `owner` is never mapped and an owner's membership is never changed by a mapping. A linked `(issuer, subject)` signs in as its user; an unknown address is provisioned; an account this tenant provisioned links on first sign-in; any other existing account links only when the complete request carries that user's own bearer session. The session is `auth_method = sso` and bound to the tenant. With `enforced`, a non-owner gets no session in the tenant by another method, and enabling it revokes their non-SSO sessions.
- Provisioning (OPS-001): SCIM 2.0 at `/scim/v2/ServiceProviderConfig`, `/scim/v2/Users`, `/scim/v2/Users/{id}`, `/scim/v2/Groups`, `/scim/v2/Groups/{id}` (the User `id` is the `usr_` id, the Group `id` the `dgr_` id), authenticated by a service principal holding the `Provisioning` scope. `active=false` suspends the tenant membership and the user's workspace memberships and revokes every session the user holds in the tenant, in one transaction with `member.*` events and audit; `DELETE` removes the membership; `active=true` restores it. Group membership changes recompute the member's mapped roles.
- Service principals (OPS-001): scopes are §14 command families plus `Provisioning`, explicit, non-empty and without duplicates; `Trust` is not grantable (approval decisions stay human). The key `qsk_<ULID>_<secret>` is returned once and stored only as its argon2id hash. A principal presents it as a bearer token and is admitted only to `POST /v1/commands/<Name>` for a command whose family it holds, and to SCIM with `Provisioning`; every other route refuses it, and the enterprise Identity commands (`CreateServicePrincipal`, `RevokeServicePrincipal`, `ConfigureSso`, `SetGroupRoleMapping`, `RemoveGroupRoleMapping`, `RevokeUserSessions`) need a person's session even when a principal holds the Identity family. Each principal-authenticated command appends an AuditEntry with actor `{kind: service, id: sp_…}`, applied or refused. Revocation takes effect on the next request.
- Session revocation (OPS-001): an access token is accepted only while its session is unrevoked, the user is `active` and the user's membership in the session's tenant is `active`. `RemoveMember` and a tenant-scope suspension revoke the user's sessions in the tenant; `RevokeUserSessions {user_id}` does so for a tenant administrator. `GET /v1/auth/sessions` (bearer) lists the caller's sessions in the tenant (client, method, created, last used, current) and `POST /v1/auth/sessions/{session_id}/revoke` (bearer) revokes one.

---

## 3. Conversation model

### 3.1 Thread
`id, workspace_id, kind (direct|group|objective), title, participants[] ({kind: user|teammate|worker, id, role: owner|member, joined_at, left_at}), objective_id?, archived_at, last_activity_at`.

Membership and protection (APP-009): a `direct` or `group` thread that has participants is *protected* — only its active participants (`left_at` empty) who hold an active workspace membership read its messages and questions, receive its `thread.*`/`question.*` events and live frames, subscribe to its `thread:` channel and post to it. An `objective` thread is readable by every workspace reader. Adding a participant requires the author-work role and active participation (a user added must hold an active workspace membership; a teammate must be active in the workspace); removing another participant requires the thread `owner` role or the workspace `admin` role; anyone may leave; the last owner cannot leave while other people remain. Attention is derived, never stored: for a user participant, the messages that mention them after their own last message plus the thread's open questions; for a teammate, its AgentThread status and its run in the thread. `HandOffThread {thread_id, to_teammate_id}` moves the thread's responsible teammate (the most recently joined active teammate participant, which also receives un-addressed messages): the source leaves, the target joins, a system notice is posted and a handoff item is delivered to the target's AgentThread mailbox; it is refused while the source has live work in the thread (a run executing a turn, or parked on an approval, a question, an event, a timer, a child or a takeover). It is a conversation handoff, not the AgentThread `handoff` of §5.1, which consumes the source thread.

### 3.2 Message
`id, thread_id, seq (thread-monotonic), author {kind: user|agent|system, id}, run_id?, turn_id?, content_blocks[], attachments[] (artifact ids), reply_to?, reactions[] ({emoji, user_id}), mentions[] ({kind: user|teammate, id}), trust_level (see §12), created_at, edited_at?, deleted_at?`. `seq` is assigned under the thread's row lock, so concurrent posts get distinct, gap-free sequence numbers in commit order.

`content_blocks[]` types: `text`, `code`, `artifact_ref`, `evidence_ref`, `question` (see §3.4), `tool_card_ref` (projection pointer, not content), `system_notice`.

Messages are canonical rows; every insert emits `thread.message_posted`. Model token deltas are LiveFrames; the assistant message becomes durable only when the Step completes.

### 3.3 Attachment
Attachments are Artifacts (§10) with `origin: {kind: message, message_id}`. Upload is a `content.upload` effect tier 1 (workspace-internal) and produces an ArtifactVersion before the message is posted.

### 3.4 Question (agent → human)
`id, run_id, step_id, thread_id, kind (free_text|single_choice|multi_choice|confirm), prompt, options[], required, expires_at, status (open|answered|expired|cancelled), answer {user_id, value, answered_at}`. A Run in `WAITING_QUESTION` resumes on `question.answered`; expiry follows `Policy.question_default_ttl`.

---

## 4. Work model

### 4.1 WorkNode
`id, workspace_id, kind (objective|task|subtask|wait|milestone), title, description, parent_id?, status (draft|ready|blocked|in_progress|waiting|verifying|done|failed|cancelled), owner_agent_thread_id?, created_by {kind,id}, completion_contract (§4.4), capability_needs[] (grant templates §6), budget (§13.2), priority (0-3), revision, thread_id?, origin (user|plan_proposal|routine|pack)`.

### 4.2 WorkEdge
`id, workspace_id, from_node_id, to_node_id, kind (depends_on|parent_of|produces_artifact|verified_by|blocked_by), revision`. Graph MUST be acyclic for `depends_on`/`parent_of`, taken together with `work_nodes.parent_id`; every such relation runs from the node that waits to the node it waits on: `depends_on` dependent → dependency, `parent_of` parent → child, and `parent_id` likewise parent → child (a parent waits on its children), so restating a node's parent as a `parent_of` edge is no cycle and a child may not depend on its parent.

### 4.3 AgentGraph edge
`parent_agent_thread_id → child_agent_thread_id` with `work_node_id, delegation_capability_id (cap_), delegated_at, joined_at?`. Child capability MUST be a narrowing of parent capability (§6).

### 4.4 CompletionContract
```json
{
  "deterministic_checks": [
    {"kind": "artifact_exists", "artifact_role": "report", "min_versions": 1},
    {"kind": "artifact_valid", "artifact_role": "report"},
    {"kind": "test_command", "command": "cargo test -p x", "expect_exit": 0},
    {"kind": "assertion", "expr": "typed predicate id", "args": {}},
    {"kind": "effects_settled", "effect_classes": ["message.send"]},
    {"kind": "citations_valid", "min_coverage": 0.95}
  ],
  "semantic_verification": {"required": false, "rubric_id": "...", "independent_model": true},
  "human_signoff_required": false
}
```
A WorkNode reaches `done` only via `run.verification_passed`, never via a model completion claim alone (RUN-008). Independent semantic verification runs in the intelligence plane: `VerifyCompletion {scope, run_id, work_node_id, claim_summary, rubric_id?, evidence_ids[], independent_model, claimant_model?}` → `{agrees, critique, model}`. The verifier routes to the catalog's `routing.verification` model (else `routing.primary`); `claimant_model` is the model of the run's latest recorded model call, and when the contract requires `independent_model` the plane refuses to judge a claimant it cannot name or whose model is its own. Every refusal — a malformed verdict, an unavailable verifier, unprovable independence — rejects the claim with the refusal as feedback; none passes. `artifact_valid` passes only when the current version of the workspace's latest artifact with that role has `format_check.ok` (§10.1), so a malformed deliverable blocks completion (CAP-003).

### 4.5 PlanProposal (model → runtime)
`{proposal_id, run_id, base_revision, nodes_add[], nodes_update[], edges_add[], edges_remove[], rationale, capability_needs[]}`. Validation (RUN-003): bounded size (≤ `Policy.max_plan_nodes`), acyclic, every node has a CompletionContract or inherits one, `capability_needs ⊆ proposer capability`, `base_revision` current. Accepted → one GraphTransaction → `work.plan_applied`.

---

## 5. Runtime model

### 5.1 AgentThread
`id, workspace_id, agent {kind: teammate|worker, definition_id?}, parent_id?, work_node_id?, capability_projection_id, generation, status, mailbox_cursor, execution_target_id?, budget_id, suspended_reason?, handoff {to_agent_thread_id, at}?`.

States: `PROVISIONED → ACTIVE ⇄ SUSPENDED; ACTIVE → HANDING_OFF → HANDED_OFF; ACTIVE → JOINING → JOINED (worker merged into parent); any → TERMINATED`. Persistent teammates never reach `JOINED`; workers must.

### 5.2 Run
`id, workspace_id, work_node_id, agent_thread_id, generation, status, trigger {kind: message|routine|wake|child_result|manual, ref}, current_turn_id?, budget_snapshot, started_at, ended_at?, terminal_reason? (typed), execution_target_id?`.

States and transitions:
```text
CREATED → QUEUED → RUNNING
RUNNING → WAITING_APPROVAL | WAITING_QUESTION | WAITING_EVENT | WAITING_TIMER | WAITING_CHILD | WAITING_TAKEOVER
WAITING_* → RUNNING (on resolution) | CANCELLED | FAILED (expiry)
RUNNING → VERIFYING → SUCCEEDED | RUNNING (verification feedback, bounded)
RUNNING | WAITING_* → SUSPENDED (budget, kill switch, target loss) → RUNNING | FAILED
RUNNING → FAILED (typed) | CANCELLED | BLOCKED_UNRECOVERABLE (typed reason)
```
Terminal: `SUCCEEDED, FAILED, CANCELLED, BLOCKED_UNRECOVERABLE`. Illegal transitions fail closed with `RUNTIME_ILLEGAL_TRANSITION`.

### 5.3 Turn
`id, run_id, seq, input {kind, ref}, context_projection_id, status (active|completed|aborted), step_count, token_ledger, started_at, ended_at`.

### 5.4 Step
`id, turn_id, seq, kind (model_call|tool_call|delegate|wait|verify|checkpoint|compact), status (pending|dispatched|completed|failed|cancelled|unknown), ref (model call id | tool_call id | child agent thread id | wait id), evidence_ids[], effect_id?`.

### 5.5 Attempt
`id, step_id, seq, generation, status (started|succeeded|failed|timed_out|fenced), dispatched_at, finished_at, error? (typed)`.

### 5.6 Turn loop (RUN-011) — normative
```text
on input → load ProtocolState(run) → build ContextProjection (INT-005; via intelligence RPC)
loop while turn.step_count < budget.max_steps and budget not exhausted:
  MODEL_CALL via Model Gateway (stream LiveFrames to projection; durable ModelCallRecord at end)
  parse ModelProposal := { assistant_text?, tool_calls[], plan_proposal?, completion_claim?,
                           memory_candidates[], question?, delegate_requests[] }
  for each tool_call (parallel where independent, results returned together):
      ToolCall protocol (§7.4) — may park run in WAITING_* ; on resume continue loop
  plan_proposal → RUN-003 validate → GraphTransaction
  delegate_requests → RUN-002 spawn worker with narrowed capability → WAITING_CHILD if join required
  memory_candidates → INT-007 gated write proposals (never direct)
  question → WAITING_QUESTION
  completion_claim → RUN-008 verify → SUCCEEDED, or feedback appended and loop continues (bounded)
  if no tool_calls, no claim, no question → durable assistant Message; turn completes
```
The model never sees raw credentials, other tenants' data, or unbounded tool output. Tool results enter context with `trust_level = UNTRUSTED_EXTERNAL` unless the Tool declares a higher source trust (§12).

Completion claims (RUN-008, RUN-011). A model claims completion by calling the control tool `work.complete {summary, evidence_ids?}`; on the gateway path nothing else the model says is a claim. The call passes the ToolCall protocol (§7.4) like any tool — registry, strict schema, capability, policy, Effect Ledger (`read.internal` on `{kind: work}`: the call records a claim and changes nothing) — and the dispatcher hands the validated call back as the `completion_claim` (claim id = the `tool_call` id). The engine then records a `verify` Step and moves the run `RUNNING → VERIFYING`; the verifier (§4.4) decides:
- verified → `SUCCEEDED`; the claim's tool result is `{verified: true, run_status: "SUCCEEDED"}` and the turn's assistant text is still the reply;
- rejected → `VERIFYING → RUNNING`; the claim's tool result is `{verified: false, feedback, run_status: "RUNNING"}`, so the feedback enters the next model call as that call's result and the loop continues. Rejected claims are bounded per turn by `budget_snapshot.max_verification_rounds` (default 3): the claim that reaches the bound ends the turn (`completed`, the run stays `RUNNING`, the work incomplete with its feedback recorded), and the step budget bounds the loop as always.
A run that stopped in `VERIFYING` (crash mid-verification) is recovered to `RUNNING`; the claim must be made again.
Conversations. A claim means "the run's WorkNode is done", never "the conversation is over": a thread is never closed by a claim, and each message still starts its own run. `work.complete` is exposed like every tool — only when the run's CapabilityProjection covers it (a policy allowing `read.internal` on `{kind: work}`) — and the runtime narrows further: a run whose WorkNode's CompletionContract binds nothing is not offered it, so a plain conversation (whose WorkNode certifies nothing) is never offered the tool and its run stays open between turns. A claim that still reaches a contract that binds nothing (a direct `completion_claim` from a conformance source) is rejected with feedback like any other.

### 5.7 ProtocolState (CORE-006)
Per Run: `run_id, generation, pending_model_call?, pending_tool_calls[] (with dispatch tokens), pending_approvals[], open_questions[], waits[] ({kind, key, expires_at}), browser_control {session_id, holder: agent|user, since}, terminal_sessions[] ({id, cursor}), child_agent_threads[], cancellation {requested: bool, at}, last_compaction_epoch_id, updated_at`. Recovery reads this, RuntimeEvents, Checkpoints, Evidence and the Effect Ledger — never MemoryEntries. A starting runtime recovers every tenant's non-terminal runs, oldest first, before it accepts a request: it honours a cancellation requested before the stop and reports each run holding an uncertain effect, which resumes only once that effect is reconciled.

### 5.8 Checkpoint
`id, execution_target_id, run_id?, kind (workspace_files|browser_session|terminal|full), storage_ref (object key + digest), generation, size_bytes, created_at, expires_at, restore_policy`.

### 5.9 CompactionEpoch (INT-008)
`id, thread_id, run_id?, seq, source_range {from_sequence, to_sequence}, summary_artifact_id, token_estimate, status (pending|installed|rejected_stale), created_by_model_route_id`. Installation is rejected if the thread's current event range no longer contains `source_range` (fork/revert).

---

## 6. Capability model (RUN-005)

### 6.1 Grant
```json
{"effect_class": "message.send",
 "resource": {"kind": "connector", "selector": "cnx_.../channel:#eng-*"},
 "constraints": {"max_tier": 3, "approval": "ask|always|never", "expires_at": "...", "budget_ref": "..."}}
```
Resource selector kinds: `fs` (path glob under a root), `domain` (host glob, port set), `connector` (connector id + resource glob), `app` (native app identity), `artifact` (artifact/role glob), `model` (route/provider), `work` (node subtree), `secret` (handle id), `target` (execution target class/id).

### 6.2 CapabilityProjection
`id, subject {run|agent_thread|tool_call, id}, inputs[] ({layer, ref, digest}) in fixed order: platform → tenant policy → workspace policy → user role → teammate/worker definition → delegation → active skills → tool declaration → execution target class → user rules, grants[] (result), computed_at, expires_at, inputs_digest`.

### 6.3 Algebra
- Composition is **intersection** of grant sets; constraints combine as most-restrictive (`max_tier = min`, `approval` order `never < ask < always` narrowing only toward `ask`/`never`, expiry = earliest).
- A layer may **only remove or narrow**; any layer attempting to add a grant absent from the layer above is ignored and logged `capability.widening_rejected`.
- Missing or unresolvable input (e.g., policy fetch failed) → **fail closed**: empty projection, `CAPABILITY_INPUTS_UNAVAILABLE`.
- Every EffectRecord and ApprovalReceipt records `capability_projection_id` + `inputs_digest`; a stale projection (inputs changed) cannot authorize dispatch.

---

## 7. Effects, policy and approvals

### 7.1 Effect classes and consequence tiers
Tier: 0 read · 1 internal/reversible · 2 external/reversible · 3 external/irreversible · 4 financial, identity or protected data.

| Effect class | Tier | Default policy | Reconciliation strategy |
|---|---|---|---|
| `read.internal` | 0 | allow | none |
| `read.external` (within egress policy) | 0 | allow | none |
| `fs.write.workspace` (inside target root) | 1 | allow | idempotent (content digest) |
| `fs.write.host` (outside sandbox, host FS) | 3 | ask | query (stat/digest) |
| `process.exec.sandboxed` | 1 | allow | none (evidence only) |
| `process.exec.host` | 3 | ask | manual |
| `network.egress.new_destination` | 2 | ask (grant then allow) | none |
| `content.upload` (workspace artifact) | 1 | allow | idempotent (digest) |
| `record.create` | 2 | allow+log (policy may raise) | query by correlation key |
| `record.update` | 2 | allow+log | query by id + version |
| `record.delete` | 3 | ask | query by id |
| `message.send` (email, chat, comment, DM) | 3 | ask | query by idempotency/thread |
| `content.publish` (public or org-wide) | 3 | ask | query by id |
| `scm.remote.write` (push, PR, comment, merge) | 3 | ask (push to own branch: allow+log) | query by ref/PR |
| `computer.input.privileged` (system keys, clipboard write, native app action) | 3 | ask | manual |
| `browser.session.import` (cookies/logins) | 4 | ask (never `always`) | none |
| `payment.execute` | 4 | ask (never `always`) | query by transaction id; manual fallback |
| `data.upload.protected` (classified data to external) | 4 | ask (never `always`) | manual |
| `credential.access` (materialize SecretHandle) | 4 | policy-gated, always audited | none |
| `identity.change` (roles, members, policies, settings) | 4 | ask (admin only) | query |
| `memory.write` | 1 | allow (governed by INT-007) | idempotent |
| `knowledge.write` | 1 | allow (governed by INT-006) | idempotent |
| `skill.promote` / `pack.publish` | 3 | ask (admin) | query |
| `runtime.control` (kill switch, drain, freeze) | 3 | admin only, audited | query |

Rules:
- Tier ≥ 3 requires an ApprovalReceipt unless a valid UserRule/Policy `always` applies. Tier 4 never accepts `always`.
- Any proposal whose causal chain includes `UNTRUSTED_EXTERNAL` content (§12) is escalated one tier for policy purposes and cannot use `always` rules.
- A browser click, a connector call and a terminal command that cause the same consequence share the same class (RUN-007 equivalence fixture).

### 7.2 EffectRecord
`id, workspace_id, run_id, step_id, tool_call_id, effect_class, tier, resource, params_digest, idempotency_key, capability_projection_id, policy_decision_id, approval_receipt_id?, status, dispatch_token, target {kind: server|qworkerd|browser|adapter, id}, reserved_at, dispatched_at?, settled_at?, outcome {kind, remote_ref?, evidence_ids[]}, reconciliation {strategy, attempts, last_at, result}?, generation`.

States:
```text
PROPOSED → AUTHORIZED → RESERVED → DISPATCHED → SETTLED_SUCCESS | SETTLED_FAILED | OUTCOME_UNKNOWN
OUTCOME_UNKNOWN → RECONCILING → RECONCILED_SUCCESS | RECONCILED_FAILED | RECONCILIATION_MANUAL
PROPOSED | AUTHORIZED | RESERVED → DENIED | EXPIRED | CANCELLED
```
No retry is dispatched while a record with the same `idempotency_key` is `DISPATCHED`, `OUTCOME_UNKNOWN` or `RECONCILING`.

### 7.3 Policy, UserRule, Approval
- `Policy`: `id, scope {tenant|workspace}, rules[] ({effect_class, resource_selector, decision: allow|ask|deny, conditions: {trust_max, tier_max, data_classes[], time_window, sequence_guards[], allowed_address_classes[] (network rules only: the address classes a `network.egress.new_destination` destination may resolve to, `public` when absent — EXEC-008, APP-015)}}), question_default_ttl, approval_default_ttl, max_plan_nodes, version`.
- `UserRule`: `id, user_id, workspace_id, effect_class, resource_selector, decision (ask|always|never), expires_at?`. Evaluated after Policy; can only move `ask → always` where Policy permits and never on tier 4.
- `ApprovalRequest`: `id, run_id, effect_id, requested_of (user ids or role), summary, consequence_preview (typed: recipients, amounts, targets, data classes), params_digest, capability_projection_id, expires_at, status (requested|granted|denied|expired|superseded)`. If effect params change after preview, the request becomes `superseded` and a new one is created.
- `ApprovalReceipt`: `id, request_id, effect_id, approver_user_id, params_digest, scope (single_use), generation, granted_at, expires_at, signature (server HMAC over fields)`. Dispatch verifies `params_digest` and `generation` equality.

### 7.4 ToolCall protocol (RUN-011)
```text
model proposes {tool, args}
→ schema validate (strict JSON Schema; reject unknown fields)
→ derive effect_class + resource from Tool declaration + args
→ CapabilityProjection check → policy → user rules → trust escalation (§12)
→ if approval required: ApprovalRequest; Run → WAITING_APPROVAL (ProtocolState records pending call)
→ EffectRecord RESERVED with idempotency_key and dispatch_token
→ dispatch to host (server | qworkerd via machine gateway | browser session | adapter) with {dispatch_token, generation, fence}
→ host executes, streams LiveFrames, returns ToolResult {status, output (bounded), evidence_ids[], remote_ref?}
→ settle EffectRecord; append ToolResult to context with trust label; emit tool.completed
timeout/disconnect → OUTCOME_UNKNOWN → reconcile before any retry
```

### 7.5 Tool
`name (namespaced: fs.read, fs.write, fs.patch, terminal.exec, process.spawn, browser.navigate, browser.click, browser.type, browser.extract, browser.screenshot, web.search, web.fetch, computer.read, computer.click, computer.type, computer.clipboard, computer.system_key, connector.<id>.<op>, scm.git.<op>, scm.pr.<op>, artifact.create, artifact.update, work.propose_plan, work.delegate, work.complete, user.ask, memory.propose, knowledge.cite), version, description, input_schema (JSON Schema, additionalProperties=false), output_schema, effect_class (static or derived by function of args), resource_derivation, required_grant_template, idempotency_key_derivation, host (server|qworkerd|browser|machine|adapter), max_output_bytes, evidence_capture (what is recorded), source_trust (trust level assigned to results), timeout_ms, cancellable`.

Tools are registered in the Tool Registry (control plane) and exposed to models only through CapabilityProjection filtering; a model never sees a tool it cannot use.

### 7.6 Operator kill switches (OPS-008)
`KillSwitch: id (ksw_), tenant_id, switch, target, reason, engaged_by, engaged_at, released_by?, released_at?, release_reason?` — one row per engagement, history kept, at most one active per (switch, target). Engaging and releasing are `runtime.control` (tier 3, tenant administrators, audited) and emit `ops.kill_switch_engaged` / `ops.kill_switch_released`. Releasing restores service; nothing is deleted.

| Switch | Target | While engaged |
|---|---|---|
| `effect.freeze` | `*` or a `ws_` | New tier ≥ 2 effects are refused `EFFECTS_FROZEN` before any effect is reserved or approval requested (the refusal is recorded like any rejected tool call), and an approved effect is refused before reservation with its receipt unconsumed. Tier 0/1 calls, reads, evidence and effects already in the ledger are untouched. |
| `policy.emergency_deny` | an effect class, `<family>.*` or `*` | Matching calls are refused `POLICY_DENIED` before policy evaluation. |
| `provider.disable` | a provider name | Model calls carry `disabled_providers`; the gateway routes to an enabled fallback, else `PROVIDER_UNAVAILABLE`. |
| `worker.quarantine` | a `tgt_` | The target's lease is revoked on engage (generation bump fences the worker); every tool call of a run on it is refused `TARGET_UNAVAILABLE`. |
| `target.drain` | a `tgt_` | The target is `DRAINING`: no new lease, in-flight work finishes; release returns it to `READY`. |
| `connector.revoke` | a `cnx_` | The instance is revoked and its credential handle cleared (terminal); recovery is reconnecting, release closes the incident. |
| `stream.shed` | `*` | Every live `/v1/stream` subscriber of the tenant is disconnected with `STREAM_BACKPRESSURE` (reconnect with the cursor); while engaged a lagging subscriber is disconnected instead of missing frames. |

---

## 8. Execution model

### 8.1 Target class × substrate
| Target class | Purpose | Lifetime |
|---|---|---|
| `persistent_workspace_computer` | Durable environment per workspace (files, browser profile, installed tooling) shared by its teammates | Long-lived; checkpointed |
| `isolated_task_runtime` | Disposable environment for one Run/WorkNode | Run-scoped; destroyed after evidence capture |

| Substrate | Where | Notes |
|---|---|---|
| `cloud_microvm` | Managed cloud, Firecracker microVM (default) | Immutable image, CoW restore, warm pool |
| `local_capsule_macos` | User's Mac, Linux guest on Virtualization.framework | Apple Silicon primary |
| `local_capsule_windows` | User's PC, WSL2 Linux guest | Generic workloads |
| `windows_native` | Windows VM/host via native broker | Windows-only app automation |
| `customer_private_worker` | Customer VPC/on-prem running qworkerd | Outbound-only control channel |

The managed browser (§8.4) runs **inside the execution target**, never in the desktop host process. Native computer-use (EXEC-010) is the only host-side automation and goes through the native broker.

### 8.2 ExecutionTarget
`id, workspace_id, class, substrate, status, desired_state, observed_state, image_digest, lease_id?, generation, network_policy_id, resources {cpu, mem, disk}, endpoint (private control channel ref), last_heartbeat_at, checkpoint_ids[], owner {workspace|user|run}`.

States: `REQUESTED → PROVISIONING → READY ⇄ BUSY; READY → DRAINING → STOPPED → (SNAPSHOTTED) → DESTROYED; DRAINING → READY (an operator releases a drain, OPS-008); any → FAILED (typed) → REPLACING → PROVISIONING`.

### 8.3 Lease
`id, target_id, holder {controller_id, generation}, acquired_at, expires_at, renewed_at, status (held|released|expired|revoked)`. qworkerd rejects any action whose `(lease_id, generation)` does not match its current lease.

### 8.4 BrowserSession
`id, target_id, run_id?, profile_ref, control_holder (agent|user|none), control_since, current_url, tabs[], screencast {enabled, stream_ref}, checkpoint_id?, status (active|paused_takeover|paused_policy|closed)`. Takeover: `RequestTakeover` → `control_holder=user` fences agent input; `Handback` → agent resumes. No second session is created on takeover (APP-007).

Understanding order for agents: DOM/CDP/network metadata → accessibility tree → screenshot/vision fallback (EXEC-009). Live view: CDP screencast frames as binary LiveFrames over the client stream; user input during takeover flows client → server → qworkerd → CDP input.

### 8.5 TerminalSession
`id, target_id, run_id?, pty_ref, cursor (durable byte offset), status, last_command_id`. Replay resumes from `cursor`; commands are effects with `process.exec.*` class.

### 8.6 ComputerControl (EXEC-010)
`target_id (primary key), run_id?, holder (agent|user|none), generation, takeover_pending, active_tool_call_id?, control_since`.

The row is the durable native-computer input fence for one ExecutionTarget. Action begin, action finish,
`RequestTakeover`, and `Handback` lock this row. Only one native input ToolCall may be active: begin is
allowed only while `holder=agent`, `takeover_pending=false`, and `active_tool_call_id` is empty. A takeover
first sets `takeover_pending=true`, fencing new input; it may change the holder to `user` only after the
active ToolCall settles or is reconciled, and increments `generation`. `Handback` is the only transition
from `user` to `agent` and increments `generation` again. A restart reloads the row; it never assumes an
active ToolCall failed and never clears user ownership. Browser takeover continues to use BrowserSession
§8.4; both forms park the owning Run through ProtocolState.

---

## 9. Events and streaming

### 9.1 RuntimeEvent envelope (CORE-003)
`event_id, tenant_id, workspace_id?, sequence (tenant-monotonic), aggregate_type, aggregate_id, aggregate_version, type, schema_version, occurred_at, command_id?, correlation_id, causation_id?, actor {kind: user|agent|system|service, id}, generation?, payload (typed by `type`)`.

Written in the same PostgreSQL transaction as the mutation, plus an outbox row; the publisher relays to NATS JetStream subject `q.<tenant>.<aggregate_type>.<type>` with `Nats-Msg-Id = event_id` for dedup.

### 9.2 Event families (minimum)
`tenant.*`, `workspace.*`, `member.*`, `thread.*` (created, participant_added/removed, message_posted, reaction_added/removed, attention_changed, handed_off), `work.*` (node_created/updated, edge_added/removed, plan_proposed/applied/rejected, node_status_changed, revision_committed), `agent.*` (thread_provisioned, activated, suspended, delegated, handoff_started/completed, mailbox_delivered, mailbox_cursor_advanced, joined, terminated), `run.*` (created, queued, started, waiting, resumed, verifying, verification_passed/failed, succeeded, failed, cancelled, blocked, suspended), `turn.*`, `step.*`, `model.*` (call_started, call_completed, usage_recorded, route_selected, failover), `tool.*` (proposed, validated, rejected, dispatched, completed, failed, unknown), `effect.*` (one per state in §7.2), `approval.*` (requested, granted, denied, expired, superseded), `question.*`, `target.*`, `lease.*`, `browser.*` (session_opened, control_changed, checkpointed), `terminal.*`, `checkpoint.*`, `artifact.*` (created, version_added, grant_changed), `evidence.*`, `knowledge.*`, `memory.*`, `skill.*`, `pack.*`, `routine.*` (created, updated, fired, skipped, paused, resumed, deleted, fire_completed, fire_failed), `notification.*` (created, read, preferences_updated), `connector.*` (connected, degraded, revoked, webhook_received), `webhook.*` (subscription_created, subscription_updated, subscription_deleted, secret_rotated, delivery_dead_lettered, delivery_redelivered), `policy.*`, `audit.*`, `usage.*`, `capability.*` (projected, widening_rejected), `compaction.*`, `ops.*` (kill_switch_engaged/released, provider_disabled, worker_quarantined).

### 9.3 Client stream (CORE-009, APP-001)
- Transport: WebSocket `GET /v1/stream`; Server-Sent Events `GET /v1/stream/events?channel=…&cursor=…` for read-only consumers (web fallback, integrators — APP-015): `id:` carries the cursor and `Last-Event-ID` resumes.
- Subscribe: `{subscribe: [{channel: "workspace:ws_…" | "thread:thr_…" | "run:run_…" | "target:tgt_…"}], cursor?}`.
- Frames: `{kind: "event", cursor, event}` (durable, replayable) and `{kind: "live", channel, frame}` (transient: `model.delta`, `terminal.bytes`, `browser.frame`, `progress`). Live frames are never persisted and clients must tolerate their absence. A `model.delta` with `final: false` carries the next piece of a model's text as the model writes it; the one with `final: true` carries the whole text, which replaces what streamed.
- Reconnect with last `cursor` replays missed events in order; slow consumers are disconnected with `STREAM_BACKPRESSURE` rather than buffered unboundedly — a subscriber whose bounded buffer stays full for the stall window (5 s) without it taking a frame; a replay longer than the buffer waits for the subscriber rather than disconnecting it.

---

## 10. Artifacts and evidence

### 10.1 Artifact / ArtifactVersion
`Artifact: id, workspace_id, kind (document|spreadsheet|presentation|code|data|image|audio|video|archive|other), title, role? (report|source|attachment|…), origin {kind: message|run|routine|upload|connector, ref}, current_version_id, grants[] ({principal, level: read|write}), retention {class, expires_at?}, deleted_at?`.
`ArtifactVersion: id, artifact_id, seq, content_digest (sha256), size_bytes, media_type, object_key, produced_by {run_id?, step_id?, user_id?}, parent_version_id?, format_check? {ok, rule, detail}, created_at`.
Bytes are immutable per version; a new version is a new identity. Preview never executes active content with host privileges (APP-008).

Source formats (CAP-003). An editable artifact is a versioned source file, not a chat blob: `document` — `text/markdown`/`text/plain`, UTF-8, first non-blank line `# Title`, a body, no active content (`<script`, `javascript:`, `onerror=`, `onload=`); `spreadsheet` — `text/csv`/`text/tab-separated-values`, unique non-empty headers, rectangular rows, at least one row; `presentation` — `application/vnd.quansio.slides+json` `{schema_version: "v1", slides: [{title, body, notes?}]}`, every slide with a body; `code` — `application/vnd.quansio.code+json` `{schema_version: "v1", language, files: [{path (relative, no `..`), content}]}`, every file with content; `data` — `application/json` `{schema_version: "v1", columns[], rows[{column: value}]}`, at least one row. Media kinds need a matching media type and real bytes; any source that is a chat transcript (`{role, content}` objects) is refused. Every version the store adds records `format_check` (rules `artifact.media_type`, `artifact.opaque_blob`, `artifact.format`, `artifact.quality`); uploads are stored either way, but the governed tools `artifact.create {role, kind, title, content | content_base64, content_digest, media_type?}` and `artifact.update {artifact_id, content | content_base64, content_digest, media_type?}` (server-hosted, `content.upload`) refuse a source whose digest does not match or whose check fails, so a run never produces an invalid version. The intelligence plane applies the same rules before proposing (`python/intelligence/artifacts`); `packs/skills/artifacts/fixtures` is the corpus both must agree on.

### 10.2 Evidence
`id, workspace_id, run_id, step_id?, effect_id?, kind (tool_output|screenshot|dom_snapshot|log|test_result|http_exchange|diff|digest|external_ref), content_digest, object_key?, inline_summary (bounded), captured_at, captured_by {host kind, id}, redaction_applied: bool`. Immutable; never overwritten.

### 10.3 EvidenceBundle (CAP-002)
`id, run_id, artifact_version_id (the output), claims[] ({claim_id, text_span, evidence_ids[], support_score?}), sources[] ({source_id, uri|artifact_ref, retrieved_at, excerpt_locator, excerpt_digest}), coverage, verification {deterministic_passed, semantic_score?, verified_at}`.

---

## 11. Intelligence model

### 11.1 Model Gateway contract (INT-002/003)
`ModelCallRequest: call_id, run_id, turn_id, route_hint? (explicit user choice), capability_projection_id, context_projection_id, messages (rendered by Context plane; stable prefix first for caching), tools[] (filtered), output_constraints?, effort?, max_output_tokens, stream: true, dlp_profile, timeout_ms, cancellation_token`.
`ModelCallRequest.disabled_providers[]` (OPS-008): provider names an operator disabled (`provider.disable`); the gateway never routes to them, fails over to an enabled fallback, and refuses with `PROVIDER_UNAVAILABLE` when none is left.

`ModelEvent (stream): call_started {route}, delta {text|tool_input_partial}, tool_call {id, name, args}, thinking_summary?, usage {input, output, cache_read, cache_write}, stop {reason: end_turn|tool_use|max_tokens|refusal|cancelled|error}, error {code, retryable}, call_completed {route, latency_ms, cost_estimate}`.

Provider adapters MUST normalize: streaming, tool/function calling with strict JSON schemas, system/operator instructions, multimodal image and document input, prompt-cache hints, usage reporting, stop reasons (including refusal), cancellation. Provider-side context compaction features MUST NOT be enabled; Quansio owns compaction (INT-008).

`ModelRoute: id, request_class (chat|planning|tool_heavy|synthesis|verification|embedding|cheap_worker), provider, model_id (from `config/models.yaml`, never hard-coded), config {effort, max_tokens, thinking}, dlp_profile, fallbacks[] (bounded), cost_class, chosen_by (rule id), selected_at`. Selection is deterministic for fixed inputs and performs no LLM call.

### 11.2 ContextProjection (INT-005)
`id, run_id, turn_id, segments[] ({seq, source {kind: system|policy|skill|thread|work|tool_result|knowledge|memory|artifact|search, ref}, trust_level, token_estimate, snapshot_ref, redactions[]}), token_ledger {budget, used, by_source}, degradation[] (what was dropped and why), search_program_id?, policy_snapshot_digest, snapshot_id, messages[] (RenderedMessage, §11.1), created_at`.

`BuildContext` request: `run_id, turn_id, snapshot_id, token_budget, policy_snapshot_digest, segment_requests[] ({seq, source {kind, ref}, role: system|operator|user|assistant|tool, content (RenderedMessage content), trust_hint?, snapshot_ref, required}), skill_candidates[] ({skill_id, version_id, name, semver, status, manifest}), task_text, exposed_tools[], granted_effect_classes[], max_skills`. The runtime supplies canonical content it owns (instructions, thread turns, tool results) and the snapshot it read them at; the Context plane labels each segment from its source (a `trust_hint` may only lower it), packs the budget (required segments must fit; an assistant turn and the tool results answering it are kept or dropped together; dropping is newest-kept and recorded in `degradation`), and renders `messages[]`: system and operator instructions first as the stable prefix, then the conversation in `seq` order, with untrusted content inside the §12 typed boundary. The projection id (`ctx_`) is minted by the plane. WorkGraph (RUN-003): when the run's projection exposes `work.propose_plan`, the runtime offers the workspace graph as one optional `work` segment right after the instructions — `{workspace_id, revision, nodes[] {id, kind, title, status, parent_id, revision}, edges[] {id, from_node_id, to_node_id, kind, revision}, omitted_nodes}`, at most `model_call.max_work_nodes` nodes in creation order, `ref` = `<workspace_id>@<revision>` — hinted `agent_generated` when any node was not created by a user. A served `work.propose_plan` is compiled as the proposing run: its proposal id, run and workspace are the call's (a proposal naming others is `VALIDATION_SCHEMA`), the proposer projection is the run's stored Capability Projection, created nodes are `created_by {kind: agent, id: <agent thread>}`, and a refusal settles the call `rejected` with the planner's own code. Skills (INT-009): the runtime offers the registry's candidate versions; the plane resolves them against `task_text` within the caller's own authority (`exposed_tools`, `granted_effect_classes`; §11.5 — only `ACTIVE` resolves, a skill never widens authority, at most `max_skills`) and renders each resolved version as an optional operator segment of source `skill` (ref = version id) at `verified_knowledge`, after the teammate's instructions; every excluded candidate is recorded in `degradation` with the rule that excluded it.

### 11.3 SearchProgram
Typed AST, no free-form predicates: `{channels: [exact|lexical|semantic|graph|history|memory], filters: typed, budget {max_results, max_tokens}, snapshot: index epoch, scope: tenant/workspace/target}`. Results carry provenance `(source_id, locator, snapshot)`.

### 11.4 KnowledgeEntry / MemoryEntry
`KnowledgeEntry: id, scope {tenant|workspace|pack}, kind, content_ref, provenance[] ({source_kind, ref, digest, retrieved_at}), confidence (0-1), version, status (candidate|verified|active|superseded|quarantined|deleted), superseded_by?, embedding_ref? (derived)`.
`MemoryEntry: id, scope {user|workspace|teammate}, subject_ref, content, provenance {kind: explicit_user|verified_run, ref}, confidence, status (candidate|active|deleted), last_used_at, expires_at?`.
Deleting a source quarantines derived knowledge; deleting memory removes it from retrieval after index refresh (bounded delay published in SLOs).
Proposing memory (INT-007): a run's `memory.propose` reaches the owner over `ProposeMemory` (`MemoryCandidate {scope, subject_ref, content, provenance_kind, provenance_ref, confidence, scope_hint}`). The model supplies only `subject_ref`, `content`, `confidence` and a scope hint; the runtime pins provenance to `explicit_user` with `ref` = the message that triggered the run (a run no message triggered is `POLICY_DENIED`; a candidate claiming other provenance is `VALIDATION_SCHEMA`). The owner stores it as `candidate`, which retrieval never returns until promoted. An intelligence process without `QUANSIO_INTELLIGENCE_MEMORY_DATABASE_URL` has no memory store and refuses with `ROUTE_UNAVAILABLE`.

### 11.5 Skill / SkillVersion
`Skill: id, scope, name, owner, current_active_version_id`. `SkillVersion: id, skill_id, semver, manifest {instructions, examples, tool_needs[] (tool names), capability_needs[] (grant templates), eval_suite_id, compatibility, recovery_guidance}, provenance, status`.
States: `DRAFT → CANDIDATE → EVALUATING → APPROVED → ACTIVE → DEPRECATED → RETIRED; EVALUATING → REJECTED`. Only `ACTIVE` versions resolve into production context. `InstallSkill`, `PromoteSkillVersion` and `DisableSkill` are tenant administrators' commands (`ManageTenant`): each changes what may enter production context for the whole tenant (QA-007).

Controlled evolution (CAP-008): `ProposeSkillCandidates {workspace_id}` (tenant administrators) gathers runs that reached a verified terminal state (`SUCCEEDED` → success, or recovery when a tool call failed on the way; `FAILED` → failure), each with its latest evidence and the skill versions its recorded ContextProjections named, and the plane returns evaluated patches. Each is stored as a `CANDIDATE` version (never `ACTIVE`) whose `provenance` is `{origin: evolution, from_version_id, pattern_kind: success|failure|recovery, evidence[] ({evidence_id, digest, run_id}), evaluation {eval_run_id, digest, passed, reason, regressions, widened_needs[]}}`; a semver already registered is not proposed again. An evolution candidate cannot be promoted to `APPROVED` or `ACTIVE` unless its provenance names its evidence and a passing evaluation (refusal: `POLICY_DENIED`). The evaluation is deterministic resolution behaviour against the skill's suite; protected regressions and widened needs block promotion (`config/evolution.yaml`).

### 11.6 Business Capability Pack
`Pack: id, tenant_id, name, current_published_version_id`. `PackVersion: id, semver, contents {knowledge_ids[], skill_version_ids[], tool_names[], connector_requirements[], policy_requirements[], rbac_requirements[], approval_requirements[], workflow_templates[] (WorkGraph templates), examples[], eval_suite_id, compatibility}, provenance, evidence_requirements, status (draft|candidate|qualifying|qualified|published|deprecated|withdrawn)`. Execution resolves to WorkGraph + Skills + Tools + Knowledge + Policy; a pack cannot widen caller authority.

---

## 12. Content trust and injection defense (INT-012)

| Trust level | Source | Treatment |
|---|---|---|
| `TRUSTED_SYSTEM` | Runtime-rendered system/policy/tool definitions | Instructions honoured |
| `TRUSTED_USER` | Messages authored by authenticated workspace members in-thread | Instructions honoured within capability |
| `VERIFIED_KNOWLEDGE` | ACTIVE KnowledgeEntry / ACTIVE Skill | Guidance honoured; cannot grant |
| `AGENT_GENERATED` | Prior assistant/worker output | Context only |
| `UNTRUSTED_EXTERNAL` | Web pages, fetched documents, emails, connector payloads, tool outputs, uploaded files, attachments from outside the workspace | **Data only.** Any imperative content is never treated as user intent |

Rules:
1. Every ContextProjection segment carries `trust_level`; rendered prompts delimit untrusted segments with typed boundaries and an explicit data-only instruction.
2. Proposals (tool calls, plans, memory candidates) record `derived_from_trust = max-untrusted of segments referenced in the causal window`. Tier ≥ 2 proposals derived from `UNTRUSTED_EXTERNAL` are escalated one tier and cannot satisfy `always` rules; tier ≥ 3 always requires a fresh ApprovalReceipt whose preview shows the untrusted origin.
3. Data exfiltration guard: content originating from `UNTRUSTED_EXTERNAL` or protected data classes cannot be sent to a destination not already in the egress grant set without approval (`data.upload.protected`).
4. Deterministic injection detectors (pattern/heuristic, non-LLM) tag segments `injection_suspected`; suspected segments are truncated to evidence references in context and surfaced in UI.
5. Adversarial suite (QA-007) asserts zero unauthorized effect executions across the injection corpus.

---

## 13. Automation, notifications, usage

### 13.1 Routine
`id, workspace_id, owner_user_id, teammate_id, trigger {kind: cron|interval|event, spec, timezone}, objective_template (WorkNode template + CompletionContract), budget, notification_prefs, absence_policy (skip|queue|catch_up_once), status (active|paused|deleted), last_fired_at, next_due_at`. Firing creates one Objective + Run via the same command path as interactive work; timers are generation-fenced (CORE-008). A schedule window is on time when the scheduler reaches it within the misfire threshold (60 s by default) and no later window has also come due; an on-time window fires under every absence policy, which decides only what a missed window means.

Firing ledger (APP-010): every fire is one `routine_fires` row under a unique `fire_key` — `routine:<id>:<window>` for a schedule window, `routine:<id>:event:<event_id>` for an event trigger, `routine:<id>:manual:<command_id>` for `TriggerRoutineNow`. A schedule window is claimed in the transaction that advances `next_due_at` (with `routine.fired`); an event fire in the transaction that advances the trigger consumer's cursor. Executing a fire is leased (`pending → running → fired`); a failed execution returns to `pending` after exponential backoff and becomes `failed` (`routine.fire_failed`) after a bounded number of attempts; a fire whose routine was paused or deleted after the claim is cancelled. Execution is the interactive path: the prompt is posted to the routine's objective thread, the teammate is routed as for a message, the run's capability projection is seeded from the same policies, the turn runs through the same engine and effect path, and the reply is posted; each step is looked up by the fire before it is created, so a restarted execution continues and never duplicates a run (`routine.fire_completed` names it). Event triggers: `spec` is `family.name` or `family.*`, matched in the routine's workspace against events committed after the routine was created; events of the `routine` and `notification` families and events of any routine's own thread or runs never trigger a routine. Pausing drops the windows missed while paused (resume computes the next window from now).

### 13.2 Budget
`id, scope {tenant|workspace|run|agent_thread}, limits {tokens, cost_minor_units, wall_time_ms, tool_calls, concurrency, machine_minutes, max_steps}, consumed {…}, parent_budget_id?, status (active|exhausted|suspended)`. Child ≤ parent remaining. Enforced (RUN-010): a run is governed by the budgets of its agent thread, its workspace and its tenant, when granted. Before each model call and each tool dispatch the runtime checks that none is exhausted — otherwise the turn stops, the run is `SUSPENDED` with `BUDGET_EXHAUSTED`, and nothing new starts; after each model call it charges `tokens` (input + output as the provider reported them) and after each tool dispatch `tool_calls` to every one of them. A charge beyond a limit is refused and exhausts that budget; work already dispatched is left to the Effect Ledger. `SetBudget {scope: tenant|workspace, workspace_id?, limits}` grants or re-limits the tenant's or a workspace's budget (`ManageTenant` / `ManageWorkspace`); a workspace budget's limits never exceed its tenant budget's remaining, and consumption carries over a re-limit. Quotas (OPS-004): these limits are the hard quota; a charge that takes a meter past 80 % of its limit emits `usage.threshold_crossed` once per budget and meter (the soft quota).

### 13.3 Notification
`id, user_id, workspace_id, kind (needs_approval|needs_answer|blocked|completed|failed|mention|attention|system), title, ref {thread|run|effect|question|routine}, channels[] (selected), channels_delivered[] (in_app|desktop|email|webhook), source_event_id?, read_at?, created_at`. Preferences per user × workspace × kind; without one a kind goes to `in_app` and `desktop`, an empty list mutes it, and `email` is opt-in (APP-010). Notifications are made from RuntimeEvents by the notifier (mentions, questions, approval requests, routine outcomes) — once per (event, user, kind) — and announced by a `notification.created` event that the stream delivers to that user only; the desktop client shows a native notification when it says `desktop: true`. Email is sent server-side (OD-009) as a delivery with retry and exponential backoff; a deployment without a mail transport records the delivery `unavailable`.

### 13.4 UsageRecord
`id, tenant_id, workspace_id, scope_refs {run_id?, agent_thread_id?, target_id?, connector_id?}, meter (model_input_tokens|model_output_tokens|model_cache_tokens|model_cost|machine_seconds|storage_bytes|connector_calls|browser_seconds), quantity, unit, cost_minor_units?, source_event_id, occurred_at`. Rebuildable from RuntimeEvents; billing is a projection.

### 13.5 Webhook subscription (APP-015)
`id, tenant_id, workspace_id?, url, secret_handle_id, event_types[], status (active|paused|failing), delivery {attempts, backoff, last_status}, created_by, start_sequence`. Outbound delivery is a `network.egress` effect under egress policy; payload is the RuntimeEvent envelope with redaction applied.

- **The subscription is the destination's grant.** `CreateWebhookSubscription {url, workspace_id?, event_types[]}` is an admin command (`ManageWorkspace`, or `ManageTenant` without a workspace), audited, and it issues the `network.egress.new_destination` grant for the URL's host and port: it is refused unless the tenant (and workspace) Policy has a `network.egress.new_destination` rule whose decision is not `deny` — with no rule egress is denied, as for execution targets (EXEC-008) — and unless the egress broker accepts the URL (absolute `http`/`https`, no credentials in the authority, not a blocked host). The server generates the signing secret (`whsec_…`), keeps it as a SecretHandle (`provider: webhook`), and returns it once in the command result; `UpdateWebhookSubscription {secret: "rotate"}` replaces it (`webhook.secret_rotated`). The URL of a subscription never changes: a new destination is a new subscription.
- **What is delivered.** `event_types[]` holds exact types (`run.succeeded`), family wildcards (`run.*`), or nothing for every type. A subscription receives the RuntimeEvents of its tenant — only of its workspace when `workspace_id` is set — committed after it was created (`sequence > start_sequence`). Events of the `webhook` family are never delivered (a delivery must not cause a delivery), nor events a protected thread withholds from non-participants (§3.2).
- **Delivery ledger.** `webhook_deliveries {id (whd_), subscription_id, event_id, event_type, event_sequence, status (pending|delivering|delivered|dead_lettered), attempts, next_attempt_at, last_status, last_attempt_at, delivered_at}`, one row per (subscription, event): the fan-out enqueues in the transaction that advances its cursor, so a replayed batch, a restart or a second instance enqueues an event once. An attempt is leased; a delivery whose instance crashed is attempted again when the lease lapses. Delivery is at-least-once under a stable `event_id`: a failure, a timeout or a non-2xx answer is retried after 30 s · 2^(attempt−1) (capped at 1 h), and consumers dedupe on `event_id`. After 8 failed attempts the delivery is `dead_lettered` (`webhook.delivery_dead_lettered`) and the subscription becomes `failing`; `RedeliverWebhook {delivery_id}` re-queues a dead-lettered delivery (`webhook.delivery_redelivered`), and `UpdateWebhookSubscription {status: active}` clears `failing`. `last_status` is `http_<code>`, `timeout`, `network`, `egress_denied:<rule>` or `signing_unavailable` — never a response body.
- **Each attempt goes through the egress broker** (origin `webhook`) against the subscription's grant and the network policy *as it is at the attempt*, with the addresses the host resolves to then; the request is sent to those vetted addresses only and redirects are not followed. A `paused` subscription holds its deliveries without attempting them (the per-destination kill switch); a Policy whose network rule is `deny` (the tenant-wide kill switch, `SetPolicy`) or a destination that now resolves outside the allowed address classes stops delivery at the next attempt, queued retries included.
- **Request.** `POST <url>`, `content-type: application/json`, body = the redacted RuntimeEvent envelope (§9.1). Headers: `quansio-event-id`, `quansio-event-type`, `quansio-delivery-id`, and `quansio-signature: t=<unix seconds>,v1=<hex HMAC-SHA256(secret, "<t>.<body>")>`. The SDKs verify it (`verifyWebhookSignature` / `verify_webhook_signature`, default tolerance 300 s).
- **Redaction.** Values under secret-named keys (`secret`, `password`, `passphrase`, `token`, `authorization`, `api_key`, `apikey`, `credential`, `cookie`, `private_key`) are replaced; every string is scrubbed of credential-shaped tokens and email addresses; an object that declares `data_classes` containing a protected class (`pii`, `phi`, `financial`, `payment`, `credential`, `secret`, `protected`, `confidential`) keeps only its identifiers, `kind`/`type`/`status` and `data_classes`.
- **Projections.** `GET /v1/webhooks` (subscriptions with their delivery summary — never the secret) and `GET /v1/webhooks/{id}/deliveries` (the ledger, newest first).

The public event stream integrators may also read is §9.3's: WebSocket `GET /v1/stream`, or `GET /v1/stream/events?channel=…&cursor=…` as Server-Sent Events (`id:` is the cursor, `Last-Event-ID` resumes), with the same channels, the same frames and the same protected-thread gate.

---

## 14. Public command catalog (API v1)

Commands are `POST /v1/commands/<Name>` (one endpoint per command; OpenAPI-generated) carrying `command_id`, tenant/workspace scope from auth context. Reads are `GET /v1/...` projections. All commands return `{command_id, accepted_at, result | error}`; mutations are idempotent by `command_id`.

| Family | Commands |
|---|---|
| Identity | `CreateWorkspace`, `UpdateWorkspace`, `InviteMember`, `UpdateMembership`, `RemoveMember`, `CreateServicePrincipal`, `RevokeServicePrincipal`, `UpdateUserSettings`, `ConfigureSso`, `SetGroupRoleMapping`, `RemoveGroupRoleMapping`, `RevokeUserSessions` |
| Teammates | `CreateTeammate`, `UpdateTeammate`, `ArchiveTeammate` |
| Conversation | `CreateThread`, `PostMessage`, `EditMessage`, `DeleteMessage`, `ReactToMessage`, `AddParticipant`, `RemoveParticipant`, `HandOffThread`, `AnswerQuestion`, `CancelTurn` |
| Work | `CreateObjective`, `ProposeGraphEdit` (user-originated, validated like a PlanProposal), `CancelRun`, `RetryRun`, `PauseRun`, `ResumeRun`, `RequestVerification` |
| Trust | `ApproveEffect`, `DenyEffect`, `SetUserRule`, `RevokeUserRule` |
| Execution | `ProvisionTarget`, `StopTarget`, `ResetTarget`, `CreateCheckpoint`, `RestoreCheckpoint` |
| Browser/computer | `RequestTakeover`, `Handback`, `ImportBrowserSession`, `GrantComputerTier` |
| Artifacts | `CreateArtifact` (upload init/complete), `AddArtifactVersion`, `GrantArtifactAccess`, `DeleteArtifact` |
| Knowledge & memory | `IngestSource`, `DeleteKnowledge`, `SupersedeKnowledge`, `CreateMemory`, `DeleteMemory` |
| Skills & packs | `InstallSkill`, `PromoteSkillVersion`, `DisableSkill`, `ProposeSkillCandidates`, `SubmitPack`, `QualifyPack`, `PublishPack`, `RollbackPack` |
| Routines | `CreateRoutine`, `UpdateRoutine`, `PauseRoutine`, `ResumeRoutine`, `TriggerRoutineNow`, `DeleteRoutine` |
| Connectors | `BeginConnectorAuth`, `CompleteConnectorAuth`, `RevokeConnector`, `CreateWebhookSubscription`, `UpdateWebhookSubscription`, `RedeliverWebhook`, `DeleteWebhookSubscription` |
| Notifications | `UpdateNotificationPreferences`, `MarkNotificationRead` |
| Admin/ops | `SetPolicy`, `SetBudget`, `FreezeEffects`, `UnfreezeEffects`, `DisableProvider`, `EnableProvider`, `QuarantineWorker`, `DrainTarget`, `ExportTenantData`, `DeleteUserData`, `EngageKillSwitch`, `ReleaseKillSwitch` |
| Read projections | `GET /v1/threads/{id}`, `/messages`, `/workgraph`, `/runs/{id}`, `/runs/{id}/timeline`, `/effects`, `/approvals`, `/questions`, `/artifacts`, `/artifacts/{id}/versions`, `/artifacts/{id}/versions/{vid}/content`, `/evidence/{id}`, `/targets`, `/browser-sessions/{id}`, `/knowledge`, `/memory`, `/skills`, `/packs`, `/routines`, `/connectors`, `/members`, `/service-principals`, `/sso`, `/policies`, `/teammates`, `/notifications`, `/usage`, `/audit`, `/search?q=` (typed SearchProgram), `/webhooks`, `/webhooks/{id}/deliveries`, `/ops/switches` (tenant administrators; active kill switches and recent history), `/diagnostics/bundle` (tenant administrators; the tenant's recent redacted log lines, bounded), `GET /v1/stream` (WS), `GET /v1/stream/events` (SSE) |

Agent-originated intents (`work.delegate`, `work.propose_plan`, `user.ask`, `memory.propose`) are Tools (§7.5), not public commands.

---

## 15. Error taxonomy

Errors are `{code, message, correlation_id, retryable, details?}`; HTTP mapping in parentheses.

Tracing (OPS-003): a client may send a W3C `traceparent` header with any request; the server continues it (or starts a trace), answers with its own span's `traceparent`, and passes a child `traceparent` in the gRPC metadata of every call to the intelligence plane. A trace id is correlation, never authority. Operators read Prometheus metrics at `GET /metrics` (bearer `QUANSIO_METRICS_TOKEN`; not part of API v1) and may export spans over OTLP/HTTP (`QUANSIO_OTLP_ENDPOINT`).

| Family | Codes |
|---|---|
| Auth (401/403) | `AUTH_REQUIRED`, `AUTH_INVALID_TOKEN`, `AUTH_EXPIRED`, `SCOPE_FORBIDDEN`, `TENANT_MISMATCH` |
| Validation (400) | `VALIDATION_SCHEMA`, `VALIDATION_UNKNOWN_FIELD`, `VALIDATION_BOUNDS` |
| Conflict (409) | `CONFLICT_REVISION`, `CONFLICT_IDEMPOTENCY_MISMATCH`, `CONFLICT_STATE` |
| Authority (403) | `CAPABILITY_DENIED`, `CAPABILITY_INPUTS_UNAVAILABLE`, `POLICY_DENIED`, `APPROVAL_REQUIRED`, `APPROVAL_INVALID`, `APPROVAL_EXPIRED`, `APPROVAL_SUPERSEDED` |
| Runtime (409/422) | `RUNTIME_ILLEGAL_TRANSITION`, `FENCED_STALE_GENERATION`, `LEASE_LOST`, `EFFECT_UNKNOWN_PENDING_RECONCILIATION`, `BUDGET_EXHAUSTED`, `EFFECTS_FROZEN` |
| Execution (503/504) | `TARGET_UNAVAILABLE`, `TARGET_PROVISION_FAILED`, `TOOL_TIMEOUT`, `TOOL_OUTPUT_TRUNCATED` (warning), `EGRESS_DENIED` |
| Model (502/503) | `PROVIDER_UNAVAILABLE`, `PROVIDER_RATE_LIMITED`, `PROVIDER_REFUSAL`, `ROUTE_UNAVAILABLE`, `DLP_DENIED` |
| Generic | `NOT_FOUND` (404), `RATE_LIMITED` (429), `STREAM_BACKPRESSURE` (WS close), `INTERNAL` (500) |

---

## 16. Audit and data lifecycle

- `AuditEntry: id, tenant_id, workspace_id?, actor, action (command name | ops action | policy decision), target_ref, decision, reason, correlation_id, occurred_at, prev_hash, hash` — append-only, hash-chained per tenant.
- Data classes for DLP and retention: `public`, `internal`, `confidential`, `restricted`, `personal`. Artifacts, knowledge, connectors and tool results may carry a data class; policies reference classes.
- Deletion workflow (OPS-002): authoritative delete → tombstone → derived stores (indexes, embeddings, memory, caches) purge → `audit.deletion_completed` with counts. Legal hold overrides with explicit audit.

---

## 17. Schema and contract conventions

- Protobuf package `quansio.v1.<area>`; enums prefixed; all messages carry `schema_version`.
- OpenAPI 3.1 `/v1`; error schema shared; every command has a request/response schema generated from the same source as Protobuf where they overlap (single source in `schemas/`).
- JSON Schema for persisted config/manifests: `schemas/*.schema.json` with `$id` and version.
- Compatibility: additive changes only within `v1`; field removal requires a deprecation window of one minor release and a fixture in `tests/contract/compat/`.
- Time: RFC 3339 UTC strings in JSON; `TIMESTAMPTZ` in PostgreSQL. Money: integer minor units + ISO 4217 code.
