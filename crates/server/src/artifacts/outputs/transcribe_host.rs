//! `audio.transcribe` (ADP-006, DOMAIN.md §10.4 E): submit and poll, idempotent by `request_id`.
//!
//! The authority reads the audio Artifact the caller may read, asks the transcription port (the
//! gateway's route) for a transcript, validates what comes back against its own rules, and commits a
//! transcript Artifact with a quality record it wrote. A transcript is untrusted external data: it
//! is labelled so inside the Artifact, in the record and in the result.

use serde_json::{json, Value};

use super::ports::PortError;
use super::quality::{Check, QualityRecord, Trust};
use super::support::{
    commit_output, find_output, read_source, read_version_bytes, refuse, refuse_with, success,
    unavailable, Ctx, OutputSpec,
};
use super::transcript::{self, Source, Transcript};
use super::transcription::{TranscribeRequest, TranscribeStatus};
use crate::artifacts::ArtifactKind;
use crate::runtime::turn_loop::{HostFailure, HostOutcome};

/// Error codes a failed job may carry through to the model, as the gateway names them.
const PASSED_CODES: &[&str] = &[
    "VALIDATION_SCHEMA",
    "VALIDATION_BOUNDS",
    "PROVIDER_UNAVAILABLE",
    "PROVIDER_RATE_LIMITED",
    "PROVIDER_REFUSAL",
    "DLP_DENIED",
    "POLICY_DENIED",
    "TOOL_TIMEOUT",
    "CONFLICT_STATE",
];

fn no_route(detail: impl Into<String>) -> HostOutcome {
    unavailable("transcription", detail, None)
}

/// `audio.transcribe`.
///
/// # Errors
/// Never: every refusal is a [`HostOutcome`].
pub async fn transcribe(ctx: &Ctx<'_>) -> Result<HostOutcome, HostFailure> {
    Ok(transcribe_inner(ctx)
        .await
        .unwrap_or_else(|outcome| outcome))
}

