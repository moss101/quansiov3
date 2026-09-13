# Evidence notes

Narrative that the canonical `summary.json` schema does not carry (DOSSIER.md 19 governs the summary;
a bundle may carry other evidence files).

## acceptance

**Secret values are absent from model/tool logs and normal RPC payloads — by construction, not by a
filter.** `SecretMaterial` has no `Display`, no `Serialize` and no `Clone`, and its `Debug` prints
`SecretMaterial(<n> bytes redacted)`; `SecretHandle` — the type that travels on the wire — has no field
the material could occupy. A secret therefore cannot enter a prompt, a log line or an RPC payload by
being formatted into one, and the test that scans for a canary value is a check on that design rather
than on a redaction rule that has to keep up with every new call site. The scan drives the shipped
broker against real PostgreSQL with a canary value and asserts the canary appears in none of: the
handle's rendering, the access record's rendering, the handle listing, the `Debug` of the
materialization, the `id`/`data_key_ref` columns, and the stored `ciphertext` bytes. It also asserts the
value *is* recoverable at the boundary and that `last_used_at` moved, so the scan cannot pass by the
value having been lost.

**A revoked handle cannot be reused from cached worker state.** A `Materialization` carries the handle's
`generation`, and `validate_materialization` compares it against the handle as it is *now*. `revoke`
changes the status and bumps the generation in one statement, so there is no instant in which the handle
is revoked but a materialization still validates; the test materializes, validates, revokes, and then
asserts the cached materialization is refused with `HandleRevoked`, and that a fresh materialization is
refused too. A disconnected worker cannot be told, so it has to ask — which is what the check is for.

## the three named tests

- **secret scan** — above.
- **revocation** — above, plus the fence rather than only the status: a *rotated* handle fences earlier
  materializations with `FencedStaleGeneration` while the handle stays usable, and the stored frame no
  longer contains either the old or the new value.
- **scope substitution** — a materialization issued for one connector is refused with
  `ScopeSubstitution` when presented for another connector, a target or a tool, naming both the scope it
  was issued for and the one presented, while the issued scope still validates.

## recorded_decisions

- **Envelope encryption, with the KEK where the deployment says it is.** `KeyProvider` is the seam:
  `KmsKeyProvider` delegates `wrap`/`unwrap` to a `KmsClient`, so a cloud KMS holds the KEK and the
  unwrapped data key exists in this process only for the moment it is used; `LocalMasterKeyProvider`
  reads a master key from a file and wraps with an AEAD under it. The local path is a real seal, not a
  stub, so a test of the shipped path is a test of the shipped cryptography.
- **The frame carries its own version and its parts are bound together.** `version ‖ nonce ‖
  wrapped-key-length ‖ wrapped key ‖ sealed material`, with the wrapped data key as the AEAD's
  associated data, so a frame whose pieces were swapped from another secret does not open — and a
  changed byte anywhere is a refusal rather than a wrong value. A frame from a version this broker does
  not write is named as such instead of guessed at.
- **A fresh data key and nonce per seal.** Sealing the same value twice produces different frames, so a
  stored frame is not a fingerprint of the value and two handles for one credential are not linkable by
  comparing ciphertext.
- **`chacha20poly1305` is a new dependency, and a needed one.** There was no symmetric AEAD in the tree
  (`hmac`/`sha2` only), and rolling one is not an option. It is RustCrypto, already resolvable to
  versions whose transitive dependencies are present (`aead`, `cipher`, `chacha20`, `poly1305`,
  `universal-hash`), and the supply-chain gate stays CLEAN with it.
- **The audit trail is the handle plus the returned record, not a second store.** `secret_handles` is
  this module's table, so `last_used_at` is written in the same transaction as the read; the actor,
  effect, scope, generation and decision come back as a typed `SecretAccess` for the audit owner to
  persist, because `audit_entries` is OPS-002's hash-chained authority and a second audit table would be
  a second authority.
- **`generation` and `expires_at` needed a migration** (`0010_secret_handle_fencing.sql`). A
  materialization cannot be fenced without something that changes when the handle does, and a
  short-lived handle needs a scheduled expiry. Both are additive with a defined meaning for existing
  rows, and a `CHECK (expires_at > created_at)` refuses a handle that expires before it was created —
  which the test fixture had to respect rather than work around.

## what is not claimed

- The task's `real_boundary` is `false`. No cloud KMS is contacted: the `KmsClient` seam is implemented
  by a recording double in the tests to prove the broker delegates rather than sealing locally, and
  wiring a real KMS SDK is the deployment's job. The `LocalMasterKeyProvider` path is exercised for
  real.
- **Revocation cannot un-deliver material already handed to a worker.** It stops the *materialization*
  being used, and the lifetime bounds how long a delivered value is worth anything. That is why
  materializations are short-lived (`DEFAULT_MATERIALIZATION_SECONDS = 60`) rather than convenient, and
  it is recorded here rather than left implied.
- Materialization secrets are held in memory without a `zeroize`-style wipe on drop. Nothing in the tree
  has that dependency; adding it is a separate decision, and the exposure is the same as any other
  `Vec<u8>` in the process.
