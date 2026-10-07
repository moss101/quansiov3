# ADP-008 plan (checklist)

A. Tool visibility split
- [x] `model_visible` (default true) and `host_callable` (default false) on the declaration; unknown keys refused; neither flag set refused.
- [x] `Caller` (model | host) on every call; `plan_call` refuses a model naming a non-model-visible tool (`TOOL_NOT_MODEL_VISIBLE`) before its
      arguments are read, and the host a non-host-callable one (`TOOL_NOT_HOST_CALLABLE`). The default caller is the model.
- [x] One model-facing filter in the registry (`exposed_*`, `model_visible_names`, latest version only); the gateway's schema source skips
      what is not model-visible; the recording compile is told only model-visible tools; a source scan keeps the unfiltered lists inside `crates/tools`.
- [x] `tool_calls.caller` (migration 0029); a rejected model attempt is recorded and evented with its caller; a host call takes the same
      capability, policy and ledger path (`dispatch_as_host`).
- [x] Audit of the 44 declarations; one host-only tool (`webdev.preview_scopes`) so the shipped registry exercises the split.
- [x] `quansio-tool-check` refuses a host-only tool (`not_model_visible`).
- [x] DOMAIN 7.5, 10.4 F, 15; generated catalogs, OpenAPI, SDK tables.

B. Content-hash pack revisions
- [x] Canonical content and revision in the runtime and the plane, pinned to a shared corpus.
- [x] `CompilePack` on the IntelligenceGateway (proto, Rust and Python bindings): verify (CAP-004 credential rule, quarantine record,
      digest, closed shape) and compile (tool visibility, CAP-005 gate).
- [x] `control/packs`: `PackStore` (activation in one transaction, retention bound from `config/pack-install.yaml`, rollback) and
      `PackInstaller` (fetch, verify, compile, activate); `InstallPack` / `RollbackPack`; `GET /v1/packs`.
- [x] A failed install at any phase keeps the previous revision active.

C. Redacted install reports
- [x] `pack_install_reports` (RLS), keyed by command id; redaction and bounds where a diagnostic enters; lazy expiry sweep; stale `running`
      closed as interrupted.
- [x] API read and the desktop Knowledge surface (state, phase, outcome, findings).

Tests: tools (`visibility`, `tool_check_binary`), server (`tool_visibility`, `outputs_preview`, `intelligence_gateway`, `pack_install`), Python
(`test_pack_install`, `test_tool_visibility`), desktop (`packs.test.ts`), contract (`pack-install-config` schema).
