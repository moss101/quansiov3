//! The native helper: identity pinning, versioned install, rollback and orphan cleanup (ADP-002,
//! DOMAIN.md §8.6.1 D).
//!
//! The helper holds the OS permissions an operator grants (Accessibility, Screen Recording), so what
//! runs under that name must be exactly what was signed for it. Nothing here trusts a path, a file name
//! or a version string:
//!
//! * a helper is launched only after `codesign` says its signature is valid, its identifier is one of
//!   the pinned bundle ids, it satisfies the designated requirement built from the configured team id
//!   (an Apple anchor and the leaf certificate's organizational unit), and it carries the hardened
//!   runtime;
//! * it is copied into `<root>/versions/<sha256 of the tree>/` and verified again *after* the copy,
//!   against the same fingerprint and CDHash, and again immediately before every launch;
//! * `active` and `previous` pointers make an activation reversible;
//! * processes still running from the install directory under the runtime's profile prefix when it
//!   starts are orphans of a crash and are terminated before a new helper is launched.
//!
//! Verification shells out to the system's `codesign`: the platform's own verifier is the authority on
//! what a signature means, and this crate holds no unsafe code with which to call Security.framework
//! directly. On a host without it every verification fails closed with
//! [`HelperError::ToolUnavailable`].

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::config::{HelperConfig, HelperStatus};

/// The typed error code a refused identity is reported as (DOMAIN.md §15).
pub const IDENTITY_REFUSED: &str = "COMPUTER_HELPER_IDENTITY_REFUSED";

/// Why a helper was refused or could not be managed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HelperError {
    /// Configuration has no signing identity yet; no helper may be launched.
    #[error(
        "the helper pin is unpublished: no signing identity is configured, so no helper may run"
    )]
    PinUnpublished,
    /// Verification needs macOS.
    #[error("helper identity can only be verified on macOS")]
    UnsupportedPlatform,
    /// The platform verifier could not be run.
    #[error("codesign is unavailable: {0}")]
    ToolUnavailable(String),
    /// The path does not exist.
    #[error("{0} does not exist")]
    Missing(String),
    /// The code object carries no signature.
    #[error("the helper is not signed")]
    Unsigned,
    /// The signature does not match the bytes on disk, or the code object is malformed.
    #[error("the helper's signature is not valid: {0}")]
    SignatureInvalid(String),
    /// The signing identifier is not one of the pinned bundle ids.
    #[error("the helper's identifier {actual:?} is not a pinned bundle id")]
    IdentifierNotPinned {
        /// What the signature says, if anything.
        actual: Option<String>,
    },
    /// The signature is valid but is not the pinned identity: wrong team, or no Apple anchor.
    #[error("the helper does not satisfy the pinned designated requirement: {detail}")]
    RequirementNotSatisfied {
        /// What differed.
        detail: String,
    },
    /// The hardened runtime flag is missing.
    #[error("the helper does not carry the hardened runtime")]
    NotHardened,
    /// The tree on disk is not the tree that was verified.
    #[error(
        "the helper's contents changed after verification (expected {expected}, found {actual})"
    )]
    FingerprintMismatch {
        /// SHA-256 recorded when it was verified.
        expected: String,
        /// SHA-256 now.
        actual: String,
    },
    /// An `active` or `previous` pointer is not a version id or names nothing.
    #[error("the {pointer} pointer is invalid: {detail}")]
    PointerInvalid {
        /// `active` or `previous`.
        pointer: &'static str,
        /// What is wrong.
        detail: String,
    },
    /// There is nothing installed.
    #[error("no helper is installed")]
    NoActiveVersion,
    /// There is no earlier version to return to.
    #[error("there is no previous helper version to roll back to")]
    NoPreviousVersion,
    /// The activation probe failed and the previous version was restored.
    #[error(
        "the new helper failed its activation probe and the previous version was restored: {0}"
    )]
    ProbeFailed(String),
    /// The helper could not be started.
    #[error("the helper could not be launched: {0}")]
    LaunchFailed(String),
    /// A filesystem step failed.
    #[error("helper install failed: {0}")]
    Io(String),
}

