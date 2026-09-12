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
- Role semantics: `approver` can approve effects within the workspace; `editor` can create work and messages; `viewer` reads; `admin` manages members, policies, connectors, teammates and packs; tenant `owner/admin` inherit workspace `admin` on all workspaces; `billing` sees usage only.
- Baseline authentication (self-serve): passwordless email link + OAuth sign-in (Google, Microsoft, GitHub). Sessions: short-lived access token (ES256 JWT, ≤15 min) + opaque rotating refresh token (stored in OS keychain on desktop, httpOnly cookie on web). Enterprise SSO/SCIM is OPS-001. API keys for service principals are hashed at rest (argon2id) and prefixed `qsk_`.

---

## 3. Conversation model

### 3.1 Thread
`id, workspace_id, kind (direct|group|objective), title, participants[] ({kind: user|teammate|worker, id, role: owner|member, joined_at, left_at}), objective_id?, archived_at, last_activity_at`.

### 3.2 Message
`id, thread_id, seq (thread-monotonic), author {kind: user|agent|system, id}, run_id?, turn_id?, content_blocks[], attachments[] (artifact ids), reply_to?, reactions[] ({emoji, user_id}), mentions[], trust_level (see §12), created_at, edited_at?, deleted_at?`.

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
`id, workspace_id, from_node_id, to_node_id, kind (depends_on|parent_of|produces_artifact|verified_by|blocked_by), revision`. Graph MUST be acyclic for `depends_on`/`parent_of`.

### 4.3 AgentGraph edge
`parent_agent_thread_id → child_agent_thread_id` with `work_node_id, delegation_capability_id (cap_), delegated_at, joined_at?`. Child capability MUST be a narrowing of parent capability (§6).

### 4.4 CompletionContract
```json
{
  "deterministic_checks": [
    {"kind": "artifact_exists", "artifact_role": "report", "min_versions": 1},
    {"kind": "test_command", "command": "cargo test -p x", "expect_exit": 0},
    {"kind": "assertion", "expr": "typed predicate id", "args": {}},
    {"kind": "effects_settled", "effect_classes": ["message.send"]},
    {"kind": "citations_valid", "min_coverage": 0.95}
  ],
  "semantic_verification": {"required": false, "rubric_id": "...", "independent_model": true},
  "human_signoff_required": false
}
```
A WorkNode reaches `done` only via `run.verification_passed`, never via a model completion claim alone (RUN-008).

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

### 5.7 ProtocolState (CORE-006)
Per Run: `run_id, generation, pending_model_call?, pending_tool_calls[] (with dispatch tokens), pending_approvals[], open_questions[], waits[] ({kind, key, expires_at}), browser_control {session_id, holder: agent|user, since}, terminal_sessions[] ({id, cursor}), child_agent_threads[], cancellation {requested: bool, at}, last_compaction_epoch_id, updated_at`. Recovery reads this, RuntimeEvents, Checkpoints, Evidence and the Effect Ledger — never MemoryEntries.

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
- `Policy`: `id, scope {tenant|workspace}, rules[] ({effect_class, resource_selector, decision: allow|ask|deny, conditions: {trust_max, tier_max, data_classes[], time_window, sequence_guards[]}}), question_default_ttl, approval_default_ttl, max_plan_nodes, version`.
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
`name (namespaced: fs.read, fs.write, fs.patch, terminal.exec, process.spawn, browser.navigate, browser.click, browser.type, browser.extract, browser.screenshot, web.search, web.fetch, computer.click, computer.type, connector.<id>.<op>, scm.git.<op>, scm.pr.<op>, artifact.create, artifact.update, work.propose_plan, work.delegate, user.ask, memory.propose, knowledge.cite), version, description, input_schema (JSON Schema, additionalProperties=false), output_schema, effect_class (static or derived by function of args), resource_derivation, required_grant_template, idempotency_key_derivation, host (server|qworkerd|browser|adapter), max_output_bytes, evidence_capture (what is recorded), source_trust (trust level assigned to results), timeout_ms, cancellable`.

Tools are registered in the Tool Registry (control plane) and exposed to models only through CapabilityProjection filtering; a model never sees a tool it cannot use.

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

States: `REQUESTED → PROVISIONING → READY ⇄ BUSY; READY → DRAINING → STOPPED → (SNAPSHOTTED) → DESTROYED; any → FAILED (typed) → REPLACING → PROVISIONING`.

### 8.3 Lease
`id, target_id, holder {controller_id, generation}, acquired_at, expires_at, renewed_at, status (held|released|expired|revoked)`. qworkerd rejects any action whose `(lease_id, generation)` does not match its current lease.

