//! Supervised media jobs on the execution target: render a timeline, inspect a video (ADP-006,
//! DOMAIN.md §10.4 D).
//!
//! A job is a typed plan — staged input files, two small text files and an argument vector — that the
//! trusted runtime built. This module runs it with the `ffmpeg` binary named by the environment, **with no
//! shell**, and owns what a long process needs:
//!
//! * **identity.** A job's id is the digest of its plan. Running the same plan again *attaches*: a
//!   caller that arrives while the job runs waits for the one process, a caller that arrives after it
//!   finished gets its recorded result without a process.
//! * **cancel.** [`MediaSupervisor::cancel`] kills the job's whole process group, removes the partial
//!   output and records the cancellation; the waiting callers are told.
//! * **timeout.** A job that outruns its bound is killed the same way.
//! * **restart.** State lives in the job's directory under the managed root (`.quansio/media/<id>/`): a
//!   finished job's `done.json`, a running job's `status.json`. A supervisor that starts and finds a
//!   `running` status whose process is gone knows the job crashed; it discards the partial output and
//!   runs the plan again (rendering is not an external effect, so re-running is safe).
//! * **bounds.** An output over its limit is removed and refused.
//!
//! With no binary configured, or one that does not run, every call fails
//! [`MediaError::Unavailable`] naming the variable; nothing is simulated. The state is the target's own
//! bookkeeping inside its managed root (D-019), not a store.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use quansio_core::Digest;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::process::Command;
use tokio::sync::{watch, Mutex};

/// What a media job can come to.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MediaError {
    /// There is no usable `ffmpeg`: nothing was run.
    #[error("{0}")]
    Unavailable(String),
    /// The plan is not one this supervisor will run.
    #[error("{0}")]
    Invalid(String),
    /// The process ran and failed.
    #[error("{0}")]
    Failed(String),
    /// The job outran its bound and was killed.
    #[error("the job did not finish within {0} ms and was killed")]
    Timeout(u64),
    /// The job was cancelled.
    #[error("the job was cancelled")]
    Cancelled,
    /// An output passed its bound.
    #[error("{0}")]
    TooLarge(String),
}

/// A file staged for a job.
#[derive(Debug, Clone)]
pub struct StagedFile {
    /// Its name inside the job directory: a plain file name.
    pub name: String,
    /// Its bytes.
    pub bytes: Vec<u8>,
}

/// A render job.
#[derive(Debug, Clone)]
pub struct RenderJob {
    /// The plan's identity.
    pub job_id: String,
    /// The inputs.
    pub inputs: Vec<StagedFile>,
    /// Small text files the arguments name.
    pub files: Vec<StagedFile>,
    /// The argument vector, after the binary.
    pub argv: Vec<String>,
    /// The file the job writes.
    pub output: String,
    /// How long it may run.
    pub timeout: Duration,
    /// The largest output.
    pub max_output_bytes: u64,
}

/// What `ffprobe` said of an output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Probe {
    /// The container's duration.
    pub duration_ms: u64,
    /// The first video stream's width.
    pub width: u32,
    /// The first video stream's height.
    pub height: u32,
    /// How many video streams.
    pub video_streams: u32,
    /// How many audio streams.
    pub audio_streams: u32,
    /// How many subtitle streams.
    pub subtitle_streams: u32,
    /// The container's format name.
    pub format: String,
}

/// A finished render.
#[derive(Debug, Clone)]
pub struct Rendered {
    /// The output's bytes.
    pub bytes: Vec<u8>,
    /// What `ffprobe` made of it.
    pub probe: Probe,
    /// The `ffmpeg` that made it.
    pub tool: String,
}

/// An inspection job.
#[derive(Debug, Clone)]
pub struct InspectJob {
    /// The job's identity.
    pub job_id: String,
    /// The video.
    pub input: StagedFile,
    /// The times to look at.
    pub times_ms: Vec<u64>,
    /// The width of each thumbnail.
    pub thumbnail_width: u32,
    /// Contact sheet columns.
    pub columns: u32,
    /// How long it may run.
    pub timeout: Duration,
}

/// One frame of an inspection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frame {
    /// When it was taken.
    pub time_ms: u64,
    /// The SHA-256 of the frame's PNG.
    pub digest: String,
    /// Whether the picture is one flat colour.
    pub uniform: bool,
    /// Its width.
    pub width: u32,
    /// Its height.
    pub height: u32,
}