impl HelperError {
    /// The typed error code when the helper's *identity* is what was refused, else `None`.
    #[must_use]
    pub const fn identity_code(&self) -> Option<&'static str> {
        match self {
            Self::PinUnpublished
            | Self::Unsigned
            | Self::SignatureInvalid(_)
            | Self::IdentifierNotPinned { .. }
            | Self::RequirementNotSatisfied { .. }
            | Self::NotHardened
            | Self::FingerprintMismatch { .. } => Some(IDENTITY_REFUSED),
            _ => None,
        }
    }
}

impl From<std::io::Error> for HelperError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Anchor {
    /// Apple's anchor and the leaf certificate's organizational unit equal the team id.
    AppleDeveloper { team_id: String },
    /// Identifier only. Reachable only through [`HelperPin::unanchored_for_local_test`].
    Unanchored,
}

/// The identity a helper must have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperPin {
    allowed_bundle_ids: Vec<String>,
    anchor: Anchor,
}

impl HelperPin {
    /// The pin the configuration states.
    ///
    /// # Errors
    /// [`HelperError::PinUnpublished`] until the configuration is published with a team id.
    pub fn from_config(config: &HelperConfig) -> Result<Self, HelperError> {
        match (&config.status, &config.team_id) {
            (HelperStatus::Published, Some(team_id)) => Ok(Self {
                allowed_bundle_ids: config.allowed_bundle_ids.clone(),
                anchor: Anchor::AppleDeveloper {
                    team_id: team_id.clone(),
                },
            }),
            _ => Err(HelperError::PinUnpublished),
        }
    }

    /// A pin that checks the identifier and the signature but has no Apple anchor, for a helper signed
    /// ad hoc on a developer's own machine.
    ///
    /// Configuration cannot express this: the schema fixes `require_apple_anchor` to `true` and
    /// [`Self::from_config`] never builds it. It exists so the suite can run `codesign` against real
    /// signed binaries without a Developer ID.
    #[must_use]
    pub fn unanchored_for_local_test(bundle_ids: &[&str]) -> Self {
        Self {
            allowed_bundle_ids: bundle_ids.iter().map(ToString::to_string).collect(),
            anchor: Anchor::Unanchored,
        }
    }

    /// The code-signing requirement a helper with `identifier` must satisfy.
    #[must_use]
    pub fn requirement(&self, identifier: &str) -> String {
        match &self.anchor {
            Anchor::AppleDeveloper { team_id } => format!(
                "anchor apple generic and identifier \"{identifier}\" \
                 and certificate leaf[subject.OU] = \"{team_id}\""
            ),
            Anchor::Unanchored => format!("identifier \"{identifier}\""),
        }
    }

    /// Verify the helper at `path` and fingerprint it.
    ///
    /// # Errors
    /// The first thing wrong, as a typed [`HelperError`]; see its variants.
    pub fn verify(&self, path: &Path) -> Result<VerifiedHelper, HelperError> {
        if !cfg!(target_os = "macos") {
            return Err(HelperError::UnsupportedPlatform);
        }
        if !path.exists() {
            return Err(HelperError::Missing(path.display().to_string()));
        }
        let identity = inspect(path)?;
        let identifier = identity
            .identifier
            .clone()
            .filter(|id| self.allowed_bundle_ids.iter().any(|allowed| allowed == id))
            .ok_or_else(|| HelperError::IdentifierNotPinned {
                actual: identity.identifier.clone(),
            })?;
        let verification = run_codesign(&[
            "--verify",
            "--strict",
            "--deep",
            &path.display().to_string(),
        ])?;
        if !verification.success {
            return Err(HelperError::SignatureInvalid(verification.stderr));
        }
        let requirement = format!("={}", self.requirement(&identifier));
        let test = run_codesign(&[
            "--verify",
            "--strict",
            "--deep",
            "-R",
            &requirement,
            &path.display().to_string(),
        ])?;
        if !test.success {
            let detail =
                match &self.anchor {
                    Anchor::AppleDeveloper { team_id } => {
                        format!(
                    "signed by team {:?} with {}, required an Apple anchor and team {team_id}",
                    identity.team_id.as_deref().unwrap_or("none"),
                    if identity.adhoc { "an ad-hoc signature" } else { "another identity" },
                )
                    }
                    Anchor::Unanchored => "the identifier requirement failed".to_string(),
                };
            return Err(HelperError::RequirementNotSatisfied { detail });
        }
        if !identity.hardened_runtime {
            return Err(HelperError::NotHardened);
        }
        Ok(VerifiedHelper {
            fingerprint: tree_fingerprint(path)?,
            identity,
        })
    }
}

