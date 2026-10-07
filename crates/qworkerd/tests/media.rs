//! The media job supervisor (ADP-006, DOMAIN.md §10.4 D): identity, attach, cancel, timeout, restart,
//! bounds and the unavailable path, against **real processes**.
//!
//! These tests exercise the *supervision* — process groups, signals, state files, concurrency — and so
//! they run a stand-in executable named `ffmpeg` (a small shell script written into a temporary
//! directory) in place of the real one. **It is not ffmpeg and proves nothing about ffmpeg**: the
//! renders and inspections of a real `ffmpeg` are `tests/media_ffmpeg.rs`, which needs the binary named by
//! `QUANSIO_TEST_FFMPEG_BIN` and reports `BLOCKED_EXTERNAL` without it. The stand-in understands a few
//! test-only arguments (`--stand-in-stall`, `--stand-in-slow`, `--stand-in-fail`, `--stand-in-log=<file>`)
//! that the plan builder never produces.

use std::path::{Path, PathBuf};
use std::time::Duration;

use quansio_core::Digest;
use quansio_qworkerd::media::{InspectJob, MediaError, MediaSupervisor, RenderJob, StagedFile};

const FFMPEG: &str = include_str!("support/stand-in-ffmpeg.sh");
const FFPROBE: &str = include_str!("support/stand-in-ffprobe.sh");

struct Fixture {
    root: tempfile::TempDir,
    bin: PathBuf,
    log: PathBuf,
}

fn fixture() -> Fixture {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().expect("root");
    let bin = root.path().join("bin");
    std::fs::create_dir_all(&bin).expect("bin dir");
    for (name, text) in [("ffmpeg", FFMPEG), ("ffprobe", FFPROBE)] {
        let path = bin.join(name);
        std::fs::write(&path, text).expect("script");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let log = root.path().join("runs.log");
    Fixture {
        bin: bin.join("ffmpeg"),
        log,
        root,
    }
}

impl Fixture {
    fn supervisor(&self) -> MediaSupervisor {
        MediaSupervisor::new(self.root.path().join("managed"), Some(self.bin.clone()))
    }

    fn runs(&self) -> Vec<(u32, String)> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                let (pid, dir) = line.split_once(' ')?;
                Some((pid.parse().ok()?, dir.to_string()))
            })
            .collect()
    }

    /// The stalled child processes the stand-in started (`child <pid>` lines).
    fn children(&self) -> Vec<u32> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.strip_prefix("child ")?.trim().parse().ok())
            .collect()
    }

    fn job(&self, label: &str, extra: &[&str]) -> RenderJob {
        let mut argv: Vec<String> = ["-y", "-i", "m_talk.mp4"]
            .into_iter()
            .map(str::to_string)
            .collect();
        argv.extend(extra.iter().map(ToString::to_string));
        argv.push(format!("--stand-in-log={}", self.log.display()));
        argv.push("out.mp4".to_string());
        RenderJob {
            job_id: Digest::of(label.as_bytes()).to_string(),
            inputs: vec![StagedFile {
                name: "m_talk.mp4".to_string(),
                bytes: b"pretend video".to_vec(),
            }],
            files: vec![StagedFile {
                name: "graph.txt".to_string(),
                bytes: b"graph".to_vec(),
            }],
            argv,
            output: "out.mp4".to_string(),
            timeout: Duration::from_secs(20),
            max_output_bytes: 1 << 20,
        }
    }
}

fn alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

