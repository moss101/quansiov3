//! The video timeline: media imported by id, tracks of clips, keyframes and subtitles (ADP-006,
//! DOMAIN.md §10.4 D).
//!
//! A timeline is a versioned source, not a render: an Artifact whose version number is its
//! **revision**. It is closed (an unknown key is refused) and every invariant is checked wherever a
//! timeline is read or made: clips stay inside their media, never overlap on a track and never ask
//! for sound from a picture track, and everything fits the bounds in `config/outputs.yaml`.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::artifacts::outputs::config::Video as Limits;

/// The media type of a timeline.
pub const MEDIA_TYPE: &str = "application/vnd.quansio.video-timeline+json";

/// `[a-z][a-z0-9_]{0,31}`.
#[must_use]
pub fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && id.len() <= 32
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// The kind of a medium.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    /// Moving pictures, with or without sound.
    Video,
    /// Sound.
    Audio,
    /// A still picture.
    Image,
}

impl MediaKind {
    /// The canonical spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Image => "image",
        }
    }
}

/// The kind of a track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    /// Holds video and image clips.
    Video,
    /// Holds audio clips.
    Audio,
}

impl TrackKind {
    /// The canonical spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Video => "video",
            Self::Audio => "audio",
        }
    }

    /// Whether a track of this kind may hold a clip of that medium.
    #[must_use]
    pub const fn holds(self, media: MediaKind) -> bool {
        matches!(
            (self, media),
            (Self::Video, MediaKind::Video | MediaKind::Image) | (Self::Audio, MediaKind::Audio)
        )
    }
}

/// The picture the render makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Output {
    /// Pixels; even.
    pub width: u32,
    /// Pixels; even.
    pub height: u32,
    /// Frames per second.
    pub fps: u32,
}

/// A medium the timeline may cut from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Media {
    /// The timeline's id for it.
    pub id: String,
    /// The Artifact that holds the bytes.
    pub artifact_id: String,
    /// What it is.
    pub kind: MediaKind,
    /// How long it plays (for an image: how long it may be shown).
    pub duration_ms: u64,
    /// Whether a video medium carries sound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_audio: Option<bool>,
}

/// A keyframe: the clip's volume at a time since its start.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Keyframe {
    /// Milliseconds since the clip's start on the timeline.
    pub at_ms: u64,
    /// Always `volume`.
    pub property: String,
    /// 0 to 4.
    pub value: f64,
}

/// A clip: a stretch of a medium placed on a track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clip {
    /// The timeline's id for it.
    pub id: String,
    /// The medium it cuts from.
    pub media_id: String,
    /// Where it starts on the timeline.
    pub start_ms: u64,
    /// Where in the medium it starts.
    pub in_ms: u64,
    /// Where in the medium it ends.
    pub out_ms: u64,
    /// Its volume (default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<f64>,
    /// Its volume over time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keyframes: Vec<Keyframe>,
}

impl Clip {
    /// How long it plays.
    #[must_use]
    pub const fn length_ms(&self) -> u64 {
        self.out_ms.saturating_sub(self.in_ms)
    }

    /// Where it ends on the timeline.
    #[must_use]
    pub const fn end_ms(&self) -> u64 {
        self.start_ms + self.length_ms()
    }
}

/// A track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Track {
    /// The timeline's id for it.
    pub id: String,
    /// What it holds.
    pub kind: TrackKind,
    /// Its clips.
    pub clips: Vec<Clip>,
}

/// Where a subtitle came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubtitleSource {
    /// The transcript Artifact.
    pub artifact_id: String,
    /// The version of it that was read.
    pub version_id: String,
    /// The segment.
    pub index: u64,
}

/// A subtitle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Subtitle {
    /// The timeline's id for it.
    pub id: String,
    /// When it appears.
    pub start_ms: u64,
    /// When it goes.
    pub end_ms: u64,
    /// What it says (plain text; from a transcript it is untrusted data).
    pub text: String,
    /// The transcript segment it came from, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SubtitleSource>,
}