/// A finished inspection.
#[derive(Debug, Clone)]
pub struct Inspected {
    /// The contact sheet.
    pub sheet: Vec<u8>,
    /// Its width.
    pub sheet_width: u32,
    /// Its height.
    pub sheet_height: u32,
    /// The frames, in the order asked.
    pub frames: Vec<Frame>,
    /// The `ffmpeg` that made them.
    pub tool: String,
}

#[derive(Debug, Clone)]
enum Outcome {
    Rendered(Result<Arc<Rendered>, MediaError>),
    Inspected(Result<Arc<Inspected>, MediaError>),
}

type Running = Arc<Mutex<HashMap<String, watch::Receiver<Option<Outcome>>>>>;

/// Runs and supervises media jobs under one managed root.
#[derive(Clone)]
pub struct MediaSupervisor {
    root: PathBuf,
    ffmpeg: Option<PathBuf>,
    running: Running,
    pids: Arc<Mutex<HashMap<String, u32>>>,
    cancelled: Arc<Mutex<std::collections::HashSet<String>>>,
}

impl std::fmt::Debug for MediaSupervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaSupervisor")
            .field("root", &self.root)
            .field("ffmpeg", &self.ffmpeg)
            .finish_non_exhaustive()
    }
}

const STATE_DIR: &str = ".quansio/media";

fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 96
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn plain_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

impl MediaSupervisor {
    /// A supervisor over a managed root, with the binary named by the environment variable
    /// (`QUANSIO_FFMPEG_BIN` by `config/outputs.yaml`). A variable that is unset, or names a file that
    /// is not there, leaves the supervisor unavailable.
    #[must_use]
    pub fn from_env(root: impl Into<PathBuf>, variable: &str) -> Self {
        let ffmpeg = std::env::var_os(variable)
            .map(PathBuf::from)
            .filter(|path| path.is_file());
        Self::new(root, ffmpeg)
    }

