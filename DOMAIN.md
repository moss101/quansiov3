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
| **DocumentRevision** | Opaque per-tab token that changes when a browser tab navigates or its frame tree changes; every mutating browser Tool names the one the model last observed (§8.4.1). |
| **Snapshot / Capture** | A browser observation whose element refs (Snapshot) or screenshot points (Capture) are bound to a tab and DocumentRevision and are invalidated by any mutation (§8.4.1). |
| **In-doubt outcome** | The explicit result of a browser action whose effect on the page cannot be established; settles as `OUTCOME_UNKNOWN`, is never retried, and is reconciled by a fresh observation (§8.4.1). |
| **SecretPlaceholder** | `{{secret:<name>.<field>}}` in a browser Tool argument; only the trusted runtime resolves it, at insertion, against the SecretHandle's allowed origins (§8.4.1). |
| **DecisionReceipt** | Proof, minted only by machine control for a user, that a person resolved a certificate error or granted a media permission (§8.4.1). |
| **ActionReceipt** | What a native computer action reports about itself: `{method, execution_method, dispatched, effect_verified (true\|false\|null), outcome}`; `null` means the effect could not be verified and is never read as `true` (§8.6.1 A). |
| **Terminal condition** | User takeover, revoked permission, exceeded resource limit or user stop: it ends the computer action sequence it occurs in and returns a typed `COMPUTER_*` result (§8.6.1 B). |
| **PermissionStatus** | The bridge's typed answer for Accessibility, Screen Recording, Input Monitoring and Microphone: `granted\|denied\|not_determined\|restricted` (§8.6.1 E). |
| **Pinned helper** | The native helper process, launched only after its code signature matches the bundle ids and team id in `config/computer-use.yaml`, from a versioned install directory (§8.6.1 D). |
| **Recording** | A person's demonstration of a workflow, captured by the native recorder, redacted before it is stored or sent, and kept as an Artifact. Evidence of intent, never a script to replay (§11.7). |
| **Recorder owner fence** | The `{tenant_id, user_id, epoch}` every recorder and Recording call carries; any account or session change makes the next call fail with `RECORDING_OWNER_CHANGED` (§11.7 B). |
| **Redaction summary** | The counts, per category, of what redaction removed from a Recording. Never the values (§11.7 C). |
| **Input slot** | A named value in a compiled skill that the person must classify as a constant or a runtime input; an example value from a Recording is never baked into a skill unclassified (§11.7 D). |
| **Maintenance status** | What a client may read before it restarts itself: `{idle, active_turns, inflight_actions, open_terminals}`, derived from Run/Turn/Effect/terminal rows by their owners; a report, never a lock (§13.6 A). |
| **Window type** | The kind of a desktop window the main process manages: `main`, `popout`, `quick_entry` or `indicator`. It, not anything the page says, decides which IPC channels the window may use (§13.6 B). |
| **Activity item** | One entry of the desktop's attention list, derived from RuntimeEvents and ordered waiting-for-user, running, error, completed (§13.6 C). |
| **File grant** | Files a person picked in a native open dialog, held by the main process for one window under a single-use token with a short expiry; the renderer holds the token and never a path (§13.7 A). |
| **Granted root** | A folder a person picked in a native folder dialog and the main process holds for one window; the only place a local file may be listed, previewed, edited or watched, named by an opaque entry id and never by a path (§13.7 B). |
| **Media URL** | A `quansio-media:` URL whose path is a capability token bound to one window and one granted file; GET/HEAD only, revoked with the window, the grant or the root (§13.7 C). |
| **Edit baseline** | The size and modification time captured when a local file was read; a write replaces the file only if both still match after the new bytes are on disk (§13.7 D). |
| **Managed worktree** | A Git worktree the runtime created under a target's managed root for agent work; removed only through two-phase retention (§8.7). |
| **Retention token** | The 64-hex digest that binds a worktree cleanup to the exact set and state a person previewed (§8.7 B). |
| **Recovery record** | The durable per-worktree record, with Git refs under `refs/quansio/recovery/`, that lets a removed or half-removed managed worktree be restored (§8.7 A, C). |
| **Output adapter** | A deterministic generator in the intelligence plane (slides, diagrams, documents, PDF, speech-to-text, video) whose bytes the trusted runtime verifies before it commits a version of them; it proposes, it never certifies (§10.4 A). |
| **Quality record** | The immutable `{adapter, inputs, ok, checks[]}` the authority writes with an output version, so a deliverable carries the checks it passed and the evidence for them (§10.4 A). |
| **Deck pages** | The tool-owned Artifact of a presentation's rendered pages: per-slide HTML, digest, state and checks. Only the deck tools write it (§10.4 B). |
| **Deck gate** | The completion gate of a deck: it is deliverable only when every page of its current version is `ready` (§10.4 B). |
| **Render sandbox** | The throwaway browser context an output is laid out in: one document in, no network, no files, no script, every refusal recorded (§10.4 C). |
| **Video timeline** | The versioned source of an edit: media ids, tracks, clips, keyframes and subtitles. Its revision is the Artifact version number; edits are typed atomic batches (§10.4 D). |
| **Transcript** | The result of speech-to-text: segments with timestamps, labelled `UNTRUSTED_EXTERNAL` (§10.4 E). |
| **Preview scope** | A short-lived capability that serves one directory of a workspace's roots on loopback, whose authority is re-resolved on every request (§10.4 F). |
| **Preview suggestion** | A visual edit the person proposes from the preview, staged as an untrusted local Artifact the agent reads through a Tool (§10.4 F). |

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
| Recording | `rec_` | | | |

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
| `fs.delete.workspace` (remove a managed worktree, §8.7) | 2 | ask (a sweep of fully clean managed worktrees: allow+log) | query (path and recovery record) |
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
`name (namespaced: fs.read, fs.write, fs.patch, terminal.exec, process.spawn, browser.navigate, browser.observe, browser.click, browser.type, browser.press_key, browser.handle_dialog, browser.extract, browser.screenshot, web.search, web.fetch, computer.read, computer.click, computer.type, computer.clipboard, computer.system_key, connector.<id>.<op>, scm.git.<op>, scm.pr.<op>, artifact.create, artifact.update, deck.render, deck.export, deck.deliver, diagram.render, document.export_pdf, video.edit, video.render, video.inspect, audio.transcribe, webdev.preview_open, webdev.preview_renew, webdev.preview_close, webdev.preview_suggestions, work.propose_plan, work.delegate, work.complete, user.ask, memory.propose, knowledge.cite), version, description, input_schema (JSON Schema, additionalProperties=false), output_schema, effect_class (static or derived by function of args), resource_derivation, required_grant_template, idempotency_key_derivation, side_effect (browser Tools only: below), host (server|qworkerd|browser|machine|adapter), max_output_bytes, evidence_capture (what is recorded), source_trust (trust level assigned to results), timeout_ms, cancellable`.

Tools are registered in the Tool Registry (control plane) and exposed to models only through CapabilityProjection filtering; a model never sees a tool it cannot use.

**Browser side-effect classes (ADP-001).** Every `browser.*` Tool declares a `side_effect` class, and `config/effects.yaml` (`browser_side_effects`) maps it onto an effect class of the §7.1 table. The mapping adds no class and no tier; the registry refuses a browser Tool whose declared `effect_class` differs from its row, so a Tool cannot lower its tier by choosing a different class.

| Side-effect class | Meaning | Settles as (journaled?) |
|---|---|---|
| `read` | reads a page, changes nothing | `read.external` (not journaled) |
| `read_sensitive` | reads what may expose sensitive on-screen content (a screenshot) | `read.internal` (not journaled) |
| `local_mutation` | changes the page's own state inside the sandbox (typing, a key, a dialog answer) | `computer.input.privileged` (journaled) |
| `open_world_mutation` | reaches a destination outside the sandbox (navigation) | `network.egress.new_destination` (journaled) |
| `potential_external_effect` | may cause whatever the page decides (a click); the driver refines the class from the control before dispatch (`message.send`, `record.delete`, `payment.execute`, `credential.access`, …) | `computer.input.privileged` (journaled) |

A **journaled** action is keyed by `(tool_call_id, action_id)` with a canonical-argument digest taken from the arguments in placeholder form (§8.4.1 B). Its outcome settles on the Effect Ledger as `SETTLED_SUCCESS`, `SETTLED_FAILED` or — when the worker cannot establish it — `OUTCOME_UNKNOWN`, which is reconciled (§7.2) and never retried.

**Computer Tool results (ADP-002).** Every `computer.*` Tool's `output_schema` requires a `receipt` (§8.6.1 A); the registry refuses a `computer.*` declaration whose schema omits it or loosens it, and a result whose receipt is incoherent (for example `effect_verified=true` with `dispatched=false`) fails output validation. Terminal conditions (§8.6.1 B) are returned as `COMPUTER_*` Tool errors, not as results.

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
`id, target_id, run_id?, profile_ref, control_holder (agent|user|none), control_since, current_url, tabs[] {target_id, url, title, document_revision?}, screencast {enabled, stream_ref}, checkpoint_id?, status (active|paused_takeover|paused_policy|closed), guard (§8.4.1)`. Takeover: `RequestTakeover` → `control_holder=user` fences agent input; `Handback` → agent resumes. No second session is created on takeover (APP-007).

Understanding order for agents: DOM/CDP/network metadata → accessibility tree → screenshot/vision fallback (EXEC-009). Live view: CDP screencast frames as binary LiveFrames over the client stream; user input during takeover flows client → server → qworkerd → CDP input.

#### 8.4.1 Hardened browser actions (ADP-001) — normative

The managed browser is driven only through `browser.*` Tools (§7.5). Each Tool is dispatched by the qworkerd browser driver, serialized per session in one action queue. Page-derived content in every result — snapshot text and element names, console lines, dialog messages, download names, certificate error URLs — is `UNTRUSTED_EXTERNAL` (§12); results are validated against the Tool's `output_schema` and a result the schema does not describe is not returned.

**A. Stale-page fencing.**
- A tab carries an opaque `document_revision` that changes on every navigation (including same-document navigation) and every frame-tree change, and whenever the driver lost events.
- `browser.observe` returns `snapshot_id`, `document_revision` and an element `ref` (`e1`, `e2`, …) for each interactive element. A ref is bound to its snapshot, its tab and its revision, and is never silently re-resolved. `browser.screenshot` returns a `capture_id` bound to the screenshot, the revision and the scroll position, and records one digest per viewport tile.
- A mutating Tool names its target as `{snapshot_id, ref}` or `{capture_id, x, y}` — exactly one form — plus `expected_document_revision`. Before dispatch the driver re-verifies: the revision; that the snapshot or capture is still valid; for a ref, that the backend node resolves and its semantic fingerprint (`role`, accessible `name`, `tag`, frame origin) is unchanged; and by hit test at the point that will be clicked that the point lands on the target (or inside it). A point whose capture was taken at another scroll position is stale. Pixels changing in the tiles around a point produce a `pixels_changed_near_point` warning, not a refusal.
- Any mismatch fails closed with `BROWSER_STALE_PAGE` before any input event is sent. Any mutation invalidates the tab's snapshots and captures, so a ref is good for one action; the model observes again.

