//! `video.edit`, `video.render` and `video.inspect` (ADP-006, DOMAIN.md §10.4 D).

use std::collections::BTreeMap;
use std::sync::Arc;

use quansio_core::Digest;
use serde_json::{json, Value};

use super::media::{InspectJobSpec, MediaFile, MediaJobPort, RenderJobSpec};
use super::ports::PortError;
use super::quality::{Check, QualityRecord, Trust};
use super::support::{
    add_version_to, artifact_info, commit_output, read_seq, read_source, record_screenshot, refuse,
    refuse_with, success, unavailable, Ctx, OutputSpec,
};
use super::tool_owned::TIMELINE_MEDIA_TYPE;
use super::transcript::{self, Transcript};
use super::video::ops::{self, OpError, RawOp, TranscriptSource, Transcripts};
use super::video::plan::{render_plan, RenderPlan};
use super::video::timeline::{MediaKind, Timeline};
use crate::artifacts::{ArtifactId, ArtifactKind};
use crate::runtime::turn_loop::{HostFailure, HostOutcome};

const EDIT: &str = "video.edit";
const RENDER: &str = "video.render";
const INSPECT: &str = "video.inspect";

fn ffmpeg_missing(ctx: &Ctx<'_>, detail: impl Into<String>) -> HostOutcome {
    unavailable("ffmpeg", detail, Some(&ctx.env.config.video.ffmpeg_bin_env))
}

fn port_failure(ctx: &Ctx<'_>, error: PortError) -> HostOutcome {
    match error {
        PortError::Unavailable { detail, .. } => ffmpeg_missing(ctx, detail),
        PortError::Failed(detail) => HostOutcome::Failure {
            retryable: true,
            error: json!({ "code": "INTERNAL", "rule": "output.video.job", "message": detail }),
            evidence_ids: Vec::new(),
        },
        PortError::TooLarge(detail) => {
            refuse("VALIDATION_BOUNDS", "output.video.render.size", detail)
        }
    }
}

fn parse_timeline(ctx: &Ctx<'_>, bytes: &[u8]) -> Result<Timeline, HostOutcome> {
    Timeline::parse(bytes, &ctx.env.config.video).map_err(|detail| {
        refuse(
            "VALIDATION_SCHEMA",
            "artifact.format",
            format!("the timeline is not valid: {detail}"),
        )
    })
}

fn conflict(expected: u64, current: u64) -> HostOutcome {
    refuse_with(
        "CONFLICT_REVISION",
        "output.video.revision",
        format!(
            "the timeline is at revision {current}, not {expected}: read it again before editing"
        ),
        Some(json!({ "expected_revision": expected, "current_revision": current })),
    )
}

fn op_refusal(error: &OpError) -> HostOutcome {
    refuse_with(
        if error.field {
            "VALIDATION_UNKNOWN_FIELD"
        } else {
            "VALIDATION_SCHEMA"
        },
        error.rule,
        format!(
            "operation {} was refused, so none was applied: {}",
            error.index, error.detail
        ),
        Some(json!({ "op_index": error.index, "rule": error.rule })),
    )
}

/// The transcripts a batch reads, resolved and checked to be the authority's own.
async fn resolve_transcripts(ctx: &Ctx<'_>, ids: &[String]) -> Result<Transcripts, HostOutcome> {
    let mut out = Transcripts::new();
    for id in ids {
        let loaded =
            read_source(ctx, id, ArtifactKind::Other, Some(transcript::MEDIA_TYPE)).await?;
        let by_the_authority = loaded
            .version
            .quality_record
            .as_ref()
            .and_then(|record| record.get("adapter"))
            .and_then(Value::as_str)
            == Some(transcript::ADAPTER);
        if !by_the_authority {
            return Err(refuse(
                "VALIDATION_SCHEMA",
                ops::RULE_TRANSCRIPT,
                format!("{id} was not made by audio.transcribe, so it is not a transcript this call will read"),
            ));
        }
        let parsed =
            Transcript::parse(&loaded.bytes, &ctx.env.config.transcription).map_err(|detail| {
                refuse(
                    "VALIDATION_SCHEMA",
                    ops::RULE_TRANSCRIPT,
                    format!("{id} is not a valid transcript: {detail}"),
                )
            })?;
        out.insert(
            id.clone(),
            TranscriptSource {
                version_id: loaded.version.id.to_canonical(),
                segments: parsed
                    .segments
                    .into_iter()
                    .map(|segment| (segment.start_ms, segment.end_ms, segment.text))
                    .collect(),
            },
        );
    }
    Ok(out)
}