/// What `codesign` says a code object is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperIdentity {
    /// The signing identifier.
    pub identifier: Option<String>,
    /// The Apple Team ID, when there is one.
    pub team_id: Option<String>,
    /// The code directory hash.
    pub cdhash: String,
    /// Whether the hardened runtime flag is set.
    pub hardened_runtime: bool,
    /// Whether the signature is ad hoc.
    pub adhoc: bool,
}

/// A helper that passed verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedHelper {
    /// What the signature says.
    pub identity: HelperIdentity,
    /// SHA-256 over the whole tree, in a canonical order.
    pub fingerprint: String,
}

struct CodesignRun {
    success: bool,
    stderr: String,
}

fn run_codesign(args: &[&str]) -> Result<CodesignRun, HelperError> {
    let output = Command::new("/usr/bin/codesign")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| HelperError::ToolUnavailable(error.to_string()))?;
    Ok(CodesignRun {
        success: output.status.success(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
    })
}

/// Read a code object's signature details with `codesign -d`.
///
/// # Errors
/// [`HelperError::Unsigned`] for a code object with no signature.
pub fn inspect(path: &Path) -> Result<HelperIdentity, HelperError> {
    let run = run_codesign(&["-dv", "--verbose=4", &path.display().to_string()])?;
    if run.stderr.contains("not signed at all") {
        return Err(HelperError::Unsigned);
    }
    if !run.success {
        return Err(HelperError::SignatureInvalid(run.stderr));
    }
    let field = |name: &str| -> Option<String> {
        run.stderr
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{name}=")))
            .map(str::trim)
            .filter(|value| !value.is_empty() && *value != "not set")
            .map(str::to_string)
    };
    let flags = run
        .stderr
        .lines()
        .find(|line| line.starts_with("CodeDirectory "))
        .and_then(|line| line.split("flags=").nth(1))
        .unwrap_or("");
    let flag_names = flags
        .split_once('(')
        .map(|(_, rest)| rest.split(')').next().unwrap_or(""))
        .unwrap_or("");
    let names: BTreeSet<&str> = flag_names.split(',').map(str::trim).collect();
    Ok(HelperIdentity {
        identifier: field("Identifier"),
        team_id: field("TeamIdentifier"),
        cdhash: field("CDHash").unwrap_or_default(),
        hardened_runtime: names.contains("runtime"),
        adhoc: names.contains("adhoc") || field("Signature").as_deref() == Some("adhoc"),
    })
}

/// SHA-256 over a file or a directory tree: names, kinds, permission bits, contents and link targets,
/// in sorted order, so two trees have one fingerprint exactly when they are byte-for-byte the same.
///
/// # Errors
/// [`HelperError::Io`] when something cannot be read.
pub fn tree_fingerprint(path: &Path) -> Result<String, HelperError> {
    let mut hasher = Sha256::new();
    hash_entry(&mut hasher, path, "")?;
    Ok(format!("{:x}", hasher.finalize()))
}

fn hash_entry(hasher: &mut Sha256, path: &Path, name: &str) -> Result<(), HelperError> {
    use std::os::unix::fs::PermissionsExt;
    let meta = fs::symlink_metadata(path)?;
    let mode = meta.permissions().mode() & 0o7777;
    hasher.update(name.as_bytes());
    hasher.update([0]);
    if meta.file_type().is_symlink() {
        hasher.update(b"l");
        hasher.update(fs::read_link(path)?.as_os_str().as_encoded_bytes());
    } else if meta.is_dir() {
        hasher.update(b"d");
        hasher.update(mode.to_le_bytes());
        let mut children = fs::read_dir(path)?
            .map(|entry| entry.map(|e| e.file_name()))
            .collect::<Result<Vec<_>, _>>()?;
        children.sort();
        for child in children {
            let child_name = format!("{name}/{}", child.to_string_lossy());
            hash_entry(hasher, &path.join(&child), &child_name)?;
        }
    } else {
        hasher.update(b"f");
        hasher.update(mode.to_le_bytes());
        hasher.update(fs::read(path)?);
    }
    hasher.update([0xff]);
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), HelperError> {
    let meta = fs::symlink_metadata(source)?;
    if meta.file_type().is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(source)?, destination)?;
    } else if meta.is_dir() {
        fs::create_dir(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_tree(&entry.path(), &destination.join(entry.file_name()))?;
        }
        fs::set_permissions(destination, meta.permissions())?;
    } else {
        fs::copy(source, destination)?;
    }
    Ok(())
}

