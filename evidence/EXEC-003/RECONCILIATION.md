# EXEC-003 reconciliation

## Canonical owner and existing coverage

`native/macos/` owns the minimal Virtualization.framework bridge and
`crates/machine/src/substrates/macos_capsule/` owns the Rust substrate controller that maps it onto the
existing ExecutionTarget, Lease, generation, gateway and checkpoint contracts. Coverage is
`GENUINE_GAP`: the repository has only macOS architecture detection. EXEC-001 and EXEC-002 already
provide the canonical target lifecycle and typed, fenced worker envelope; EXEC-006/009 provide the
file/terminal/browser services qworkerd exposes. This task must not recreate any of them in the guest.

## Persistent state, contracts, and events

No new authoritative table is needed. `execution_targets` stores substrate, desired/observed state,
image digest, endpoint reference, generation and checkpoint ids; `checkpoints` stores recoverable
snapshot metadata. The capsule controller drives those owners through their existing APIs and emits
the existing `target.*`, `lease.*`, and `checkpoint.*` families. A host-local VM bundle and runtime
socket are ephemeral substrate data, never an alternate machine store.

The existing ExecutionTarget and machine-gateway contracts are sufficient. The native bridge needs a
narrow typed configuration/result shape for configure/start/requestStop/stop/restore and a private
virtio-socket control endpoint. If that exposes a missing canonical field, `DOMAIN.md` is updated
additively and contracts regenerated in the implementation commit.

## Effects, capabilities, approvals, and isolation

Lifecycle operations require the existing target-scoped machine lease/generation. Guest services are
reachable only through the private typed qworkerd channel. The guest receives no host filesystem,
Keychain, cloud metadata route, raw connector/provider credential, or arbitrary port forward. Host-side
networking is deny by default and delegates destinations to the existing egress broker. Browser
cookie/saved-login import remains `browser.session.import` tier 4 with exact approval; this task adds no
alternate import path.

## Image, migration, and recovery

The desktop channel ships or downloads one configured Linux guest image containing qworkerd,
Chromium and base tooling. Its URL/path, SHA-256 digest and architecture live in `config/`; the
controller verifies the bytes before first boot and refuses mismatches. No legacy capsule state exists,
so migration is additive. Reset discards the writable overlay; restore creates a new controller
generation from a verified checkpoint; stale guest output is fenced by EXEC-001/002. Rollback stops the
VM, preserves declared checkpoints, and removes only ephemeral overlays/sockets.

## Real boundary

The real suite requires `QUANSIO_TEST_MACOS_VM=1`,
`QUANSIO_TEST_MACOS_GUEST_IMAGE=/absolute/path/to/image`, and a matching configured digest. It must boot
the actual Linux guest with Virtualization.framework, observe qworkerd over the private channel, prove
cloud metadata and host-secret paths are unreachable, and exercise repeatable start/stop/reset and
checkpoint restore. Missing image, entitlement, interactive macOS host, or virtualization support is
reported as `BLOCKED_EXTERNAL`; no fake VM counts.

## Implementation checklist

- [ ] Add the pinned guest-image configuration/schema and digest verifier.
- [ ] Implement the Rust macOS capsule state adapter over ExecutionTarget/Lease/generation.
- [ ] Implement the narrow Swift Virtualization.framework VM configuration and lifecycle bridge.
- [ ] Connect qworkerd only through a private virtio-socket typed channel and deny host/cloud access.
- [ ] Implement repeatable start/stop/reset/checkpoint/restore with stale-generation fencing.
- [ ] Add negative image, permission, host-secret, metadata and cookie-import tests.
- [ ] Run the real macOS VM suite or record the exact `BLOCKED_EXTERNAL` boundary.
- [ ] Qualify, record evidence, update progress and merge to `main`.
