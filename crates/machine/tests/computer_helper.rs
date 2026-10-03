//! Helper identity pinning (ADP-002, DOMAIN.md §8.6.1 D), against the platform's own `codesign`.
//!
//! The helpers here are real Mach-O executables built with the system C compiler and signed with real
//! `codesign` (ad hoc, because no Developer ID is available to a test): one with the pinned identifier,
//! one with a wrong identifier, one unsigned, one without the hardened runtime and one altered after
//! signing. The production requirement (an Apple anchor and the configured team id) is checked
//! negatively: an ad-hoc signature must not satisfy it. There is no positive production run here because
//! that needs the Developer ID the configuration is still waiting for (`helper.status: unpublished`).
//!
//! Environment: macOS with `/usr/bin/codesign` and `cc`. Without them each test prints a
//! `BLOCKED_EXTERNAL` marker and returns.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use quansio_machine::computer_use::config::{ComputerUseConfig, COMPUTER_USE_YAML};
use quansio_machine::computer_use::helper::{
    inspect, process_alive, tree_fingerprint, HelperError, HelperInstaller, HelperPin,
    IDENTITY_REFUSED,
};

const PINNED_ID: &str = "com.quansio.computer-helper";
const SECOND_ID: &str = "com.quansio.computer-helper.next";

const SOURCE: &str = r#"
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#ifndef VARIANT
#define VARIANT 0
#endif
int main(int argc, char **argv) {
    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "--sleep") == 0) { sleep(60); }
    }
    printf("helper-ok %d\n", VARIANT);
    return 0;
}
"#;

fn have_tools() -> bool {
    cfg!(target_os = "macos")
        && Path::new("/usr/bin/codesign").exists()
        && Command::new("cc").arg("--version").output().is_ok()
}