/// One installed version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledHelper {
    /// SHA-256 of the tree; also the version directory's name.
    pub fingerprint: String,
    /// The helper's path inside its version directory.
    pub path: PathBuf,
    /// What verification said.
    pub identity: HelperIdentity,
}

/// A launched helper.
#[derive(Debug)]
pub struct HelperProcess {
    child: Child,
    /// The version that was launched.
    pub installed: InstalledHelper,
    /// The `--profile` value it carries.
    pub profile: String,
}

impl HelperProcess {
    /// The process id.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Stop the process and reap it.
    pub fn terminate(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Whether the process has exited.
    pub fn has_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }
}

/// The versioned install directory and the pin it is held to.
#[derive(Debug, Clone)]
pub struct HelperInstaller {
    root: PathBuf,
    pin: HelperPin,
    keep_versions: usize,
    profile_prefix: String,
    terminate_grace: Duration,
}

impl HelperInstaller {
    /// An installer over `<app_data>/<config.install_directory>`.
    ///
    /// # Errors
    /// [`HelperError::PinUnpublished`] when configuration has no signing identity, [`HelperError::Io`]
    /// when the directory cannot be created.
    pub fn from_config(app_data: &Path, config: &HelperConfig) -> Result<Self, HelperError> {
        Self::new(
            &app_data.join(&config.install_directory),
            HelperPin::from_config(config)?,
            config,
        )
    }

    /// An installer over `root` under an explicit pin, taking its other settings from `config`.
    ///
    /// # Errors
    /// [`HelperError::Io`] when the directory cannot be created.
    pub fn new(root: &Path, pin: HelperPin, config: &HelperConfig) -> Result<Self, HelperError> {
        fs::create_dir_all(root.join("versions"))?;
        Ok(Self {
            root: fs::canonicalize(root)?,
            pin,
            keep_versions: config.keep_versions,
            profile_prefix: config.profile_prefix.clone(),
            terminate_grace: config.terminate_grace,
        })
    }

