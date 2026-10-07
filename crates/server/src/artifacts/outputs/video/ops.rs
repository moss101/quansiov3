//! Typed, atomic video edit operations (ADP-006, DOMAIN.md §10.4 D).
//!
//! A batch is applied to a **copy**: operations run in order, the timeline is validated after each,
//! and the first one that breaks a rule stops the batch with its index and the rule it broke. The
//! caller only sees a new timeline when every operation held, so a batch is all or none whether it is
//! stored (`dry_run: false`) or only reported (`dry_run: true`).
//!
//! The wire form is the flat object the Tool Registry's schema describes (every field optional, the
//! operation named by `op`); this module is where each operation's own fields are enforced: one that
//! carries a field its operation does not take, or lacks one it needs, is refused by name before
//! anything is applied.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use super::timeline::{
    valid_id, Clip, Keyframe, Media, MediaKind, Output, Subtitle, SubtitleSource, Timeline, Track,
    TrackKind,
};
use crate::artifacts::outputs::config::Video as Limits;

/// A refused operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpError {
    /// The operation's position in the batch.
    pub index: usize,
    /// The rule it broke.
    pub rule: &'static str,
    /// What was wrong.
    pub detail: String,
    /// Whether the refusal is about an unknown or misplaced field (`VALIDATION_UNKNOWN_FIELD`).
    pub field: bool,
}

/// An operation carries a field it does not take.
pub const RULE_FIELDS: &str = "output.video.op.fields";
/// An operation lacks a field it needs.
pub const RULE_MISSING: &str = "output.video.op.missing";
/// An operation names something that is not there.
pub const RULE_TARGET: &str = "output.video.op.target";
/// An operation would leave the timeline broken.
pub const RULE_TIMELINE: &str = "output.video.op.timeline";
/// An operation names a transcript that cannot be read.
pub const RULE_TRANSCRIPT: &str = "output.video.op.transcript";

/// A clip as an operation gives it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawClip {
    id: String,
    media_id: String,
    start_ms: u64,
    in_ms: u64,
    out_ms: u64,
    #[serde(default)]
    volume: Option<f64>,
}

/// A subtitle as an operation gives it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawSubtitle {
    id: String,
    start_ms: u64,
    end_ms: u64,
    text: String,
}

/// The wire form of an operation.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawOp {
    /// Which operation.
    pub op: String,
    id: Option<String>,
    media_id: Option<String>,
    track_id: Option<String>,
    clip_id: Option<String>,
    new_clip_id: Option<String>,
    artifact_id: Option<String>,
    transcript_artifact_id: Option<String>,
    id_prefix: Option<String>,
    kind: Option<String>,
    has_audio: Option<bool>,
    duration_ms: Option<u64>,
    at_ms: Option<u64>,
    start_ms: Option<u64>,
    in_ms: Option<u64>,
    out_ms: Option<u64>,
    offset_ms: Option<u64>,
    volume: Option<f64>,
    width: Option<u32>,
    height: Option<u32>,
    fps: Option<u32>,
    clip_ids: Option<Vec<String>>,
    ids: Option<Vec<String>>,
    clips: Option<Vec<RawClip>>,
    keyframes: Option<Vec<Keyframe>>,
    subtitles: Option<Vec<RawSubtitle>>,
}

/// What an operation takes: `(required, optional)`.
fn fields_of(op: &str) -> Option<(&'static [&'static str], &'static [&'static str])> {
    Some(match op {
        "add_media" => (
            &["id", "artifact_id", "kind", "duration_ms"],
            &["has_audio"],
        ),
        "remove_media" => (&["id"], &[]),
        "add_track" => (&["id", "kind"], &[]),
        "add_clips" => (&["track_id", "clips"], &[]),
        "trim_clip" => (&["clip_id"], &["in_ms", "out_ms"]),
        "split_clip" => (&["clip_id", "at_ms", "new_clip_id"], &[]),
        "move_clip" => (&["clip_id", "start_ms"], &["track_id"]),
        "remove_clips" => (&["clip_ids"], &[]),
        "set_keyframes" => (&["clip_id", "keyframes"], &[]),
        "set_volume" => (&["clip_id", "volume"], &[]),
        "add_subtitles" => (&["subtitles"], &[]),
        "remove_subtitles" => (&["ids"], &[]),
        "subtitles_from_transcript" => (&["transcript_artifact_id", "id_prefix"], &["offset_ms"]),
        "set_output" => (&[], &["width", "height", "fps"]),
        _ => return None,
    })
}