**B. Action idempotency and the in-doubt outcome.**
- A journaled action (§7.5) is keyed by `(tool_call_id, action_id)`; `action_id` is the effect's idempotency key, derived by the runtime and never supplied by the model. Its digest is the SHA-256 of the canonical arguments **in placeholder form** (§C).
- The same key and digest after completion returns the stored result without touching the page. The same key with a different digest is `BROWSER_ACTION_REPLAY_CONFLICT`. The `start` record is made durable immediately before the first input event; refusals that happen earlier write nothing.
- When input may have reached the page and the browser then fails (timeout, connection lost, target closed), or a started action has no recorded outcome after a restart, the result is **in doubt**: status `in_doubt`, `retryable=false`, `requires_new_observation=true`, settled on the Effect Ledger as `OUTCOME_UNKNOWN`. Nothing retries it; the model reconciles it with a fresh observation, and a reconciled key stays spent — repeating the work is a new action with a new id.
- The journal is the worker's half of the same record; it holds no policy, approval or outcome the server has not been told. The server holds the doubt in `BrowserSession.guard.in_doubt`.
- The classified consequence of a click (the control it lands on) is checked against the tier the runtime approved for the call; a higher one is `BROWSER_EFFECT_TIER_EXCEEDED` before dispatch.

**C. Secret placeholders.**
- The model writes `{{secret:<name>.<field>}}` in `browser.type` arguments. `<name>` is the label of an active SecretHandle (§0) and `<field>` a field of its sealed material, which holds `{fields, allowed_origins}`; there is no second secret store. Only the trusted runtime substitutes, at insertion, through the secret broker (`credential.access`, tier 4: the call needs an approved tier of 4).
- The live origin of the top frame **and** of the frame containing the target field is compared with the secret's `allowed_origins` before insertion and again, inside the page and in the same task as the insertion, immediately before the text is inserted. A mismatch is `BROWSER_SECRET_ORIGIN_MISMATCH` and nothing is typed. The element's fingerprint and sensitivity are re-checked after focus.
- Plain text typed into a password, one-time-code or card number/security-code/expiry field (`type=password`, `autocomplete` `current-password|new-password|one-time-code|cc-number|cc-csc|cc-exp*`, or a name/id/placeholder that says so) is `BROWSER_SENSITIVE_INPUT_DENIED`; such a field accepts only placeholders. A printable `browser.press_key` is refused while focus is in a sensitive field, or in a frame whose focus the driver cannot read.
- The resolved value is never written to a ledger digest, log, event, observation, journal record or model-facing result; results carry `used_secrets: [{name, field}]` only.

**D. Dialogs, downloads, permissions, certificates, gesture.**
- A pending JavaScript dialog (`alert`, `confirm`, `prompt`, `beforeunload`) is typed state (`dialog {id, kind, message, url, document_revision}`) and fences every other action on the tab (`BROWSER_DIALOG_PENDING`) except `browser.handle_dialog`, which must quote the exact `dialog_id` and the revision it was raised under (`BROWSER_STALE_DIALOG` otherwise). An action during which a dialog opens returns `dialog_pending` instead of hanging.
- Downloads are tracked per tab and directed to the session's bounded directory. A download is cancelled when it passes its size cap, receives no byte within its start timeout, stalls past its stall timeout, or exceeds the per-tab count; each is a typed record `{id, url, suggested_filename, state (in_progress|completed|cancelled), received_bytes, total_bytes, file?, sha256?, failure? (too_large|start_timeout|stalled|too_many)}` with the file name sanitised.
- Every permission is denied when a tab is attached and stays denied. Only `camera` and `microphone` can be granted, only for one origin, and only by a user through `DecisionReceipt` (below); agent input has no argument or Tool that names a permission.
- A certificate error does not load the page: it becomes `certificates[]` state `pending` and `BROWSER_CERTIFICATE_DECISION_REQUIRED`. Only a user can resolve it, per origin and error, by `continue` or `cancel`; unresolved errors are cancelled after a bounded wait. The browser reveals a certificate only after a *continue*, so the leaf's SHA-256 is recorded in the decision once it is known. A `DecisionReceipt` can be minted only by machine control for a user actor; an agent actor is refused and the row is unchanged.
- A popup, or a handoff to another application (`mailto:`, an application scheme), is allowed only when it follows genuine user input — input relayed during a takeover. Agent-synthesized input is flagged `input_origin: agent` and does not count: popups it causes are closed (`popups_blocked`), a handoff is stopped (`handoffs_blocked`), and a click on a link that would hand off is `BROWSER_GESTURE_REQUIRED`.
- Navigation is `http`, `https` or `about:blank` with no credentials in the URL (`BROWSER_NAVIGATION_DENIED`).

**E. Guard state.** `BrowserSession.guard` is `{reported_at, tabs[] {tab_id, url, document_revision, dialog?, certificates[], downloads[], popups_blocked}, in_doubt[] {call_id, action_id, reason}, decisions[] {kind (certificate|permission), tab_id, subject, decision (allow|deny), decided_by, decided_at}}`. The worker reports `tabs` and `in_doubt` (strict, bounded); machine control writes `decisions`. A report never reverts a decided certificate to `pending`.

**F. User-only decisions.** `ResolveBrowserCertificate {session_id, tab_id, certificate_id, decision}` and `DecideBrowserPermission {session_id, tab_id, origin, permission, decision}` (§14) are user commands. They are recorded by machine control with the user's identity and yield the receipt the worker acts on; no agent Tool corresponds to either.

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

#### 8.6.1 Native computer actions (ADP-002) — normative

Every `computer.*` Tool result carries an `ActionReceipt`; a sequence of actions in one ToolCall ends at the first terminal condition; the helper that performs them is pinned; and a missing permission is a place the run waits for the user, never a model retry. Limits, the helper's identity and the guide's Settings panes are `config/computer-use.yaml` (D-018), validated by `schemas/json/computer-use-config.schema.json`.

**A. Action receipts.** The bridge reports and the runtime re-checks: `receipt {method, execution_method, dispatched, effect_verified, outcome, invariant_violated, invariants?}`.
- `method`: `accessibility_tree`, `accessibility_action`, `process_event` (an event posted to one process), `hid_event` (an event through the system input stream), `pasteboard`. `execution_method`: `background` (the action claims to change neither the frontmost application nor the real pointer) or `foreground`.
- `dispatched` is whether the bridge handed input to the system. `effect_verified` is `true` only when the bridge observed the intended effect (a read-back), `false` when it observed that the effect is absent or an invariant was violated, and `null` when the effect could not be observed. `null` is not `false` and is never reported as `true`; a bridge that cannot tell says `null`.
- `outcome`: `applied` (`true`), `observed` (a read: `null`, data returned), `unverified` (`null`), `not_applied` (`false`), `invariant_violated` (`false`), `refused` (`dispatched=false`, `false`: nothing was sent).
- Coherence, enforced by the runtime on every receipt and by `crates/tools` on the Tool result: `dispatched=false` ⇒ `refused`; `effect_verified=true` ⇒ `dispatched` and `applied` and not `invariant_violated`; `effect_verified=null` ⇒ `dispatched` and `observed` or `unverified`; a `background` receipt may be `applied` only with measured `invariants` that show no violation, else the runtime lowers `true` to `null` (`unverified`). An incoherent receipt is a bridge fault: for input it is an unknown outcome (the ToolCall slot stays active), for a read a failed read.
- Settlement: `applied` and `observed` settle `SETTLED_SUCCESS`, `refused` settles `SETTLED_FAILED`, and every other receipt of a dispatched action (`unverified`, `not_applied`, `invariant_violated`) settles `OUTCOME_UNKNOWN`: input reached the machine, so the Effect Ledger reconciles it (§7.1: `computer.input.privileged`, manual) and never retries it blind. The dispatch slot (§8.6) is released once the bridge has answered; it stays active only when the bridge itself failed after dispatch.

**B. Terminal conditions.** A ToolCall may carry a sequence of actions. Before every action and after every settled action the runtime checks, in this order: a user stop, the control row (`holder` is not `agent`, `takeover_pending`, or a changed `generation` is a takeover), the permissions the next action needs, and the sequence limits (`config/computer-use.yaml: sequence_limits`: actions, wall time, typed bytes). The first condition found ends the sequence: no further action is dispatched, the actions already settled keep their receipts, the unattempted ones are reported as `skipped`, and the result is the typed terminal result. The sequence is a closed list the runtime consumes; a model's later output cannot extend, resume or re-order it, and a terminal state is final for that ToolCall. A permission that was granted when the sequence began and is missing later is `COMPUTER_PERMISSION_REVOKED`; one that is missing at its first action is the park of §E. A receipt of `not_applied` or `invariant_violated` also stops the sequence (the premise of the next action is gone) but is a result, not a terminal condition; an `unverified` receipt does not stop it, and the ToolCall settles as the least certain of its actions.

**C. Background invariants.** An action whose `execution_method` is `background` is measured by the bridge before and after: `invariants {physical_input, moved_system_cursor, frontmost_before {bundle_id, pid}, frontmost_after {bundle_id, pid}, frontmost_changed}`. `physical_input` is any input from the system input stream during the action window (the bridge cannot tell a person from a synthetic event, and either breaks the claim). A violation sets `invariant_violated=true`, `effect_verified=false`, `outcome=invariant_violated`; the runtime recomputes this from `invariants`, keeps a flag the bridge raised, and never accepts a bridge's "no violation" over numbers that show one. A background action the bridge could not measure (no frontmost application or pointer to read) carries `invariants: null`, and the runtime lowers its `applied` to `unverified`.

**D. Helper identity.** The helper is launched only after `codesign` verification of its code signature, its designated requirement (`identifier` in `helper.allowed_bundle_ids`, Apple anchor and `certificate leaf[subject.OU]` equal to `helper.team_id`) and the hardened-runtime flag. Until the configuration is `published` with a team id, no helper may be launched (`COMPUTER_HELPER_IDENTITY_REFUSED`). The helper is installed into `<app data>/<helper.install.directory>/versions/<fingerprint>/`, re-verified after the copy (SHA-256 of the tree and CDHash), with `active` and `previous` pointers so a failed activation rolls back; helper processes found running from that directory with the configured profile prefix when the runtime starts are orphans of a crash and are terminated.