    /// The install root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn versions(&self) -> PathBuf {
        self.root.join("versions")
    }

    fn pointer_path(&self, pointer: &'static str) -> PathBuf {
        self.root.join(pointer)
    }

    fn read_pointer(&self, pointer: &'static str) -> Result<Option<String>, HelperError> {
        let text = match fs::read_to_string(self.pointer_path(pointer)) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let id = text.trim();
        if id.len() == 64
            && id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            Ok(Some(id.to_string()))
        } else {
            Err(HelperError::PointerInvalid {
                pointer,
                detail: "not a 64-character version id".into(),
            })
        }
    }

    fn write_pointer(&self, pointer: &'static str, id: Option<&str>) -> Result<(), HelperError> {
        let path = self.pointer_path(pointer);
        match id {
            None => {
                let _ = fs::remove_file(&path);
            }
            Some(id) => {
                let temporary = self
                    .root
                    .join(format!(".{pointer}.{}.tmp", std::process::id()));
                let mut file = fs::File::create(&temporary)?;
                file.write_all(format!("{id}\n").as_bytes())?;
                file.sync_all()?;
                fs::rename(&temporary, &path)?;
            }
        }
        Ok(())
    }

    fn resolve(&self, pointer: &'static str, id: &str) -> Result<PathBuf, HelperError> {
        let directory = self.versions().join(id);
        let mut entries = fs::read_dir(&directory)
            .map_err(|error| HelperError::PointerInvalid {
                pointer,
                detail: format!("version {id} is not installed ({error})"),
            })?
            .collect::<Result<Vec<_>, _>>()?;
        if entries.len() != 1 {
            return Err(HelperError::PointerInvalid {
                pointer,
                detail: format!("version {id} must hold exactly one helper"),
            });
        }
        Ok(entries.remove(0).path())
    }

    /// Verify a version in place against the fingerprint it is named by.
    fn verify_installed(
        &self,
        pointer: &'static str,
        id: &str,
    ) -> Result<InstalledHelper, HelperError> {
        let path = self.resolve(pointer, id)?;
        let verified = self.pin.verify(&path)?;
        if verified.fingerprint != id {
            return Err(HelperError::FingerprintMismatch {
                expected: id.to_string(),
                actual: verified.fingerprint,
            });
        }
        Ok(InstalledHelper {
            fingerprint: id.to_string(),
            path,
            identity: verified.identity,
        })
    }

    /// The active version, verified.
    ///
    /// # Errors
    /// [`HelperError::NoActiveVersion`] when nothing is installed; any verification error otherwise.
    pub fn active(&self) -> Result<InstalledHelper, HelperError> {
        let id = self
            .read_pointer("active")?
            .ok_or(HelperError::NoActiveVersion)?;
        self.verify_installed("active", &id)
    }

    /// The previous version, verified, when there is one.
    ///
    /// # Errors
    /// Any verification error for the version the pointer names.
    pub fn previous(&self) -> Result<Option<InstalledHelper>, HelperError> {
        match self.read_pointer("previous")? {
            None => Ok(None),
            Some(id) => self.verify_installed("previous", &id).map(Some),
        }
    }

    /// Verify `source`, copy it into its version directory, verify the copy against the same
    /// fingerprint and CDHash, and make it the active version (the old one becomes `previous`).
    ///
    /// # Errors
    /// Any verification error for the source or the copy. A refused source touches nothing.
    pub fn install(&self, source: &Path) -> Result<InstalledHelper, HelperError> {
        let verified = self.pin.verify(source)?;
        let id = verified.fingerprint.clone();
        let destination = self.versions().join(&id);
        if destination.exists() {
            // Already installed: trust nothing about what is there, verify it like a fresh copy.
            self.verify_installed("active", &id)?;
        } else {
            let name = source
                .file_name()
                .ok_or_else(|| HelperError::Io("the helper path has no file name".into()))?;
            let staging = self
                .versions()
                .join(format!(".staging-{id}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&staging);
            fs::create_dir(&staging)?;
            let outcome = (|| {
                let copy = staging.join(name);
                copy_tree(source, &copy)?;
                let again = self.pin.verify(&copy)?;
                if again.fingerprint != id {
                    return Err(HelperError::FingerprintMismatch {
                        expected: id.clone(),
                        actual: again.fingerprint,
                    });
                }
                if again.identity.cdhash != verified.identity.cdhash {
                    return Err(HelperError::FingerprintMismatch {
                        expected: verified.identity.cdhash.clone(),
                        actual: again.identity.cdhash,
                    });
                }
                Ok(())
            })();
            if let Err(error) = outcome {
                let _ = fs::remove_dir_all(&staging);
                return Err(error);
            }
            fs::rename(&staging, &destination)?;
        }
        let installed = self.verify_installed("active", &id)?;
        let current = self.read_pointer("active").ok().flatten();
        if current.as_deref() != Some(id.as_str()) {
            self.write_pointer("previous", current.as_deref())?;
            self.write_pointer("active", Some(&id))?;
        }
        self.prune()?;
        Ok(installed)
    }

    /// As [`Self::install`], then run `probe` against the new version; a failed probe restores the
    /// version that was active and returns [`HelperError::ProbeFailed`].
    ///
    /// # Errors
    /// As [`Self::install`], or [`HelperError::ProbeFailed`].
    pub fn install_checked(
        &self,
        source: &Path,
        probe: impl FnOnce(&InstalledHelper) -> Result<(), String>,
    ) -> Result<InstalledHelper, HelperError> {
        let before_active = self.read_pointer("active").ok().flatten();
        let before_previous = self.read_pointer("previous").ok().flatten();
        let installed = self.install(source)?;
        match probe(&installed) {
            Ok(()) => Ok(installed),
            Err(reason) => {
                self.write_pointer("active", before_active.as_deref())?;
                self.write_pointer("previous", before_previous.as_deref())?;
                Err(HelperError::ProbeFailed(reason))
            }
        }
    }

    /// Make the previous version active again; the version that was active becomes `previous`, so a
    /// second call undoes the first.
    ///
    /// # Errors
    /// [`HelperError::NoPreviousVersion`] when there is none; a verification error when it no longer
    /// matches its fingerprint.
    pub fn rollback(&self) -> Result<InstalledHelper, HelperError> {
        let previous = self
            .read_pointer("previous")?
            .ok_or(HelperError::NoPreviousVersion)?;
        let installed = self.verify_installed("previous", &previous)?;
        let active = self.read_pointer("active")?;
        self.write_pointer("previous", active.as_deref())?;
        self.write_pointer("active", Some(&previous))?;
        Ok(installed)
    }

    /// Keep the active and previous versions and the newest others up to `keep_versions`.
    fn prune(&self) -> Result<(), HelperError> {
        let keep: BTreeSet<String> = ["active", "previous"]
            .into_iter()
            .filter_map(|pointer| self.read_pointer(pointer).ok().flatten())
            .collect();
        let mut others = Vec::new();
        for entry in fs::read_dir(self.versions())? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || keep.contains(&name) {
                continue;
            }
            let modified = entry.metadata()?.modified()?;
            others.push((modified, entry.path()));
        }
        others.sort();
        let room = self.keep_versions.saturating_sub(keep.len());
        let excess = others.len().saturating_sub(room);
        for (_, path) in others.into_iter().take(excess) {
            fs::remove_dir_all(path)?;
        }
        Ok(())
    }

    /// Launch the active version: verify it again first, then start it with
    /// `--profile <prefix><suffix>`.
    ///
    /// # Errors
    /// Any verification error (the helper on disk is not what was installed), or
    /// [`HelperError::LaunchFailed`].
    pub fn launch(
        &self,
        profile_suffix: &str,
        args: &[&str],
    ) -> Result<HelperProcess, HelperError> {
        if profile_suffix.is_empty()
            || !profile_suffix
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(HelperError::LaunchFailed(
                "the profile suffix must be lowercase letters, digits and hyphens".into(),
            ));
        }
        let installed = self.active()?;
        let profile = format!("{}{profile_suffix}", self.profile_prefix);
        let child = Command::new(&installed.path)
            .arg("--profile")
            .arg(&profile)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| HelperError::LaunchFailed(error.to_string()))?;
        Ok(HelperProcess {
            child,
            installed,
            profile,
        })
    }

    /// Terminate helpers a crashed runtime left behind: processes whose command line names a file under
    /// this install directory and the configured profile prefix. `keep` lists pids that are this
    /// runtime's own and are left alone.
    ///
    /// # Errors
    /// [`HelperError::Io`] when the process table cannot be read.
    pub fn terminate_orphans(&self, keep: &[u32]) -> Result<Vec<OrphanOutcome>, HelperError> {
        let prefix = format!("{}/", self.versions().display());
        let profile = format!("--profile {}", self.profile_prefix);
        let own = std::process::id();
        let mut outcomes = Vec::new();
        for (pid, command) in process_table()? {
            if pid == own || keep.contains(&pid) {
                continue;
            }
            if !command.starts_with(&prefix) || !command.contains(&profile) {
                continue;
            }
            outcomes.push(OrphanOutcome {
                pid,
                command,
                killed: stop_process(pid, self.terminate_grace),
            });
        }
        Ok(outcomes)
    }
}