/// A timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timeline {
    /// Always `v1`.
    pub schema_version: String,
    /// The version number this timeline is (§10.4 D).
    pub revision: u64,
    /// What the render makes.
    pub output: Output,
    /// What it cuts from.
    pub media: Vec<Media>,
    /// Where it puts it.
    pub tracks: Vec<Track>,
    /// What it says.
    pub subtitles: Vec<Subtitle>,
}

impl Timeline {
    /// An empty timeline at revision 1.
    #[must_use]
    pub fn empty(output: Output) -> Self {
        Self {
            schema_version: "v1".to_string(),
            revision: 1,
            output,
            media: Vec::new(),
            tracks: Vec::new(),
            subtitles: Vec::new(),
        }
    }

    /// Parse and validate.
    ///
    /// # Errors
    /// A sentence naming what is wrong.
    pub fn parse(bytes: &[u8], limits: &Limits) -> Result<Self, String> {
        let timeline: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        timeline.validate(limits)?;
        Ok(timeline)
    }

    /// The canonical bytes a version stores.
    ///
    /// # Panics
    /// Never: the structure serializes.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a timeline serializes")
    }

    /// The medium with this id.
    #[must_use]
    pub fn medium(&self, id: &str) -> Option<&Media> {
        self.media.iter().find(|media| media.id == id)
    }

    /// Every clip, with the track it is on.
    pub fn clips(&self) -> impl Iterator<Item = (&Track, &Clip)> {
        self.tracks
            .iter()
            .flat_map(|track| track.clips.iter().map(move |clip| (track, clip)))
    }

    /// How long the timeline plays: the end of its last clip or subtitle.
    #[must_use]
    pub fn duration_ms(&self) -> u64 {
        let clips = self
            .clips()
            .map(|(_, clip)| clip.end_ms())
            .max()
            .unwrap_or(0);
        let subtitles = self
            .subtitles
            .iter()
            .map(|subtitle| subtitle.end_ms)
            .max()
            .unwrap_or(0);
        clips.max(subtitles)
    }

    /// Check every invariant.
    ///
    /// # Errors
    /// A sentence naming the first thing wrong.
    pub fn validate(&self, limits: &Limits) -> Result<(), String> {
        if self.schema_version != "v1" {
            return Err("schema_version must be v1".to_string());
        }
        if self.revision == 0 {
            return Err("revision starts at 1".to_string());
        }
        let Output { width, height, fps } = self.output;
        let dimension = 16..=limits.max_dimension;
        if !dimension.contains(&width)
            || !dimension.contains(&height)
            || width % 2 != 0
            || height % 2 != 0
        {
            return Err(format!(
                "the output is {width}x{height}; each side is an even number from 16 to {}",
                limits.max_dimension
            ));
        }
        if fps == 0 || fps > limits.max_fps {
            return Err(format!("fps is 1 to {}", limits.max_fps));
        }
        if self.media.len() > limits.max_media {
            return Err(format!("a timeline has at most {} media", limits.max_media));
        }
        if self.tracks.len() > limits.max_tracks {
            return Err(format!(
                "a timeline has at most {} tracks",
                limits.max_tracks
            ));
        }
        if self.subtitles.len() > limits.max_subtitles {
            return Err(format!(
                "a timeline has at most {} subtitles",
                limits.max_subtitles
            ));
        }
        let mut media_ids = BTreeSet::new();
        for media in &self.media {
            if !valid_id(&media.id) || !media_ids.insert(media.id.as_str()) {
                return Err(format!("{:?} is not a unique media id", media.id));
            }
            if !media.artifact_id.starts_with("art_") {
                return Err(format!(
                    "media {} names {:?}, which is not an artifact id",
                    media.id, media.artifact_id
                ));
            }
            if media.duration_ms == 0 || media.duration_ms > limits.max_duration_ms {
                return Err(format!(
                    "media {} has a duration of 1 to {} ms",
                    media.id, limits.max_duration_ms
                ));
            }
            if media.has_audio.is_some() && media.kind != MediaKind::Video {
                return Err(format!("media {}: has_audio is for video", media.id));
            }
        }
        let mut track_ids = BTreeSet::new();
        let mut clip_ids = BTreeSet::new();
        let mut clip_count = 0usize;
        for track in &self.tracks {
            if !valid_id(&track.id) || !track_ids.insert(track.id.as_str()) {
                return Err(format!("{:?} is not a unique track id", track.id));
            }
            let mut spans: Vec<(u64, u64, &str)> = Vec::new();
            for clip in &track.clips {
                clip_count += 1;
                if !valid_id(&clip.id) || !clip_ids.insert(clip.id.as_str()) {
                    return Err(format!("{:?} is not a unique clip id", clip.id));
                }
                let medium = self.medium(&clip.media_id).ok_or_else(|| {
                    format!(
                        "clip {} cuts from {}, which is not imported",
                        clip.id, clip.media_id
                    )
                })?;
                if !track.kind.holds(medium.kind) {
                    return Err(format!(
                        "clip {} is {} media on {} track {}",
                        clip.id,
                        medium.kind.as_str(),
                        track.kind.as_str(),
                        track.id
                    ));
                }
                if clip.in_ms >= clip.out_ms || clip.out_ms > medium.duration_ms {
                    return Err(format!(
                        "clip {} cuts {}..{} ms from a medium of {} ms",
                        clip.id, clip.in_ms, clip.out_ms, medium.duration_ms
                    ));
                }
                if clip.end_ms() > limits.max_duration_ms {
                    return Err(format!(
                        "clip {} ends after {} ms",
                        clip.id, limits.max_duration_ms
                    ));
                }
                if let Some(volume) = clip.volume {
                    if !(0.0..=4.0).contains(&volume) {
                        return Err(format!("clip {} volume is 0 to 4", clip.id));
                    }
                }
                check_keyframes(clip, limits)?;
                spans.push((clip.start_ms, clip.end_ms(), clip.id.as_str()));
            }
            spans.sort_unstable();
            for pair in spans.windows(2) {
                if pair[0].1 > pair[1].0 {
                    return Err(format!(
                        "clips {} and {} overlap on track {}",
                        pair[0].2, pair[1].2, track.id
                    ));
                }
            }
        }
        if clip_count > limits.max_clips {
            return Err(format!("a timeline has at most {} clips", limits.max_clips));
        }
        let mut subtitle_ids = BTreeSet::new();
        for subtitle in &self.subtitles {
            if !valid_id(&subtitle.id) || !subtitle_ids.insert(subtitle.id.as_str()) {
                return Err(format!("{:?} is not a unique subtitle id", subtitle.id));
            }
            if subtitle.start_ms >= subtitle.end_ms || subtitle.end_ms > limits.max_duration_ms {
                return Err(format!(
                    "subtitle {} runs {}..{} ms, inside 0..{}",
                    subtitle.id, subtitle.start_ms, subtitle.end_ms, limits.max_duration_ms
                ));
            }
            let length = subtitle.text.chars().count();
            if subtitle.text.trim().is_empty()
                || length > 500
                || subtitle.text.chars().any(|c| c.is_control() && c != '\n')
            {
                return Err(format!(
                    "subtitle {} is 1 to 500 characters of plain text",
                    subtitle.id
                ));
            }
        }
        if self.duration_ms() > limits.max_duration_ms {
            return Err(format!(
                "a timeline is at most {} ms long",
                limits.max_duration_ms
            ));
        }
        Ok(())
    }
}

