# native/macos

**Canonical owner:** `native/macos` — minimal Swift bridge for macOS-only APIs.

Used by the Rust machine module for Virtualization.framework (local capsule), Accessibility
(AX) computer-use inspection and Keychain access. Narrow typed surface only: no product
orchestration, no capability/policy/effect decisions (DOSSIER.md §3, §5).

Build/test (macOS only): `swift test --package-path native/macos`
