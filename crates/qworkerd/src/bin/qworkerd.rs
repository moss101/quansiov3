//! `qworkerd`: the worker daemon. It dials the server's worker socket, holds the lease the server grants,
//! and runs fenced tool dispatches inside the workspace root it was started with (EXEC-002).
//!
//! ```text
//! qworkerd --socket PATH --tenant tnt_… --target tgt_… --workspace-root DIR [--shell /bin/sh] [--heartbeat-secs 15]
//! ```
//!
//! The socket is the server's `QUANSIO_WORKER_SOCKET`. The daemon is outbound-only: it connects, it never
//! listens. It reconnects with a bounded backoff, keeping the host (and so its record of what already ran)
//! across connections.

use std::path::PathBuf;
use std::time::Duration;

use quansio_qworkerd::daemon::{serve, UnattachedEvidence};
use quansio_qworkerd::executor::LocalToolExecutor;
use quansio_qworkerd::host::WorkerHost;
use quansio_qworkerd::tools::{RootScope, Sandbox};

struct Args {
    socket: PathBuf,
    tenant: String,
    target: String,
    root: PathBuf,
    shell: Option<String>,
    heartbeat: Duration,
}

fn parse(mut input: impl Iterator<Item = String>) -> Result<Args, String> {
    let (mut socket, mut tenant, mut target, mut root, mut shell) = (None, None, None, None, None);
    let mut heartbeat = Duration::from_secs(15);
    while let Some(flag) = input.next() {
        let value = input
            .next()
            .ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--socket" => socket = Some(PathBuf::from(value)),
            "--tenant" => tenant = Some(value),
            "--target" => target = Some(value),
            "--workspace-root" => root = Some(PathBuf::from(value)),
            "--shell" => shell = Some(value),
            "--heartbeat-secs" => {
                let seconds: u64 = value
                    .parse()
                    .map_err(|_| "--heartbeat-secs needs a whole number".to_string())?;
                heartbeat = Duration::from_secs(seconds.max(1));
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    fn need<T>(value: Option<T>, name: &str) -> Result<T, String> {
        value.ok_or_else(|| format!("{name} is required"))
    }
    Ok(Args {
        socket: need(socket, "--socket")?,
        tenant: need(tenant, "--tenant")?,
        target: need(target, "--target")?,
        root: need(root, "--workspace-root")?,
        shell,
        heartbeat,
    })
}

#[cfg(unix)]
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args = match parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("qworkerd: {message}\nusage: qworkerd --socket PATH --tenant ID --target ID --workspace-root DIR [--shell PATH] [--heartbeat-secs N]");
            std::process::exit(2);
        }
    };
    let sandbox = match Sandbox::empty().authorize(RootScope::Workspace, &args.root) {
        Ok(sandbox) => sandbox,
        Err(refusal) => {
            eprintln!("qworkerd: {refusal}");
            std::process::exit(2);
        }
    };
    let mut executor = LocalToolExecutor::new(sandbox, args.tenant.clone());
    if let Some(shell) = &args.shell {
        executor = executor.with_shell(shell.clone());
    }
    let mut host = WorkerHost::new(executor, UnattachedEvidence);

    let mut backoff = Duration::from_millis(250);
    loop {
        match tokio::net::UnixStream::connect(&args.socket).await {
            Ok(stream) => {
                backoff = Duration::from_millis(250);
                let (reader, writer) = stream.into_split();
                match serve(
                    &mut host,
                    &args.tenant,
                    &args.target,
                    args.heartbeat,
                    reader,
                    writer,
                )
                .await
                {
                    Ok(()) => eprintln!("qworkerd: the server closed the channel; reconnecting"),
                    Err(error) => eprintln!("qworkerd: {error}; reconnecting"),
                }
            }
            Err(error) => eprintln!("qworkerd: cannot reach {}: {error}", args.socket.display()),
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(5));
    }
}

#[cfg(not(unix))]
fn main() {
    eprintln!("qworkerd: this build serves a Unix socket; a Windows named-pipe channel is not implemented");
    std::process::exit(2);
}