/// `video.edit`.
pub async fn edit(ctx: &Ctx<'_>) -> Result<HostOutcome, HostFailure> {
    Ok(edit_inner(ctx).await.unwrap_or_else(|outcome| outcome))
}

#[allow(clippy::too_many_lines)]
async fn edit_inner(ctx: &Ctx<'_>) -> Result<HostOutcome, HostOutcome> {
    let limits = &ctx.env.config.video;
    let artifact_id = ctx.arg_str("artifact_id").unwrap_or_default().to_string();
    let expected = ctx
        .dispatch
        .args
        .get("expected_revision")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let dry_run = ctx
        .dispatch
        .args
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let raw = ctx.dispatch.args.get("ops").cloned().unwrap_or(Value::Null);
    if raw
        .as_array()
        .is_some_and(|ops| ops.len() > limits.max_ops_per_batch)
    {
        return Err(refuse(
            "VALIDATION_BOUNDS",
            "output.video.op.batch",
            format!("a batch is at most {} operations", limits.max_ops_per_batch),
        ));
    }
    let batch: Vec<RawOp> = serde_json::from_value(raw).map_err(|error| {
        refuse(
            "VALIDATION_UNKNOWN_FIELD",
            ops::RULE_FIELDS,
            format!("the operations are not the typed ones: {error}"),
        )
    })?;
    if batch.is_empty() {
        return Err(refuse(
            "VALIDATION_SCHEMA",
            ops::RULE_MISSING,
            "a batch has at least one operation",
        ));
    }

    let loaded = read_source(
        ctx,
        &artifact_id,
        ArtifactKind::Other,
        Some(TIMELINE_MEDIA_TYPE),
    )
    .await?;
    let current = parse_timeline(ctx, &loaded.bytes)?;
    let seq = u64::try_from(loaded.version.seq).unwrap_or(0);
    if current.revision != seq {
        return Err(refuse(
            "CONFLICT_STATE",
            "output.video.revision",
            format!(
                "version {seq} of this timeline says it is revision {}: it was not made by video.edit",
                current.revision
            ),
        ));
    }
    let transcripts = resolve_transcripts(ctx, &ops::transcripts_read(&batch)).await?;
    let id = ArtifactId::parse(&artifact_id)
        .map_err(|_| refuse("VALIDATION_SCHEMA", "artifact.id", "not an artifact id"))?;

    if expected != seq {
        // The one stale call that is not a conflict: a retry of a batch that already landed.
        if expected >= 1 && expected < seq {
            let base_seq = i32::try_from(expected).unwrap_or(i32::MAX);
            if let (Some((_, base_bytes)), Some((_, next_bytes))) = (
                read_seq(ctx, &id, base_seq).await?,
                read_seq(ctx, &id, base_seq + 1).await?,
            ) {
                let base = parse_timeline(ctx, &base_bytes)?;
                if let Ok(mut again) = ops::apply_batch(&base, &batch, limits, &transcripts) {
                    again.timeline.revision = expected + 1;
                    if again.timeline.to_bytes() == next_bytes {
                        let (version, _) =
                            read_seq(ctx, &id, base_seq + 1).await?.ok_or_else(|| {
                                refuse("INTERNAL", "output.video.revision", "the version vanished")
                            })?;
                        return Ok(success(
                            json!({
                                "artifact_id": artifact_id,
                                "revision": expected + 1,
                                "applied": batch.len(),
                                "dry_run": false,
                                "replayed": true,
                                "version_id": version.id.to_canonical(),
                                "duration_ms": again.timeline.duration_ms(),
                                "summary": again.summary.iter().map(|s| json!({"op": s.op, "detail": s.detail})).collect::<Vec<_>>(),
                                "trust": Trust::AgentGenerated.as_str(),
                            }),
                            Vec::new(),
                            Some(version.id.to_canonical()),
                        ));
                    }
                }
            }
        }
        return Err(conflict(expected, seq));
    }

    let applied = ops::apply_batch(&current, &batch, limits, &transcripts)
        .map_err(|error| op_refusal(&error))?;
    // The media this batch imported must be real artifacts of this workspace, of the kind they claim.
    for media in &applied.imported {
        let artifact = artifact_info(ctx, &media.artifact_id).await?;
        let wanted = match media.kind {
            MediaKind::Video => ArtifactKind::Video,
            MediaKind::Audio => ArtifactKind::Audio,
            MediaKind::Image => ArtifactKind::Image,
        };
        if artifact.kind != wanted {
            return Err(refuse_with(
                "VALIDATION_SCHEMA",
                ops::RULE_TARGET,
                format!(
                    "medium {} is {} but {} is a {} artifact, so no operation was applied",
                    media.id,
                    media.kind.as_str(),
                    media.artifact_id,
                    artifact.kind.as_str()
                ),
                Some(json!({ "media_id": media.id })),
            ));
        }
    }
    let mut next = applied.timeline;
    next.revision = seq + 1;
    next.validate(limits)
        .map_err(|detail| refuse("VALIDATION_SCHEMA", ops::RULE_TIMELINE, detail))?;
    let summary: Vec<Value> = applied
        .summary
        .iter()
        .map(|line| json!({ "op": line.op, "detail": line.detail }))
        .collect();
    let used_transcripts = !transcripts.is_empty();
    let trust = if used_transcripts {
        Trust::UntrustedExternal
    } else {
        Trust::AgentGenerated
    };
    if dry_run {
        return Ok(success(
            json!({
                "artifact_id": artifact_id,
                "revision": seq,
                "applied": batch.len(),
                "dry_run": true,
                "duration_ms": next.duration_ms(),
                "summary": summary,
                "trust": trust.as_str(),
            }),
            Vec::new(),
            None,
        ));
    }
    let mut record = QualityRecord::new(EDIT, trust);
    record.inputs.push(loaded.input());
    record.checks.push(Check::passed(
        "output.video.edit.atomic",
        format!("{} operation(s) applied as one batch", batch.len()),
    ));
    record.checks.push(Check::passed(
        "output.video.edit.revision",
        format!("the batch was made against revision {seq}, which was current"),
    ));
    record.checks.push(Check::passed(
        "output.video.edit.timeline",
        format!(
            "{} media, {} clip(s), {} subtitle(s), {} ms",
            next.media.len(),
            next.clips().count(),
            next.subtitles.len(),
            next.duration_ms()
        ),
    ));
    let committed = add_version_to(ctx, &id, TIMELINE_MEDIA_TYPE, next.to_bytes(), &record).await?;
    let version_id = committed.version.id.to_canonical();
    Ok(success(
        json!({
            "artifact_id": artifact_id,
            "revision": seq + 1,
            "applied": batch.len(),
            "dry_run": false,
            "replayed": committed.replayed,
            "version_id": version_id,
            "duration_ms": next.duration_ms(),
            "summary": summary,
            "trust": trust.as_str(),
        }),
        Vec::new(),
        Some(version_id),
    ))
}

