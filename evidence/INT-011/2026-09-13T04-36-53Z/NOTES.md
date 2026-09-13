# Evidence notes

Narrative that the canonical `summary.json` schema does not carry.

## real_boundary_evidence



## note

INT-011 is real_boundary=false: its proof is the shipped Python path plus real PostgreSQL+pgvector and real CORE-007 object storage in the dev stack, not a live model provider. The embedding wire is exercised against the loopback conformance stub, which is a wire proof and not live-provider evidence (that belongs to INT-002/INT-003, both BLOCKED_EXTERNAL on QUANSIO_TEST_* provider credentials).

## environment_blocker

bash scripts/ci/ci.sh cannot complete on this host right now: two of its gates execute a newly linked binary, and the host is again refusing to execute newly created inodes (the incident HANDOFF.md records). Proven with a byte-identical copy: a copy of a working test binary hangs in _dyld_start while the original runs. contract-drift was still verified by running the shipped generator's bytes in a pre-existing inode (zero drift), and every other gate ran individually (all CLEAN); the toolchains gate (cargo test --workspace) could not run because the workspace target directory had been emptied for disk space and every freshly linked test binary would hang. Unblock: reboot the host (or let the incident clear, as it did earlier in this mission), then run `bash scripts/ci/ci.sh`.