fn valid_request_id(id: &str) -> bool {
    (8..=64).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn valid_language(language: &str) -> bool {
    (2..=12).contains(&language.len())
        && language
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
}

#[allow(clippy::too_many_lines)]
async fn transcribe_inner(ctx: &Ctx<'_>) -> Result<HostOutcome, HostOutcome> {
    let limits = &ctx.env.config.transcription;
    let artifact_id = ctx.arg_str("artifact_id").unwrap_or_default().to_string();
    let request_id = ctx.arg_str("request_id").unwrap_or_default().to_string();
    if !valid_request_id(&request_id) {
        return Err(refuse(
            "VALIDATION_SCHEMA",
            "output.audio.transcribe.request",
            "request_id is 8 to 64 letters, digits, '-' or '_'",
        ));
    }
    let language = ctx.arg_str("language").map(str::to_string);
    if language.as_deref().is_some_and(|l| !valid_language(l)) {
        return Err(refuse(
            "VALIDATION_SCHEMA",
            "output.audio.transcribe.language",
            "language is a short tag such as en or pt-BR",
        ));
    }
    let wait_ms = ctx
        .dispatch
        .args
        .get("wait_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(limits.wait_max_ms);
    let Some(port) = ctx.env.transcription.clone() else {
        return Err(no_route(
            "no gateway transcription route is composed on this deployment, so nothing can be transcribed",
        ));
    };
    let audio = read_source(ctx, &artifact_id, ArtifactKind::Audio, None).await?;
    if audio.bytes.len() > limits.max_audio_bytes {
        return Err(refuse(
            "VALIDATION_BOUNDS",
            "output.audio.transcribe.size",
            format!(
                "the audio is {} bytes; a transcription takes at most {}",
                audio.bytes.len(),
                limits.max_audio_bytes
            ),
        ));
    }
    let source = Source {
        artifact_id: audio.artifact.id.to_canonical(),
        version_id: audio.version.id.to_canonical(),
        content_digest: audio.version.content_digest.to_string(),
    };
    let role = format!("{}:{request_id}", transcript::ROLE);

    // A transcript for this request id already exists: it is the answer, or the id is taken.
    if let Some((existing, version)) = find_output(ctx, &role).await? {
        let record = version.quality_record.clone().unwrap_or(Value::Null);
        let same_source = record["inputs"]
            .as_array()
            .and_then(|inputs| inputs.first())
            .is_some_and(|input| {
                input["version_id"] == source.version_id
                    && input["content_digest"] == source.content_digest
            });
        let same_language =
            record["environment"]["requested_language"].as_str() == language.as_deref();
        if !same_source || !same_language || record["adapter"] != transcript::ADAPTER {
            return Err(refuse_with(
                "CONFLICT_STATE",
                "output.audio.transcribe.request",
                format!(
                    "request {request_id} is already a transcription of other audio or another language"
                ),
                Some(json!({ "artifact_id": existing.id.to_canonical() })),
            ));
        }
        let bytes = read_version_bytes(ctx, &version.id).await?;
        let found = Transcript::parse(&bytes, limits).map_err(|detail| {
            refuse(
                "VALIDATION_SCHEMA",
                "artifact.format",
                format!("the stored transcript is not valid: {detail}"),
            )
        })?;
        return Ok(done_output(
            &existing.id.to_canonical(),
            &version.id.to_canonical(),
            version.seq,
            &found,
            true,
            None,
        ));
    }

    let status = port
        .request(TranscribeRequest {
            request_id: request_id.clone(),
            audio: audio.bytes.clone(),
            media_type: audio.version.media_type.clone(),
            language: language.clone(),
            source: source.clone(),
            wait_ms,
        })
        .await
        .map_err(|error| match error {
            PortError::Unavailable { detail, .. } => no_route(detail),
            PortError::Failed(detail) => HostOutcome::Failure {
                retryable: true,
                error: json!({ "code": "INTERNAL", "rule": "output.audio.transcribe.job", "message": detail }),
                evidence_ids: Vec::new(),
            },
            PortError::TooLarge(detail) => {
                refuse("VALIDATION_BOUNDS", "output.audio.transcribe.size", detail)
            }
        })?;

    match status {
        TranscribeStatus::Pending {
            job_id,
            poll_after_ms,
        } => Ok(success(
            json!({
                "status": "pending",
                "job_id": job_id,
                "poll_after_ms": poll_after_ms,
                "trust": Trust::UntrustedExternal.as_str(),
            }),
            Vec::new(),
            None,
        )),
        TranscribeStatus::Failed {
            code,
            message,
            retryable,
        } => {
            let code = if PASSED_CODES.contains(&code.as_str()) {
                code
            } else {
                "PROVIDER_UNAVAILABLE".to_string()
            };
            Ok(HostOutcome::Failure {
                retryable,
                error: json!({ "code": code, "rule": "output.audio.transcribe.provider", "message": message }),
                evidence_ids: Vec::new(),
            })
        }
        TranscribeStatus::Done {
            transcript: bytes,
            model,
            provider,
            ..
        } => {
            let mut record = QualityRecord::new(transcript::ADAPTER, Trust::UntrustedExternal);
            record.inputs.push(audio.input());
            record
                .environment
                .insert("model".to_string(), Value::String(model));
            record
                .environment
                .insert("provider".to_string(), Value::String(provider));
            record.environment.insert(
                "requested_language".to_string(),
                language.clone().map_or(Value::Null, Value::String),
            );
            let parsed = Transcript::parse(&bytes, limits);
            record.checks.push(match &parsed {
                Ok(found) => Check::passed(
                    "output.audio.transcribe.shape",
                    format!(
                        "{} ordered segment(s) inside {} ms, labelled {}",
                        found.segments.len(),
                        found.duration_ms,
                        transcript::TRUST
                    ),
                ),
                Err(detail) => Check::failed("output.audio.transcribe.shape", detail.clone()),
            });
            let Ok(found) = parsed else {
                return Err(refuse_with(
                    "VALIDATION_SCHEMA",
                    "output.audio.transcribe.shape",
                    "the transcription route returned something that is not a transcript",
                    Some(
                        json!({ "checks": record.checks.iter().map(Check::to_json).collect::<Vec<_>>() }),
                    ),
                ));
            };
            let from_this_audio = found.source == source;
            record.checks.push(Check::of(
                "output.audio.transcribe.source",
                from_this_audio,
                "the transcript names the audio version that was sent",
            ));
            if !from_this_audio {
                return Err(refuse_with(
                    "VALIDATION_SCHEMA",
                    "output.audio.transcribe.source",
                    "the transcript is not of the audio that was sent",
                    Some(
                        json!({ "checks": record.checks.iter().map(Check::to_json).collect::<Vec<_>>() }),
                    ),
                ));
            }
            record.checks.push(Check::of(
                "output.audio.transcribe.speech",
                !found.segments.is_empty(),
                format!("{} segment(s) of speech were found", found.segments.len()),
            ));
            let committed = commit_output(
                ctx,
                OutputSpec {
                    role,
                    kind: ArtifactKind::Other,
                    title: format!("Transcript of {}", audio.artifact.title),
                    media_type: transcript::MEDIA_TYPE.to_string(),
                    bytes: found.to_bytes(),
                    record,
                },
            )
            .await?;
            Ok(done_output(
                &committed.artifact_id.to_canonical(),
                &committed.version.id.to_canonical(),
                committed.version.seq,
                &found,
                committed.replayed,
                Some(committed.version.id.to_canonical()),
            ))
        }
    }
}

fn done_output(
    artifact_id: &str,
    version_id: &str,
    seq: i32,
    found: &Transcript,
    replayed: bool,
    remote_ref: Option<String>,
) -> HostOutcome {
    let preview: String = found.text().chars().take(400).collect();
    success(
        json!({
            "status": "done",
            "artifact_id": artifact_id,
            "version_id": version_id,
            "version": seq,
            "language": found.language,
            "duration_ms": found.duration_ms,
            "segment_count": found.segments.len(),
            "preview": preview,
            "replayed": replayed,
            "trust": transcript::TRUST,
        }),
        Vec::new(),
        remote_ref,
    )
}
