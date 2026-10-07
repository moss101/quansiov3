//! From a timeline to the `ffmpeg` job that renders it (ADP-006, DOMAIN.md §10.4 D).
//!
//! The plan is **typed data**: a list of staged input files, two small text files (the filter graph and
//! the subtitles) and an argument vector. It is never a shell string. Every value that reaches an
//! argument is a number the plan formatted or a file name the plan chose (`m_<media id>.<ext>`, where
//! the id is `[a-z][a-z0-9_]*`), so nothing a person or a model wrote can become an option.
//!
//! The plan is deterministic, and its identity — the digest of everything that decides what the job
//! does, the bytes of the inputs included — is the job's id. Starting the same plan again means
//! attaching to the same job.

use std::collections::BTreeMap;

use sha2::{Digest as _, Sha256};

use super::timeline::{Clip, Keyframe, MediaKind, Timeline, TrackKind};
use crate::artifacts::outputs::deck::hex;

/// What the job reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanInput {
    /// The file name the input is staged under.
    pub file: String,
    /// The timeline's medium.
    pub media_id: String,
    /// What it is.
    pub kind: MediaKind,
    /// The digest of its bytes.
    pub digest: String,
}

/// A render job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderPlan {
    /// The digest of the plan: the job's identity.
    pub job_id: String,
    /// The inputs, in `-i` order.
    pub inputs: Vec<PlanInput>,
    /// Small text files the job reads, by name (`graph.txt`, `subs.srt`).
    pub files: Vec<(String, String)>,
    /// The argument vector, after the binary.
    pub argv: Vec<String>,
    /// The file the job writes.
    pub output: String,
    /// How long the render is, in milliseconds.
    pub duration_ms: u64,
    /// The picture's width.
    pub width: u32,
    /// The picture's height.
    pub height: u32,
    /// The frame rate.
    pub fps: u32,
    /// Whether the output carries a subtitle stream.
    pub subtitles: bool,
}

/// The file the render writes.
pub const OUTPUT: &str = "out.mp4";

fn seconds(ms: u64) -> String {
    format!("{}.{:03}", ms / 1000, ms % 1000)
}

