//! Artifact and evidence metadata authority plus object-storage coordination (CORE-007).
//!
//! Canonical owner: `crates/server` artifacts module (DOSSIER.md §5, §17). PostgreSQL is
//! authoritative for `Artifact`, `ArtifactVersion`, `Evidence`, `EvidenceBundle` and
//! `artifact_grants` metadata; S3-compatible object storage holds only the bytes, keyed
//! per tenant and addressed by sha256 (DOMAIN.md §10, DOSSIER.md §9).
//!
//! # Guarantees
//!
//! * **Digest integrity** — object keys embed the sha256 of the bytes, and every read
//!   re-hashes the payload; a mismatch fails closed with
//!   [`ArtifactError::DigestMismatch`]. Uploading the same bytes for the same artifact
//!   twice is idempotent (same `artv_` identity, no rewrite).
//! * **Immutability** — evidence is insert-only; a second capture mints a new `evd_`
//!   identity. A new artifact version is a new `artv_` identity carrying
//!   `parent_version_id`; bytes are never overwritten in place.
//! * **Grants** — reads and writes require a per-principal `artifact_grants` row; no
//!   grant means denied, and a `read` grant cannot write.
//! * **Retention** — deletion respects legal hold and retention windows, tombstones
//!   metadata instead of removing it silently and records the reason it decided.
//!
//! # Boundary environment variables
//!
//! The object-storage boundary reads these variables (documented dev defaults match
//! `config/dev.yaml`; the secret is a documented dev-only default from
//! `scripts/dev/_common.sh`, never a production credential):
//!
//! * `QUANSIO_TEST_MINIO_ENDPOINT` — default `http://127.0.0.1:59010`
//! * `QUANSIO_TEST_MINIO_REGION` — default `us-east-1`
//! * `QUANSIO_TEST_MINIO_BUCKET` — default `quansio-dev`
//! * `QUANSIO_TEST_MINIO_ACCESS_KEY` — default `quansio-dev`
//! * `QUANSIO_TEST_MINIO_SECRET_KEY` — default `quansio-dev-only` (dev only)
//! * `QUANSIO_TEST_MINIO_SESSION_TOKEN` — optional session token
//! * `QUANSIO_TEST_MINIO_MULTIPART_THRESHOLD_BYTES` — default 8 MiB
//! * `QUANSIO_TEST_MINIO_PART_SIZE_BYTES` — default 8 MiB
//!
//! The test suite additionally gates on `QUANSIO_TEST_POSTGRES_URL` for its scratch
//! databases (see `crates/server/tests/artifacts.rs`).

mod metadata;
mod model;
mod object_store;
mod sigv4;

pub use metadata::{
    Artifact, ArtifactError, ArtifactStore, ArtifactVersion, DeletionOutcome, Evidence,
    EvidenceBundle, EvidenceBundleStore, EvidenceContent, EvidenceStore, NewArtifact, NewEvidence,
    NewEvidenceBundle, NewVersion, StoredVersion,
};
pub use model::{
    ArtifactGrant, ArtifactId, ArtifactKind, ArtifactVersionId, CapturedBy, Claim,
    DeletionDecision, DeletionRecord, EvidenceBundleId, EvidenceId, EvidenceKind, GrantLevel,
    Origin, OriginKind, ProducedBy, Retention, RetentionClass, Source, Verification,
};
pub use object_store::{
    ObjectKey, ObjectStore, ObjectStoreError, S3Config, S3ObjectStore, DEFAULT_MULTIPART_PART_SIZE,
    DEFAULT_MULTIPART_THRESHOLD, OBJECT_KEY_ROOT,
};