impl RawOp {
    fn present(&self) -> BTreeSet<&'static str> {
        let mut out = BTreeSet::new();
        macro_rules! has {
            ($($field:ident),*) => { $( if self.$field.is_some() { out.insert(stringify!($field)); } )* };
        }
        has!(
            id,
            media_id,
            track_id,
            clip_id,
            new_clip_id,
            artifact_id,
            transcript_artifact_id,
            id_prefix,
            kind,
            has_audio,
            duration_ms,
            at_ms,
            start_ms,
            in_ms,
            out_ms,
            offset_ms,
            volume,
            width,
            height,
            fps,
            clip_ids,
            ids,
            clips,
            keyframes,
            subtitles
        );
        out
    }

    /// Check the operation carries exactly the fields its kind takes.
    fn check_fields(&self, index: usize) -> Result<(), OpError> {
        let error = |rule, detail: String, field| OpError {
            index,
            rule,
            detail,
            field,
        };
        let Some((required, optional)) = fields_of(&self.op) else {
            return Err(error(
                RULE_FIELDS,
                format!("{:?} is not an operation", self.op),
                true,
            ));
        };
        let present = self.present();
        if let Some(extra) = present
            .iter()
            .find(|name| !required.contains(name) && !optional.contains(name))
        {
            return Err(error(
                RULE_FIELDS,
                format!("{} does not take {extra}", self.op),
                true,
            ));
        }
        if let Some(missing) = required.iter().find(|name| !present.contains(*name)) {
            return Err(error(
                RULE_MISSING,
                format!("{} needs {missing}", self.op),
                false,
            ));
        }
        if self.op == "trim_clip" && self.in_ms.is_none() && self.out_ms.is_none() {
            return Err(error(
                RULE_MISSING,
                "trim_clip needs in_ms or out_ms".to_string(),
                false,
            ));
        }
        if self.op == "set_output" && present.is_empty() {
            return Err(error(
                RULE_MISSING,
                "set_output needs width, height or fps".to_string(),
                false,
            ));
        }
        Ok(())
    }
}

/// A transcript an operation may read: its version and its segments.
#[derive(Debug, Clone)]
pub struct TranscriptSource {
    /// The version that was read.
    pub version_id: String,
    /// `(start_ms, end_ms, text)` of each segment, in order.
    pub segments: Vec<(u64, u64, String)>,
}

/// Transcripts by Artifact id, resolved by the host before the batch is applied.
pub type Transcripts = BTreeMap<String, TranscriptSource>;

/// What an operation did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// The operation.
    pub op: String,
    /// What it did.
    pub detail: String,
}

/// The result of a batch that held.
#[derive(Debug, Clone, PartialEq)]
pub struct Applied {
    /// The timeline after every operation.
    pub timeline: Timeline,
    /// One line per operation.
    pub summary: Vec<Summary>,
    /// The media this batch imported (their Artifacts must be checked by the caller).
    pub imported: Vec<Media>,
}