**E. Permission onboarding.** `PermissionStatus {accessibility, screen_recording, input_monitoring, microphone}` (each `granted|denied|not_determined|restricted`) comes from the bridge. The permissions an action needs: `computer.read` Accessibility, plus Screen Recording when it asks for a screenshot; `computer.click`, `computer.type`, `computer.system_key` Accessibility; `computer.clipboard` none. When one is missing the dispatcher refuses before any slot is taken, with an attention request `{reason, permission, status, guide}`; nothing is sent to the bridge and a repeated proposal gets the same answer. The runtime parks the Run `SUSPENDED` (target capability loss) with `COMPUTER_PERMISSION_REQUIRED` and a `blocked` Notification (§13.3); it leaves `SUSPENDED` only through `ResumeRun`, which the user sends. `guide.action` is `open_settings` (the desktop deep-links the user to the Privacy pane named by `guide.pane`; only a user click opens it and the agent never operates System Settings), `contact_administrator` for `restricted`, or `pause_input` for a lapsed foreground-control grant (`ForegroundControl: held|lapsed`, reported by the bridge when input from the person arrived after the agent's last input): that one asks the person to stop using the keyboard and mouse for a moment and never opens Settings (`COMPUTER_FOREGROUND_CONTROL_LAPSED`). The desktop polls the status and enables its Continue control only when every permission the guide names is `granted`.

### 8.7 Worktree retention (ADP-005) — normative

An agent that works on a repository does so in a **managed worktree**: a Git worktree the runtime creates (`git worktree add --detach`) under the target's one managed root, as `<managed root>/<repo key>/<worktree id>`, never inside the user's own checkout. Retention is the rule for when one is removed. It lives in `qworkerd` (`crates/qworkerd/src/tools/scm/retention.rs`) and is host-callable (a runtime or person's command), not a model-visible Tool; it adds no store (D-019): its state is a recovery record file and Git refs, both inside the target. Limits, the sweep policy and the ref namespace are `config/worktree-retention.yaml` (D-018), validated by `schemas/json/worktree-retention-config.schema.json`. A `worktree id` is a runtime-chosen slug `^[a-z0-9][a-z0-9_-]{0,62}$`, local to one target, not a §1 identity.

**A. Recovery record.** One JSON file per managed worktree, `<managed root>/.quansio/worktrees/<worktree id>.json`, written atomically (temporary file, `fsync`, rename): `{id, repo, path, branch?, head, revision, activity {holders, last_activity_at_ms}, state, created_at_ms, recovery?, removal?}` (times are Unix milliseconds; the record is the target's own file, not an API schema). `revision` is a counter that increases on every recorded change of `head`, `activity` or `state`. `state`: `IDLE ⇄ ACTIVE` (a Run holds it while `holders > 0`), `IDLE → REMOVING → REMOVED`, `REMOVED → IDLE` (restore). `recovery` is `{head_ref, index_ref?, snapshot_ref?, taken_at_ms, complete, ignored_files}` and `removal` is `{mode: manual|auto, started_at_ms, fingerprint, finished_at_ms?}`.

**B. Two-phase cleanup.** `prepare(ids)` reads each worktree's `{state, head, revision, activity, fingerprint}` and returns a **retention token**, the SHA-256 (64 hex) of the canonical JSON of the sorted ids and those states, together with the prepared set, which is written durably as `<managed root>/.quansio/prepared/<token>.json` and expires after `prepare_ttl_ms`. `fingerprint` is the SHA-256 of `HEAD`, the live index (`git ls-files -s`), the non-ignored status (`git status --porcelain=v2 -z --untracked-files=all`) and the content of every file that status names, so a file edited again under an unchanged status line is a different state. `cleanup(token, ids, mode)` first refuses the whole call with `WORKTREE_TOKEN_INVALID` (`details.reason`: `unknown`, `expired`, `set_mismatch` when `ids` is not exactly the prepared set). It then **accounts for every requested id** as `removed` or `skipped` with a reason, rechecking each worktree immediately before removing it against what was previewed: `not_found`, `already_removed`, `busy` (another cleanup holds it), `active` (state `ACTIVE` or `holders > 0`), `revision_changed`, `head_moved`, `index_changed` (fingerprint), `unmerged_index`, `not_clean` (automatic mode only), `recovery_failed`, `removal_failed`. A skipped worktree is left exactly as it was. The prepared file is deleted only after every id is accounted for, so a cleanup that crashed is resumed by repeating it with the same token (ids already removed are `already_removed`).

**C. Recovery snapshot.** Before a **manual** cleanup removes a worktree, the runtime writes, under the private namespace `refs/quansio/recovery/<worktree id>/`: `head` (the original `HEAD` commit, which also keeps it from being collected), `index` (a commit whose tree is the live index's staged entries, written from a temporary index file because `git write-tree` on the live index would rewrite its cache extension, and whose parent is `HEAD`) and `snapshot` (a commit whose tree is the working tree, tracked changes and untracked non-ignored files, built with a temporary index file outside the repository, and whose parents are `HEAD` then the `index` commit). It never runs `git stash`, never writes the worktree's real index, never creates or moves a branch or a tag and never touches a ref outside that namespace. Ignored files are not captured: `recovery.ignored_files` records how many there were. The record is marked `recovery.complete` only after the three refs exist and name the commits written, and removal never starts before that.

**D. Removal rules.** A manual cleanup may remove a worktree that has changes, after C. The **automatic sweep** removes only a fully clean (no staged, unstaged or untracked non-ignored change, no unmerged entry) idle worktree, idle for at least `sweep.min_idle_ms`, and only while more than `max_resident` managed worktrees exist, oldest `last_activity_at` first, at most `sweep.max_per_run` per run; it writes the `head` ref and the record but no snapshot. Removal writes `state: REMOVING` with the fingerprint, runs `git worktree remove --force`, then `git worktree prune`, then marks `REMOVED`. Nothing in cleanup runs `git branch -D`, `git stash` or any command that rewrites a ref outside the namespace.

**E. Crash recovery.** `recover()` runs when the runtime starts and resolves every record found in `REMOVING`, changing nothing else: a worktree whose recovery is not `complete` is returned to `IDLE` (it was never touched); one whose recovery is `complete` and whose checkout is intact is completed only if its fingerprint still equals `removal.fingerprint`, otherwise returned to `IDLE`; one whose checkout is already damaged or gone is completed (its state is in the snapshot). A prepared token survives a restart until it expires. Whatever the crash point, the working state is either still in its checkout or restorable from C.

**F. Missing worktree and restore.** Asking for a worktree whose checkout is gone, when a record names it, fails with `WORKTREE_MISSING` and `details {id, state, recovery_record}`, never with a bare path error. `restore(id)` recreates the checkout at its original path (refused if the path is occupied) with `git worktree add --detach <path> <head>`, then `git read-tree --reset -u <snapshot tree>` and `git read-tree <index tree>`, so the working tree is the snapshot and the index is what was staged; with no snapshot (an automatic sweep) the checkout of `head` is the whole state. `REMOVED → IDLE`.

**G. Effects.** `prepare` is `read.internal`; a removal (manual or automatic, with its recovery refs) settles `fs.delete.workspace` (§7.1: tier 2, because the Git-visible state is restorable and ignored files are not); `restore` and worktree creation settle `fs.write.workspace`. `config/effects.yaml: worktree_operations` maps each operation to its class, and a test holds the code to that map. Agent-side file writes keep `fs.write`/`fs.patch`: a call may carry `base_digest`, the digest of the file as the caller last read it, and the host refuses with `FILE_EDIT_CONFLICT` (details `{expected_digest, actual_digest}`) when the file's digest is different after the new bytes are written to a temporary file and just before they replace it; a patch is applied to the bytes it was checked against or not at all.

---

## 9. Events and streaming

### 9.1 RuntimeEvent envelope (CORE-003)
`event_id, tenant_id, workspace_id?, sequence (tenant-monotonic), aggregate_type, aggregate_id, aggregate_version, type, schema_version, occurred_at, command_id?, correlation_id, causation_id?, actor {kind: user|agent|system|service, id}, generation?, payload (typed by `type`)`.

Written in the same PostgreSQL transaction as the mutation, plus an outbox row; the publisher relays to NATS JetStream subject `q.<tenant>.<aggregate_type>.<type>` with `Nats-Msg-Id = event_id` for dedup.

### 9.2 Event families (minimum)
`tenant.*`, `workspace.*`, `member.*`, `thread.*` (created, participant_added/removed, message_posted, reaction_added/removed, attention_changed, handed_off), `work.*` (node_created/updated, edge_added/removed, plan_proposed/applied/rejected, node_status_changed, revision_committed), `agent.*` (thread_provisioned, activated, suspended, delegated, handoff_started/completed, mailbox_delivered, mailbox_cursor_advanced, joined, terminated), `run.*` (created, queued, started, waiting, resumed, verifying, verification_passed/failed, succeeded, failed, cancelled, blocked, suspended), `turn.*`, `step.*`, `model.*` (call_started, call_completed, usage_recorded, route_selected, failover), `tool.*` (proposed, validated, rejected, dispatched, completed, failed, unknown), `effect.*` (one per state in §7.2), `approval.*` (requested, granted, denied, expired, superseded), `question.*`, `target.*`, `lease.*`, `browser.*` (session_opened, control_changed, checkpointed), `terminal.*`, `checkpoint.*`, `artifact.*` (created, version_added, grant_changed), `evidence.*`, `knowledge.*`, `memory.*`, `skill.*`, `recording.*` (started, finished, cancelled, aborted, compiled, submitted), `pack.*`, `routine.*` (created, updated, fired, skipped, paused, resumed, deleted, fire_completed, fire_failed), `notification.*` (created, read, preferences_updated), `connector.*` (connected, degraded, revoked, webhook_received), `webhook.*` (subscription_created, subscription_updated, subscription_deleted, secret_rotated, delivery_dead_lettered, delivery_redelivered), `policy.*`, `audit.*`, `usage.*`, `capability.*` (projected, widening_rejected), `compaction.*`, `ops.*` (kill_switch_engaged/released, provider_disabled, worker_quarantined).

### 9.3 Client stream (CORE-009, APP-001)
- Transport: WebSocket `GET /v1/stream`; Server-Sent Events `GET /v1/stream/events?channel=…&cursor=…` for read-only consumers (web fallback, integrators — APP-015): `id:` carries the cursor and `Last-Event-ID` resumes.
- Subscribe: `{subscribe: [{channel: "workspace:ws_…" | "thread:thr_…" | "run:run_…" | "target:tgt_…"}], cursor?}`.
- Frames: `{kind: "event", cursor, event}` (durable, replayable) and `{kind: "live", channel, frame}` (transient: `model.delta`, `terminal.bytes`, `browser.frame`, `progress`). Live frames are never persisted and clients must tolerate their absence. A `model.delta` with `final: false` carries the next piece of a model's text as the model writes it; the one with `final: true` carries the whole text, which replaces what streamed.
- Reconnect with last `cursor` replays missed events in order; slow consumers are disconnected with `STREAM_BACKPRESSURE` rather than buffered unboundedly — a subscriber whose bounded buffer stays full for the stall window (5 s) without it taking a frame; a replay longer than the buffer waits for the subscriber rather than disconnecting it.

---

## 10. Artifacts and evidence

### 10.1 Artifact / ArtifactVersion
`Artifact: id, workspace_id, kind (document|spreadsheet|presentation|code|data|image|audio|video|archive|other), title, role? (report|source|attachment|…), origin {kind: message|run|routine|upload|connector, ref}, current_version_id, grants[] ({principal, level: read|write}), retention {class, expires_at?}, deleted_at?`.
`ArtifactVersion: id, artifact_id, seq, content_digest (sha256), size_bytes, media_type, object_key, produced_by {run_id?, step_id?, user_id?}, parent_version_id?, format_check? {ok, rule, detail}, quality_record? (§10.4 A), created_at`.
Bytes are immutable per version; a new version is a new identity. Preview never executes active content with host privileges (APP-008).

Source formats (CAP-003). An editable artifact is a versioned source file, not a chat blob: `document` — `text/markdown`/`text/plain`, UTF-8, first non-blank line `# Title`, a body, no active content (`<script`, `javascript:`, `onerror=`, `onload=`); `spreadsheet` — `text/csv`/`text/tab-separated-values`, unique non-empty headers, rectangular rows, at least one row; `presentation` — `application/vnd.quansio.slides+json` `{schema_version: "v1", slides: [{title, body, notes?, layout?, alt?, image?, chart?}]}`, every slide with a body (the optional keys are §10.4 B and stay a closed set); `code` — `application/vnd.quansio.code+json` `{schema_version: "v1", language, files: [{path (relative, no `..`), content}]}`, every file with content; `data` — `application/json` `{schema_version: "v1", columns[], rows[{column: value}]}`, at least one row. Media kinds need a matching media type and real bytes; any source that is a chat transcript (`{role, content}` objects) is refused. Every version the store adds records `format_check` (rules `artifact.media_type`, `artifact.opaque_blob`, `artifact.format`, `artifact.quality`); uploads are stored either way, but the governed tools `artifact.create {role, kind, title, content | content_base64, content_digest, media_type?}` and `artifact.update {artifact_id, content | content_base64, content_digest, media_type?}` (server-hosted, `content.upload`) refuse a source whose digest does not match or whose check fails, so a run never produces an invalid version. The intelligence plane applies the same rules before proposing (`python/intelligence/artifacts`); `packs/skills/artifacts/fixtures` is the corpus both must agree on.

### 10.4 Output generators (ADP-006) — normative

Slides, diagrams, printed documents, transcripts, edited video and the web-dev preview are produced by **output adapters**: deterministic generators in the intelligence plane (`python/intelligence/artifacts`) that turn a source Artifact (or a typed request) into bytes. The model path is unchanged (D-014): the plane proposes the bytes through a governed, server-hosted Tool (§7.5), the runtime reserves the effect, and the **server verifies the deliverable itself before it commits a version**. Worker-side operations that need a process on the target (a browser layout, `ffmpeg`) sit behind a port the Tool host calls; when no port is composed the Tool fails closed with `ADAPTER_UNAVAILABLE`, never with a stand-in result. Values (limits, binaries, timeouts) are `config/outputs.yaml`, validated by `schemas/json/outputs-config.schema.json`; model identifiers are `config/models.yaml`.

**A. Outputs and the quality record.** A version made by an output Tool carries `quality_record: {record_version: 1, adapter, adapter_version, ok, inputs[] ({artifact_id, version_id, content_digest}), checks[] ({rule, ok, detail, subject?}), environment? {browser?, ffmpeg?, provider?}, trust, evidence_ids[]}`. `ok` is true exactly when every check is. The authority writes the record in the same transaction as the version; the proposer cannot supply or alter it, a version made by the upload path or by `artifact.create`/`artifact.update` has none, and it is returned beside `format_check` by the version listing. Rules are named `output.<adapter>.<check>`. A deliverable with a failing check is refused (`VALIDATION_SCHEMA`, naming the rule) and no version is created; the one exception is the deck pages Artifact, which records failing pages by design (B). `trust` is `AGENT_GENERATED` for a transformation of workspace sources and `UNTRUSTED_EXTERNAL` for anything whose content came from outside (a transcript, a rendered page); the Tool result repeats it. Artifacts made by output Tools keep their kind's closed shape: a generated file is `kind: other` with its own media type (`application/vnd.openxmlformats-officedocument.presentationml.presentation`, `application/pdf`, `application/vnd.quansio.deck-pages+json`, `application/vnd.quansio.video-timeline+json`, `application/vnd.quansio.transcript+json`, `application/vnd.quansio.diagram+json`), a diagram image is `kind: image` (`image/svg+xml`), a video `kind: video`, and each media type has a shape check in the authority (`format_check`, rule `artifact.format`). `artifact.create`/`artifact.update` refuse a tool-owned media type or a role starting `deck_` (`artifact.tool_owned`), so a page, a manifest or a quality record cannot be hand-made.

**B. Decks.** The source is the `presentation` Artifact. A slide may add `layout` (`title|section|content|two_column|image|quote|chart`, default `content`), `alt` (at most 300 characters), `image` (a `data:image/(png|jpeg|gif|webp);base64,…` URI of at most `deck.image_max_bytes`) and `chart` (`{kind: bar|line, labels[], values[], unit?}` of at most 24 points); an `image` layout needs `image` and `alt`, a `chart` layout needs `chart` and `alt`, and the keys are refused on any other layout. `body` is plain text: blank-line paragraphs, `- ` bullets, a line `---` between the two columns of `two_column`, a last line `— Name` as a quote's attribution; nothing in it is markup. The plane renders each slide with a built-in template (v1) to a fragment `<section class="slide" data-slide="N" data-layout="…">…</section>` for a 1280×720 page plus one shared stylesheet, deterministically (the same slide gives the same bytes). `deck.render {artifact_id, css, pages[] ({index, fragment})}` hands them to the authority, which (1) refuses markup outside the page allowlist (no script, frame, object, form, link, base, event handler or `javascript:`; a URL only as `data:` for images; a stylesheet without `@import` or a non-`data:` `url()`), (2) composes the one trusted page shell (content-security-policy `default-src 'none'`, `@page` 1280×720), (3) lays each page out in the render sandbox (C) of the managed browser and (4) writes the **deck pages** Artifact (`kind: other`, `application/vnd.quansio.deck-pages+json`, role `deck_pages:<deck artifact id>`): `{schema_version: "v1", template_version, deck {artifact_id, version_id, content_digest, slide_count}, css, slides[] ({index, source_digest, state, page_digest, html, checks[], text_digest, evidence_ids[]})}`. Each page is measured by a program the worker owns, not by anything in the markup: `output.deck.page.overflow` (content clipped by a box or painted outside the page, more than `deck.overflow_tolerance_px`), `.empty` (no text and no figure), `.alt_text` (an image, SVG, canvas or media element with no accessible name, unless `aria-hidden` or `role="presentation"`), `.contrast` (every text-bearing element at least `min_contrast_text`, or `min_contrast_large` for large text; a background that cannot be established from style is a failure), `.source_text` (the rendered text contains the slide's title and every body line: the page is the slide, not something else), `.network` (the page requested nothing it was refused). A slide's **state** is `ready` (all checks pass), `failed_quality`, `edited` (its source differs from what the page was rendered from) or `pending` (it has no page yet). A slide whose `source_digest` is unchanged keeps its page when the deck gets a new version; a changed one is `edited`, a new one `pending`, and a `deck.render` renders only the pages it is given. The **deck gate**: `deck.deliver {artifact_id}` succeeds only if the pages Artifact is the authority's own (its version has a `quality_record` by `deck.render`) for the deck's current version and every slide is `ready`; otherwise it fails with `DECK_NOT_DELIVERABLE` and `details {pages[] ({index, state, failed_rules[]}), stale}` and writes nothing. It returns `uri: quansio://artifact/<art_…>` (stable across versions), `version_id`, `content_digest` and the exports whose input is the current version. `deck.export {artifact_id, format: pdf|pptx, content_base64?, content_digest?}` passes the same gate first. A PDF is printed from the stored pages in one render (one PDF page per slide, `config/outputs.yaml` `deck.pdf`), and its checks are `output.deck.pdf.pages` (page count equals slide count), `.structure` and `.network`. A PPTX is built by the plane with a minimal OOXML writer and verified by the authority on every write: a ZIP whose parts are stored and whose CRCs hold, every XML part well-formed, `[Content_Types].xml`, the package and presentation relationships resolving to parts that exist, one `ppt/slides/slideN.xml` per slide in `sldIdLst` order, each slide's text containing the slide's title and every body line, notes in a notes part, and no external relationship (`output.deck.pptx.package`, `.parts`, `.slides`, `.text`, `.external`). Speaker notes are the slide's `notes`.

**C. Printing, diagrams and the render sandbox.** `document.export_pdf {artifact_id, html}`: the plane renders the `document` Artifact's Markdown to HTML with a safe CommonMark subset (headings, paragraphs, emphasis, code, lists, block quotes, tables, links shown as text with their target, `data:` images; raw HTML is escaped, nothing is fetched); the authority checks the HTML against the same allowlist as B, prints it in the render sandbox and records `output.document.pdf.source_text` (at least `pdf.document.min_text_coverage` of the source's words appear in the rendered text), `.pages` (at most `pdf.document.max_pages`), `.network` and `.structure`. The **render sandbox** is one throwaway browser context of the one managed browser (no second launcher): the document is installed with `Page.setDocumentContent`, every request is intercepted and refused unless it is a pinned local asset at `https://assets.quansio.invalid/<name>`, script is disabled in the renderer, downloads are denied, and the context is disposed of on every path; every refused request is listed in the result and is a failing `.network` check. `diagram.render {artifact_id, svg}`: a diagram source is a typed IR (`application/vnd.quansio.diagram+json`: `{schema_version: "v1", kind: flow|sequence|tree, title, nodes[] ({id, label, shape?}), edges[] ({from, to, label?}), direction?}`) or a Mermaid flowchart subset the plane parses into it; the plane lays it out with a deterministic algorithm and writes SVG with no external binary. The authority accepts an SVG only if it is well-formed, has a `viewBox`, no script, `foreignObject`, event handler, external reference or `style` import, at most `diagram.svg_max_bytes`, and one `data-node` element per IR node whose text is the node's label (`output.diagram.svg.safe`, `.nodes`, `.edges`).

**D. Video.** A **video timeline** is an Artifact (`kind: other`, `application/vnd.quansio.video-timeline+json`, role `video_timeline`) `{schema_version: "v1", revision, output {width, height, fps}, media[] ({id, artifact_id, kind: video|audio|image, duration_ms, has_audio?}), tracks[] ({id, kind: video|audio, clips[] ({id, media_id, start_ms, in_ms, out_ms, volume?, keyframes[]? ({at_ms, property: volume, value})})}), subtitles[] ({id, start_ms, end_ms, text, source?})}`; ids are `[a-z][a-z0-9_]{0,31}`; a clip has `in_ms < out_ms ≤` its media's duration, clips on a track never overlap, and a video track holds only video and image media and an audio track only audio media (a video medium with sound also plays its sound unless its clip has `volume: 0`). A keyframe sets the clip's `volume` (0 to 4) at a time relative to the clip's start; between keyframes the volume changes linearly. Subtitles are a soft track in the output, not burned into the picture. Its **revision** is the Artifact version number, and the host writes it into the source too (`revision` equals the version's `seq`, so two edits that reach the same state are still two versions); a version whose `revision` is not its `seq` was not made by `video.edit` and `video.edit` refuses it (`CONFLICT_STATE`, `output.video.revision`). A timeline is created with `artifact.create` at `revision: 1`; every later version is made by `video.edit`. `video.edit {artifact_id, expected_revision, dry_run, ops[]}` applies a batch of at most `video.max_ops_per_batch` (64) typed operations: `add_media`, `remove_media`, `add_track`, `add_clips`, `trim_clip`, `split_clip`, `move_clip`, `remove_clips`, `set_keyframes`, `set_volume`, `add_subtitles`, `remove_subtitles`, `subtitles_from_transcript`, `set_output`; an unknown operation or field is `VALIDATION_UNKNOWN_FIELD`. A batch is **atomic**: every operation is applied to a copy in order, and the first that breaks a rule fails the batch with `VALIDATION_SCHEMA` and `details {op_index, rule}` and writes nothing. `expected_revision` must equal the current revision, else `CONFLICT_REVISION` with `details {expected_revision, current_revision}` and nothing is written (a batch whose result is exactly the next version already stored is answered as that version with `replayed: true`). `dry_run: true` validates the batch and returns what it would change (`{applied, revision (unchanged), summary[], duration_ms}`) and writes nothing. `subtitles_from_transcript` reads the transcript Artifact (E) and its text stays `UNTRUSTED_EXTERNAL`. `video.render {artifact_id, expected_revision}` and `video.inspect {artifact_id, times_ms[]}` run as **supervised jobs** behind a `MediaJobPort` on the target: the job's identity is the digest of its typed plan (an argument vector built without a shell from the timeline), starting the same plan again attaches to the running or finished job instead of starting another, a cancelled call kills the process group and discards a partial output, and a restart finds the job by identity and returns its finished result or re-runs it (rendering is not an external effect). The binary comes from `QUANSIO_FFMPEG_BIN` (config `video.ffmpeg_bin_env`); with no binary, or no port, both fail with `ADAPTER_UNAVAILABLE` and `details {adapter: "ffmpeg"}`. A rendered video is a `video` Artifact whose checks are `output.video.render.probe` (the probe's duration is within `video.duration_tolerance_ms` of the timeline's, its dimensions equal the output's) and `.streams`; an inspection is a contact sheet `image` Artifact plus `frames[] ({time_ms, digest, uniform})`, stored with its record even when a check fails (an inspection exists to show what is there), whose checks are `output.video.inspect.frames` (as many frames as times, each a decoded picture of the sheet's size) and `output.video.inspect.content` (at least one frame is not a single flat colour). A claim that a video is done is judged on what the frames show, not on a tool's exit status.

**E. Speech-to-text.** `audio.transcribe {artifact_id, request_id, language?, wait_ms?}` sends an `audio` Artifact (at most `transcription.max_audio_bytes`, 25 MiB) through the model gateway's `transcription` capability (`config/models.yaml` names the route; the provider credential exists only in the gateway and never reaches the worker or the model). The call is **submit and poll**: the first call for a `request_id` submits and waits at most `wait_ms`; while the job is open the result is `{status: "pending", job_id, poll_after_ms}` and a later call with the same `request_id` polls it; a finished call creates a **transcript** Artifact (`kind: other`, `application/vnd.quansio.transcript+json`, role `transcript:<request_id>`): `{schema_version: "v1", trust: "UNTRUSTED_EXTERNAL", source {artifact_id, version_id, content_digest}, language, duration_ms, segments[] ({index, start_ms, end_ms, text})}` with segments ordered, `start_ms < end_ms`, inside the duration and not overlapping by more than `transcription.segment_overlap_ms`. A transcript is data, not intent (§12 rule 8): its text never becomes an instruction, a tool, a capability or a subtitle the person did not ask for. Idempotency is by `request_id`: a second call after the transcript exists returns it with `replayed: true`, and the same id with a different audio version or language is `CONFLICT_STATE`. With no composed gateway route the Tool fails `ADAPTER_UNAVAILABLE` with `details {adapter: "transcription"}`.

**F. Web-dev preview.** A **preview scope** binds `{tenant, workspace, session (run), resource (a directory relative to a workspace root), root generation}` and a random capability token (24 bytes, hex) and lives at most `preview.ttl_ms` (5 minutes) from its last renewal, at most `preview.max_lifetime_ms` in all; `webdev.preview_renew` extends it, `webdev.preview_close` ends it, and nothing about it survives a restart. It is served on loopback only (`127.0.0.1`, the egress policy is not widened and nothing is proxied off the machine) at `/p/<token>/…`, GET and HEAD for files, and `POST /p/<token>/__quansio/suggest` for suggestions. **Authority is re-resolved on every request**: the scope's workspace, session, acting member and root generation are read again through the `PreviewAuthority` port and the file path is re-proved inside the resolved root (no `..`, no link leaving the root, no dotfile, regular files only); a scope whose run ended, whose member was removed or suspended, whose workspace or target is gone, or whose workspace roots changed (the root's path or generation) is closed at that request and answers `PREVIEW_SCOPE_LAPSED` with `details.reason` of `expired|closed|session_ended|member_removed|workspace_changed|roots_changed|unknown`, and every later request answers the same. A token that was never issued answers `NOT_FOUND`, not `PREVIEW_SCOPE_LAPSED`, so the scope space cannot be probed. Responses carry `Cache-Control: no-store`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer` and a sandboxing `Content-Security-Policy` (`preview.csp`). A suggestion (`{kind: text|style|comment, selector, before?, after?, note?}`, at most `preview.suggestion_max_bytes`, at most `preview.max_suggestions` per scope) is appended to the scope's suggestions Artifact (`kind: data`, role `preview_suggestions:<token digest prefix>`) by the authority on the acting member's behalf; `webdev.preview_suggestions {scope_id}` returns the newest that fit the result's bound (`omitted` counts the older ones left out) with `trust: "UNTRUSTED_EXTERNAL"` and each value as data. The `scope_id` is `pvs_` and the first 16 hex digits of the token's SHA-256, so a tool argument never carries the capability; only the opening member (or, without one, the opening run) of the same workspace may renew, close or read a scope (`SCOPE_FORBIDDEN` otherwise), opening a directory that already has a live scope for the same binding returns that scope, and a lapsed scope's `PREVIEW_SCOPE_LAPSED` details carry `scope_id` and `suggestions_role` so what the person suggested stays readable as an Artifact. A full in-iframe edit runtime (an injected script that finds elements and sends suggestions) and a proxy to a running dev server are not part of this contract.

**G. Errors.** `DECK_NOT_DELIVERABLE` (409, B), `ADAPTER_UNAVAILABLE` (503, C, D, E), `PREVIEW_SCOPE_LAPSED` (403, F); a stale revision is `CONFLICT_REVISION`, a bound is `VALIDATION_BOUNDS`, an unknown field `VALIDATION_UNKNOWN_FIELD`. Effects: every output Tool but `deck.deliver` and `webdev.preview_suggestions` settles as `content.upload` (tier 1, idempotent by digest) — the version it adds; those two are `read.internal`; the preview's file reads are `read.internal` and are not journaled.

---

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

Recorded skills (ADP-003): a version submitted from a Recording (§11.7) is `DRAFT` with `provenance` `{origin: recording, recording_id, artifact_id, artifact_version_id, content_digest, slots[] ({slot_id, classification}), validator {rule_set, passed}}`. It climbs the same ladder under the same `PromoteSkillVersion` (`ManageTenant`); the person who recorded it holds no extra promotion right. The manifest vocabulary gains one key, `procedure` (§11.7 D): data the resolver reads, never a grant.

### 11.6 Business Capability Pack
`Pack: id, tenant_id, name, current_published_version_id`. `PackVersion: id, semver, contents {knowledge_ids[], skill_version_ids[], tool_names[], connector_requirements[], policy_requirements[], rbac_requirements[], approval_requirements[], workflow_templates[] (WorkGraph templates), examples[], eval_suite_id, compatibility}, provenance, evidence_requirements, status (draft|candidate|qualifying|qualified|published|deprecated|withdrawn)`. Execution resolves to WorkGraph + Skills + Tools + Knowledge + Policy; a pack cannot widen caller authority.

### 11.7 Skill recording (ADP-003) — normative

A person demonstrates a workflow once; the platform keeps what it observed as evidence of intent and offers a reviewable `DRAFT` skill compiled from it. Limits, the redaction rule-set version and the tool-path preferences are `config/recording.yaml` (D-018), validated by `schemas/json/recording-config.schema.json`. A service principal never records (`Recording` is an ungrantable command family, `config/identity.yaml`).

`Recording: id (rec_), tenant_id, workspace_id, owner_user_id, session_family_id, epoch, status (recording|finished|compiled|submitted|cancelled|aborted), start_key, started_at, expires_at, finished_at?, artifact_id?, artifact_version_id?, content_digest?, size_bytes?, redaction? {rule_set, counts{api_key, bearer_token, card_number, verification_code, password_field}, screenshots_withheld}, stats? {actions, screenshots, transcript_segments, paused_ms}, proposal? (§D), skill_version_id?, abort_reason?, created_at, updated_at`. The row is the recorder-session state, owned by `crates/server/src/control/skills/recording`; the archive's bytes are an Artifact (§10.1: `kind=archive`, `role=recording`, `origin={kind: upload, ref: rec_…}`, `media_type=application/vnd.quansio.recording+json`) whose only grant is the owner's `write`, so no run, teammate or other member can read it through the artifact surface.

States and transitions: `recording → finished` (only `FinishRecording`), `recording → cancelled` (only `CancelRecording`), `recording → aborted` (an owner-fence failure or expiry), `finished → compiled` (`CompileRecording`; repeatable), `compiled → submitted` (`SubmitRecordingSkill`). Every other edge is `CONFLICT_STATE`. Each transition commits with its `recording.*` event in one transaction.

**A. The person starts it and the person finishes it.** `StartRecording {workspace_id, start_key, epoch}` needs a session user with `AuthorWork` in the workspace; it returns `{recording_id, epoch, expires_at, limits, rule_set}` and a repeated `start_key` returns the same recording. A user has at most one `recording` at a time (a second start with another key is `CONFLICT_STATE`). `FinishRecording {recording_id, epoch, archive {content_base64, content_digest}}` is the only transition to `finished`: it is refused for a service principal, for any actor that is not the owner and for anything that is not a command from the person's own session. No message, model output, tool result, notification or approval can finish a recording: the commands that can are not Tools (§14), the plane has no write path to them, and a thread's text never reaches the recording row. A recording not finished by `expires_at` is `aborted` (`expired`) the next time anything touches it (a lazy sweep: no timer, no daemon) and its buffered content, which never left the person's machine, is lost by design. `CompileRecording` and `SubmitRecordingSkill` on a `recording` are `RECORDING_NOT_FINISHED`.

**B. Owner and epoch fence.** Every Recording command and every recorder control request carries `owner {tenant_id, user_id}` (the tenant and user come from the authenticated session on the server, from the signed-in account on the desktop) and `epoch` (a positive integer the desktop advances whenever the signed-in account changes: sign-in, sign-out or switch). The call is accepted only when the user is the owner, the session family is the one that started the recording (a refresh rotation stays in its family; a sign-out and sign-in does not) and `epoch` equals the recording's. Otherwise the answer is `RECORDING_OWNER_CHANGED`, and a `recording` is moved to `aborted` (`owner_changed`) in the same transaction, so a later call by the original owner finds it aborted. The recorder (macOS) applies the same rule to its own control channel and, on a mismatch, discards everything it buffered. The recorder's control channel is a Unix-domain socket in a directory only the user can enter; it authenticates with a random one-time token delivered in a bootstrap file with mode `0600` that the recorder deletes as it reads it, and a second connection with the same token is refused. A start or finish request carries an id and a fingerprint of its arguments: repeating both returns the first answer, the same id with another fingerprint is refused.

**C. What is captured and what is removed before anything leaves.** The native recorder (`native/macos`; Windows is later, behind the same trait) captures, while the person demonstrates:
- an action log: for each action its kind (`click`, `double_click`, `right_click`, `type`, `key`, `scroll`, `focus`, `app_switch`, `menu_select`, `set_value`), time, the application's bundle id and name and the control's accessibility attributes (`ax_role`, `ax_subrole`, `ax_title`, `ax_identifier`, `ax_description`); a click whose control cannot be resolved is marked `target_unresolved` and is the only action that keeps a screen point, as evidence for the person's review and never as an instruction;
- bounded screenshots: at most `limits.max_screenshots`, no closer than `limits.min_screenshot_interval_ms`, none identical to the last kept (SHA-256), each at most `limits.max_screenshot_bytes`, and the text on each is recognised on the machine and kept (redacted, bounded) as `screen_text`;
- an optional transcript of the person's narration (`transcript[] {at, text}`), only when the person turned it on.
Capture pauses while secure event input is enabled or the focused control is a secure text field (`paused[]` records the interval and the reason, never what was typed); text typed into a secure field, or into a control whose title or identifier names a password, is dropped entirely and counted as `password_field`.

Redaction runs in the recorder before the archive is built, again on the server before an archive is stored, and again in the plane before a compiler reads it. Rule set `v1` removes, replacing each with `[REDACTED:<category>]`: `api_key` (provider-shaped keys and tokens, JSON web tokens, private-key blocks), `bearer_token` (`Bearer …` and `Authorization:` values), `card_number` (13–19 digits, with single spaces or dashes between groups), `verification_code` (4–8 digits that follow a word such as code, otp, passcode, verification, 2fa or one-time, and any text typed into a control so named) and `password_field`. A screenshot whose recognised text matches any category is withheld whole (`screenshots_withheld`), because pixels cannot be edited in place. The `redaction` summary holds counts, never values. Redaction is idempotent and the three implementations agree on one corpus (`tests/contract/fixtures/recording/redaction_corpus.json`). The server refuses an archive in which any text field still matches (`RECORDING_UNREDACTED`, the details name the categories and JSON pointers, never the matched text) and stores nothing; a recording therefore never reaches a model, an index or a log unredacted, and the screenshots' bytes never reach the plane at all.

**D. Compilation is evidence of intent, not a replay.** `CompileRecording {recording_id, epoch}` (owner, `finished` or `compiled`) hands the plane a text-only projection of the archive (actions, transcript, screen text, window titles, the tools the Tool Registry offers) through `CompileSkillRecording`; the plane proposes and the server validates and keeps the proposal on the row. A proposal is `{schema_version, trust {source: recording, level: UNTRUSTED_EXTERNAL, recording_id, injection_suspected[]}, manifest, slots[], questions[]}` where the manifest is a §11.5 manifest whose `procedure` is a closed document: `{schema_version: "v1", platform, apps[] ({bundle_id, name}), inputs[] ({slot_id, name, description, type}), preconditions[] ({kind, bundle_id?, slot_id?, description}), steps[] ({id, intent, path: tool|ui, tools[]?, ui? {bundle_id, ax_role, ax_subrole?, ax_title?, ax_identifier?, ax_description?, action: press|set_value|focus|select|open_menu|key, slot_id?, key?}, provenance {action_seqs[]}}), postconditions[] ({kind, description, slot_id?, bundle_id?}), recovery[] ({on, action: reobserve_then_ask|stop_and_ask|reconcile_by_observation, guidance})}`. Rules the validator enforces in the plane and again on the server:
- **No coordinates, no element indexes.** An application is its bundle id and a control is its accessibility attributes. A key named `x`, `y`, `point`, `position`, `coordinates`, `frame`, `bounds`, `index`, `element_index`, `ref`, `nth` or `offset`, a value that reads as a screen point, or any key outside the closed vocabulary fails the proposal (`RECORDING_PROPOSAL_REJECTED`).
- **Slots, not baked values.** Every value the person typed or chose becomes an input slot with a proposed name and type and no classification. `SubmitRecordingSkill` needs a decision for every slot (`input`: the skill asks for it at run time; `constant`: the value from the recording becomes part of the skill, after the same redaction scan); an undecided slot is `VALIDATION_SCHEMA`, and an `input` slot's example never enters the manifest.
- **Preconditions, postconditions, recovery.** Applications present, slots provided; the last control and each typed value read back; and recovery that re-observes once and then asks a person, or reconciles an unknown outcome by observation, and never repeats a consequential action blind.
- **Tools before pixels.** Where `config/recording.yaml: tool_paths` says an application has an equivalent in the Tool Registry (a connector family, the managed browser, the file or terminal tools) and the registry offers it, the step is `path: tool`; only otherwise is it `path: ui`.
- **Nothing the recording says is intent.** Window titles, screen text, the transcript and control titles are `UNTRUSTED_EXTERNAL` data (§12 rule 7): they are never copied into instructions, never add a tool or a capability, and text that the deterministic injection heuristics flag is listed in `trust.injection_suspected` and replaced in the manifest. `tool_needs` are names that exist in the Tool Registry and `capability_needs` are the effect classes of those tools; a skill declares needs and grants nothing (§11.5).

**E. Into the registry as DRAFT.** `SubmitRecordingSkill {recording_id, epoch, name, semver, slots[] ({slot_id, classification: input|constant})}` (owner, `compiled`, `AuthorWork`) creates one skill (`scope: workspace`, owner the person) with one `DRAFT` version through `SkillStore` and records `skill_version_id`; the plane never writes the registry. A name already registered is `CONFLICT_STATE`. Promotion of that version is `PromoteSkillVersion` (`ManageTenant`) along the §11.5 ladder: `DRAFT` does not resolve, and a submission cannot start anywhere but `DRAFT`.

**F. The person's surface.** The desktop shows an indicator that a recording is capturing in every window until it finishes or is cancelled, a finish control only a person's own click can operate, the redaction summary, the proposed slots with their classification choices and the proposal before submission. Starting needs Accessibility and Input Monitoring (and Screen Recording when screenshots are on, Microphone when narration is); a missing one is the §8.6.1 E guide, `PermissionStatus` and deep link included, and nothing is captured until every named permission is `granted`. Projections: `GET /v1/recordings` and `GET /v1/recordings/{id}` return the caller's own recordings with their state, summary and proposal, never the archive.

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
6. Browser results (ADP-001, §8.4.1) are `UNTRUSTED_EXTERNAL` in full: the declaration's `source_trust`, the `trust` label on the worker's reply and the Tool's schema-validated output agree, and a dialog message, console line, element name or download name is data even when it reads as an instruction. Typing a secret never needs page content: the placeholder form is the only form the model sees or the ledger digests.
7. A Recording (ADP-003, §11.7) is `UNTRUSTED_EXTERNAL` in full. Its window titles, control titles, recognised screen text and transcript are data even when they read as instructions; a compiler that reads them is an untrusted-input reader with no effect path, and what it proposes is labelled with that origin, validated against the Tool Registry, and enters the registry as `DRAFT` only.
8. A transcript (ADP-006, §10.4 E), a preview suggestion (§10.4 F) and the text a render sandbox read from a page are `UNTRUSTED_EXTERNAL` in full: data even when it reads as an instruction. They are carried in schema-validated results and Artifacts with the label, never copied into instructions, and cannot add a tool, a capability or a subtitle nobody asked for.

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

### 13.6 Desktop shell (ADP-004) — normative

The desktop's native surfaces (application menu, tray, shortcuts, keep-awake, quit confirmation, connectivity, secondary windows, update deferral) are projection and command surface only (D-003): nothing below moves authority out of the server, and the only server change is A. Limits and defaults are `config/desktop-shell.yaml` (D-018), validated by `schemas/json/desktop-shell-config.schema.json`; the person's own choices (shortcut bindings, keep-awake, indicator position) are a preferences file under the user's app data beside the session file, not a store the server reads (D-019).

**A. Maintenance status.** `GET /v1/maintenance[?workspace_id=ws_…]` (any authenticated member; tenant-scoped, narrowed to a workspace when named) answers `{idle, active_turns, inflight_actions, open_terminals, workspace_id|null, as_of}`. `active_turns` counts Turns with status `active` whose Run is `RUNNING` or `VERIFYING` (the Run store's count: a Run is `RUNNING` between Turns and a Run that waits holds no computation, so neither counts). `inflight_actions` counts effects `RESERVED`, `DISPATCHED` or `RECONCILING` (the Effect Ledger's count; `OUTCOME_UNKNOWN` is unresolved but nothing executes it, and a restart changes nothing about it). `open_terminals` counts terminal sessions with status `open` (the machine's count) and is informational: a session may stay open for days, so `idle` is `active_turns == 0 && inflight_actions == 0`. The projection is derived on every read from existing rows; it keeps no state and blocks nothing.

**B. Window types and IPC trust.** Every window the desktop opens is created by the main process with the one sandboxed preload and the baseline `webPreferences` (sandbox, `contextIsolation`, no Node integration, `webSecurity` on, no webview), loads only the `app:` scheme and denies navigation and `window.open`. The main process keeps a registry of the windows it created, keyed by `webContents`; a window it did not create is unmanaged. Every IPC invoke, every pull of queued events and every push is authorised in main before any handler runs: (1) the sender's frame URL passes the same trusted-URL check as navigation, otherwise the call is a logged no-op that answers nothing and the handler does not run; (2) the sender is a managed window and its **window type** is allowed that channel (`main`: every channel; `popout`: the conversation channels for its own thread; `quick_entry`: read the thread list and the teammate roster, post one message or create one thread, and close itself; `indicator`: its activity list, dismiss and open); (3) a `popout` names only its own thread, and an id it acts on (a question) is one its own reads returned. A call refused under (2) or (3) answers `{ok: false, error: {code: "SCOPE_FORBIDDEN"}}`; none of the three reaches the server. Launch data for a secondary window (`{thread_id, tab, sidebar}` for a pop-out) is validated against a schema in main before the window is created and is read back by the window through its own IPC call; a deep link or a notification click whose window has not yet subscribed is queued per window (at most 64, dropped oldest first, cleared when the window is destroyed) and pulled with `shell.pending.consume`. A raw credential is never sent to any window, including through a synchronous IPC call.

**C. Activity items and their order.** The indicator window and the main window list `{key, kind, ref, since}` items derived from durable events: `approval.requested` (`needs_approval`, until `approval.granted|denied|expired|superseded`), `question.asked` (`needs_answer`, until another `question.*` event for it), `turn.started` (`running`, until `turn.completed|aborted`: Turn state, never Run status, because a Run stays `RUNNING` between Turns), `run.failed` (`failed`), `run.blocked` (`blocked`) and `run.succeeded` (`completed`). Items are ordered **waiting-for-user** (`needs_approval`, `needs_answer`), **running**, **error** (`blocked`, `failed`), **completed**, each group oldest first; the kinds are the §13.3 attention kinds. A finished item stays until dismissed or until `activity.completed_ttl_ms` has passed; a dismissal is a client preference and never an event. Opening an item brings the main window forward and navigates it; it grants nothing.

**D. Shortcuts.** A binding is a lower-case string `[mod+][ctrl+][alt+][shift+]<key>` in exactly that modifier order, at most 64 characters, at least one modifier unless the key is a function key, with `<key>` a single character or a name from the closed key table. `mod` is Command on macOS and Control elsewhere and never appears together with `ctrl`. A new binding is refused with a typed result when it is invalid, when it equals another binding or a menu accelerator (`conflict`, naming it), or, for a global shortcut, when the operating system declines to register it (`registration_failed`): the previous binding stays in force and nothing is half-applied. Global shortcuts (`toggle_window`, `quick_entry`) are registered only from stored preferences; `quick_entry` has none until the person sets one and `toggle_window` starts at `global_shortcut.toggle_default`. While the person records a new binding, every global shortcut and menu accelerator is paused; recording ends on its own on blur, on navigation or when the renderer crashes, and the paused shortcuts are restored.

**E. Keep-awake and quitting.** A power-save blocker is held exactly while at least one Turn is active (the activity list has a `running` item) **and** the machine is on AC power **and** the person has not turned keep-awake off; it is released the moment any of the three stops being true. Quitting while a Turn is active or an effect is in flight asks first and names what is running; the wording says the work is paused and resumes from the server when the app is opened again (a quit does not destroy a Run), and the person chooses to quit or to stay. If the status cannot be read, quitting asks as if work were running.

**F. Connectivity.** The renderer reports `online`/`offline`. An offline report is held for `connectivity.offline_grace_ms` (10 s) before the shell treats the machine as offline, so a brief flap is absorbed; an online report cancels it. A return to online, a resume from sleep and a window focus after the shell was offline each call the existing stream reconnect, which resumes from the durable cursor (§9.3, backoff 250 ms to 8 s). There is no second reconnection path.

**G. Updates.** Installing a downloaded update (restart) is allowed without asking only when the maintenance status (A) says `idle`. When it says busy, or cannot be read, the person is asked, with the counts, and may defer; "install when idle" applies the update at the first idle reading and never while busy or unreadable. A failed install turns install-on-quit off. A build that is not signed reports `unavailable` as before.

### 13.7 Local files (ADP-005) — normative

The desktop gives a person's files to the product through narrow, window-bound, auditable doors, and gives the renderer no way to name a path (D-003, D-019). Everything below is the main process (`apps/desktop/src/main/local-files/`); a renderer sends opaque ids and never a path. Limits and timings are `config/local-files.yaml` (D-018), validated by `schemas/json/local-files-config.schema.json`. All state is in main's memory: a grant, a root, an entry id, a media token and an edit session do not survive a restart, and the server never reads any of it. Every IPC payload is validated against a schema (`main/schema.ts`) before a handler runs, and every channel is authorised per window type (§13.6 B): a pop-out may use the grant channels for its own thread only; everything else is `main` only. A sender that fails the §13.6 B checks gets the same logged no-op as any other channel.

**A. One-time file grants.** A deep link or a button can only ask main to open a **native** open dialog (`quansio://attach[/<thr_…>]` and the composer's attach control are the same request; one dialog per window at a time, a second request while it is open is dropped). The person's picks become a **file grant**: a random 32-byte token (64 hex), bound to the requesting window's `webContents`, **single use**, valid `local_files.grant_ttl_ms` (60 s), at most `grant_max_live` (32) live grants, `grant_max_files` (20) files, `grant_max_file_bytes` (50 MiB) per file and `grant_max_total_bytes` (100 MiB) in all. Each file is opened by file descriptor at the moment of the grant with `O_NOFOLLOW`, must be a regular file (a directory, a device, a FIFO and a symbolic link are refused with `FILE_REFUSED`) and must not be inside the shell's own data directory (the session and its keys; `FILE_REFUSED` `root_not_allowed`), and is read from that descriptor, so replacing the path after the pick changes nothing the grant holds. `claim(token, window)` deletes the grant on its first call, returns the bytes to the attach pipeline, which sends each file through the existing upload (`CreateArtifact`, the server's own size bound applies and a file over it is reported as skipped), and closes the descriptors. A claim from another window, a second claim, an expired or unknown token and a claim after the window closed all answer `FILE_GRANT_INVALID`; only an expired token says so (`details.reason: expired`), the rest say `invalid`, so a window cannot probe another's tokens. A grant is revoked, with its descriptors, when it is claimed, discarded, expired or its window is destroyed. A deep link that carries a query, a fragment or any path-like parameter (`path`, `paths`, `file`, `files`, `localFilePaths`, a `file:` URL) is refused whole by `parseDeepLink` (APP-003): a link supplies no path and no token.