// ------------------------------------------------------------------------------------------------

/// Cancels a job on the port when the call that started it is dropped before it finished.
struct CancelOnDrop {
    port: Arc<dyn MediaJobPort>,
    job_id: String,
    armed: bool,
}

impl CancelOnDrop {
    fn new(port: Arc<dyn MediaJobPort>, job_id: String) -> Self {
        Self {
            port,
            job_id,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.armed {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let (port, job_id) = (Arc::clone(&self.port), self.job_id.clone());
                handle.spawn(async move { port.cancel(&job_id).await });
            }
        }
    }
}

fn extension_of(media_type: &str) -> &'static str {
    match media_type {
        "video/mp4" => "mp4",
        "video/quicktime" => "mov",
        "video/webm" => "webm",
        "video/x-matroska" => "mkv",
        "audio/mpeg" => "mp3",
        "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
        "audio/mp4" | "audio/x-m4a" | "audio/aac" => "m4a",
        "audio/ogg" => "ogg",
        "audio/flac" => "flac",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "bin",
    }
}

fn mp4_signature(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[4..8] == b"ftyp"
}

/// `video.render`.
pub async fn render(ctx: &Ctx<'_>) -> Result<HostOutcome, HostFailure> {
    Ok(render_inner(ctx).await.unwrap_or_else(|outcome| outcome))
}

