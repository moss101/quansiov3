# EXEC-003 evidence notes

Implementation of the macOS local Linux capsule is complete. The real Virtualization.framework boot cannot run because the signed V8.1 guest image is unpublished.

## acceptance

**Guest cannot reach host secrets or cloud metadata by default.** The native device graph has zero network devices and zero host directory shares. The embedded `config/guest-images.yaml` catalog encodes the same isolation and refuses to carry a digest or URL while unpublished, so there is no artifact a controller could accept as a real image. Cookie/saved-login import remains `browser.session.import` and is refused as a guest service.

**Start/stop/reset and checkpoint flows are repeatable.** The Rust controller proves idempotent start/stop, overlay-only reset, checkpoint/restore sequencing and stale-generation fencing before any native call. Swift unit tests prove the device graph, arbitrary-port denial, host URL denial, and overlay reset preserving the immutable root.

## real boundary

`QUANSIO_TEST_MACOS_VM=1` plus digest-verified kernel/root-disk artifacts are required. They are absent. The live test skips with an explicit `BLOCKED_EXTERNAL` marker. Status is therefore `BLOCKED_EXTERNAL` with `implementation_complete: true`, never PASS.
