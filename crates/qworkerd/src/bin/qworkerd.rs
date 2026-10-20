//! `qworkerd`: the worker daemon. It dials the server's worker socket, holds the lease the server grants,
//! and runs fenced tool dispatches inside the workspace root it was started with (EXEC-002).
//!
//! ```text
//! qworkerd --socket PATH --tenant tnt_… --target tgt_… --workspace-root DIR [--shell /bin/sh] [--heartbeat-secs 15] \
//!         [--max-concurrent 8] [--chrome PATH --state-dir DIR]
//! ```
//!
//! The socket is the server's `QUANSIO_WORKER_SOCKET`. The daemon is outbound-only: it connects, it never
//! listens. It reconnects with a bounded backoff, keeping the host (and so its record of what already ran)
//! across connections. Up to `--max-concurrent` dispatches run at once; one more waits for a turn.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use quansio_qworkerd::browser_runtime::BrowserRuntime;
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
    max_concurrent: usize,
    chrome: Option<PathBuf>,
    state_dir: Option<PathBuf>,
    allow_private_network: bool,
}

fn parse(mut input: impl Iterator<Item = String>) -> Result<Args, String> {
    let (mut socket, mut tenant, mut target, mut root, mut shell) = (None, None, None, None, None);
    let mut heartbeat = Duration::from_secs(15);
    let mut max_concurrent = quansio_qworkerd::host::DEFAULT_CONCURRENCY;
    let (mut chrome, mut state_dir) = (None, None);
    let mut allow_private_network = false;
    while let Some(flag) = input.next() {
        if flag == "--allow-private-network" {
            allow_private_network = true;
            continue;
        }
        let value = input
            .next()
            .ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--socket" => socket = Some(PathBuf::from(value)),
            "--tenant" => tenant = Some(value),
            "--target" => target = Some(value),
            "--workspace-root" => root = Some(PathBuf::from(value)),
            "--shell" => shell = Some(value),
            "--chrome" => chrome = Some(PathBuf::from(value)),
            "--state-dir" => state_dir = Some(PathBuf::from(value)),
            "--heartbeat-secs" => {
                let seconds: u64 = value
                    .parse()
                    .map_err(|_| "--heartbeat-secs needs a whole number".to_string())?;
                heartbeat = Duration::from_secs(seconds.max(1));
            }
            "--max-concurrent" => {
                max_concurrent = value
                    .parse::<usize>()
                    .ok()
                    .filter(|count| (1..=64).contains(count))
                    .ok_or_else(|| {
                        "--max-concurrent needs a whole number from 1 to 64".to_string()
                    })?;
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
        max_concurrent,
        chrome,
        state_dir,
        allow_private_network,
    })
}

#[cfg(unix)]
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args = match parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("qworkerd: {message}\nusage: qworkerd --socket PATH --tenant ID --target ID --workspace-root DIR [--shell PATH] [--heartbeat-secs N] [--max-concurrent N] [--chrome PATH --state-dir DIR [--allow-private-network]]");
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
    if let Some(chrome) = &args.chrome {
        // The profile holds cookies and the action journal: it must not be somewhere the file tools can read.
        let Some(state) = &args.state_dir else {
            eprintln!("qworkerd: --chrome needs --state-dir, a directory outside the workspace root for the browser's profile and journal");
            std::process::exit(2);
        };
        let inside = std::fs::create_dir_all(state)
            .and_then(|()| {
                Ok((
                    std::fs::canonicalize(state)?,
                    std::fs::canonicalize(&args.root)?,
                ))
            })
            .map(|(state, root)| state.starts_with(root));
        match inside {
            Ok(false) => {}
            Ok(true) => {
                eprintln!("qworkerd: --state-dir must not be inside --workspace-root: the file tools could read the browser's cookies");
                std::process::exit(2);
            }
            Err(error) => {
                eprintln!("qworkerd: --state-dir {}: {error}", state.display());
                std::process::exit(2);
            }
        }
        if !chrome.is_file() {
            eprintln!("qworkerd: --chrome {} is not a file", chrome.display());
            std::process::exit(2);
        }
        executor = executor.with_browser(
            BrowserRuntime::new(chrome.clone(), state.clone(), args.tenant.clone())
                .allow_private_network(args.allow_private_network),
        );
    } else if args.state_dir.is_some() || args.allow_private_network {
        eprintln!("qworkerd: --state-dir and --allow-private-network are only used with --chrome");
        std::process::exit(2);
    }
    let host = Arc::new(WorkerHost::with_concurrency(
        executor,
        UnattachedEvidence,
        args.max_concurrent,
    ));

    let mut backoff = Duration::from_millis(250);
    loop {
        match tokio::net::UnixStream::connect(&args.socket).await {
            Ok(stream) => {
                backoff = Duration::from_millis(250);
                let (reader, writer) = stream.into_split();
                match serve(
                    Arc::clone(&host),
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