fn check_keyframes(clip: &Clip, limits: &Limits) -> Result<(), String> {
    if clip.keyframes.len() > limits.max_keyframes_per_clip {
        return Err(format!(
            "clip {} has at most {} keyframes",
            clip.id, limits.max_keyframes_per_clip
        ));
    }
    let mut previous: Option<u64> = None;
    for keyframe in &clip.keyframes {
        if keyframe.property != "volume" {
            return Err(format!(
                "clip {}: {:?} is not a keyframe property",
                clip.id, keyframe.property
            ));
        }
        if !(0.0..=4.0).contains(&keyframe.value) || !keyframe.value.is_finite() {
            return Err(format!("clip {}: a volume keyframe is 0 to 4", clip.id));
        }
        if keyframe.at_ms > clip.length_ms() {
            return Err(format!(
                "clip {}: a keyframe is after the clip ends",
                clip.id
            ));
        }
        if previous.is_some_and(|last| keyframe.at_ms <= last) {
            return Err(format!(
                "clip {}: keyframes are in strictly increasing time",
                clip.id
            ));
        }
        previous = Some(keyframe.at_ms);
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::artifacts::outputs::config::OutputsConfig;

    pub(crate) fn limits() -> Limits {
        OutputsConfig::shipped().video
    }

    pub(crate) fn sample() -> Timeline {
        Timeline {
            schema_version: "v1".to_string(),
            revision: 3,
            output: Output {
                width: 640,
                height: 360,
                fps: 25,
            },
            media: vec![
                Media {
                    id: "talk".into(),
                    artifact_id: "art_V".into(),
                    kind: MediaKind::Video,
                    duration_ms: 20_000,
                    has_audio: Some(true),
                },
                Media {
                    id: "music".into(),
                    artifact_id: "art_A".into(),
                    kind: MediaKind::Audio,
                    duration_ms: 30_000,
                    has_audio: None,
                },
                Media {
                    id: "logo".into(),
                    artifact_id: "art_I".into(),
                    kind: MediaKind::Image,
                    duration_ms: 10_000,
                    has_audio: None,
                },
            ],
            tracks: vec![
                Track {
                    id: "v1".into(),
                    kind: TrackKind::Video,
                    clips: vec![
                        Clip {
                            id: "c1".into(),
                            media_id: "talk".into(),
                            start_ms: 0,
                            in_ms: 2_000,
                            out_ms: 7_000,
                            volume: None,
                            keyframes: vec![],
                        },
                        Clip {
                            id: "c2".into(),
                            media_id: "logo".into(),
                            start_ms: 5_000,
                            in_ms: 0,
                            out_ms: 3_000,
                            volume: None,
                            keyframes: vec![],
                        },
                    ],
                },
                Track {
                    id: "a1".into(),
                    kind: TrackKind::Audio,
                    clips: vec![Clip {
                        id: "c3".into(),
                        media_id: "music".into(),
                        start_ms: 0,
                        in_ms: 0,
                        out_ms: 8_000,
                        volume: Some(0.5),
                        keyframes: vec![
                            Keyframe {
                                at_ms: 0,
                                property: "volume".into(),
                                value: 0.0,
                            },
                            Keyframe {
                                at_ms: 1_000,
                                property: "volume".into(),
                                value: 0.5,
                            },
                        ],
                    }],
                },
            ],
            subtitles: vec![Subtitle {
                id: "s1".into(),
                start_ms: 500,
                end_ms: 2_500,
                text: "Hello".into(),
                source: None,
            }],
        }
    }

    #[test]
    fn a_sound_timeline_validates_round_trips_and_knows_how_long_it_plays() {
        let timeline = sample();
        timeline.validate(&limits()).expect("valid");
        let bytes = timeline.to_bytes();
        assert_eq!(Timeline::parse(&bytes, &limits()).expect("parse"), timeline);
        assert_eq!(timeline.duration_ms(), 8_000);
        assert_eq!(timeline.clips().count(), 3);
        assert_eq!(
            timeline.medium("logo").map(|m| m.kind),
            Some(MediaKind::Image)
        );
    }

    type Edit = Box<dyn Fn(&mut Timeline)>;

    #[test]
    fn every_invariant_is_checked() {
        let limits = limits();
        let broken: Vec<(&str, Edit)> = vec![
            (
                "wrong version",
                Box::new(|t| t.schema_version = "v2".into()),
            ),
            ("revision 0", Box::new(|t| t.revision = 0)),
            ("odd width", Box::new(|t| t.output.width = 641)),
            ("tiny width", Box::new(|t| t.output.width = 8)),
            ("huge fps", Box::new(|t| t.output.fps = 1_000)),
            ("zero fps", Box::new(|t| t.output.fps = 0)),
            (
                "duplicate media id",
                Box::new(|t| t.media[1].id = "talk".into()),
            ),
            ("bad media id", Box::new(|t| t.media[0].id = "Talk".into())),
            (
                "media artifact id",
                Box::new(|t| t.media[0].artifact_id = "nope".into()),
            ),
            ("zero duration", Box::new(|t| t.media[0].duration_ms = 0)),
            (
                "has_audio on audio",
                Box::new(|t| t.media[1].has_audio = Some(true)),
            ),
            (
                "duplicate track",
                Box::new(|t| t.tracks[1].id = "v1".into()),
            ),
            (
                "duplicate clip",
                Box::new(|t| t.tracks[1].clips[0].id = "c1".into()),
            ),
            (
                "unknown medium",
                Box::new(|t| t.tracks[0].clips[0].media_id = "nope".into()),
            ),
            (
                "audio on a video track",
                Box::new(|t| t.tracks[0].clips[0].media_id = "music".into()),
            ),
            (
                "video on an audio track",
                Box::new(|t| t.tracks[1].clips[0].media_id = "talk".into()),
            ),
            (
                "out before in",
                Box::new(|t| t.tracks[0].clips[0].out_ms = 1_000),
            ),
            (
                "out past the medium",
                Box::new(|t| t.tracks[0].clips[0].out_ms = 25_000),
            ),
            (
                "overlap",
                Box::new(|t| t.tracks[0].clips[1].start_ms = 4_000),
            ),
            (
                "volume too loud",
                Box::new(|t| t.tracks[1].clips[0].volume = Some(9.0)),
            ),
            (
                "keyframe property",
                Box::new(|t| t.tracks[1].clips[0].keyframes[0].property = "opacity".into()),
            ),
            (
                "keyframes out of order",
                Box::new(|t| t.tracks[1].clips[0].keyframes[1].at_ms = 0),
            ),
            (
                "keyframe after the end",
                Box::new(|t| t.tracks[1].clips[0].keyframes[1].at_ms = 9_000),
            ),
            (
                "keyframe value",
                Box::new(|t| t.tracks[1].clips[0].keyframes[1].value = -1.0),
            ),
            (
                "subtitle backwards",
                Box::new(|t| t.subtitles[0].end_ms = 100),
            ),
            (
                "subtitle empty",
                Box::new(|t| t.subtitles[0].text = "  ".into()),
            ),
            (
                "subtitle control char",
                Box::new(|t| t.subtitles[0].text = "a\u{7}b".into()),
            ),
            (
                "subtitle too long",
                Box::new(|t| t.subtitles[0].text = "x".repeat(501)),
            ),
            (
                "duplicate subtitle",
                Box::new(|t| t.subtitles.push(t.subtitles[0].clone())),
            ),
            (
                "too long a timeline",
                Box::new(|t| t.subtitles[0].end_ms = 4_000_000),
            ),
        ];
        for (label, edit) in broken {
            let mut timeline = sample();
            edit(&mut timeline);
            assert!(
                timeline.validate(&limits).is_err(),
                "{label} should be refused"
            );
        }
    }

    #[test]
    fn bounds_come_from_the_configuration() {
        let mut limits = limits();
        limits.max_clips = 2;
        assert!(sample()
            .validate(&limits)
            .expect_err("clips")
            .contains("at most 2 clips"));
        limits.max_clips = 100;
        limits.max_tracks = 1;
        assert!(sample().validate(&limits).is_err());
    }

    #[test]
    fn an_unknown_key_is_refused_not_ignored() {
        let mut value: serde_json::Value =
            serde_json::from_slice(&sample().to_bytes()).expect("json");
        value["surprise"] = serde_json::json!(1);
        assert!(Timeline::parse(&serde_json::to_vec(&value).expect("bytes"), &limits()).is_err());
        let mut value: serde_json::Value =
            serde_json::from_slice(&sample().to_bytes()).expect("json");
        value["tracks"][0]["clips"][0]["opacity"] = serde_json::json!(0.5);
        assert!(Timeline::parse(&serde_json::to_vec(&value).expect("bytes"), &limits()).is_err());
    }

    #[test]
    fn ids_are_lower_case_words() {
        for good in ["a", "clip_1", "x9"] {
            assert!(valid_id(good), "{good}");
        }
        for bad in ["", "1a", "A", "a-b", "a b", &"a".repeat(33)] {
            assert!(!valid_id(bad), "{bad:?}");
        }
    }
}