### 8.4 BrowserSession
`id, target_id, run_id?, profile_ref, control_holder (agent|user|none), control_since, current_url, tabs[], screencast {enabled, stream_ref}, checkpoint_id?, status (active|paused_takeover|paused_policy|closed)`. Takeover: `RequestTakeover` → `control_holder=user` fences agent input; `Handback` → agent resumes. No second session is created on takeover (APP-007).

Understanding order for agents: DOM/CDP/network metadata → accessibility tree → screenshot/vision fallback (EXEC-009). Live view: CDP screencast frames as binary LiveFrames over the client stream; user input during takeover flows client → server → qworkerd → CDP input.

### 8.5 TerminalSession
`id, target_id, run_id?, pty_ref, cursor (durable byte offset), status, last_command_id`. Replay resumes from `cursor`; commands are effects with `process.exec.*` class.

---

## 9. Events and streaming

### 9.1 RuntimeEvent envelope (CORE-003)
`event_id, tenant_id, workspace_id?, sequence (tenant-monotonic), aggregate_type, aggregate_id, aggregate_version, type, schema_version, occurred_at, command_id?, correlation_id, causation_id?, actor {kind: user|agent|system|service, id}, generation?, payload (typed by `type`)`.

Written in the same PostgreSQL transaction as the mutation, plus an outbox row; the publisher relays to NATS JetStream subject `q.<tenant>.<aggregate_type>.<type>` with `Nats-Msg-Id = event_id` for dedup.

### 9.2 Event families (minimum)
`tenant.*`, `workspace.*`, `member.*`, `thread.*` (created, participant_added/removed, message_posted, reaction_added, attention_changed), `work.*` (node_created/updated, edge_added/removed, plan_proposed/applied/rejected, node_status_changed, revision_committed), `agent.*` (thread_provisioned, activated, suspended, delegated, handoff_started/completed, joined, terminated), `run.*` (created, queued, started, waiting, resumed, verifying, verification_passed/failed, succeeded, failed, cancelled, blocked, suspended), `turn.*`, `step.*`, `model.*` (call_started, call_completed, usage_recorded, route_selected, failover), `tool.*` (proposed, validated, rejected, dispatched, completed, failed, unknown), `effect.*` (one per state in §7.2), `approval.*` (requested, granted, denied, expired, superseded), `question.*`, `target.*`, `lease.*`, `browser.*` (session_opened, control_changed, checkpointed), `terminal.*`, `checkpoint.*`, `artifact.*` (created, version_added, grant_changed), `evidence.*`, `knowledge.*`, `memory.*`, `skill.*`, `pack.*`, `routine.*` (created, fired, skipped, paused), `notification.*`, `connector.*` (connected, degraded, revoked, webhook_received), `policy.*`, `audit.*`, `usage.*`, `capability.*` (projected, widening_rejected), `compaction.*`, `ops.*` (kill_switch_engaged/released, provider_disabled, worker_quarantined).

### 9.3 Client stream (CORE-009, APP-001)
- Transport: WebSocket `GET /v1/stream` (SSE fallback for web read-only).
- Subscribe: `{subscribe: [{channel: "workspace:ws_…" | "thread:thr_…" | "run:run_…" | "target:tgt_…"}], cursor?}`.
- Frames: `{kind: "event", cursor, event}` (durable, replayable) and `{kind: "live", channel, frame}` (transient: `model.delta`, `terminal.bytes`, `browser.frame`, `progress`). Live frames are never persisted and clients must tolerate their absence.
- Reconnect with last `cursor` replays missed events in order; slow consumers are disconnected with `STREAM_BACKPRESSURE` rather than buffered unboundedly.

---

## 10. Artifacts and evidence

### 10.1 Artifact / ArtifactVersion
`Artifact: id, workspace_id, kind (document|spreadsheet|presentation|code|data|image|audio|video|archive|other), title, role? (report|source|attachment|…), origin {kind: message|run|routine|upload|connector, ref}, current_version_id, grants[] ({principal, level: read|write}), retention {class, expires_at?}, deleted_at?`.
`ArtifactVersion: id, artifact_id, seq, content_digest (sha256), size_bytes, media_type, object_key, produced_by {run_id?, step_id?, user_id?}, parent_version_id?, created_at`.
Bytes are immutable per version; a new version is a new identity. Preview never executes active content with host privileges (APP-008).

### 10.2 Evidence
`id, workspace_id, run_id, step_id?, effect_id?, kind (tool_output|screenshot|dom_snapshot|log|test_result|http_exchange|diff|digest|external_ref), content_digest, object_key?, inline_summary (bounded), captured_at, captured_by {host kind, id}, redaction_applied: bool`. Immutable; never overwritten.