/// What happened to one orphaned helper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanOutcome {
    /// Process id.
    pub pid: u32,
    /// Its command line.
    pub command: String,
    /// `false` when it exited on `SIGTERM`, `true` when it had to be killed.
    pub killed: bool,
}

fn process_table() -> Result<Vec<(u32, String)>, HelperError> {
    let output = Command::new("/bin/ps")
        .args(["-axww", "-o", "pid=,command="])
        .output()
        .map_err(|error| HelperError::Io(format!("ps: {error}")))?;
    if !output.status.success() {
        return Err(HelperError::Io("ps failed".into()));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let (pid, command) = line.split_once(char::is_whitespace)?;
            Some((pid.parse().ok()?, command.trim_start().to_string()))
        })
        .collect())
}

/// Whether a process exists and is not a zombie awaiting its parent.
#[must_use]
pub fn process_alive(pid: u32) -> bool {
    Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "stat="])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .is_some_and(|stat| !stat.is_empty() && !stat.starts_with('Z'))
}

fn signal(pid: u32, name: &str) {
    let _ = Command::new("/bin/kill")
        .args([&format!("-{name}"), &pid.to_string()])
        .stderr(Stdio::null())
        .status();
}

/// `SIGTERM`, wait up to `grace`, then `SIGKILL`. Returns whether the kill was needed.
fn stop_process(pid: u32, grace: Duration) -> bool {
    signal(pid, "TERM");
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if !process_alive(pid) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    signal(pid, "KILL");
    let settle = Instant::now() + Duration::from_secs(2);
    while Instant::now() < settle && process_alive(pid) {
        std::thread::sleep(Duration::from_millis(25));
    }
    true
}