/// The Artifact ids of the transcripts a batch reads.
#[must_use]
pub fn transcripts_read(ops: &[RawOp]) -> Vec<String> {
    let mut out: Vec<String> = ops
        .iter()
        .filter(|op| op.op == "subtitles_from_transcript")
        .filter_map(|op| op.transcript_artifact_id.clone())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Apply a batch to a copy of `base`. The result is all of it or an error naming the first
/// operation that broke a rule.
///
/// # Errors
/// [`OpError`].
pub fn apply_batch(
    base: &Timeline,
    ops: &[RawOp],
    limits: &Limits,
    transcripts: &Transcripts,
) -> Result<Applied, OpError> {
    let mut work = base.clone();
    let mut summary = Vec::with_capacity(ops.len());
    let mut imported = Vec::new();
    for (index, op) in ops.iter().enumerate() {
        op.check_fields(index)?;
        let detail = apply_one(&mut work, op, index, limits, transcripts, &mut imported)?;
        work.validate(limits).map_err(|detail| OpError {
            index,
            rule: RULE_TIMELINE,
            detail,
            field: false,
        })?;
        summary.push(Summary {
            op: op.op.clone(),
            detail,
        });
    }
    Ok(Applied {
        timeline: work,
        summary,
        imported,
    })
}

fn err(index: usize, rule: &'static str, detail: impl Into<String>) -> OpError {
    OpError {
        index,
        rule,
        detail: detail.into(),
        field: false,
    }
}

fn text(value: &Option<String>) -> &str {
    value.as_deref().unwrap_or_default()
}

fn find_clip<'a>(timeline: &'a mut Timeline, id: &str) -> Option<(usize, usize, &'a mut Clip)> {
    for (track_index, track) in timeline.tracks.iter_mut().enumerate() {
        if let Some(clip_index) = track.clips.iter().position(|clip| clip.id == id) {
            return Some((track_index, clip_index, &mut track.clips[clip_index]));
        }
    }
    None
}

fn media_kind(value: &str) -> Option<MediaKind> {
    match value {
        "video" => Some(MediaKind::Video),
        "audio" => Some(MediaKind::Audio),
        "image" => Some(MediaKind::Image),
        _ => None,
    }
}