**B. Granted roots and entries.** A **granted root** is a folder picked in a native folder dialog, kept by main under a random root id for that window until released or the window closes (at most `roots_max`). The filesystem root, the person's home folder itself and any folder that is, contains or sits inside the shell's own data directory are refused (`FILE_REFUSED` `root_not_allowed`); the root is stored by `realpath`. The renderer lists a root with `files.root.entries`, which returns `{entryId, name, kind (file|directory), sizeBytes, mtimeMs}`; an **entry id** is an opaque random id (at most `entries_max` per window, oldest forgotten first) that main maps to `(root, relative path)`. No channel accepts a path: reading, editing, previewing and watching take an entry id. A listing returns at most `watch_max_entries` entries and says when it was cut short. Symbolic links are listed as nothing and are never followed. Every resolution re-checks containment on the real path at the moment of use (`realpath` of the target is inside the root's `realpath`, the final element is not a symbolic link): a path that leaves its root answers `FILE_OUTSIDE_ROOTS`, an entry id the window was never given answers the same. A root, its entries, its edit sessions, its media tokens and its watches die with its window.

**C. Media URLs.** `files.media.url {entryId}` and `files.grant.preview {token, index}` return a URL `quansio-media://m/<capability token>/<name>` for the previewer. The token (32 random bytes) is bound to the requesting window and one file, up to `media_max_tokens` per window. Main serves it through a custom protocol: only `GET` and `HEAD` (others `405`), a single `Range` (`416` for an unsatisfiable or multiple range), `Cache-Control: no-store`, `Content-Security-Policy: sandbox`, `X-Content-Type-Options: nosniff`, and a content type from a closed table of image, audio, video, PDF and plain-text types, with every other type, HTML and SVG included, served as `application/octet-stream`. The path is re-resolved and re-checked for containment on each request (a grant file is read from its descriptor); an unknown or revoked token is `404` with no body. Every request is attributed to the window that sent it (`session.webRequest.onBeforeRequest` names the `webContents` before the handler runs): a request from another window is cancelled before any file is touched, a request the shell cannot attribute (the main process itself, for one) is refused, and the page cannot read the bytes with `fetch` (another scheme, no CORS), only embed them. A token is revoked when its window is destroyed, its grant is revoked (claimed, discarded, expired) or its root is released, and the page's CSP allows the scheme for `img-src` and `media-src` only; a media response's own policy is `sandbox` and the page's policy never replaces it.

**D. Safe local edits.** `files.edit.open {entryId}` reads a UTF-8 text file (at most `edit_max_bytes`, no NUL byte) through a descriptor and records the **edit baseline** `{size_bytes, mtime_ns}` from that descriptor's `fstat`, with the file mode, in an edit session main holds (the renderer never supplies a baseline). `files.edit.save {editId, text, onConflict}` writes the new bytes to a temporary file in the target's directory (created exclusively, mode and permissions of the original, `fsync`ed), **then** re-reads the target's size and modification time and compares them with the baseline; any difference, or a target that is gone or is now a symbolic link, deletes the temporary file, leaves the target untouched and answers `FILE_EDIT_CONFLICT` with `details {baseline {size_bytes, mtime_ns}, current {size_bytes, mtime_ns} | null}` (both versions' metadata). Otherwise the temporary file is renamed over the target atomically, and the new stat becomes the session's baseline. With `onConflict: "overwrite"` the version on disk is first copied to a sibling backup created exclusively (`<name>.quansio-backup-<n>`; an existing name is never replaced), the baseline is replaced by that version's metadata and the same check runs again. A write registers a **self-write** (path, new size and mtime, expiring after `self_write_ttl_ms`) with the watcher, so the change it causes is not reported back. The check cannot be made atomic with the rename on a POSIX filesystem; it narrows the window to the instants between the re-read and the rename, and a conflict the check cannot see is a change in those instants or one that leaves both the size and the modification time as they were (a filesystem with coarse timestamps makes that likelier). Agent writes keep the `qworkerd` path (§8.7 G).

