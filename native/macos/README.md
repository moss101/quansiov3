# native/macos

**Canonical owner:** `native/macos` — minimal Swift bridge for macOS-only APIs.

Used by the Rust machine module for Virtualization.framework (local capsule), Accessibility
(AX) computer-use inspection and Keychain access. Narrow typed surface only: no product
orchestration, no capability/policy/effect decisions (DOSSIER.md §3, §5).

`Sources/QuansioMacBridge/Recording/` and the `quansio-recorder` executable (ADP-003) are the person's
recorder: a short-lived process the desktop starts for one recording. It watches the person (listen-only
event tap, accessibility lookup), redacts before it keeps anything, pauses on secure input, fences every call
by owner and epoch and answers one authenticated Unix-socket channel. It is not a service and installs nothing.

Build/test (macOS only): `swift test --package-path native/macos`
