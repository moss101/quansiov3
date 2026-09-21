# quansio-cli

**Canonical owner:** `crates/cli` — the `quansio` CLI over the public API v1 (APP-013).

A client and nothing else (DOSSIER.md §4.1): every verb is a `/v1/` read projection or
`POST /v1/commands/<Name>`; the only credential is a bearer session in `$QUANSIO_CLI_HOME`
(default `~/.config/quansio/credentials.json`, owner-only); no flag bypasses API or
runtime policy. It must not become a second runtime, store, policy engine or effect path.

```
quansio login --server URL --email you@example.com   # then: login --server URL --token …
quansio whoami | workspaces list|use|create | threads list | messages list|post
quansio objectives create | runs get|timeline|cancel | approvals list|show|approve|deny
quansio artifacts list|versions|download | routines list | targets list
quansio diagnostics bundle --out FILE | api get /v1/… | api command <Name> --params '{…}'
quansio --json …                                       # machine output, tokens redacted
```

Build and test: `cargo test -p quansio-cli` (the E2E needs the dev stack and the built
`quansio-server`/`quansio-dev-seed`; blocked, not skipped, without them).
