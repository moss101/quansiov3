//! Video against a **real `ffmpeg`** (ADP-006, DOMAIN.md §10.4 D): the real-boundary suite for rendering
//! and inspecting.
//!
//! Environment (documented here as AGENTS.md requires):
//!
//! * `QUANSIO_TEST_FFMPEG_BIN`: the path of an `ffmpeg` binary (with `libx264` and the `aac` encoder);
//!   `ffprobe` is looked for beside it. Without it every test here prints `BLOCKED_EXTERNAL` and
//!   returns: a host without `ffmpeg` is never a pass, and nothing stands in for it.
//! * `QUANSIO_TEST_POSTGRES_URL`, `QUANSIO_TEST_MINIO_*`: as `tests/artifacts.rs`.
//!
//! What it proves, with real media made by `ffmpeg` itself (`lavfi` test sources) and read back by
//! `ffprobe` and `ffmpeg`, never by this repository's own parser: a timeline of a video cut, a still and a
//! music bed with a volume ramp and a subtitle renders to an MP4 of the right length and size with one
//! video, an audio and a subtitle stream; the same plan is answered from the supervisor's record; and an
//! inspection's frames are the right pictures (the still is flat red, the test card is not).

use std::path::{Path, PathBuf};
use std::process::Command;

use quansio_qworkerd::media::MediaSupervisor;
use quansio_server::artifacts::outputs::video::timeline::{Output, Timeline};
use serde_json::{json, Value};

mod common;
use common::blocked_marker;
use common::outputs::{Harness, SupervisedMedia};

const TIMELINE: &str = "application/vnd.quansio.video-timeline+json";

fn ffmpeg() -> Option<PathBuf> {
    std::env::var_os("QUANSIO_TEST_FFMPEG_BIN")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
}

fn blocked(reason: &str) {
    eprintln!("BLOCKED_EXTERNAL: {reason}; the suite did not run");
}

/// Make a media file with `ffmpeg` and read its bytes.
fn make(ffmpeg: &Path, dir: &Path, name: &str, args: &[&str]) -> Vec<u8> {
    let path = dir.join(name);
    let mut command = Command::new(ffmpeg);
    command
        .args(["-y", "-hide_banner", "-loglevel", "error"])
        .args(args)
        .arg(&path);
    let status = command.status().expect("ffmpeg runs");
    assert!(status.success(), "ffmpeg could not make {name}");
    std::fs::read(path).expect("media bytes")
}

