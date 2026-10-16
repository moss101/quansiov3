//! The managed Chromium process on a worker, and the guard on where it may be sent (EXEC-009 composition).
//!
//! The browser runs on the worker, never in the desktop process (DOSSIER.md §8). This launches one Chromium per
//! browser session with a DevTools port on loopback and a profile directory of its own, and stops it when it is
//! dropped.
//!
//! `--remote-allow-origins` is deliberately **not** set: the driver connects as a non-browser client (it sends no
//! `Origin`), so Chromium admits it, while a web page's script, which does send one, cannot open the DevTools
//! socket and command the browser it runs in.
//!
//! [`vet_navigation`] is the part of egress control a worker running directly on someone's machine still owes: a
//! page the model is sent to must not be the machine itself, its private network or a cloud metadata service. It
//! uses the egress broker's own address classes and refuses a name that *resolves* to one. It does not see a
//! redirect or a sub-resource the page loads afterwards; the target's network policy (EXEC-008) is what covers
//! those on a sandboxed substrate, and a worker with no sandbox is as exposed as the person who started it.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use quansio_machine::egress::{blocked_host, AddressClass, Destination, HostKind};
use tokio::process::{Child, Command};

/// How long Chromium has to start answering on its DevTools port.
const START_TIMEOUT: Duration = Duration::from_secs(25);

/// A running Chromium, stopped when dropped.
#[derive(Debug)]
pub struct ChromeProcess {
    child: Child,
    /// The loopback DevTools port.
    pub port: u16,
}

impl ChromeProcess {
    /// Launch `chrome` headless with a profile under `profile_dir`.
    ///
    /// # Errors
    /// Returns a message when the binary cannot be started or does not answer on its DevTools port in time.
    pub async fn launch(chrome: &Path, profile_dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(profile_dir).map_err(|error| {
            format!(
                "the profile directory {} is unusable: {error}",
                profile_dir.display()
            )
        })?;
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0")
                .map_err(|error| format!("no free loopback port: {error}"))?;
            listener
                .local_addr()
                .map_err(|error| format!("no free loopback port: {error}"))?
                .port()
        };
        let mut command = Command::new(chrome);
        command
            .arg("--headless=new")
            .arg("--disable-gpu")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--disable-background-networking")
            .arg("--disable-sync")
            .arg(format!("--remote-debugging-port={port}"))
            .arg(format!("--user-data-dir={}", profile_dir.display()));
        if std::env::var_os("CI").is_some() {
            command.arg("--no-sandbox").arg("--disable-dev-shm-usage");
        }
        let child = command
            .arg("about:blank")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("{} could not be started: {error}", chrome.display()))?;
        let launched = Self { child, port };
        let deadline = tokio::time::Instant::now() + START_TIMEOUT;
        while tokio::time::Instant::now() < deadline {
            if crate::browser::cdp::pages(port).await.is_ok() {
                return Ok(launched);
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        Err(format!(
            "{} did not answer on its DevTools port",
            chrome.display()
        ))
    }

    /// The process id, while it runs.
    #[must_use]
    pub fn id(&self) -> Option<u32> {
        self.child.id()
    }
}

/// Where Chromium is looked for when the operator does not name it.
#[must_use]
pub fn default_chrome_candidates() -> Vec<PathBuf> {
    [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/usr/bin/google-chrome",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
    ]
    .iter()
    .map(PathBuf::from)
    .collect()
}

/// Refuse a navigation to the machine itself, its private network or a metadata service.
///
/// # Errors
/// Returns the reason when the destination, or any address its name resolves to, is not a public one.
pub async fn vet_navigation(url: &str) -> Result<(), String> {
    if url == "about:blank" {
        return Ok(());
    }
    let destination = Destination::parse(url).map_err(|denial| denial.to_string())?;
    if let Some(why) = blocked_host(destination.host()) {
        return Err(format!("{} is refused: {why}", destination.host()));
    }
    let addresses: Vec<SocketAddr> = match destination.kind() {
        HostKind::Literal => destination
            .host()
            .trim_matches(['[', ']'])
            .parse()
            .map(|ip| vec![SocketAddr::new(ip, destination.port())])
            .map_err(|_| format!("{} is not an address", destination.host()))?,
        HostKind::Name => tokio::net::lookup_host((destination.host(), destination.port()))
            .await
            .map_err(|error| format!("{} does not resolve: {error}", destination.host()))?
            .collect(),
    };
    if addresses.is_empty() {
        return Err(format!("{} resolves to no address", destination.host()));
    }
    for address in addresses {
        let class = AddressClass::of(address.ip());
        if !AddressClass::default_allowed().contains(&class) {
            return Err(format!(
                "{} resolves to {} ({class}), which a browser on this worker may not reach",
                destination.host(),
                address.ip()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_machine_its_network_and_metadata_services_are_refused_by_address_and_by_name() {
        for url in [
            "http://127.0.0.1:8080/",
            "http://[::1]/",
            "http://localhost/",
            "http://10.0.0.5/",
            "http://192.168.1.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://metadata.google.internal/",
            "http://user:pass@93.184.216.34/",
            "file:///etc/passwd",
        ] {
            assert!(vet_navigation(url).await.is_err(), "{url} must be refused");
        }
    }

    #[tokio::test]
    async fn a_public_literal_and_about_blank_are_admitted() {
        assert!(vet_navigation("about:blank").await.is_ok());
        assert!(vet_navigation("https://93.184.216.34/").await.is_ok());
    }
}