fn blocked() {
    eprintln!("BLOCKED_EXTERNAL: macOS codesign and a C compiler are required for the helper identity suite");
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("quansio-adp002-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("scratch");
        // The canonical path is what `ps` will show for a process started from it.
        Self(fs::canonicalize(&path).expect("canonical"))
    }

    fn path(&self) -> &Path {
        &self.0
    }

    /// Build the helper with a `VARIANT` constant, so two builds differ in bytes.
    fn build(&self, name: &str, variant: u32) -> PathBuf {
        let source = self.0.join(format!("{name}.c"));
        fs::write(&source, SOURCE).expect("source");
        let out = self.0.join(name);
        let status = Command::new("cc")
            .arg(format!("-DVARIANT={variant}"))
            .arg("-o")
            .arg(&out)
            .arg(&source)
            .status()
            .expect("cc");
        assert!(status.success(), "cc failed");
        out
    }

    fn sign(&self, path: &Path, identifier: &str, hardened: bool) {
        let mut command = Command::new("/usr/bin/codesign");
        command.args(["--force", "-s", "-", "--identifier", identifier]);
        if hardened {
            command.args(["-o", "runtime"]);
        }
        let output = command.arg(path).output().expect("codesign");
        assert!(
            output.status.success(),
            "codesign failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn signed(&self, name: &str, variant: u32, identifier: &str) -> PathBuf {
        let path = self.build(name, variant);
        self.sign(&path, identifier, true);
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Change one byte of the program's own text, which the signature's page hashes cover. (The last byte of
/// the file is in the signature blob's padding and changing it proves nothing.)
fn alter_signed_code(path: &Path) {
    let mut bytes = fs::read(path).expect("read");
    let at = bytes
        .windows(b"helper-ok".len())
        .position(|window| window == b"helper-ok")
        .expect("the helper's own string is in the file");
    bytes[at] ^= 0x20;
    fs::write(path, bytes).expect("write");
}

fn local_pin() -> HelperPin {
    HelperPin::unanchored_for_local_test(&[PINNED_ID, SECOND_ID])
}

fn installer(scratch: &Scratch, keep: usize) -> HelperInstaller {
    let mut config = ComputerUseConfig::builtin().helper.clone();
    config.keep_versions = keep;
    config.terminate_grace = Duration::from_millis(1500);
    HelperInstaller::new(
        &scratch.path().join("data").join("computer-helper"),
        local_pin(),
        &config,
    )
    .expect("installer")
}

/// The configuration with the helper published under a team id, as a release would carry it.
fn published_config() -> ComputerUseConfig {
    let yaml = COMPUTER_USE_YAML
        .replace("status: unpublished", "status: published")
        .replace("team_id: null", "team_id: ABCDE12345");
    ComputerUseConfig::parse(&yaml).expect("published config")
}

// ------------------------------------------------------------------ verification

#[test]
fn a_helper_signed_as_pinned_is_accepted_and_fingerprinted() {
    if !have_tools() {
        return blocked();
    }
    let scratch = Scratch::new("accept");
    let helper = scratch.signed("helper", 1, PINNED_ID);
    let verified = local_pin().verify(&helper).expect("accepted");
    assert_eq!(verified.identity.identifier.as_deref(), Some(PINNED_ID));
    assert!(verified.identity.hardened_runtime);
    assert!(verified.identity.adhoc);
    assert_eq!(verified.identity.cdhash.len(), 40);
    assert_eq!(verified.fingerprint.len(), 64);
    // The fingerprint is a function of the bytes alone.
    let copy = scratch.path().join("copy");
    fs::copy(&helper, &copy).expect("copy");
    assert_eq!(
        tree_fingerprint(&copy).expect("fingerprint"),
        verified.fingerprint
    );
    assert_eq!(inspect(&helper).expect("inspect"), verified.identity);
}

#[test]
fn a_helper_with_the_wrong_identity_is_refused() {
    if !have_tools() {
        return blocked();
    }
    let scratch = Scratch::new("refuse");
    let pin = local_pin();

    // Validly signed, by the wrong identifier.
    let imposter = scratch.signed("imposter", 1, "com.example.imposter");
    let error = pin.verify(&imposter).expect_err("wrong identifier");
    assert_eq!(
        error,
        HelperError::IdentifierNotPinned {
            actual: Some("com.example.imposter".to_string())
        }
    );
    assert_eq!(error.identity_code(), Some(IDENTITY_REFUSED));
    assert_eq!(IDENTITY_REFUSED, "COMPUTER_HELPER_IDENTITY_REFUSED");

    // Not signed at all.
    let unsigned = scratch.build("unsigned", 1);
    let status = Command::new("/usr/bin/codesign")
        .arg("--remove-signature")
        .arg(&unsigned)
        .status()
        .expect("codesign");
    assert!(status.success());
    let error = pin.verify(&unsigned).expect_err("unsigned");
    assert_eq!(error, HelperError::Unsigned);
    assert_eq!(error.identity_code(), Some(IDENTITY_REFUSED));

    // Right identifier, but not the hardened runtime.
    let soft = scratch.build("soft", 1);
    scratch.sign(&soft, PINNED_ID, false);
    let error = pin.verify(&soft).expect_err("no hardened runtime");
    assert_eq!(error, HelperError::NotHardened);

    // Right identity, then altered after signing.
    let tampered = scratch.signed("tampered", 1, PINNED_ID);
    alter_signed_code(&tampered);
    let error = pin.verify(&tampered).expect_err("tampered");
    assert!(
        matches!(error, HelperError::SignatureInvalid(_)),
        "{error:?}"
    );
    assert_eq!(error.identity_code(), Some(IDENTITY_REFUSED));

    // Something that is not there is not an identity question.
    let missing = pin
        .verify(&scratch.path().join("nothing"))
        .expect_err("missing");
    assert!(matches!(missing, HelperError::Missing(_)));
    assert_eq!(missing.identity_code(), None);
}

#[test]
fn the_production_pin_needs_an_apple_anchor_and_the_configured_team() {
    if !have_tools() {
        return blocked();
    }
    // As committed, the configuration has no signing identity, so nothing may launch.
    let committed = ComputerUseConfig::builtin();
    assert_eq!(
        HelperPin::from_config(&committed.helper).expect_err("unpublished"),
        HelperError::PinUnpublished
    );
    assert_eq!(
        HelperPin::from_config(&committed.helper)
            .expect_err("unpublished")
            .identity_code(),
        Some(IDENTITY_REFUSED)
    );
    assert!(committed.helper.team_id.is_none());

    // Once published under a team id, a helper with the pinned identifier and the hardened runtime is
    // still refused when its signature is ad hoc: it has no Apple anchor and no team.
    let scratch = Scratch::new("production");
    let helper = scratch.signed("helper", 1, PINNED_ID);
    let pin = HelperPin::from_config(&published_config().helper).expect("published pin");
    let error = pin
        .verify(&helper)
        .expect_err("ad hoc is not the pinned team");
    let HelperError::RequirementNotSatisfied { detail } = &error else {
        panic!("expected a requirement failure, got {error:?}");
    };
    assert!(detail.contains("ABCDE12345"), "{detail}");
    assert!(detail.contains("ad-hoc"), "{detail}");
    assert_eq!(error.identity_code(), Some(IDENTITY_REFUSED));
    // The requirement the platform checks is built from the configured values, not a constant.
    let requirement = pin.requirement(PINNED_ID);
    assert!(requirement.contains("anchor apple generic"));
    assert!(requirement.contains("certificate leaf[subject.OU] = \"ABCDE12345\""));
    assert!(requirement.contains(&format!("identifier \"{PINNED_ID}\"")));

    // An installer cannot even be built from the unpublished configuration.
    let refused = HelperInstaller::from_config(scratch.path(), &committed.helper);
    assert!(matches!(refused, Err(HelperError::PinUnpublished)));
}

#[test]
fn configuration_that_weakens_the_pin_does_not_load() {
    let bad = |from: &str, to: &str| -> String {
        assert!(COMPUTER_USE_YAML.contains(from), "{from}");
        COMPUTER_USE_YAML.replace(from, to)
    };
    let cases = [
        // A published helper with no team id.
        bad("status: unpublished", "status: published"),
        // The anchor or the hardened runtime switched off.
        bad("require_apple_anchor: true", "require_apple_anchor: false"),
        bad(
            "require_hardened_runtime: true",
            "require_hardened_runtime: false",
        ),
        // A team id that is not a team id.
        bad("team_id: null", "team_id: not-a-team"),
        // An unknown key is not ignored.
        bad("version: 1", "version: 1\nallow_unsigned: true"),
        bad("keep_versions: 2", "keep_versions: 1"),
        bad("Privacy_Accessibility", "Privacy_Accessibility/../../x"),
        bad("com.quansio.computer-helper", "not a bundle id"),
    ];
    for yaml in cases {
        assert!(ComputerUseConfig::parse(&yaml).is_err(), "{yaml}");
    }
    assert!(ComputerUseConfig::parse(COMPUTER_USE_YAML).is_ok());
    assert!(ComputerUseConfig::parse("version: 1").is_err());
}

// ------------------------------------------------------------------ install, rollback, launch

#[test]
fn install_copies_into_a_versioned_directory_verifies_the_copy_and_prunes() {
    if !have_tools() {
        return blocked();
    }
    let scratch = Scratch::new("install");
    let installer = installer(&scratch, 2);
    let first_source = scratch.signed("first", 1, PINNED_ID);
    let first = installer.install(&first_source).expect("install first");
    let expected = tree_fingerprint(&first_source).expect("fingerprint");
    assert_eq!(first.fingerprint, expected);
    assert!(first
        .path
        .starts_with(installer.root().join("versions").join(&expected)));
    assert!(first.path.exists());
    // The copy is the same signed code, identified by the same CDHash.
    assert_eq!(
        first.identity.cdhash,
        inspect(&first_source).expect("inspect").cdhash
    );
    assert_eq!(installer.active().expect("active"), first);
    assert_eq!(installer.previous().expect("previous"), None);

    // Installing the same helper again changes nothing.
    assert_eq!(installer.install(&first_source).expect("again"), first);
    assert_eq!(installer.previous().expect("previous"), None);

    // A second version becomes active and the first its rollback target.
    let second = installer
        .install(&scratch.signed("second", 2, SECOND_ID))
        .expect("install second");
    assert_ne!(second.fingerprint, first.fingerprint);
    assert_eq!(installer.active().expect("active"), second);
    assert_eq!(installer.previous().expect("previous"), Some(first.clone()));

    // Rollback returns to the first; a second rollback undoes that.
    assert_eq!(installer.rollback().expect("rollback"), first);
    assert_eq!(installer.active().expect("active"), first);
    assert_eq!(
        installer.previous().expect("previous"),
        Some(second.clone())
    );
    assert_eq!(installer.rollback().expect("rollback again"), second);

    // A third version prunes the oldest that is neither active nor previous.
    let third = installer
        .install(&scratch.signed("third", 3, PINNED_ID))
        .expect("install third");
    let versions: Vec<String> = fs::read_dir(installer.root().join("versions"))
        .expect("versions")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .to_string()
        })
        .collect();
    assert_eq!(versions.len(), 2, "{versions:?}");
    assert!(versions.contains(&third.fingerprint));
    assert!(versions.contains(&second.fingerprint));
    assert!(!versions.contains(&first.fingerprint));

    // No staging directory is left behind.
    assert!(versions.iter().all(|name| !name.starts_with('.')));
}

#[test]
fn a_refused_source_installs_nothing() {
    if !have_tools() {
        return blocked();
    }
    let scratch = Scratch::new("install_refused");
    let installer = installer(&scratch, 2);
    let good = installer
        .install(&scratch.signed("good", 1, PINNED_ID))
        .expect("install");
    let imposter = scratch.signed("imposter", 2, "com.example.imposter");
    let error = installer.install(&imposter).expect_err("refused");
    assert!(matches!(error, HelperError::IdentifierNotPinned { .. }));
    // Nothing moved: the same helper is active, with no rollback target, and only one version is on disk.
    assert_eq!(installer.active().expect("active"), good);
    assert_eq!(installer.previous().expect("previous"), None);
    assert_eq!(
        fs::read_dir(installer.root().join("versions"))
            .expect("versions")
            .count(),
        1
    );
}

#[test]
fn a_helper_that_changes_after_install_is_refused_at_launch() {
    if !have_tools() {
        return blocked();
    }
    let scratch = Scratch::new("launch_tamper");
    let installer = installer(&scratch, 2);
    let installed = installer
        .install(&scratch.signed("helper", 1, PINNED_ID))
        .expect("install");

    // Before the change it launches, under the configured profile prefix.
    let mut process = installer.launch("run-one", &[]).expect("launch");
    assert!(process.profile.starts_with("quansio-computer-"));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !process.has_exited() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(process.has_exited());
    process.terminate();

    // After one byte changes, neither `active` nor `launch` will use it.
    let mut permissions = fs::metadata(&installed.path).expect("meta").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&installed.path, permissions).expect("chmod");
    alter_signed_code(&installed.path);
    let error = installer.launch("run-two", &[]).expect_err("tampered");
    assert_eq!(error.identity_code(), Some(IDENTITY_REFUSED), "{error:?}");
    assert!(installer.active().is_err());

    // Re-signing the altered file with the pinned identifier makes its signature valid again, which is
    // exactly what a local attacker with write access could do. The version is named by the fingerprint
    // of what was verified, so it is still refused.
    scratch.sign(&installed.path, PINNED_ID, true);
    assert!(
        local_pin().verify(&installed.path).is_ok(),
        "the re-signed file is validly signed"
    );
    let error = installer
        .launch("run-three", &[])
        .expect_err("fingerprint changed");
    assert!(
        matches!(error, HelperError::FingerprintMismatch { ref expected, .. } if *expected == installed.fingerprint),
        "{error:?}"
    );
    assert_eq!(error.identity_code(), Some(IDENTITY_REFUSED));
}

#[test]
fn a_pointer_that_is_not_a_version_is_refused() {
    if !have_tools() {
        return blocked();
    }
    let scratch = Scratch::new("pointer");
    let installer = installer(&scratch, 2);
    installer
        .install(&scratch.signed("helper", 1, PINNED_ID))
        .expect("install");
    for bad in ["../../etc", "not-hex", ""] {
        fs::write(installer.root().join("active"), format!("{bad}\n")).expect("write pointer");
        assert!(
            matches!(installer.active(), Err(HelperError::PointerInvalid { .. })),
            "{bad:?}"
        );
    }
    // A well-formed id that names nothing is refused too.
    fs::write(
        installer.root().join("active"),
        format!("{}\n", "a".repeat(64)),
    )
    .expect("write");
    assert!(matches!(
        installer.active(),
        Err(HelperError::PointerInvalid { .. })
    ));
    // With nothing installed there is nothing to launch.
    fs::remove_file(installer.root().join("active")).expect("remove");
    assert_eq!(
        installer.active().expect_err("none"),
        HelperError::NoActiveVersion
    );
    assert_eq!(
        installer.rollback().expect_err("none"),
        HelperError::NoPreviousVersion
    );
}

#[test]
fn a_failed_activation_probe_restores_the_previous_version() {
    if !have_tools() {
        return blocked();
    }
    let scratch = Scratch::new("probe");
    let installer = installer(&scratch, 3);
    let first = installer
        .install(&scratch.signed("first", 1, PINNED_ID))
        .expect("install");
    let result = installer.install_checked(&scratch.signed("second", 2, PINNED_ID), |_| {
        Err("the helper did not answer".to_string())
    });
    assert_eq!(
        result.expect_err("probe fails"),
        HelperError::ProbeFailed("the helper did not answer".to_string())
    );
    assert_eq!(
        installer.active().expect("active"),
        first,
        "the earlier version is active again"
    );
    assert_eq!(installer.previous().expect("previous"), None);

    // A probe that passes keeps the new version.
    let second = installer
        .install_checked(&scratch.signed("second", 2, PINNED_ID), |installed| {
            assert!(installed.path.exists());
            Ok(())
        })
        .expect("probe passes");
    assert_eq!(installer.active().expect("active"), second);
    assert_eq!(installer.previous().expect("previous"), Some(first));
}

// ------------------------------------------------------------------ orphans

fn wait_for(deadline: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + deadline;
    while Instant::now() < end {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    condition()
}

fn pids_running(needle: &str) -> Vec<u32> {
    let output = Command::new("/bin/ps")
        .args(["-axww", "-o", "pid=,command="])
        .output()
        .expect("ps");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.contains(needle))
        .filter_map(|line| line.split_whitespace().next()?.parse().ok())
        .collect()
}

/// Start `path --profile <profile> --sleep` detached from this process, as a crashed runtime's helper
/// would be: its parent is gone and launchd owns it.
fn start_orphan(path: &Path, profile: &str) {
    let status = Command::new("/bin/sh")
        .arg("-c")
        .arg("\"$0\" --profile \"$1\" --sleep >/dev/null 2>&1 &")
        .arg(path)
        .arg(profile)
        .status()
        .expect("sh");
    assert!(status.success());
}

#[test]
fn orphaned_helpers_are_terminated_at_start_and_nothing_else_is() {
    if !have_tools() {
        return blocked();
    }
    let scratch = Scratch::new("orphans");
    let installer = installer(&scratch, 2);
    let installed = installer
        .install(&scratch.signed("helper", 1, PINNED_ID))
        .expect("install");
    let install_dir = installer.root().display().to_string();

    // The orphan: the installed helper, under the runtime's profile prefix, whose runtime died.
    start_orphan(&installed.path, "quansio-computer-crashed-run");
    // Controls that must survive: the same helper under another profile prefix, and a copy of the helper
    // outside the install directory carrying the right prefix.
    start_orphan(&installed.path, "someone-elses-profile");
    let elsewhere = scratch.path().join("elsewhere-helper");
    fs::copy(&installed.path, &elsewhere).expect("copy");
    start_orphan(&elsewhere, "quansio-computer-not-ours");

    assert!(
        wait_for(Duration::from_secs(5), || {
            pids_running("quansio-computer-crashed-run").len() == 1
                && pids_running("someone-elses-profile").len() == 1
                && pids_running("quansio-computer-not-ours").len() == 1
        }),
        "the processes did not start"
    );
    let orphan_pid = pids_running("quansio-computer-crashed-run")[0];
    let other_profile = pids_running("someone-elses-profile")[0];
    let other_path = pids_running("quansio-computer-not-ours")[0];
    assert!(pids_running(&install_dir).len() >= 2);

    // A helper this runtime launched itself is passed in `keep` and is not touched.
    let mut own = installer.launch("mine", &["--sleep"]).expect("launch");
    let outcomes = installer.terminate_orphans(&[own.pid()]).expect("sweep");

    assert_eq!(outcomes.len(), 1, "{outcomes:?}");
    assert_eq!(outcomes[0].pid, orphan_pid);
    assert!(
        !outcomes[0].killed,
        "it exits on SIGTERM; no kill was needed"
    );
    assert!(wait_for(Duration::from_secs(3), || !process_alive(
        orphan_pid
    )));
    assert!(
        process_alive(other_profile),
        "another profile prefix was terminated"
    );
    assert!(
        process_alive(other_path),
        "a helper outside the install directory was terminated"
    );
    assert!(
        process_alive(own.pid()),
        "the runtime's own helper was terminated"
    );

    // Clean up the controls.
    for pid in [other_profile, other_path] {
        let _ = Command::new("/bin/kill").arg(pid.to_string()).status();
    }
    own.terminate();
    assert!(wait_for(Duration::from_secs(3), || {
        !process_alive(other_profile) && !process_alive(other_path)
    }));
}

#[test]
fn a_second_sweep_finds_nothing_to_do() {
    if !have_tools() {
        return blocked();
    }
    let scratch = Scratch::new("orphans_none");
    let installer = installer(&scratch, 2);
    installer
        .install(&scratch.signed("helper", 1, PINNED_ID))
        .expect("install");
    assert!(installer.terminate_orphans(&[]).expect("sweep").is_empty());
}