    /// A supervisor over a managed root and an explicit binary.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>, ffmpeg: Option<PathBuf>) -> Self {
        Self {
            root: root.into(),
            ffmpeg,
            running: Arc::new(Mutex::new(HashMap::new())),
            pids: Arc::new(Mutex::new(HashMap::new())),
            cancelled: Arc::new(Mutex::new(std::collections::HashSet::new())),
        }
    }

    fn job_dir(&self, job_id: &str) -> PathBuf {
        self.root.join(STATE_DIR).join(job_id)
    }

    fn unavailable(&self, why: &str) -> MediaError {
        MediaError::Unavailable(format!(
            "{why}; set QUANSIO_FFMPEG_BIN to an ffmpeg binary on the execution target"
        ))
    }

    /// The `ffmpeg` this supervisor runs, by its first version line.
    ///
    /// # Errors
    /// [`MediaError::Unavailable`] when there is no binary or it does not run.
    pub async fn toolchain(&self) -> Result<String, MediaError> {
        let Some(ffmpeg) = &self.ffmpeg else {
            return Err(self.unavailable("no ffmpeg is configured"));
        };
        let output = Command::new(ffmpeg)
            .arg("-version")
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|error| {
                self.unavailable(&format!("{} does not run: {error}", ffmpeg.display()))
            })?;
        if !output.status.success() {
            return Err(self.unavailable(&format!("{} -version failed", ffmpeg.display())));
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .unwrap_or_default()
            .to_string())
    }

    /// Run a render job, or attach to the one with this id.
    ///
    /// # Errors
    /// [`MediaError`].
    pub async fn render(&self, job: RenderJob) -> Result<Arc<Rendered>, MediaError> {
        if !plain_id(&job.job_id) {
            return Err(MediaError::Invalid("a job id is a SHA-256".to_string()));
        }
        for file in job.inputs.iter().chain(&job.files) {
            if !plain_name(&file.name) {
                return Err(MediaError::Invalid(format!(
                    "{:?} is not a plain file name",
                    file.name
                )));
            }
        }
        if !plain_name(&job.output) {
            return Err(MediaError::Invalid(format!(
                "{:?} is not a plain file name",
                job.output
            )));
        }
        let tool = self.toolchain().await?;
        let id = job.job_id.clone();
        let supervisor = self.clone();
        match self
            .attach_or_start(id, move |tx| async move {
                let outcome = supervisor.execute_render(job, tool).await;
                let _ = tx.send(Some(Outcome::Rendered(outcome)));
            })
            .await?
        {
            Outcome::Rendered(result) => result,
            Outcome::Inspected(_) => Err(MediaError::Invalid(
                "the job id belongs to an inspection".to_string(),
            )),
        }
    }

    /// Run an inspection job, or attach to the one with this id.
    ///
    /// # Errors
    /// [`MediaError`].
    pub async fn inspect(&self, job: InspectJob) -> Result<Arc<Inspected>, MediaError> {
        if !plain_id(&job.job_id) || !plain_name(&job.input.name) {
            return Err(MediaError::Invalid(
                "an inspection needs a SHA-256 id and a plain file name".to_string(),
            ));
        }
        if job.times_ms.is_empty()
            || job.times_ms.len() > 64
            || job.columns == 0
            || job.columns > 16
        {
            return Err(MediaError::Invalid(
                "an inspection takes 1 to 64 frames in 1 to 16 columns".to_string(),
            ));
        }
        let tool = self.toolchain().await?;
        let id = job.job_id.clone();
        let supervisor = self.clone();
        match self
            .attach_or_start(id, move |tx| async move {
                let outcome = supervisor.execute_inspect(job, tool).await;
                let _ = tx.send(Some(Outcome::Inspected(outcome)));
            })
            .await?
        {
            Outcome::Inspected(result) => result,
            Outcome::Rendered(_) => Err(MediaError::Invalid(
                "the job id belongs to a render".to_string(),
            )),
        }
    }

    async fn attach_or_start<F, Fut>(&self, id: String, start: F) -> Result<Outcome, MediaError>
    where
        F: FnOnce(watch::Sender<Option<Outcome>>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut receiver = {
            let mut running = self.running.lock().await;
            if let Some(existing) = running.get(&id) {
                existing.clone()
            } else {
                let (tx, rx) = watch::channel(None);
                running.insert(id.clone(), rx.clone());
                let running_map = Arc::clone(&self.running);
                let cleanup_id = id.clone();
                let work = start(tx);
                tokio::spawn(async move {
                    work.await;
                    running_map.lock().await.remove(&cleanup_id);
                });
                rx
            }
        };
        while receiver.borrow().is_none() {
            if receiver.changed().await.is_err() {
                break;
            }
        }
        let outcome = receiver.borrow().clone();
        outcome.ok_or_else(|| MediaError::Failed("the job's supervisor stopped".to_string()))
    }

    /// Cancel a job: kill its process group, discard a partial output and tell the waiting callers.
    pub async fn cancel(&self, job_id: &str) {
        if !plain_id(job_id) {
            return;
        }
        self.cancelled.lock().await.insert(job_id.to_string());
        if let Some(pid) = self.pids.lock().await.get(job_id).copied() {
            kill_group(pid).await;
        }
    }

    // ---- render ------------------------------------------------------------------------------------

    async fn execute_render(
        &self,
        job: RenderJob,
        tool: String,
    ) -> Result<Arc<Rendered>, MediaError> {
        let dir = self.job_dir(&job.job_id);
        if let Some(done) = read_done(&dir).await {
            let bytes = tokio::fs::read(dir.join(&job.output)).await.ok();
            if let Some(bytes) = bytes.filter(|bytes| Digest::of(bytes).as_str() == done.digest) {
                if let Ok(probe) = serde_json::from_value::<Probe>(done.probe.clone()) {
                    return Ok(Arc::new(Rendered {
                        bytes,
                        probe,
                        tool: done.tool,
                    }));
                }
            }
        }
        self.cancelled.lock().await.remove(&job.job_id);
        // A previous supervisor may have died mid-job: its partial output is not trustworthy.
        if status_running(&dir).await {
            let _ = tokio::fs::remove_dir_all(&dir).await;
        }
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|error| MediaError::Failed(format!("the job directory: {error}")))?;
        for file in job.inputs.iter().chain(&job.files) {
            tokio::fs::write(dir.join(&file.name), &file.bytes)
                .await
                .map_err(|error| MediaError::Failed(format!("staging {}: {error}", file.name)))?;
        }
        let _ = tokio::fs::remove_file(dir.join(&job.output)).await;
        let ffmpeg = self
            .ffmpeg
            .clone()
            .ok_or_else(|| self.unavailable("no ffmpeg is configured"))?;
        self.run_process(&job.job_id, &ffmpeg, &job.argv, &dir, job.timeout)
            .await?;
        let metadata = tokio::fs::metadata(dir.join(&job.output))
            .await
            .map_err(|_| MediaError::Failed("ffmpeg finished and wrote no output".to_string()))?;
        if metadata.len() > job.max_output_bytes {
            let _ = tokio::fs::remove_file(dir.join(&job.output)).await;
            return Err(MediaError::TooLarge(format!(
                "the output is {} bytes, over the {}-byte limit",
                metadata.len(),
                job.max_output_bytes
            )));
        }
        let probe = self.probe(&ffmpeg, &dir, &job.output).await?;
        let bytes = tokio::fs::read(dir.join(&job.output))
            .await
            .map_err(|error| MediaError::Failed(format!("reading the output: {error}")))?;
        write_json(
            &dir.join("done.json"),
            &json!({
                "digest": Digest::of(&bytes).as_str(),
                "probe": probe,
                "tool": tool,
            }),
        )
        .await;
        let _ = tokio::fs::remove_file(dir.join("status.json")).await;
        Ok(Arc::new(Rendered { bytes, probe, tool }))
    }

    async fn run_process(
        &self,
        job_id: &str,
        binary: &Path,
        argv: &[String],
        dir: &Path,
        timeout: Duration,
    ) -> Result<(), MediaError> {
        let stderr = std::fs::File::create(dir.join("stderr.log"))
            .map_err(|error| MediaError::Failed(format!("the job log: {error}")))?;
        let mut child = Command::new(binary)
            .args(argv)
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr))
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                self.unavailable(&format!("{} does not run: {error}", binary.display()))
            })?;
        let pid = child.id().unwrap_or(0);
        self.pids.lock().await.insert(job_id.to_string(), pid);
        write_json(
            &dir.join("status.json"),
            &json!({ "state": "running", "pid": pid }),
        )
        .await;
        // A cancel that arrived while the process was starting still applies.
        if self.cancelled.lock().await.contains(job_id) {
            kill_group(pid).await;
        }
        let waited = tokio::time::timeout(timeout, child.wait()).await;
        self.pids.lock().await.remove(job_id);
        let cancelled = self.cancelled.lock().await.remove(job_id);
        match waited {
            Err(_) => {
                kill_group(pid).await;
                let _ = child.wait().await;
                let _ = tokio::fs::remove_dir_all(dir).await;
                Err(MediaError::Timeout(
                    u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
                ))
            }
            Ok(Ok(_)) if cancelled => {
                let _ = tokio::fs::remove_dir_all(dir).await;
                Err(MediaError::Cancelled)
            }
            Ok(Ok(status)) if status.success() => Ok(()),
            Ok(Ok(status)) => {
                let tail = tail_of(&dir.join("stderr.log")).await;
                let _ = tokio::fs::remove_file(dir.join("status.json")).await;
                Err(MediaError::Failed(format!(
                    "ffmpeg exited {status}: {tail}"
                )))
            }
            Ok(Err(error)) => Err(MediaError::Failed(format!("waiting for ffmpeg: {error}"))),
        }
    }

    async fn probe(&self, ffmpeg: &Path, dir: &Path, output: &str) -> Result<Probe, MediaError> {
        let ffprobe = ffmpeg.with_file_name("ffprobe");
        let result = Command::new(&ffprobe)
            .args([
                "-v",
                "error",
                "-print_format",
                "json",
                "-show_format",
                "-show_streams",
                output,
            ])
            .current_dir(dir)
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|error| {
                self.unavailable(&format!("{} does not run: {error}", ffprobe.display()))
            })?;
        if !result.status.success() {
            return Err(MediaError::Failed(format!(
                "ffprobe could not read the output: {}",
                String::from_utf8_lossy(&result.stderr).trim()
            )));
        }
        parse_probe(&result.stdout)
    }

    // ---- inspect -----------------------------------------------------------------------------------

    async fn execute_inspect(
        &self,
        job: InspectJob,
        tool: String,
    ) -> Result<Arc<Inspected>, MediaError> {
        let dir = self.job_dir(&job.job_id);
        if let Some(done) = read_done(&dir).await {
            if let (Ok(frames), Ok(sheet)) = (
                serde_json::from_value::<Vec<Frame>>(done.probe.clone()),
                tokio::fs::read(dir.join("sheet.png")).await,
            ) {
                if Digest::of(&sheet).as_str() == done.digest {
                    let (sheet_width, sheet_height) = png_size(&sheet).unwrap_or((0, 0));
                    return Ok(Arc::new(Inspected {
                        sheet,
                        sheet_width,
                        sheet_height,
                        frames,
                        tool: done.tool,
                    }));
                }
            }
        }
        self.cancelled.lock().await.remove(&job.job_id);
        if status_running(&dir).await {
            let _ = tokio::fs::remove_dir_all(&dir).await;
        }
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|error| MediaError::Failed(format!("the job directory: {error}")))?;
        tokio::fs::write(dir.join(&job.input.name), &job.input.bytes)
            .await
            .map_err(|error| MediaError::Failed(format!("staging the video: {error}")))?;
        let ffmpeg = self
            .ffmpeg
            .clone()
            .ok_or_else(|| self.unavailable("no ffmpeg is configured"))?;
        let mut frames = Vec::new();
        for (number, time) in job.times_ms.iter().enumerate() {
            let file = format!("frame_{:02}.png", number + 1);
            let argv: Vec<String> = ["-y", "-hide_banner", "-nostdin", "-ss"]
                .into_iter()
                .map(str::to_string)
                .chain([
                    format!("{}.{:03}", time / 1000, time % 1000),
                    "-i".to_string(),
                    job.input.name.clone(),
                    "-frames:v".to_string(),
                    "1".to_string(),
                    "-vf".to_string(),
                    format!("scale={}:-2", job.thumbnail_width),
                    file.clone(),
                ])
                .collect();
            self.run_process(&job.job_id, &ffmpeg, &argv, &dir, job.timeout)
                .await?;
            let png = tokio::fs::read(dir.join(&file)).await.map_err(|_| {
                MediaError::Failed(format!(
                    "there is no frame at {time} ms: the video ends before it"
                ))
            })?;
            let (width, height) = png_size(&png).ok_or_else(|| {
                MediaError::Failed(format!("the frame at {time} ms is not a PNG"))
            })?;
            let uniform = self.uniform(&ffmpeg, &dir, &job, &file, number).await?;
            frames.push(Frame {
                time_ms: *time,
                digest: Digest::of(&png).to_string(),
                uniform,
                width,
                height,
            });
        }
        let rows = job.times_ms.len().div_ceil(job.columns as usize);
        let argv: Vec<String> = [
            "-y",
            "-hide_banner",
            "-nostdin",
            "-framerate",
            "1",
            "-i",
            "frame_%02d.png",
            "-vf",
        ]
        .into_iter()
        .map(str::to_string)
        .chain([
            format!("tile={}x{rows}:padding=4:margin=4", job.columns),
            "-frames:v".to_string(),
            "1".to_string(),
            "sheet.png".to_string(),
        ])
        .collect();
        self.run_process(&job.job_id, &ffmpeg, &argv, &dir, job.timeout)
            .await?;
        let sheet = tokio::fs::read(dir.join("sheet.png"))
            .await
            .map_err(|_| MediaError::Failed("ffmpeg made no contact sheet".to_string()))?;
        let (sheet_width, sheet_height) = png_size(&sheet)
            .ok_or_else(|| MediaError::Failed("the contact sheet is not a PNG".to_string()))?;
        write_json(
            &dir.join("done.json"),
            &json!({
                "digest": Digest::of(&sheet).as_str(),
                "probe": frames,
                "tool": tool,
            }),
        )
        .await;
        let _ = tokio::fs::remove_file(dir.join("status.json")).await;
        Ok(Arc::new(Inspected {
            sheet,
            sheet_width,
            sheet_height,
            frames,
            tool,
        }))
    }

    async fn uniform(
        &self,
        ffmpeg: &Path,
        dir: &Path,
        job: &InspectJob,
        file: &str,
        number: usize,
    ) -> Result<bool, MediaError> {
        let stats = format!("stats_{:02}.txt", number + 1);
        let argv: Vec<String> = ["-hide_banner", "-nostdin", "-i"]
            .into_iter()
            .map(str::to_string)
            .chain([
                file.to_string(),
                "-vf".to_string(),
                format!("signalstats,metadata=print:file={stats}"),
                "-f".to_string(),
                "null".to_string(),
                "-".to_string(),
            ])
            .collect();
        self.run_process(&job.job_id, ffmpeg, &argv, dir, job.timeout)
            .await?;
        let text = tokio::fs::read_to_string(dir.join(&stats))
            .await
            .unwrap_or_default();
        let value = |key: &str| {
            text.lines().find_map(|line| {
                line.split_once(&format!("lavfi.signalstats.{key}="))
                    .map(|(_, v)| v.trim().to_string())
            })
        };
        Ok(["Y", "U", "V"].iter().all(|channel| {
            let (low, high) = (
                value(&format!("{channel}MIN")),
                value(&format!("{channel}MAX")),
            );
            low.is_some() && low == high
        }))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Done {
    digest: String,
    probe: Value,
    tool: String,
}

async fn read_done(dir: &Path) -> Option<Done> {
    serde_json::from_slice(&tokio::fs::read(dir.join("done.json")).await.ok()?).ok()
}

async fn status_running(dir: &Path) -> bool {
    tokio::fs::read(dir.join("status.json"))
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .is_some_and(|status| status["state"] == "running")
}

async fn write_json(path: &Path, value: &Value) {
    let _ = tokio::fs::write(path, serde_json::to_vec(value).unwrap_or_default()).await;
}

async fn tail_of(path: &Path) -> String {
    let text = tokio::fs::read_to_string(path).await.unwrap_or_default();
    let lines: Vec<&str> = text.lines().rev().take(6).collect();
    lines
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" | ")
        .chars()
        .take(600)
        .collect()
}

