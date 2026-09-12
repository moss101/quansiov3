"""Supply-chain and dependency-security toolkit (OPS-007).

One entry point, `scripts/ci/supply_chain/check.py`, runs every supply-chain gate and
exits non-zero on any finding, printing `rule: detail`:

  * `lockfiles.py`  - every manifest dependency is pinned by its lockfile.
  * `policy.py`     - `deny.toml` structure, license allow-list coverage, `cargo-deny`.
  * `sbom.py`       - deterministic CycloneDX SBOM generation and drift verification.
  * `advisories.py` - known-vulnerable dependency scan against a committed offline
                      advisory DB (real RustSec records; no network).
  * `skills.py`     - quarantine -> provenance -> review -> normalization -> evaluation
                      -> approved lifecycle for imported skill/tool manifests.

Standard library only; the gate never requires network access.
"""