#[allow(clippy::too_many_lines)]
async fn render_inner(ctx: &Ctx<'_>) -> Result<HostOutcome, HostOutcome> {
    let config = &ctx.env.config;
    let artifact_id = ctx.arg_str("artifact_id").unwrap_or_default().to_string();
    let expected = ctx
        .dispatch
        .args
        .get("expected_revision")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let Some(port) = ctx.env.media.clone() else {
        return Err(ffmpeg_missing(
            ctx,
            "no media worker is composed on this deployment, so nothing can be rendered",
        ));
    };
    let loaded = read_source(
        ctx,
        &artifact_id,
        ArtifactKind::Other,
        Some(TIMELINE_MEDIA_TYPE),
    )
    .await?;
    let timeline = parse_timeline(ctx, &loaded.bytes)?;
    let seq = u64::try_from(loaded.version.seq).unwrap_or(0);
    if timeline.revision != seq {
        return Err(refuse(
            "CONFLICT_STATE",
            "output.video.revision",
            format!(
                "version {seq} of this timeline says it is revision {}",
                timeline.revision
            ),
        ));
    }
    if expected != seq {
        return Err(conflict(expected, seq));
    }
    let tool = port
        .toolchain()
        .await
        .map_err(|error| port_failure(ctx, error))?;

    // Stage the media the clips cut from, each read through the artifact store as the caller.
    let mut staged: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut record = QualityRecord::new(RENDER, Trust::AgentGenerated);
    record.inputs.push(loaded.input());
    let mut total = 0u64;
    for medium in &timeline.media {
        if !timeline.clips().any(|(_, clip)| clip.media_id == medium.id) {
            continue;
        }
        let wanted = match medium.kind {
            MediaKind::Video => ArtifactKind::Video,
            MediaKind::Audio => ArtifactKind::Audio,
            MediaKind::Image => ArtifactKind::Image,
        };
        let source = read_source(ctx, &medium.artifact_id, wanted, None).await?;
        total += source.bytes.len() as u64;
        if total > config.video.render.max_input_bytes {
            return Err(refuse(
                "VALIDATION_BOUNDS",
                "output.video.render.input",
                format!(
                    "the media a render reads is at most {} bytes",
                    config.video.render.max_input_bytes
                ),
            ));
        }
        let extension = extension_of(&source.version.media_type);
        staged.insert(
            medium.id.clone(),
            (
                extension.to_string(),
                source.version.content_digest.to_string(),
            ),
        );
        record.inputs.push(source.input());
        files.push((format!("m_{}.{extension}", medium.id), source.bytes));
    }
    let plan: RenderPlan = render_plan(&timeline, &staged)
        .map_err(|detail| refuse("VALIDATION_SCHEMA", "output.video.render.plan", detail))?;
    let spec = RenderJobSpec {
        job_id: plan.job_id.clone(),
        inputs: files
            .into_iter()
            .map(|(name, bytes)| MediaFile { name, bytes })
            .collect(),
        files: plan
            .files
            .iter()
            .map(|(name, text)| MediaFile {
                name: name.clone(),
                bytes: text.clone().into_bytes(),
            })
            .collect(),
        argv: plan.argv.clone(),
        output: plan.output.clone(),
        timeout_ms: config.video.render.timeout_ms,
        max_output_bytes: config.video.render.max_output_bytes,
    };
    let mut guard = CancelOnDrop::new(Arc::clone(&port), plan.job_id.clone());
    let rendered = port.render(spec).await;
    guard.disarm();
    let rendered = rendered.map_err(|error| port_failure(ctx, error))?;

    let tolerance = config.video.duration_tolerance_ms;
    let probe = &rendered.probe;
    record
        .environment
        .insert("ffmpeg".to_string(), Value::String(tool));
    record
        .environment
        .insert("job_id".to_string(), Value::String(plan.job_id.clone()));
    record.checks.push(Check::of(
        "output.video.render.probe",
        probe.duration_ms.abs_diff(plan.duration_ms) <= tolerance
            && (probe.width, probe.height) == (plan.width, plan.height),
        format!(
            "the file is {} ms at {}x{}; the timeline is {} ms at {}x{} (tolerance {tolerance} ms)",
            probe.duration_ms, probe.width, probe.height, plan.duration_ms, plan.width, plan.height
        ),
    ));
    record.checks.push(Check::of(
        "output.video.render.streams",
        probe.video_streams == 1
            && probe.audio_streams >= 1
            && probe.subtitle_streams == u32::from(plan.subtitles),
        format!(
            "{} video, {} audio and {} subtitle stream(s); expected 1, at least 1 and {}",
            probe.video_streams,
            probe.audio_streams,
            probe.subtitle_streams,
            u32::from(plan.subtitles)
        ),
    ));
    record.checks.push(Check::of(
        "output.video.render.container",
        mp4_signature(&rendered.bytes),
        "the file is an MP4 (an ftyp box opens it)".to_string(),
    ));
    record.checks.push(Check::of(
        "output.video.render.size",
        rendered.bytes.len() as u64 <= config.video.render.max_output_bytes,
        format!("{} bytes", rendered.bytes.len()),
    ));
    if let Some(failure) = record.first_failure() {
        return Err(refuse_with(
            "VALIDATION_SCHEMA",
            record
                .failures()
                .first()
                .map_or(RENDER, |check| check.rule.as_str()),
            format!("the render failed its checks, so no version was made: {failure}"),
            Some(json!({ "checks": record.checks.iter().map(Check::to_json).collect::<Vec<_>>() })),
        ));
    }
    let size = rendered.bytes.len();
    let committed = commit_output(
        ctx,
        OutputSpec {
            role: format!("video_render:{artifact_id}"),
            kind: ArtifactKind::Video,
            title: format!("{} (render)", loaded.artifact.title),
            media_type: "video/mp4".to_string(),
            bytes: rendered.bytes,
            record,
        },
    )
    .await?;
    let version_id = committed.version.id.to_canonical();
    Ok(success(
        json!({
            "artifact_id": committed.artifact_id.to_canonical(),
            "version_id": version_id,
            "version": committed.version.seq,
            "content_digest": committed.version.content_digest.to_string(),
            "size_bytes": size,
            "duration_ms": probe.duration_ms,
            "width": probe.width,
            "height": probe.height,
            "job_id": plan.job_id,
            "replayed": committed.replayed,
            "trust": Trust::AgentGenerated.as_str(),
        }),
        Vec::new(),
        Some(version_id),
    ))
}