### 10.3 EvidenceBundle (CAP-002)
`id, run_id, artifact_version_id (the output), claims[] ({claim_id, text_span, evidence_ids[], support_score?}), sources[] ({source_id, uri|artifact_ref, retrieved_at, excerpt_locator, excerpt_digest}), coverage, verification {deterministic_passed, semantic_score?, verified_at}`.

---

## 11. Intelligence model

### 11.1 Model Gateway contract (INT-002/003)
`ModelCallRequest: call_id, run_id, turn_id, route_hint? (explicit user choice), capability_projection_id, context_projection_id, messages (rendered by Context plane; stable prefix first for caching), tools[] (filtered), output_constraints?, effort?, max_output_tokens, stream: true, dlp_profile, timeout_ms, cancellation_token`.

`ModelEvent (stream): call_started {route}, delta {text|tool_input_partial}, tool_call {id, name, args}, thinking_summary?, usage {input, output, cache_read, cache_write}, stop {reason: end_turn|tool_use|max_tokens|refusal|cancelled|error}, error {code, retryable}, call_completed {route, latency_ms, cost_estimate}`.

Provider adapters MUST normalize: streaming, tool/function calling with strict JSON schemas, system/operator instructions, multimodal image and document input, prompt-cache hints, usage reporting, stop reasons (including refusal), cancellation. Provider-side context compaction features MUST NOT be enabled; Quansio owns compaction (INT-008).

`ModelRoute: id, request_class (chat|planning|tool_heavy|synthesis|verification|embedding|cheap_worker), provider, model_id (from `config/models.yaml`, never hard-coded), config {effort, max_tokens, thinking}, dlp_profile, fallbacks[] (bounded), cost_class, chosen_by (rule id), selected_at`. Selection is deterministic for fixed inputs and performs no LLM call.

### 11.2 ContextProjection (INT-005)
`id, run_id, turn_id, segments[] ({seq, source {kind: system|policy|thread|work|tool_result|knowledge|memory|artifact|search, ref}, trust_level, token_estimate, snapshot_ref, redactions[]}), token_ledger {budget, used, by_source}, degradation[] (what was dropped and why), search_program_id?, policy_snapshot_digest, created_at`.

### 11.3 SearchProgram
Typed AST, no free-form predicates: `{channels: [exact|lexical|semantic|graph|history|memory], filters: typed, budget {max_results, max_tokens}, snapshot: index epoch, scope: tenant/workspace/target}`. Results carry provenance `(source_id, locator, snapshot)`.

### 11.4 KnowledgeEntry / MemoryEntry
`KnowledgeEntry: id, scope {tenant|workspace|pack}, kind, content_ref, provenance[] ({source_kind, ref, digest, retrieved_at}), confidence (0-1), version, status (candidate|verified|active|superseded|quarantined|deleted), superseded_by?, embedding_ref? (derived)`.
`MemoryEntry: id, scope {user|workspace|teammate}, subject_ref, content, provenance {kind: explicit_user|verified_run, ref}, confidence, status (candidate|active|deleted), last_used_at, expires_at?`.
Deleting a source quarantines derived knowledge; deleting memory removes it from retrieval after index refresh (bounded delay published in SLOs).

### 11.5 Skill / SkillVersion
`Skill: id, scope, name, owner, current_active_version_id`. `SkillVersion: id, skill_id, semver, manifest {instructions, examples, tool_needs[] (tool names), capability_needs[] (grant templates), eval_suite_id, compatibility, recovery_guidance}, provenance, status`.
States: `DRAFT → CANDIDATE → EVALUATING → APPROVED → ACTIVE → DEPRECATED → RETIRED; EVALUATING → REJECTED`. Only `ACTIVE` versions resolve into production context.

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
`id, workspace_id, owner_user_id, teammate_id, trigger {kind: cron|interval|event, spec, timezone}, objective_template (WorkNode template + CompletionContract), budget, notification_prefs, absence_policy (skip|queue|catch_up_once), status (active|paused|deleted), last_fired_at, next_due_at`. Firing creates one Objective + Run via the same command path as interactive work; timers are generation-fenced (CORE-008).

### 13.2 Budget
`id, scope {tenant|workspace|run|agent_thread}, limits {tokens, cost_minor_units, wall_time_ms, tool_calls, concurrency, machine_minutes, max_steps}, consumed {…}, parent_budget_id?, status (active|exhausted|suspended)`. Child ≤ parent remaining.