**E. Workspace file watcher.** `files.watch.subscribe {rootId, entryId?}` watches one directory of a granted root, non-recursively. One operating-system watch is kept per `(device, inode)` of a directory however many subscribers it has, at most `watch_max_dirs` in all and `watch_max_subscribers` per window; a directory with more than `watch_max_entries` entries is refused. Raw events are coalesced per directory and delivered `watch_debounce_ms` (300 ms) after the last one (at most ten debounce intervals after the first, so a directory that never goes quiet is still reported) as typed changes `{kind: add|change|unlink, name, entryId|null, directory}` on the push channel `files.changes` (`entryId` is minted for a file or directory that still exists), found by comparing the directory with the last one reported, so a self-write (D) and an event that changes nothing are silent. If the directory's `(device, inode)` changes or it disappears (checked at every report and on a timer of ten debounce intervals, so a replacement that raises no event is still found), or the operating-system watch fails, the watch is dropped and each subscriber gets `{dropped: "directory_replaced" | "directory_removed"}`. Each subscriber's queue is bounded by `watch_queue_max`; past it the queue is replaced by one `{overflow: true}` signal and the page re-lists. A path outside the roots cannot be named, so it cannot be watched (`FILE_OUTSIDE_ROOTS`).

**F. Errors.** `FILE_GRANT_INVALID` (A), `FILE_REFUSED` (A, B, D, E: `details.reason` names the rule), `FILE_OUTSIDE_ROOTS` (B), `FILE_EDIT_CONFLICT` (D, §8.7 G); the worktree codes are §8.7. They are answered as data (`{ok: false, error}`), the shape the shell's channels already use. None carries a path; a name is only the final component of a file the person picked or listed.

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
| Browser/computer | `RequestTakeover`, `Handback`, `ImportBrowserSession`, `GrantComputerTier`, `ResolveBrowserCertificate`, `DecideBrowserPermission` |
| Artifacts | `CreateArtifact` (upload init/complete), `AddArtifactVersion`, `GrantArtifactAccess`, `DeleteArtifact` |
| Knowledge & memory | `IngestSource`, `DeleteKnowledge`, `SupersedeKnowledge`, `CreateMemory`, `DeleteMemory` |
| Skills & packs | `InstallSkill`, `PromoteSkillVersion`, `DisableSkill`, `ProposeSkillCandidates`, `SubmitPack`, `QualifyPack`, `PublishPack`, `RollbackPack` |
| Recording | `StartRecording`, `FinishRecording`, `CancelRecording`, `CompileRecording`, `SubmitRecordingSkill` |
| Routines | `CreateRoutine`, `UpdateRoutine`, `PauseRoutine`, `ResumeRoutine`, `TriggerRoutineNow`, `DeleteRoutine` |
| Connectors | `BeginConnectorAuth`, `CompleteConnectorAuth`, `RevokeConnector`, `CreateWebhookSubscription`, `UpdateWebhookSubscription`, `RedeliverWebhook`, `DeleteWebhookSubscription` |
| Notifications | `UpdateNotificationPreferences`, `MarkNotificationRead` |
| Admin/ops | `SetPolicy`, `SetBudget`, `FreezeEffects`, `UnfreezeEffects`, `DisableProvider`, `EnableProvider`, `QuarantineWorker`, `DrainTarget`, `ExportTenantData`, `DeleteUserData`, `EngageKillSwitch`, `ReleaseKillSwitch` |
| Read projections | `GET /v1/threads/{id}`, `/messages`, `/workgraph`, `/runs/{id}`, `/runs/{id}/timeline`, `/effects`, `/approvals`, `/questions`, `/artifacts`, `/artifacts/{id}/versions`, `/artifacts/{id}/versions/{vid}/content`, `/evidence/{id}`, `/targets`, `/browser-sessions/{id}`, `/knowledge`, `/memory`, `/skills`, `/recordings`, `/recordings/{id}`, `/packs`, `/routines`, `/connectors`, `/members`, `/service-principals`, `/sso`, `/policies`, `/teammates`, `/notifications`, `/usage`, `/audit`, `/search?q=` (typed SearchProgram), `/webhooks`, `/webhooks/{id}/deliveries`, `/maintenance` (any member; Turns, effects and terminals a restart would interrupt, §13.6 A), `/ops/switches` (tenant administrators; active kill switches and recent history), `/diagnostics/bundle` (tenant administrators; the tenant's recent redacted log lines, bounded), `GET /v1/stream` (WS), `GET /v1/stream/events` (SSE) |

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
| Browser (409/422) | `BROWSER_STALE_PAGE`, `BROWSER_STALE_DIALOG`, `BROWSER_DIALOG_PENDING`, `BROWSER_ACTION_REPLAY_CONFLICT`, `BROWSER_ACTION_IN_DOUBT`, `BROWSER_SECRET_ORIGIN_MISMATCH`, `BROWSER_SECRET_UNAVAILABLE`, `BROWSER_SENSITIVE_INPUT_DENIED`, `BROWSER_NAVIGATION_DENIED`, `BROWSER_CERTIFICATE_DECISION_REQUIRED`, `BROWSER_GESTURE_REQUIRED`, `BROWSER_INPUT_FENCED`, `BROWSER_EFFECT_TIER_EXCEEDED`, `BROWSER_INVALID_REQUEST`, `BROWSER_TARGET_NOT_FOUND`, `BROWSER_PAGE_FAILURE` |
| Computer (409/422) | `COMPUTER_TAKEOVER_ACTIVE`, `COMPUTER_PERMISSION_REVOKED`, `COMPUTER_LIMIT_EXCEEDED`, `COMPUTER_STOPPED_BY_USER`, `COMPUTER_PERMISSION_REQUIRED`, `COMPUTER_FOREGROUND_CONTROL_LAPSED`, `COMPUTER_HELPER_IDENTITY_REFUSED` |
| Recording (409/422) | `RECORDING_OWNER_CHANGED`, `RECORDING_NOT_FINISHED`, `RECORDING_UNREDACTED`, `RECORDING_PROPOSAL_REJECTED` |
| Files (403/409/422) | `FILE_GRANT_INVALID`, `FILE_OUTSIDE_ROOTS`, `FILE_EDIT_CONFLICT`, `FILE_REFUSED`, `WORKTREE_TOKEN_INVALID`, `WORKTREE_MISSING` |
| Output (403/409/503) | `DECK_NOT_DELIVERABLE`, `ADAPTER_UNAVAILABLE`, `PREVIEW_SCOPE_LAPSED` |
| Model (502/503) | `PROVIDER_UNAVAILABLE`, `PROVIDER_RATE_LIMITED`, `PROVIDER_REFUSAL`, `ROUTE_UNAVAILABLE`, `DLP_DENIED` |
| Generic | `NOT_FOUND` (404), `RATE_LIMITED` (429), `STREAM_BACKPRESSURE` (WS close), `INTERNAL` (500) |

**Browser error flags (ADP-001).** A `BROWSER_*` error is a Tool error (§7.4 `ToolResult`), returned with `{code, message, retryable, requires_new_observation, details?}`. `retryable` means the *same request* may be sent again once the stated condition clears — it is never true for an unknown outcome. `requires_new_observation` means the model must `browser.observe` again before acting on the page. These flags belong to the Tool error; the public API's own `retryable` (above) is about whether the same *call to the server* could succeed unchanged, which for a `BROWSER_*` code it never does, so an `ApiError` carrying one is `retryable: false`.

| Code | retryable | requires_new_observation | When |
|---|---|---|---|
| `BROWSER_STALE_PAGE` | `true` | `true` | the revision, snapshot, capture, fingerprint or hit test no longer matches the page |
| `BROWSER_STALE_DIALOG` | `true` | `true` | the dialog id or revision is not the pending dialog |
| `BROWSER_DIALOG_PENDING` | `true` | `false` | a dialog is pending; answer it with `browser.handle_dialog` first |
| `BROWSER_ACTION_REPLAY_CONFLICT` | `false` | `false` | an action id was reused with different arguments |
| `BROWSER_ACTION_IN_DOUBT` | `false` | `true` | input may have reached the page and the outcome cannot be established; settles `OUTCOME_UNKNOWN` |
| `BROWSER_SECRET_ORIGIN_MISMATCH` | `false` | `false` | the top or target frame origin is outside the secret's allowed origins |
| `BROWSER_SECRET_UNAVAILABLE` | `false` | `false` | a placeholder cannot be resolved (no broker, no such secret or field) |
| `BROWSER_SENSITIVE_INPUT_DENIED` | `false` | `false` | plain text or a printable key aimed at a password, one-time-code or card field |
| `BROWSER_NAVIGATION_DENIED` | `false` | `false` | the destination is not `http`/`https`/`about:blank`, or embeds credentials |
| `BROWSER_CERTIFICATE_DECISION_REQUIRED` | `false` | `false` | a certificate error waits for a decision only a user can make |
| `BROWSER_GESTURE_REQUIRED` | `false` | `false` | the action needs a genuine user gesture; agent input is not one |
| `BROWSER_INPUT_FENCED` | `false` | `true` | a user holds the session |
| `BROWSER_EFFECT_TIER_EXCEEDED` | `false` | `false` | the classified consequence is above the approved tier |
| `BROWSER_INVALID_REQUEST` | `false` | `false` | unknown field, missing target, malformed placeholder or key |
| `BROWSER_TARGET_NOT_FOUND` | `true` | `true` | the tab, ref or selector does not exist |
| `BROWSER_PAGE_FAILURE` | `false` | `false` | the page or browser failed with a known outcome |

**Computer error flags (ADP-002).** A `COMPUTER_*` error is a Tool error with `{code, message, retryable, requires_new_observation, requires, details?}` (§8.6.1). The four terminal conditions end a sequence (§8.6.1 B). `retryable` means the same request may be sent again once `requires` has happened; the model must observe again first when `requires_new_observation` is true.

| Code | retryable | requires_new_observation | requires | When |
|---|---|---|---|---|
| `COMPUTER_TAKEOVER_ACTIVE` | `true` | `true` | `handback` | a person holds, or has asked to hold, the computer (§8.6) |
| `COMPUTER_PERMISSION_REVOKED` | `true` | `true` | `permission_granted` | an OS permission the sequence had was withdrawn mid-sequence |
| `COMPUTER_LIMIT_EXCEEDED` | `false` | `true` | `smaller_sequence` | the sequence passed its action, wall-time or typed-byte limit |
| `COMPUTER_STOPPED_BY_USER` | `false` | `true` | `user_resume` | the person stopped the run |
| `COMPUTER_PERMISSION_REQUIRED` | `true` | `false` | `permission_granted` | a permission is missing at the first action; the run parks (§8.6.1 E) |
| `COMPUTER_FOREGROUND_CONTROL_LAPSED` | `true` | `true` | `input_paused` | the person is using the keyboard or mouse; the run parks and asks them to pause |
| `COMPUTER_HELPER_IDENTITY_REFUSED` | `false` | `false` | `none` | the helper's signature, bundle id, team id or hardened runtime does not match the configured pin |

**Recording errors (ADP-003).** None is retryable as sent. `RECORDING_OWNER_CHANGED` (409): the call's user, session family or epoch is not the recording's (§11.7 B); a `recording` is aborted by it. `RECORDING_NOT_FINISHED` (409): compile or submit on a recording the person has not finished. `RECORDING_UNREDACTED` (422): an archive text field still matches a redaction rule; `details {categories[], pointers[]}`, never the matched text. `RECORDING_PROPOSAL_REJECTED` (422): the plane's proposal broke a §11.7 D rule; `details {rule}`.

**Output errors (ADP-006).** None is retryable as sent. `DECK_NOT_DELIVERABLE` (409): a deck page is pending, edited or failed its quality check, or the pages are for an older version of the deck; `details {pages[] ({index, state, failed_rules[]}), stale}` (§10.4 B). `ADAPTER_UNAVAILABLE` (503): the worker-side operation or the provider an output needs is not composed on this deployment (no render host, no `ffmpeg`, no transcription route); `details {adapter: render|ffmpeg|transcription, env?}`; nothing was produced and nothing is simulated. `PREVIEW_SCOPE_LAPSED` (403): the scope's authority no longer holds; `details {reason}` is one of `expired`, `closed`, `session_ended`, `member_removed`, `workspace_changed`, `roots_changed`, `unknown` (§10.4 F).

**File errors (ADP-005).** None is retryable as sent; none carries a path. `FILE_GRANT_INVALID` (403): the token is unknown, already claimed, another window's, or expired (`details.reason` is `expired` or `invalid`, §13.7 A). `FILE_OUTSIDE_ROOTS` (403): the entry, root or path is not one the window was granted (§13.7 B, E). `FILE_EDIT_CONFLICT` (409): the file changed after it was read; `details {baseline, current}` for a person's edit (§13.7 D), `{expected_digest, actual_digest}` for an agent's `fs.write`/`fs.patch` (§8.7 G); the target is untouched. `FILE_REFUSED` (422): a picked or listed file breaks a rule; `details {reason}` is one of `not_regular`, `unreadable`, `symlink`, `too_large`, `too_many`, `total_too_large`, `too_many_entries`, `not_text`, `root_not_allowed`, `limit`. `WORKTREE_TOKEN_INVALID` (409): `details {reason: unknown|expired|set_mismatch}` (§8.7 B). `WORKTREE_MISSING` (409): `details {id, state, recovery_record}` (§8.7 F).

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