/// `video.inspect`.
pub async fn inspect(ctx: &Ctx<'_>) -> Result<HostOutcome, HostFailure> {
    Ok(inspect_inner(ctx).await.unwrap_or_else(|outcome| outcome))
}

#[allow(clippy::too_many_lines)]
async fn inspect_inner(ctx: &Ctx<'_>) -> Result<HostOutcome, HostOutcome> {
    let config = &ctx.env.config;
    let limits = &config.video.inspect;
    let artifact_id = ctx.arg_str("artifact_id").unwrap_or_default().to_string();
    let times: Vec<u64> = ctx
        .dispatch
        .args
        .get("times_ms")
        .and_then(Value::as_array)
        .map(|times| times.iter().filter_map(Value::as_u64).collect())
        .unwrap_or_default();
    if times.is_empty() || times.len() > limits.max_frames {
        return Err(refuse(
            "VALIDATION_BOUNDS",
            "output.video.inspect.frames",
            format!("an inspection looks at 1 to {} times", limits.max_frames),
        ));
    }
    if times
        .iter()
        .any(|time| *time > config.video.max_duration_ms)
    {
        return Err(refuse(
            "VALIDATION_BOUNDS",
            "output.video.inspect.frames",
            format!("a time is at most {} ms", config.video.max_duration_ms),
        ));
    }
    let Some(port) = ctx.env.media.clone() else {
        return Err(ffmpeg_missing(
            ctx,
            "no media worker is composed on this deployment, so no frame can be extracted",
        ));
    };
    let video = read_source(ctx, &artifact_id, ArtifactKind::Video, None).await?;
    let tool = port
        .toolchain()
        .await
        .map_err(|error| port_failure(ctx, error))?;
    let mut identity = String::new();
    identity.push_str(video.version.content_digest.as_str());
    for time in &times {
        identity.push_str(&format!(":{time}"));
    }
    identity.push_str(&format!(":{}:{}", limits.thumbnail_width, limits.columns));
    let job_id = Digest::of(identity.as_bytes()).to_string();
    let spec = InspectJobSpec {
        job_id: job_id.clone(),
        input: MediaFile {
            name: format!("in.{}", extension_of(&video.version.media_type)),
            bytes: video.bytes.clone(),
        },
        times_ms: times.clone(),
        thumbnail_width: limits.thumbnail_width,
        columns: limits.columns,
        timeout_ms: limits.timeout_ms,
    };
    let mut guard = CancelOnDrop::new(Arc::clone(&port), job_id);
    let inspected = port.inspect(spec).await;
    guard.disarm();
    let inspected = inspected.map_err(|error| port_failure(ctx, error))?;

    let mut record = QualityRecord::new(INSPECT, Trust::UntrustedExternal);
    record.inputs.push(video.input());
    record
        .environment
        .insert("ffmpeg".to_string(), Value::String(tool));
    let first = inspected.frames.first();
    let decoded = inspected.frames.len() == times.len()
        && inspected
            .frames
            .iter()
            .zip(&times)
            .all(|(frame, time)| frame.time_ms == *time)
        && inspected.frames.iter().all(|frame| {
            frame.width == limits.thumbnail_width
                && frame.height > 0
                && frame.digest.len() == 64
                && first
                    .is_some_and(|first| (frame.width, frame.height) == (first.width, first.height))
        })
        && inspected.sheet.starts_with(b"\x89PNG\r\n\x1a\n")
        && inspected.sheet_width >= limits.thumbnail_width;
    record.checks.push(Check::of(
        "output.video.inspect.frames",
        decoded,
        format!(
            "{} frame(s) for {} time(s), each a picture of the same size; the sheet is {}x{}",
            inspected.frames.len(),
            times.len(),
            inspected.sheet_width,
            inspected.sheet_height
        ),
    ));
    let flat = inspected
        .frames
        .iter()
        .filter(|frame| frame.uniform)
        .count();
    record.checks.push(Check::of(
        "output.video.inspect.content",
        flat < inspected.frames.len(),
        format!(
            "{flat} of {} frame(s) are a single flat colour",
            inspected.frames.len()
        ),
    ));
    // The frames must exist for an inspection to mean anything; a flat video is reported, not refused.
    if !decoded {
        return Err(refuse_with(
            "VALIDATION_SCHEMA",
            "output.video.inspect.frames",
            "the worker did not return one picture for each time asked, so there is nothing to show",
            Some(json!({ "checks": record.checks.iter().map(Check::to_json).collect::<Vec<_>>() })),
        ));
    }
    let evidence_id = record_screenshot(
        ctx,
        inspected.sheet.clone(),
        format!(
            "a contact sheet of {} frame(s) of {artifact_id}",
            times.len()
        ),
        INSPECT,
    )
    .await?;
    record.evidence_ids.push(evidence_id.clone());
    let frames: Vec<Value> = inspected
        .frames
        .iter()
        .map(|frame| json!({ "time_ms": frame.time_ms, "digest": frame.digest, "uniform": frame.uniform }))
        .collect();
    let committed = commit_output(
        ctx,
        OutputSpec {
            role: format!("video_contact_sheet:{artifact_id}"),
            kind: ArtifactKind::Image,
            title: format!("Contact sheet of {}", video.artifact.title),
            media_type: "image/png".to_string(),
            bytes: inspected.sheet,
            record,
        },
    )
    .await?;
    let version_id = committed.version.id.to_canonical();
    Ok(success(
        json!({
            "artifact_id": committed.artifact_id.to_canonical(),
            "version_id": version_id,
            "version": committed.version.seq,
            "content_digest": committed.version.content_digest.to_string(),
            "width": inspected.sheet_width,
            "height": inspected.sheet_height,
            "frames": frames,
            "evidence_ids": [evidence_id.clone()],
            "replayed": committed.replayed,
            "trust": Trust::UntrustedExternal.as_str(),
        }),
        vec![evidence_id],
        Some(version_id),
    ))
}