fn probe(ffmpeg: &Path, file: &Path) -> Value {
    let ffprobe = ffmpeg.with_file_name("ffprobe");
    let output = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(file)
        .output()
        .expect("ffprobe runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("ffprobe json")
}

#[tokio::test]
async fn a_timeline_renders_to_the_video_it_describes_and_the_frames_show_it() {
    let Some(ffmpeg) = ffmpeg() else {
        return blocked("QUANSIO_TEST_FFMPEG_BIN is not set");
    };
    let work = tempfile::tempdir().expect("work");
    let talk = make(
        &ffmpeg,
        work.path(),
        "talk.mp4",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=6:size=320x240:rate=25",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=6",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
        ],
    );
    let music = make(
        &ffmpeg,
        work.path(),
        "music.m4a",
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=220:duration=8",
            "-c:a",
            "aac",
        ],
    );
    let logo = make(
        &ffmpeg,
        work.path(),
        "logo.png",
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=red:s=320x240:d=1",
            "-frames:v",
            "1",
        ],
    );

    let supervisor = MediaSupervisor::new(work.path().join("managed"), Some(ffmpeg.clone()));
    let Some(h) = Harness::prepare("adp006_ffmpeg", |host| {
        host.with_media(SupervisedMedia::new(supervisor.clone()))
    })
    .await
    else {
        blocked_marker();
        return;
    };
    let video = h
        .create("video", "source", "Talk", "video/mp4", &talk)
        .await;
    let audio = h
        .create("audio", "source", "Music", "audio/mp4", &music)
        .await;
    let image = h
        .create("image", "source", "Logo", "image/png", &logo)
        .await;
    let timeline = h
        .create(
            "other",
            "video_timeline",
            "Cut",
            TIMELINE,
            &Timeline::empty(Output {
                width: 640,
                height: 360,
                fps: 25,
            })
            .to_bytes(),
        )
        .await;
    let edit = h
        .call(
            "video.edit",
            json!({
                "artifact_id": timeline,
                "expected_revision": 1,
                "dry_run": false,
                "ops": [
                    {"op": "add_media", "id": "talk", "artifact_id": video, "kind": "video", "duration_ms": 6000, "has_audio": true},
                    {"op": "add_media", "id": "music", "artifact_id": audio, "kind": "audio", "duration_ms": 8000},
                    {"op": "add_media", "id": "logo", "artifact_id": image, "kind": "image", "duration_ms": 5000},
                    {"op": "add_track", "id": "v1", "kind": "video"},
                    {"op": "add_track", "id": "a1", "kind": "audio"},
                    {"op": "add_clips", "track_id": "v1", "clips": [
                        {"id": "c1", "media_id": "talk", "start_ms": 0, "in_ms": 1000, "out_ms": 5000},
                        {"id": "c2", "media_id": "logo", "start_ms": 4000, "in_ms": 0, "out_ms": 2000}
                    ]},
                    {"op": "add_clips", "track_id": "a1", "clips": [{"id": "c3", "media_id": "music", "start_ms": 0, "in_ms": 0, "out_ms": 6000}]},
                    {"op": "set_keyframes", "clip_id": "c3", "keyframes": [{"at_ms": 0, "property": "volume", "value": 0.0}, {"at_ms": 2000, "property": "volume", "value": 0.6}]},
                    {"op": "add_subtitles", "subtitles": [{"id": "s1", "start_ms": 500, "end_ms": 2500, "text": "Hello from the render"}]},
                ],
            }),
        )
        .await;
    assert!(edit.ok(), "{edit:#?}");

    let rendered = h
        .call(
            "video.render",
            json!({ "artifact_id": timeline, "expected_revision": 2 }),
        )
        .await;
    assert!(rendered.ok(), "{rendered:#?}");
    let video_artifact = rendered.result["artifact_id"]
        .as_str()
        .expect("id")
        .to_string();
    let versions = h.versions(&video_artifact).await;
    assert_eq!(
        versions[0].quality_record["ok"], true,
        "{:#?}",
        versions[0].quality_record
    );

    // Read the file back with ffprobe, not with anything of ours.
    let bytes = h.bytes(&versions[0].id).await;
    let file = work.path().join("rendered.mp4");
    std::fs::write(&file, &bytes).expect("write");
    let described = probe(&ffmpeg, &file);
    let seconds: f64 = described["format"]["duration"]
        .as_str()
        .expect("duration")
        .parse()
        .expect("number");
    assert!(
        (seconds - 6.0).abs() < 0.25,
        "the file is {seconds} s, the timeline is 6 s"
    );
    let streams = described["streams"].as_array().expect("streams");
    let kinds: Vec<&str> = streams
        .iter()
        .map(|s| s["codec_type"].as_str().expect("type"))
        .collect();
    assert_eq!(
        kinds.iter().filter(|k| **k == "video").count(),
        1,
        "{kinds:?}"
    );
    assert!(
        kinds.iter().filter(|k| **k == "audio").count() >= 1,
        "{kinds:?}"
    );
    assert_eq!(
        kinds.iter().filter(|k| **k == "subtitle").count(),
        1,
        "{kinds:?}"
    );
    let picture = streams
        .iter()
        .find(|s| s["codec_type"] == "video")
        .expect("video stream");
    assert_eq!(
        (picture["width"].as_u64(), picture["height"].as_u64()),
        (Some(640), Some(360))
    );

    // The same plan again is answered from the supervisor's record.
    let again = h
        .call(
            "video.render",
            json!({ "artifact_id": timeline, "expected_revision": 2 }),
        )
        .await;
    assert!(again.ok(), "{again:#?}");
    assert_eq!(again.result["replayed"], true);

    // The frames: the test card at the start is not flat; the red still at the end is.
    let inspected = h
        .call(
            "video.inspect",
            json!({ "artifact_id": video_artifact, "times_ms": [500, 2000, 4500, 5500] }),
        )
        .await;
    assert!(inspected.ok(), "{inspected:#?}");
    let uniform: Vec<bool> = inspected.result["frames"]
        .as_array()
        .expect("frames")
        .iter()
        .map(|frame| frame["uniform"].as_bool().expect("uniform"))
        .collect();
    assert_eq!(
        uniform,
        [false, false, true, true],
        "the card is busy and the still is one red"
    );
    let sheet_artifact = inspected.result["artifact_id"].as_str().expect("id");
    let sheet = h.bytes(&h.versions(sheet_artifact).await[0].id).await;
    let sheet_file = work.path().join("sheet.png");
    std::fs::write(&sheet_file, &sheet).expect("write");
    let sheet_probe = probe(&ffmpeg, &sheet_file);
    assert_eq!(sheet_probe["streams"][0]["codec_name"], "png");
    // The red frame, decoded by ffmpeg itself: the middle of the last thumbnail is red.
    let pixel = Command::new(&ffmpeg)
        .args(["-v", "error", "-ss", "5.5", "-i"])
        .arg(&file)
        .args([
            "-frames:v",
            "1",
            "-vf",
            "crop=1:1:320:180",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "-",
        ])
        .output()
        .expect("ffmpeg decodes");
    assert!(pixel.status.success());
    assert_eq!(pixel.stdout.len(), 3);
    assert!(
        pixel.stdout[0] > 200 && pixel.stdout[1] < 60 && pixel.stdout[2] < 60,
        "{:?}",
        pixel.stdout
    );
    h.finish().await;
}