### 13.3 Notification
`id, user_id, workspace_id, kind (needs_approval|needs_answer|blocked|completed|failed|mention|attention|system), ref {thread|run|effect|question}, channels_delivered[] (in_app|desktop|email|webhook), read_at?, created_at`. Preferences per user × workspace × kind.

### 13.4 UsageRecord
`id, tenant_id, workspace_id, scope_refs {run_id?, agent_thread_id?, target_id?, connector_id?}, meter (model_input_tokens|model_output_tokens|model_cache_tokens|model_cost|machine_seconds|storage_bytes|connector_calls|browser_seconds), quantity, unit, cost_minor_units?, source_event_id, occurred_at`. Rebuildable from RuntimeEvents; billing is a projection.

### 13.5 Webhook subscription (APP-015)
`id, tenant_id, workspace_id?, url, secret_handle_id, event_types[], status (active|paused|failing), delivery {attempts, backoff, last_status}`. Outbound delivery is a `network.egress` effect under egress policy; payload is the RuntimeEvent envelope with redaction applied.

---

## 14. Public command catalog (API v1)

Commands are `POST /v1/commands/<Name>` (one endpoint per command; OpenAPI-generated) carrying `command_id`, tenant/workspace scope from auth context. Reads are `GET /v1/...` projections. All commands return `{command_id, accepted_at, result | error}`; mutations are idempotent by `command_id`.

| Family | Commands |
|---|---|
| Identity | `CreateWorkspace`, `UpdateWorkspace`, `InviteMember`, `UpdateMembership`, `RemoveMember`, `CreateServicePrincipal`, `RevokeServicePrincipal`, `UpdateUserSettings` |
| Teammates | `CreateTeammate`, `UpdateTeammate`, `ArchiveTeammate` |
| Conversation | `CreateThread`, `PostMessage`, `EditMessage`, `DeleteMessage`, `ReactToMessage`, `AddParticipant`, `RemoveParticipant`, `AnswerQuestion`, `CancelTurn` |
| Work | `CreateObjective`, `ProposeGraphEdit` (user-originated, validated like a PlanProposal), `CancelRun`, `RetryRun`, `PauseRun`, `ResumeRun`, `RequestVerification` |
| Trust | `ApproveEffect`, `DenyEffect`, `SetUserRule`, `RevokeUserRule` |
| Execution | `ProvisionTarget`, `StopTarget`, `ResetTarget`, `CreateCheckpoint`, `RestoreCheckpoint` |
| Browser/computer | `RequestTakeover`, `Handback`, `ImportBrowserSession`, `GrantComputerTier` |
| Artifacts | `CreateArtifact` (upload init/complete), `AddArtifactVersion`, `GrantArtifactAccess`, `DeleteArtifact` |
| Knowledge & memory | `IngestSource`, `DeleteKnowledge`, `SupersedeKnowledge`, `CreateMemory`, `DeleteMemory` |
| Skills & packs | `InstallSkill`, `PromoteSkillVersion`, `DisableSkill`, `SubmitPack`, `QualifyPack`, `PublishPack`, `RollbackPack` |
| Routines | `CreateRoutine`, `UpdateRoutine`, `PauseRoutine`, `ResumeRoutine`, `TriggerRoutineNow`, `DeleteRoutine` |
| Connectors | `BeginConnectorAuth`, `CompleteConnectorAuth`, `RevokeConnector`, `CreateWebhookSubscription`, `DeleteWebhookSubscription` |
| Notifications | `UpdateNotificationPreferences`, `MarkNotificationRead` |
| Admin/ops | `SetPolicy`, `FreezeEffects`, `UnfreezeEffects`, `DisableProvider`, `EnableProvider`, `QuarantineWorker`, `DrainTarget`, `ExportTenantData`, `DeleteUserData` |
| Read projections | `GET /v1/threads/{id}`, `/messages`, `/workgraph`, `/runs/{id}`, `/runs/{id}/timeline`, `/effects`, `/approvals`, `/questions`, `/artifacts`, `/artifacts/{id}/versions`, `/evidence/{id}`, `/targets`, `/browser-sessions/{id}`, `/knowledge`, `/memory`, `/skills`, `/packs`, `/routines`, `/connectors`, `/notifications`, `/usage`, `/audit`, `/search?q=` (typed SearchProgram), `GET /v1/stream` (WS) |

Agent-originated intents (`work.delegate`, `work.propose_plan`, `user.ask`, `memory.propose`) are Tools (§7.5), not public commands.

---

## 15. Error taxonomy

Errors are `{code, message, correlation_id, retryable, details?}`; HTTP mapping in parentheses.

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