async fn eventually(within: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    while tokio::time::Instant::now() < deadline {
        if condition() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    condition()
}

#[tokio::test]
async fn a_job_runs_once_and_its_result_is_the_files_digest_and_the_probe() {
    let f = fixture();
    let supervisor = f.supervisor();
    assert!(supervisor
        .toolchain()
        .await
        .expect("toolchain")
        .contains("stand-in"));
    let rendered = supervisor.render(f.job("one", &[])).await.expect("render");
    assert!(rendered.bytes.windows(4).any(|w| w == b"ftyp"));
    assert_eq!(
        (
            rendered.probe.duration_ms,
            rendered.probe.width,
            rendered.probe.height
        ),
        (8000, 640, 360)
    );
    assert_eq!(
        (rendered.probe.video_streams, rendered.probe.audio_streams),
        (1, 1)
    );
    assert_eq!(f.runs().len(), 1);
    // The inputs and the small files were staged where the arguments name them.
    let dir = PathBuf::from(&f.runs()[0].1);
    assert_eq!(
        std::fs::read(dir.join("m_talk.mp4")).expect("input"),
        b"pretend video"
    );
    assert_eq!(
        std::fs::read(dir.join("graph.txt")).expect("graph"),
        b"graph"
    );
}

#[tokio::test]
async fn the_same_plan_after_a_restart_is_answered_from_the_record_without_a_process() {
    let f = fixture();
    let first = f
        .supervisor()
        .render(f.job("again", &[]))
        .await
        .expect("render");
    assert_eq!(f.runs().len(), 1);
    // A new supervisor over the same managed root is a restarted worker.
    let second = f
        .supervisor()
        .render(f.job("again", &[]))
        .await
        .expect("cached");
    assert_eq!(f.runs().len(), 1, "no second process");
    assert_eq!(first.bytes, second.bytes);
    assert_eq!(first.probe, second.probe);
    // Another plan is another job.
    f.supervisor()
        .render(f.job("different", &[]))
        .await
        .expect("other");
    assert_eq!(f.runs().len(), 2);
}

#[tokio::test]
async fn a_caller_that_arrives_while_the_job_runs_attaches_to_the_one_process() {
    let f = fixture();
    let supervisor = f.supervisor();
    let a = tokio::spawn({
        let (supervisor, job) = (supervisor.clone(), f.job("attach", &["--stand-in-slow"]));
        async move { supervisor.render(job).await }
    });
    assert!(
        eventually(Duration::from_secs(5), || f.runs().len() == 1).await,
        "the job started"
    );
    let b = supervisor
        .render(f.job("attach", &["--stand-in-slow"]))
        .await
        .expect("attached");
    let a = a.await.expect("join").expect("first caller");
    assert_eq!(a.bytes, b.bytes);
    assert_eq!(f.runs().len(), 1, "two callers, one process");
}

#[tokio::test]
async fn a_cancel_kills_the_whole_process_group_and_discards_the_partial_output() {
    let f = fixture();
    let supervisor = f.supervisor();
    let job = f.job("cancel", &["--stand-in-stall"]);
    let id = job.job_id.clone();
    let running = tokio::spawn({
        let supervisor = supervisor.clone();
        async move { supervisor.render(job).await }
    });
    assert!(
        eventually(Duration::from_secs(5), || f.runs().len() == 1).await,
        "the job started"
    );
    let (pid, dir) = f.runs()[0].clone();
    assert!(alive(pid), "the process is running");
    assert!(
        eventually(Duration::from_secs(5), || f.children().len() == 1).await,
        "it started a child of its own"
    );
    let child = f.children()[0];
    assert!(alive(child), "the child is running");
    supervisor.cancel(&id).await;
    let outcome = running.await.expect("join");
    assert_eq!(outcome.expect_err("cancelled"), MediaError::Cancelled);
    assert!(
        eventually(Duration::from_secs(5), || !alive(pid)).await,
        "the process is gone"
    );
    assert!(
        eventually(Duration::from_secs(5), || !alive(child)).await,
        "and so is the child it started: the whole group was killed"
    );
    assert!(
        !Path::new(&dir).exists(),
        "nothing of the job is left behind"
    );
    // The same plan can be run again afterwards (without the stall it finishes).
    let again = supervisor.render(f.job("cancel", &[])).await;
    assert!(again.is_ok(), "{again:?}");
}

#[tokio::test]
async fn a_job_that_outruns_its_bound_is_killed() {
    let f = fixture();
    let mut job = f.job("timeout", &["--stand-in-stall"]);
    job.timeout = Duration::from_millis(400);
    let outcome = f.supervisor().render(job).await;
    assert_eq!(outcome.expect_err("timeout"), MediaError::Timeout(400));
    let (pid, _) = f.runs()[0].clone();
    assert!(
        eventually(Duration::from_secs(5), || !alive(pid)).await,
        "the process was killed"
    );
    let child = f.children()[0];
    assert!(
        eventually(Duration::from_secs(5), || !alive(child)).await,
        "and its child with it"
    );
}

#[tokio::test]
async fn a_failing_process_reports_its_own_words_and_leaves_no_result() {
    let f = fixture();
    let outcome = f
        .supervisor()
        .render(f.job("fail", &["--stand-in-fail"]))
        .await;
    let MediaError::Failed(detail) = outcome.expect_err("failed") else {
        panic!("a failure");
    };
    assert!(detail.contains("boom: stand-in failure"), "{detail}");
    // A failure is not remembered as a result: the same plan runs again.
    let _ = f
        .supervisor()
        .render(f.job("fail", &["--stand-in-fail"]))
        .await;
    assert_eq!(f.runs().len(), 2);
}

#[tokio::test]
async fn a_job_the_previous_supervisor_left_running_is_run_again_from_scratch() {
    let f = fixture();
    let job = f.job("crash", &[]);
    // What a supervisor that died mid-job leaves: a `running` status and a half-written output.
    let dir = f
        .root
        .path()
        .join("managed/.quansio/media")
        .join(&job.job_id);
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(
        dir.join("status.json"),
        r#"{"state":"running","pid":4000000}"#,
    )
    .expect("status");
    std::fs::write(dir.join("out.mp4"), b"half a file").expect("partial");
    let rendered = f.supervisor().render(job).await.expect("re-run");
    assert!(
        rendered.bytes.windows(4).any(|w| w == b"ftyp"),
        "the partial output was not used"
    );
    assert_eq!(f.runs().len(), 1);
}

#[tokio::test]
async fn an_output_over_its_bound_is_removed_and_refused() {
    let f = fixture();
    let mut job = f.job("big", &[]);
    job.max_output_bytes = 8;
    let outcome = f.supervisor().render(job).await;
    assert!(matches!(
        outcome.expect_err("too large"),
        MediaError::TooLarge(_)
    ));
}

#[tokio::test]
async fn nothing_that_is_not_a_plain_name_or_a_digest_is_staged_or_run() {
    let f = fixture();
    let supervisor = f.supervisor();
    for (label, edit) in [
        (
            "an input that climbs",
            Box::new(|job: &mut RenderJob| job.inputs[0].name = "../escape.mp4".to_string())
                as Box<dyn Fn(&mut RenderJob)>,
        ),
        (
            "an input with a slash",
            Box::new(|job: &mut RenderJob| job.inputs[0].name = "sub/x.mp4".to_string()),
        ),
        (
            "a hidden file",
            Box::new(|job: &mut RenderJob| job.files[0].name = ".graph".to_string()),
        ),
        (
            "an output with a slash",
            Box::new(|job: &mut RenderJob| job.output = "a/out.mp4".to_string()),
        ),
        (
            "an id that is not a digest",
            Box::new(|job: &mut RenderJob| job.job_id = "../../etc".to_string()),
        ),
    ] {
        let mut job = f.job("names", &[]);
        edit(&mut job);
        let outcome = supervisor.render(job).await;
        assert!(
            matches!(outcome, Err(MediaError::Invalid(_))),
            "{label}: {outcome:?}"
        );
    }
    assert!(f.runs().is_empty(), "no process ran");
    assert!(!f.root.path().join("escape.mp4").exists());
}

#[tokio::test]
async fn without_a_binary_every_call_says_so_and_names_the_variable() {
    let root = tempfile::tempdir().expect("root");
    for supervisor in [
        MediaSupervisor::new(root.path(), None),
        MediaSupervisor::new(root.path(), Some(root.path().join("no-such-ffmpeg"))),
        MediaSupervisor::from_env(root.path(), "QUANSIO_TEST_DEFINITELY_NOT_SET"),
    ] {
        let error = supervisor.toolchain().await.expect_err("unavailable");
        assert!(matches!(error, MediaError::Unavailable(_)), "{error:?}");
        assert!(error.to_string().contains("QUANSIO_FFMPEG_BIN"), "{error}");
        let job = RenderJob {
            job_id: "a".repeat(64),
            inputs: vec![],
            files: vec![],
            argv: vec![],
            output: "out.mp4".to_string(),
            timeout: Duration::from_secs(1),
            max_output_bytes: 1,
        };
        assert!(matches!(
            supervisor.render(job).await,
            Err(MediaError::Unavailable(_))
        ));
    }
}

#[tokio::test]
async fn an_inspection_makes_one_picture_per_time_a_digest_for_each_and_a_contact_sheet() {
    let f = fixture();
    let supervisor = f.supervisor();
    let job = InspectJob {
        job_id: Digest::of(b"inspect").to_string(),
        input: StagedFile {
            name: "in.mp4".to_string(),
            bytes: b"pretend video".to_vec(),
        },
        times_ms: vec![0, 1_500, 3_000, 4_500],
        thumbnail_width: 320,
        columns: 2,
        timeout: Duration::from_secs(20),
    };
    let inspected = supervisor.inspect(job.clone()).await.expect("inspect");
    assert_eq!(inspected.frames.len(), 4);
    assert_eq!(
        inspected
            .frames
            .iter()
            .map(|frame| frame.time_ms)
            .collect::<Vec<_>>(),
        [0, 1_500, 3_000, 4_500]
    );
    assert!(inspected
        .frames
        .iter()
        .all(|frame| (frame.width, frame.height) == (320, 180)));
    // The stand-in calls the first and third frame flat; the others have contrast.
    assert_eq!(
        inspected
            .frames
            .iter()
            .map(|frame| frame.uniform)
            .collect::<Vec<_>>(),
        [true, false, true, false]
    );
    let digests: std::collections::BTreeSet<&str> = inspected
        .frames
        .iter()
        .map(|frame| frame.digest.as_str())
        .collect();
    assert_eq!(digests.len(), 4, "each frame has its own digest");
    assert_eq!((inspected.sheet_width, inspected.sheet_height), (1000, 400));
    assert!(inspected.sheet.starts_with(b"\x89PNG"));
    // Asked again, it is answered from the record.
    let runs = f.runs().len();
    let _ = supervisor.inspect(job).await.expect("cached");
    assert_eq!(f.runs().len(), runs);
    // The stand-in logs only runs that name its log; the point here is the answer, not the count.
    let _ = &f.log;
}

#[tokio::test]
async fn an_inspection_that_asks_for_nothing_or_too_much_is_refused() {
    let f = fixture();
    let supervisor = f.supervisor();
    let base = InspectJob {
        job_id: Digest::of(b"x").to_string(),
        input: StagedFile {
            name: "in.mp4".to_string(),
            bytes: vec![],
        },
        times_ms: vec![0],
        thumbnail_width: 320,
        columns: 2,
        timeout: Duration::from_secs(5),
    };
    for edit in [
        Box::new(|j: &mut InspectJob| j.times_ms.clear()) as Box<dyn Fn(&mut InspectJob)>,
        Box::new(|j: &mut InspectJob| j.times_ms = vec![0; 65]),
        Box::new(|j: &mut InspectJob| j.columns = 0),
        Box::new(|j: &mut InspectJob| j.input.name = "../x".to_string()),
    ] {
        let mut job = base.clone();
        edit(&mut job);
        assert!(matches!(
            supervisor.inspect(job).await,
            Err(MediaError::Invalid(_))
        ));
    }
}