#[allow(clippy::too_many_lines)]
fn apply_one(
    timeline: &mut Timeline,
    op: &RawOp,
    index: usize,
    limits: &Limits,
    transcripts: &Transcripts,
    imported: &mut Vec<Media>,
) -> Result<String, OpError> {
    match op.op.as_str() {
        "add_media" => {
            let kind = media_kind(text(&op.kind)).ok_or_else(|| {
                err(
                    index,
                    RULE_TIMELINE,
                    format!("{:?} is not a kind of media", text(&op.kind)),
                )
            })?;
            let media = Media {
                id: text(&op.id).to_string(),
                artifact_id: text(&op.artifact_id).to_string(),
                kind,
                duration_ms: op.duration_ms.unwrap_or_default(),
                has_audio: op.has_audio,
            };
            timeline.media.push(media.clone());
            let detail = format!(
                "imported {} {} ({} ms)",
                kind.as_str(),
                media.id,
                media.duration_ms
            );
            imported.push(media);
            Ok(detail)
        }
        "remove_media" => {
            let id = text(&op.id);
            if timeline.medium(id).is_none() {
                return Err(err(index, RULE_TARGET, format!("there is no medium {id}")));
            }
            if let Some((_, clip)) = timeline.clips().find(|(_, clip)| clip.media_id == id) {
                return Err(err(
                    index,
                    RULE_TIMELINE,
                    format!("clip {} still cuts from {id}", clip.id),
                ));
            }
            timeline.media.retain(|media| media.id != id);
            imported.retain(|media| media.id != id);
            Ok(format!("removed medium {id}"))
        }
        "add_track" => {
            let kind = match text(&op.kind) {
                "video" => TrackKind::Video,
                "audio" => TrackKind::Audio,
                other => {
                    return Err(err(
                        index,
                        RULE_TIMELINE,
                        format!("{other:?} is not a kind of track"),
                    ))
                }
            };
            timeline.tracks.push(Track {
                id: text(&op.id).to_string(),
                kind,
                clips: Vec::new(),
            });
            Ok(format!("added {} track {}", kind.as_str(), text(&op.id)))
        }
        "add_clips" => {
            let id = text(&op.track_id);
            let track = timeline
                .tracks
                .iter_mut()
                .find(|track| track.id == id)
                .ok_or_else(|| err(index, RULE_TARGET, format!("there is no track {id}")))?;
            let clips = op.clips.as_deref().unwrap_or_default();
            for clip in clips {
                track.clips.push(Clip {
                    id: clip.id.clone(),
                    media_id: clip.media_id.clone(),
                    start_ms: clip.start_ms,
                    in_ms: clip.in_ms,
                    out_ms: clip.out_ms,
                    volume: clip.volume,
                    keyframes: Vec::new(),
                });
            }
            Ok(format!("added {} clip(s) to {id}", clips.len()))
        }
        "trim_clip" => {
            let id = text(&op.clip_id);
            let (_, _, clip) = find_clip(timeline, id)
                .ok_or_else(|| err(index, RULE_TARGET, format!("there is no clip {id}")))?;
            if let Some(start) = op.in_ms {
                // Trimming the head keeps the picture where it is: the clip starts later by as much.
                if start < clip.in_ms {
                    return Err(err(index, RULE_TIMELINE, "trim_clip cannot reveal footage before the clip's in point; move or re-add it"));
                }
                clip.start_ms += start - clip.in_ms;
                clip.in_ms = start;
            }
            if let Some(end) = op.out_ms {
                clip.out_ms = end;
            }
            let length = clip.length_ms();
            clip.keyframes.retain(|keyframe| keyframe.at_ms <= length);
            Ok(format!(
                "trimmed {id} to {}..{} ms of its medium",
                clip.in_ms, clip.out_ms
            ))
        }
        "split_clip" => {
            let id = text(&op.clip_id);
            let at = op.at_ms.unwrap_or_default();
            let new_id = text(&op.new_clip_id).to_string();
            let (track_index, clip_index, clip) = find_clip(timeline, id)
                .ok_or_else(|| err(index, RULE_TARGET, format!("there is no clip {id}")))?;
            if at <= clip.start_ms || at >= clip.end_ms() {
                return Err(err(
                    index,
                    RULE_TIMELINE,
                    format!(
                        "{at} ms is not inside clip {id} ({}..{} ms)",
                        clip.start_ms,
                        clip.end_ms()
                    ),
                ));
            }
            let cut = at - clip.start_ms;
            let mut second = clip.clone();
            second.id = new_id.clone();
            second.start_ms = at;
            second.in_ms = clip.in_ms + cut;
            second.keyframes = clip
                .keyframes
                .iter()
                .filter(|keyframe| keyframe.at_ms >= cut)
                .map(|keyframe| Keyframe {
                    at_ms: keyframe.at_ms - cut,
                    ..keyframe.clone()
                })
                .collect();
            clip.out_ms = clip.in_ms + cut;
            clip.keyframes.retain(|keyframe| keyframe.at_ms <= cut);
            timeline.tracks[track_index]
                .clips
                .insert(clip_index + 1, second);
            Ok(format!("split {id} at {at} ms into {id} and {new_id}"))
        }
        "move_clip" => {
            let id = text(&op.clip_id);
            let start = op.start_ms.unwrap_or_default();
            let (track_index, clip_index, _) = find_clip(timeline, id)
                .ok_or_else(|| err(index, RULE_TARGET, format!("there is no clip {id}")))?;
            let mut clip = timeline.tracks[track_index].clips.remove(clip_index);
            clip.start_ms = start;
            let target = match &op.track_id {
                None => track_index,
                Some(track) => timeline
                    .tracks
                    .iter()
                    .position(|candidate| candidate.id == *track)
                    .ok_or_else(|| err(index, RULE_TARGET, format!("there is no track {track}")))?,
            };
            timeline.tracks[target].clips.push(clip);
            Ok(format!(
                "moved {id} to {start} ms on {}",
                timeline.tracks[target].id
            ))
        }
        "remove_clips" => {
            let ids = op.clip_ids.as_deref().unwrap_or_default();
            let known: BTreeSet<&str> =
                timeline.clips().map(|(_, clip)| clip.id.as_str()).collect();
            if let Some(missing) = ids.iter().find(|id| !known.contains(id.as_str())) {
                return Err(err(
                    index,
                    RULE_TARGET,
                    format!("there is no clip {missing}"),
                ));
            }
            for track in &mut timeline.tracks {
                track.clips.retain(|clip| !ids.contains(&clip.id));
            }
            Ok(format!("removed {} clip(s)", ids.len()))
        }
        "set_keyframes" => {
            let id = text(&op.clip_id);
            let (_, _, clip) = find_clip(timeline, id)
                .ok_or_else(|| err(index, RULE_TARGET, format!("there is no clip {id}")))?;
            clip.keyframes = op.keyframes.clone().unwrap_or_default();
            Ok(format!("set {} keyframe(s) on {id}", clip.keyframes.len()))
        }
        "set_volume" => {
            let id = text(&op.clip_id);
            let (_, _, clip) = find_clip(timeline, id)
                .ok_or_else(|| err(index, RULE_TARGET, format!("there is no clip {id}")))?;
            clip.volume = op.volume;
            Ok(format!(
                "set the volume of {id} to {}",
                op.volume.unwrap_or_default()
            ))
        }
        "add_subtitles" => {
            let subtitles = op.subtitles.as_deref().unwrap_or_default();
            for subtitle in subtitles {
                timeline.subtitles.push(Subtitle {
                    id: subtitle.id.clone(),
                    start_ms: subtitle.start_ms,
                    end_ms: subtitle.end_ms,
                    text: subtitle.text.clone(),
                    source: None,
                });
            }
            Ok(format!("added {} subtitle(s)", subtitles.len()))
        }
        "remove_subtitles" => {
            let ids = op.ids.as_deref().unwrap_or_default();
            let known: BTreeSet<&str> = timeline.subtitles.iter().map(|s| s.id.as_str()).collect();
            if let Some(missing) = ids.iter().find(|id| !known.contains(id.as_str())) {
                return Err(err(
                    index,
                    RULE_TARGET,
                    format!("there is no subtitle {missing}"),
                ));
            }
            timeline
                .subtitles
                .retain(|subtitle| !ids.contains(&subtitle.id));
            Ok(format!("removed {} subtitle(s)", ids.len()))
        }
        "subtitles_from_transcript" => {
            let artifact = text(&op.transcript_artifact_id);
            let prefix = text(&op.id_prefix);
            if !valid_id(prefix) || prefix.len() > 24 {
                return Err(err(
                    index,
                    RULE_TIMELINE,
                    format!("{prefix:?} is not a subtitle id prefix"),
                ));
            }
            let source = transcripts.get(artifact).ok_or_else(|| {
                err(
                    index,
                    RULE_TRANSCRIPT,
                    format!("{artifact} is not a transcript this call can read"),
                )
            })?;
            let offset = op.offset_ms.unwrap_or(0);
            for (position, (start, end, words)) in source.segments.iter().enumerate() {
                let start = start + offset;
                let end = end + offset;
                if end > limits.max_duration_ms {
                    return Err(err(
                        index,
                        RULE_TIMELINE,
                        format!(
                            "segment {position} ends after {} ms",
                            limits.max_duration_ms
                        ),
                    ));
                }
                timeline.subtitles.push(Subtitle {
                    id: format!("{prefix}{}", position + 1),
                    start_ms: start,
                    end_ms: end,
                    text: words.trim().to_string(),
                    source: Some(SubtitleSource {
                        artifact_id: artifact.to_string(),
                        version_id: source.version_id.clone(),
                        index: position as u64,
                    }),
                });
            }
            Ok(format!(
                "added {} subtitle(s) from {artifact}, shifted {offset} ms",
                source.segments.len()
            ))
        }
        "set_output" => {
            let Output { width, height, fps } = timeline.output;
            timeline.output = Output {
                width: op.width.unwrap_or(width),
                height: op.height.unwrap_or(height),
                fps: op.fps.unwrap_or(fps),
            };
            Ok(format!(
                "the output is {}x{} at {} fps",
                timeline.output.width, timeline.output.height, timeline.output.fps
            ))
        }
        other => Err(OpError {
            index,
            rule: RULE_FIELDS,
            detail: format!("{other:?} is not an operation"),
            field: true,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::outputs::video::timeline::tests::{limits, sample};
    use serde_json::json;

    fn ops(value: serde_json::Value) -> Vec<RawOp> {
        serde_json::from_value(value).expect("ops")
    }

    fn run(timeline: &Timeline, value: serde_json::Value) -> Result<Applied, OpError> {
        apply_batch(timeline, &ops(value), &limits(), &Transcripts::new())
    }

    fn empty() -> Timeline {
        Timeline::empty(Output {
            width: 640,
            height: 360,
            fps: 25,
        })
    }

    #[test]
    fn a_timeline_is_built_from_nothing_by_one_batch() {
        let applied = run(
            &empty(),
            json!([
                {"op": "add_media", "id": "talk", "artifact_id": "art_V", "kind": "video", "duration_ms": 20000, "has_audio": true},
                {"op": "add_media", "id": "music", "artifact_id": "art_A", "kind": "audio", "duration_ms": 30000},
                {"op": "add_track", "id": "v1", "kind": "video"},
                {"op": "add_track", "id": "a1", "kind": "audio"},
                {"op": "add_clips", "track_id": "v1", "clips": [{"id": "c1", "media_id": "talk", "start_ms": 0, "in_ms": 1000, "out_ms": 9000}]},
                {"op": "add_clips", "track_id": "a1", "clips": [{"id": "c2", "media_id": "music", "start_ms": 0, "in_ms": 0, "out_ms": 8000, "volume": 0.4}]},
                {"op": "add_subtitles", "subtitles": [{"id": "s1", "start_ms": 0, "end_ms": 1500, "text": "Welcome"}]},
                {"op": "set_output", "width": 1280, "height": 720},
            ]),
        )
        .expect("applied");
        assert_eq!(applied.summary.len(), 8);
        assert_eq!(applied.imported.len(), 2);
        assert_eq!(applied.timeline.duration_ms(), 8_000);
        assert_eq!(
            applied.timeline.output,
            Output {
                width: 1280,
                height: 720,
                fps: 25
            }
        );
        assert_eq!(
            applied.summary[0],
            Summary {
                op: "add_media".into(),
                detail: "imported video talk (20000 ms)".into()
            }
        );
    }

    #[test]
    fn trim_split_move_and_remove_edit_a_clip_the_way_an_editor_expects() {
        let start = sample();
        // Trimming the head keeps the picture where it was: the clip starts later by as much.
        let trimmed = run(
            &start,
            json!([{"op": "trim_clip", "clip_id": "c1", "in_ms": 3000, "out_ms": 6000}]),
        )
        .expect("trim");
        let clip = &trimmed.timeline.tracks[0].clips[0];
        assert_eq!(
            (clip.start_ms, clip.in_ms, clip.out_ms),
            (1_000, 3_000, 6_000)
        );

        let split = run(
            &start,
            json!([{"op": "split_clip", "clip_id": "c3", "at_ms": 4000, "new_clip_id": "c3b"}]),
        )
        .expect("split");
        let clips = &split.timeline.tracks[1].clips;
        assert_eq!(clips.len(), 2);
        assert_eq!(
            (clips[0].start_ms, clips[0].in_ms, clips[0].out_ms),
            (0, 0, 4_000)
        );
        assert_eq!(
            (
                clips[1].id.as_str(),
                clips[1].start_ms,
                clips[1].in_ms,
                clips[1].out_ms
            ),
            ("c3b", 4_000, 4_000, 8_000)
        );
        assert_eq!(
            clips[0].keyframes.len(),
            2,
            "both keyframes are before the cut"
        );
        assert!(clips[1].keyframes.is_empty());
        assert_eq!(clips[0].volume, clips[1].volume);

        let moved = run(
            &start,
            json!([{"op": "move_clip", "clip_id": "c2", "start_ms": 9000}]),
        )
        .expect("move");
        assert_eq!(
            moved.timeline.tracks[0]
                .clips
                .iter()
                .find(|c| c.id == "c2")
                .map(|c| c.start_ms),
            Some(9_000)
        );
        let removed = run(&start, json!([{"op": "remove_clips", "clip_ids": ["c1", "c2"]}, {"op": "remove_media", "id": "logo"}])).expect("remove");
        assert!(removed.timeline.tracks[0].clips.is_empty());
        assert!(removed.timeline.medium("logo").is_none());
    }

    #[test]
    fn keyframes_and_volume_are_set_and_checked() {
        let applied = run(
            &sample(),
            json!([
                {"op": "set_keyframes", "clip_id": "c3", "keyframes": [{"at_ms": 0, "property": "volume", "value": 0.0}, {"at_ms": 2000, "property": "volume", "value": 1.0}]},
                {"op": "set_volume", "clip_id": "c3", "volume": 0.8},
            ]),
        )
        .expect("applied");
        let clip = &applied.timeline.tracks[1].clips[0];
        assert_eq!((clip.keyframes.len(), clip.volume), (2, Some(0.8)));
        assert!(run(&sample(), json!([{"op": "set_keyframes", "clip_id": "c3", "keyframes": [{"at_ms": 9999, "property": "volume", "value": 1.0}]}])).is_err());
        assert!(run(&sample(), json!([{"op": "set_keyframes", "clip_id": "c3", "keyframes": [{"at_ms": 0, "property": "opacity", "value": 1.0}]}])).is_err());
        assert!(run(
            &sample(),
            json!([{"op": "set_volume", "clip_id": "c3", "volume": 9.0}])
        )
        .is_err());
    }

    #[test]
    fn a_batch_is_all_or_none_and_names_the_operation_that_broke_it() {
        let base = sample();
        let error = run(
            &base,
            json!([
                {"op": "add_track", "id": "v2", "kind": "video"},
                {"op": "add_clips", "track_id": "v2", "clips": [{"id": "n1", "media_id": "talk", "start_ms": 0, "in_ms": 0, "out_ms": 4000}]},
                {"op": "add_clips", "track_id": "v2", "clips": [{"id": "n2", "media_id": "talk", "start_ms": 2000, "in_ms": 0, "out_ms": 4000}]},
                {"op": "set_output", "width": 1920},
            ]),
        )
        .expect_err("the third operation overlaps the second");
        assert_eq!((error.index, error.rule), (2, RULE_TIMELINE));
        assert!(error.detail.contains("overlap"), "{}", error.detail);
        // The base is untouched, and no partial result escapes.
        assert_eq!(base, sample());
    }

    #[test]
    fn each_operation_takes_its_own_fields_and_needs_them() {
        let refused = |value: serde_json::Value, rule: &str, field: bool| {
            let error = run(&sample(), value).expect_err("refused");
            assert_eq!(
                (error.rule, error.field),
                (rule_of(rule), field),
                "{error:?}"
            );
        };
        fn rule_of(name: &str) -> &'static str {
            match name {
                "fields" => RULE_FIELDS,
                "missing" => RULE_MISSING,
                "target" => RULE_TARGET,
                _ => RULE_TIMELINE,
            }
        }
        refused(json!([{"op": "frobnicate"}]), "fields", true);
        refused(
            json!([{"op": "add_track", "id": "x", "kind": "audio", "clips": []}]),
            "fields",
            true,
        );
        refused(
            json!([{"op": "remove_media", "id": "logo", "duration_ms": 1}]),
            "fields",
            true,
        );
        refused(json!([{"op": "add_track", "id": "x"}]), "missing", false);
        refused(
            json!([{"op": "trim_clip", "clip_id": "c1"}]),
            "missing",
            false,
        );
        refused(json!([{"op": "set_output"}]), "missing", false);
        refused(
            json!([{"op": "split_clip", "clip_id": "c1", "at_ms": 3000}]),
            "missing",
            false,
        );
        refused(
            json!([{"op": "trim_clip", "clip_id": "nope", "out_ms": 5}]),
            "target",
            false,
        );
        refused(
            json!([{"op": "remove_clips", "clip_ids": ["c1", "nope"]}]),
            "target",
            false,
        );
        refused(
            json!([{"op": "add_clips", "track_id": "nope", "clips": []}]),
            "target",
            false,
        );
        refused(
            json!([{"op": "split_clip", "clip_id": "c1", "at_ms": 0, "new_clip_id": "x"}]),
            "timeline",
            false,
        );
        refused(
            json!([{"op": "split_clip", "clip_id": "c1", "at_ms": 99999, "new_clip_id": "x"}]),
            "timeline",
            false,
        );
        refused(
            json!([{"op": "remove_media", "id": "talk"}]),
            "timeline",
            false,
        );
        refused(
            json!([{"op": "add_media", "id": "x", "artifact_id": "art_X", "kind": "hologram", "duration_ms": 5}]),
            "timeline",
            false,
        );
        refused(
            json!([{"op": "trim_clip", "clip_id": "c1", "in_ms": 1000}]),
            "timeline",
            false,
        );
        refused(
            json!([{"op": "move_clip", "clip_id": "c3", "start_ms": 0, "track_id": "v1"}]),
            "timeline",
            false,
        );
    }

    #[test]
    fn an_unknown_wire_field_is_not_even_parsed() {
        let parsed: Result<Vec<RawOp>, _> =
            serde_json::from_value(json!([{"op": "set_output", "width": 640, "opacity": 0.5}]));
        assert!(parsed.is_err());
    }

    #[test]
    fn subtitles_from_a_transcript_carry_their_source_and_stay_inside_the_limits() {
        let mut transcripts = Transcripts::new();
        transcripts.insert(
            "art_T".to_string(),
            TranscriptSource {
                version_id: "artv_T1".to_string(),
                segments: vec![
                    (0, 1_200, " Hello there ".to_string()),
                    (1_300, 2_900, "General Kenobi".to_string()),
                ],
            },
        );
        let applied = apply_batch(
            &empty(),
            &ops(json!([{"op": "subtitles_from_transcript", "transcript_artifact_id": "art_T", "id_prefix": "t", "offset_ms": 500}])),
            &limits(),
            &transcripts,
        )
        .expect("applied");
        let subs = &applied.timeline.subtitles;
        assert_eq!(subs.len(), 2);
        assert_eq!(
            (
                subs[0].id.as_str(),
                subs[0].start_ms,
                subs[0].end_ms,
                subs[0].text.as_str()
            ),
            ("t1", 500, 1_700, "Hello there")
        );
        assert_eq!(
            subs[1].source,
            Some(SubtitleSource {
                artifact_id: "art_T".into(),
                version_id: "artv_T1".into(),
                index: 1
            })
        );
        assert_eq!(
            transcripts_read(&ops(
                json!([{"op": "subtitles_from_transcript", "transcript_artifact_id": "art_T", "id_prefix": "t"}])
            )),
            vec!["art_T"]
        );
        // A transcript the call did not resolve, and a bad prefix.
        let missing = apply_batch(&empty(), &ops(json!([{"op": "subtitles_from_transcript", "transcript_artifact_id": "art_Z", "id_prefix": "t"}])), &limits(), &transcripts).expect_err("missing");
        assert_eq!(missing.rule, RULE_TRANSCRIPT);
        let bad_prefix = apply_batch(&empty(), &ops(json!([{"op": "subtitles_from_transcript", "transcript_artifact_id": "art_T", "id_prefix": "T-1"}])), &limits(), &transcripts).expect_err("prefix");
        assert_eq!(bad_prefix.rule, RULE_TIMELINE);
        // Shifting past the limit is refused rather than clipped.
        let late = apply_batch(&empty(), &ops(json!([{"op": "subtitles_from_transcript", "transcript_artifact_id": "art_T", "id_prefix": "t", "offset_ms": 3_599_000}])), &limits(), &transcripts).expect_err("late");
        assert_eq!(late.rule, RULE_TIMELINE);
    }

    #[test]
    fn removing_subtitles_and_media_added_in_the_same_batch() {
        let applied = run(
            &sample(),
            json!([
                {"op": "add_media", "id": "extra", "artifact_id": "art_X", "kind": "image", "duration_ms": 5000},
                {"op": "remove_media", "id": "extra"},
                {"op": "remove_subtitles", "ids": ["s1"]},
            ]),
        )
        .expect("applied");
        assert!(
            applied.imported.is_empty(),
            "an import that was removed again is not reported"
        );
        assert!(applied.timeline.subtitles.is_empty());
        assert!(run(
            &sample(),
            json!([{"op": "remove_subtitles", "ids": ["zz"]}])
        )
        .is_err());
    }
}