async fn kill_group(pid: u32) {
    if pid == 0 {
        return;
    }
    let _ = Command::new("kill")
        .args(["-KILL", "--", &format!("-{pid}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
}

fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    if png.len() < 24 || &png[..8] != b"\x89PNG\r\n\x1a\n" || &png[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(png[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(png[20..24].try_into().ok()?);
    (width > 0 && height > 0).then_some((width, height))
}

fn parse_probe(json: &[u8]) -> Result<Probe, MediaError> {
    let value: Value = serde_json::from_slice(json)
        .map_err(|error| MediaError::Failed(format!("ffprobe's answer is not JSON: {error}")))?;
    let seconds: f64 = value["format"]["duration"]
        .as_str()
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| MediaError::Failed("ffprobe named no duration".to_string()))?;
    let streams = value["streams"].as_array().cloned().unwrap_or_default();
    let count = |kind: &str| {
        u32::try_from(
            streams
                .iter()
                .filter(|stream| stream["codec_type"] == kind)
                .count(),
        )
        .unwrap_or(u32::MAX)
    };
    let video = streams
        .iter()
        .find(|stream| stream["codec_type"] == "video");
    let dimension = |key: &str| {
        video
            .and_then(|stream| stream[key].as_u64())
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(0)
    };
    Ok(Probe {
        duration_ms: (seconds * 1000.0).round() as u64,
        width: dimension("width"),
        height: dimension("height"),
        video_streams: count("video"),
        audio_streams: count("audio"),
        subtitle_streams: count("subtitle"),
        format: value["format"]["format_name"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_probe_is_read_from_ffprobes_json() {
        let probe = parse_probe(
            br#"{"format":{"duration":"8.004000","format_name":"mov,mp4,m4a,3gp,3g2,mj2"},
                "streams":[{"codec_type":"video","width":640,"height":360},{"codec_type":"audio"},{"codec_type":"subtitle"}]}"#,
        )
        .expect("probe");
        assert_eq!(
            probe,
            Probe {
                duration_ms: 8004,
                width: 640,
                height: 360,
                video_streams: 1,
                audio_streams: 1,
                subtitle_streams: 1,
                format: "mov,mp4,m4a,3gp,3g2,mj2".to_string()
            }
        );
        assert!(parse_probe(b"not json").is_err());
        assert!(parse_probe(br#"{"format":{},"streams":[]}"#).is_err());
    }

    #[test]
    fn a_png_size_is_read_from_its_header_and_nothing_else_is_a_png() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&320u32.to_be_bytes());
        png.extend_from_slice(&180u32.to_be_bytes());
        assert_eq!(png_size(&png), Some((320, 180)));
        assert_eq!(png_size(b"GIF89a"), None);
        assert_eq!(png_size(&png[..20]), None);
    }

    #[test]
    fn file_names_and_ids_are_plain() {
        for good in ["m_talk.mp4", "graph.txt", "out.mp4", "frame_01.png"] {
            assert!(plain_name(good), "{good}");
        }
        for bad in ["", ".hidden", "../x", "a/b", "a b", "a;b", &"a".repeat(97)] {
            assert!(!plain_name(bad), "{bad:?}");
        }
        assert!(plain_id(&"a".repeat(64)));
        assert!(!plain_id("short") && !plain_id(&"g".repeat(64)) && !plain_id(&"a".repeat(65)));
    }
}