fn number(value: f64) -> String {
    let text = format!("{value:.4}");
    let trimmed = text.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

/// The piecewise-linear volume of a clip as an `ffmpeg` expression of `t`, seconds since the clip's
/// own start: the first value before the first keyframe, the last after the last, straight lines
/// between.
fn volume_expression(keyframes: &[Keyframe]) -> String {
    let points: Vec<(f64, f64)> = keyframes
        .iter()
        .map(|keyframe| (keyframe.at_ms as f64 / 1000.0, keyframe.value))
        .collect();
    let (_, last) = points[points.len() - 1];
    let mut expression = number(last);
    for window in points.windows(2).rev() {
        let ((t0, v0), (t1, v1)) = (window[0], window[1]);
        let slope = (v1 - v0) / (t1 - t0);
        expression = format!(
            "if(lt(t,{}),{}+{}*(t-{}),{expression})",
            number(t1),
            number(v0),
            number(slope),
            number(t0)
        );
    }
    format!(
        "if(lt(t,{}),{},{expression})",
        number(points[0].0),
        number(points[0].1)
    )
}

fn srt_time(ms: u64) -> String {
    format!(
        "{:02}:{:02}:{:02},{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

/// The subtitles as an SRT file.
#[must_use]
pub fn srt(timeline: &Timeline) -> String {
    let mut ordered: Vec<_> = timeline.subtitles.iter().collect();
    ordered.sort_by_key(|subtitle| (subtitle.start_ms, subtitle.end_ms));
    let mut out = String::new();
    for (position, subtitle) in ordered.iter().enumerate() {
        out.push_str(&format!(
            "{}\n{} --> {}\n{}\n\n",
            position + 1,
            srt_time(subtitle.start_ms),
            srt_time(subtitle.end_ms),
            subtitle.text.replace('\r', "").trim()
        ));
    }
    out
}

/// Build the render plan.
///
/// `staged` maps a medium's id to `(file extension, digest of its bytes)` for every medium a clip cuts
/// from.
///
/// # Errors
/// A sentence naming what makes the timeline unrenderable.
pub fn render_plan(
    timeline: &Timeline,
    staged: &BTreeMap<String, (String, String)>,
) -> Result<RenderPlan, String> {
    let duration = timeline.duration_ms();
    if timeline.clips().next().is_none() {
        return Err("the timeline has no clips to render".to_string());
    }
    if duration == 0 {
        return Err("the timeline is empty".to_string());
    }
    // Inputs, in the order the timeline lists its media, for the media a clip uses.
    let mut inputs: Vec<PlanInput> = Vec::new();
    let mut index_of: BTreeMap<&str, usize> = BTreeMap::new();
    for media in &timeline.media {
        if !timeline.clips().any(|(_, clip)| clip.media_id == media.id) {
            continue;
        }
        let (extension, digest) = staged
            .get(&media.id)
            .ok_or_else(|| format!("medium {} was not staged", media.id))?;
        index_of.insert(media.id.as_str(), inputs.len());
        inputs.push(PlanInput {
            file: format!("m_{}.{extension}", media.id),
            media_id: media.id.clone(),
            kind: media.kind,
            digest: digest.clone(),
        });
    }

    let (width, height, fps) = (
        timeline.output.width,
        timeline.output.height,
        timeline.output.fps,
    );
    let mut graph: Vec<String> = Vec::new();
    graph.push(format!(
        "color=c=black:s={width}x{height}:r={fps}:d={}[base0]",
        seconds(duration)
    ));
    let mut layer = 0usize;
    let mut audio_labels: Vec<String> = Vec::new();
    let clips: Vec<(TrackKind, &Clip)> = timeline
        .clips()
        .map(|(track, clip)| (track.kind, clip))
        .collect();
    for (clip_number, (kind, clip)) in clips.into_iter().enumerate() {
        let medium = timeline
            .medium(&clip.media_id)
            .ok_or_else(|| format!("clip {} cuts from an unknown medium", clip.id))?;
        let input = index_of[clip.media_id.as_str()];
        let (start, end) = (clip.start_ms, clip.end_ms());
        if kind == TrackKind::Video {
            graph.push(format!(
                "[{input}:v:0]trim=start={}:end={},setpts=PTS-STARTPTS,fps={fps},\
scale={width}:{height}:force_original_aspect_ratio=decrease,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:color=black,\
setsar=1,format=yuv420p,setpts=PTS+{}/TB[v{clip_number}]",
                seconds(clip.in_ms),
                seconds(clip.out_ms),
                seconds(start)
            ));
            graph.push(format!(
                "[base{layer}][v{clip_number}]overlay=eof_action=pass:enable='between(t,{},{})'[base{}]",
                seconds(start),
                seconds(end),
                layer + 1
            ));
            layer += 1;
        }
        let carries_sound = match kind {
            TrackKind::Audio => true,
            TrackKind::Video => medium.kind == MediaKind::Video && medium.has_audio == Some(true),
        };
        let silent = clip.keyframes.is_empty() && clip.volume == Some(0.0);
        if carries_sound && !silent {
            let volume = if clip.keyframes.is_empty() {
                match clip.volume {
                    Some(volume) if (volume - 1.0).abs() > f64::EPSILON => {
                        format!(",volume={}", number(volume))
                    }
                    _ => String::new(),
                }
            } else {
                format!(
                    ",volume='{}':eval=frame",
                    volume_expression(&clip.keyframes)
                )
            };
            graph.push(format!(
                "[{input}:a:0]atrim=start={}:end={},asetpts=PTS-STARTPTS,\
aformat=sample_rates=48000:channel_layouts=stereo{volume},adelay={start}|{start}[a{clip_number}]",
                seconds(clip.in_ms),
                seconds(clip.out_ms)
            ));
            audio_labels.push(format!("[a{clip_number}]"));
        }
    }
    graph.push(format!("[base{layer}]format=yuv420p[vout]"));
    let total = seconds(duration);
    graph.push(match audio_labels.len() {
        0 => format!("anullsrc=channel_layout=stereo:sample_rate=48000,atrim=0:{total},asetpts=PTS-STARTPTS[aout]"),
        1 => format!("{}apad=whole_dur={total},atrim=0:{total}[aout]", audio_labels[0]),
        count => format!(
            "{}amix=inputs={count}:duration=longest:dropout_transition=0:normalize=0,apad=whole_dur={total},atrim=0:{total}[aout]",
            audio_labels.concat()
        ),
    });
    let graph_text = format!("{}\n", graph.join(";\n"));

    let mut argv: Vec<String> = [
        "-y",
        "-hide_banner",
        "-nostdin",
        "-nostats",
        "-progress",
        "progress.txt",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    for input in &inputs {
        if input.kind == MediaKind::Image {
            let medium = timeline.medium(&input.media_id).expect("input medium");
            argv.extend(
                ["-loop", "1", "-framerate"]
                    .into_iter()
                    .map(str::to_string)
                    .chain([
                        fps.to_string(),
                        "-t".to_string(),
                        seconds(medium.duration_ms),
                    ]),
            );
        }
        argv.extend(["-i".to_string(), input.file.clone()]);
    }
    let subtitles = !timeline.subtitles.is_empty();
    if subtitles {
        argv.extend(["-i".to_string(), "subs.srt".to_string()]);
    }
    argv.extend(
        [
            "-filter_complex_script",
            "graph.txt",
            "-map",
            "[vout]",
            "-map",
            "[aout]",
        ]
        .into_iter()
        .map(str::to_string),
    );
    if subtitles {
        argv.extend(
            ["-map".to_string(), format!("{}:s", inputs.len())]
                .into_iter()
                .chain(
                    ["-c:s", "mov_text", "-metadata:s:s:0", "language=eng"]
                        .into_iter()
                        .map(str::to_string),
                ),
        );
    }
    argv.extend(
        [
            "-c:v", "libx264", "-preset", "medium", "-crf", "20", "-pix_fmt", "yuv420p", "-r",
        ]
        .into_iter()
        .map(str::to_string)
        .chain([
            fps.to_string(),
            "-c:a".to_string(),
            "aac".to_string(),
            "-b:a".to_string(),
            "160k".to_string(),
            "-ar".to_string(),
            "48000".to_string(),
            "-ac".to_string(),
            "2".to_string(),
            "-movflags".to_string(),
            "+faststart".to_string(),
            "-t".to_string(),
            total,
            OUTPUT.to_string(),
        ]),
    );

    let mut files = vec![("graph.txt".to_string(), graph_text)];
    if subtitles {
        files.push(("subs.srt".to_string(), srt(timeline)));
    }
    let mut hasher = Sha256::new();
    for part in &argv {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    for (name, text) in &files {
        hasher.update(name.as_bytes());
        hasher.update([0]);
        hasher.update(text.as_bytes());
        hasher.update([0]);
    }
    for input in &inputs {
        hasher.update(input.digest.as_bytes());
        hasher.update([0]);
    }
    Ok(RenderPlan {
        job_id: hex(&hasher.finalize()),
        inputs,
        files,
        argv,
        output: OUTPUT.to_string(),
        duration_ms: duration,
        width,
        height,
        fps,
        subtitles,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::outputs::video::timeline::tests::sample;

    fn staged(timeline: &Timeline) -> BTreeMap<String, (String, String)> {
        timeline
            .media
            .iter()
            .map(|media| {
                let ext = match media.kind {
                    MediaKind::Video => "mp4",
                    MediaKind::Audio => "mp3",
                    MediaKind::Image => "png",
                };
                (
                    media.id.clone(),
                    (ext.to_string(), format!("{:0>64}", media.id)),
                )
            })
            .collect()
    }

    #[test]
    fn the_plan_is_the_golden_one() {
        let timeline = sample();
        let plan = render_plan(&timeline, &staged(&timeline)).expect("plan");
        let rendered = format!(
            "job {}\ninputs\n{}\nargv\n{}\n--- graph.txt\n{}--- subs.srt\n{}",
            plan.job_id,
            plan.inputs
                .iter()
                .map(|i| format!("  {} {} {}", i.file, i.media_id, i.digest))
                .collect::<Vec<_>>()
                .join("\n"),
            plan.argv
                .iter()
                .map(|a| format!("  {a}"))
                .collect::<Vec<_>>()
                .join("\n"),
            plan.files[0].1,
            plan.files[1].1,
        );
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/artifacts/outputs/video/testdata/render-plan.golden.txt");
        if std::env::var("QUANSIO_UPDATE_GOLDEN").as_deref() == Ok("1") {
            std::fs::create_dir_all(path.parent().expect("dir")).expect("mkdir");
            std::fs::write(&path, &rendered).expect("write golden");
        }
        assert_eq!(
            rendered,
            std::fs::read_to_string(&path).expect("golden; run with QUANSIO_UPDATE_GOLDEN=1")
        );
    }

    #[test]
    fn the_plan_is_deterministic_and_its_identity_follows_what_decides_the_job() {
        let timeline = sample();
        let a = render_plan(&timeline, &staged(&timeline)).expect("a");
        assert_eq!(
            a,
            render_plan(&timeline, &staged(&timeline)).expect("again")
        );
        // The revision is not part of what the job does; a changed clip, an input's bytes or the output are.
        let mut renumbered = timeline.clone();
        renumbered.revision += 1;
        assert_eq!(
            a.job_id,
            render_plan(&renumbered, &staged(&renumbered))
                .expect("b")
                .job_id
        );
        let mut trimmed = timeline.clone();
        trimmed.tracks[0].clips[0].out_ms = 6_000;
        assert_ne!(
            a.job_id,
            render_plan(&trimmed, &staged(&trimmed)).expect("c").job_id
        );
        let mut other_bytes = staged(&timeline);
        other_bytes.get_mut("talk").expect("talk").1 = "f".repeat(64);
        assert_ne!(
            a.job_id,
            render_plan(&timeline, &other_bytes).expect("d").job_id
        );
        let mut bigger = timeline.clone();
        bigger.output.width = 1280;
        assert_ne!(
            a.job_id,
            render_plan(&bigger, &staged(&bigger)).expect("e").job_id
        );
    }

    #[test]
    fn nothing_a_person_wrote_reaches_an_argument() {
        let mut timeline = sample();
        // A subtitle full of option-looking text is a line in a file, not an argument.
        timeline.subtitles[0].text = "-i /etc/passwd -f lavfi; rm -rf /".to_string();
        let plan = render_plan(&timeline, &staged(&timeline)).expect("plan");
        assert!(
            plan.argv
                .iter()
                .all(|arg| !arg.contains("passwd") && !arg.contains("rm -rf")),
            "{:?}",
            plan.argv
        );
        assert!(plan.files[1].1.contains("-i /etc/passwd"));
        // File names are chosen by the plan from ids that are lower-case words.
        assert!(plan.inputs.iter().all(|input| input.file.starts_with("m_")
            && !input.file.contains('/')
            && !input.file.contains("..")));
        // Every argument is either a fixed word, a number or one of the plan's own file names.
        for arg in &plan.argv {
            let known = arg.starts_with('-')
                || arg.chars().all(|c| c.is_ascii_digit() || c == '.')
                || arg.contains(".txt")
                || arg.contains(".srt")
                || arg.contains(".mp4")
                || arg.contains(".mp3")
                || arg.contains(".png")
                || arg.starts_with('[')
                || arg.ends_with(":s")
                || [
                    "libx264",
                    "medium",
                    "yuv420p",
                    "aac",
                    "160k",
                    "48000",
                    "+faststart",
                    "mov_text",
                    "language=eng",
                    "1",
                    "20",
                ]
                .contains(&arg.as_str());
            assert!(known, "an argument that is not the plan's own: {arg:?}");
        }
    }

    #[test]
    fn what_cannot_be_rendered_is_refused_before_a_job_exists() {
        let mut timeline = sample();
        timeline
            .tracks
            .iter_mut()
            .for_each(|track| track.clips.clear());
        assert!(render_plan(&timeline, &staged(&timeline))
            .expect_err("none")
            .contains("no clips"));
        let timeline = sample();
        let mut incomplete = staged(&timeline);
        incomplete.remove("talk");
        assert!(render_plan(&timeline, &incomplete)
            .expect_err("unstaged")
            .contains("not staged"));
    }

    #[test]
    fn a_timeline_without_sound_gets_a_silent_track_and_one_with_two_sources_is_mixed() {
        let mut quiet = sample();
        quiet.tracks.remove(1);
        quiet.media[0].has_audio = Some(false);
        let plan = render_plan(&quiet, &staged(&quiet)).expect("plan");
        assert!(plan.files[0].1.contains("anullsrc"));
        let mixed = render_plan(&sample(), &staged(&sample())).expect("mixed");
        // The talk video has sound and so does the music: two sources, one mix.
        assert!(mixed.files[0].1.contains("amix=inputs=2"));
        let mut muted = sample();
        muted.tracks[0].clips[0].volume = Some(0.0);
        let plan = render_plan(&muted, &staged(&muted)).expect("muted");
        assert!(
            !plan.files[0].1.contains("amix"),
            "a clip at volume 0 contributes no sound"
        );
    }

    #[test]
    fn keyframes_become_one_piecewise_linear_expression() {
        let keyframes = vec![
            Keyframe {
                at_ms: 0,
                property: "volume".into(),
                value: 0.0,
            },
            Keyframe {
                at_ms: 1_000,
                property: "volume".into(),
                value: 1.0,
            },
            Keyframe {
                at_ms: 3_000,
                property: "volume".into(),
                value: 0.5,
            },
        ];
        let expression = volume_expression(&keyframes);
        assert_eq!(
            expression,
            "if(lt(t,0),0,if(lt(t,1),0+1*(t-0),if(lt(t,3),1+-0.25*(t-1),0.5)))"
        );
        assert_eq!(volume_expression(&keyframes[..1]), "if(lt(t,0),0,0)");
    }

    #[test]
    fn subtitles_are_written_in_time_order_with_srt_times() {
        let mut timeline = sample();
        timeline.subtitles.push(super::super::timeline::Subtitle {
            id: "s0".into(),
            start_ms: 100,
            end_ms: 3_725_005,
            text: "Early\r\nsecond line".into(),
            source: None,
        });
        let text = srt(&timeline);
        assert_eq!(
            text,
            "1\n00:00:00,100 --> 01:02:05,005\nEarly\nsecond line\n\n2\n00:00:00,500 --> 00:00:02,500\nHello\n\n"
        );
    }

    #[test]
    fn numbers_and_seconds_are_formatted_without_surprises() {
        assert_eq!(seconds(0), "0.000");
        assert_eq!(seconds(61_007), "61.007");
        assert_eq!(number(0.5), "0.5");
        assert_eq!(number(1.0), "1");
        assert_eq!(number(0.0), "0");
        assert_eq!(number(-0.25), "-0.25");
        assert_eq!(number(0.123_456), "0.1235");
    }
}
