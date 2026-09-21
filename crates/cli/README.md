# quansio-cli

**Canonical owner:** `crates/cli` — the `quansio` CLI over the public API v1 (APP-013).

The CLI is a client and nothing else (DOSSIER.md §4.1): every verb is a `/v1/` read
projection or `POST /v1/commands/<Name>`; it holds a bearer session (in
`$QUANSIO_CLI_HOME` or `~/.config/quansio/credentials.json`, owner-only) and no other
credential; there is no flag that bypasses API or runtime policy. It must not become a
second runtime, store, policy engine or effect path.

```
quansio login --server https://quansio.example --email you@example.com
quansio login --server https://quansio.example --token <token from the link>
quansio whoami
quansio workspaces list | use <ws_…> | create --name Ops
quansio messages post --content "hello" [--thread thr_…] [--teammate agt_…]
quansio runs get|timeline|cancel <run_…>
quansio approvals list|show|approve <apr_…> --digest <sha256>|deny <apr_…>
quansio artifacts list|versions <art_…>|download <art_…> <artv_…> --out FILE
quansio diagnostics bundle --out FILE
quansio api get /v1/… | api command <Name> --params '{…}'
quansio --json …            machine-readable output (tokens redacted)
```

Build and test: `cargo test -p quansio-cli` (the E2E needs the dev stack and the built
`quansio-server` and `quansio-dev-seed` binaries; it is blocked, not skipped, without them).
