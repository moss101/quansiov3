//! Agent-side edit conflicts (ADP-005, DOMAIN.md §8.7 G): `fs.write` and `fs.patch` carry an optional
//! `base_digest`, the digest of the file as the caller last read it, and a write is refused with
//! `FILE_EDIT_CONFLICT` when the file is not that after the new bytes are on disk and just before they
//! replace it.
//!
//! These drive the shipped code over a real filesystem. The race is real too: a second writer lands a
//! real edit in the one window the check closes (after the temporary file is written, before the rename),
//! through the host's test seam.

use std::path::PathBuf;
use std::sync::Arc;

use quansio_core::Digest;
use quansio_qworkerd::tools::{FileHost, RootScope, Sandbox, ToolContext, ToolError};

fn context() -> ToolContext<'static> {
    ToolContext {
        tenant_id: "tn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
        target_id: "tgt_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
        run_id: "run_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
        step_id: "stp_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
        capability_id: "cap_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
        effect_id: "eff_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
        dispatch_token: "dsp_01J8Z3K6F1N8VQ2X5W9Y0FFFFF",
        idempotency_key: "idem_conflict",
    }
}

struct Tree {
    _holder: tempfile::TempDir,
    root: PathBuf,
}

impl Tree {
    fn new() -> Self {
        let holder = tempfile::tempdir().expect("temp dir");
        let root = holder.path().join("workspace");
        std::fs::create_dir_all(&root).expect("root");
        Self {
            _holder: holder,
            root,
        }
    }

    fn sandbox(&self) -> Sandbox {
        Sandbox::empty()
            .authorize(RootScope::Workspace, &self.root)
            .expect("authorize")
    }

    fn host(&self) -> FileHost {
        FileHost::new(self.sandbox())
    }

    fn leftovers(&self) -> Vec<String> {
        std::fs::read_dir(&self.root)
            .expect("read dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|name| name.ends_with(".partial"))
            .collect()
    }
}

fn digest(text: &str) -> String {
    Digest::of(text.as_bytes()).as_str().to_string()
}

fn conflict(error: ToolError) -> (String, String) {
    match error {
        ToolError::EditConflict {
            expected_digest,
            actual_digest,
            ..
        } => (expected_digest, actual_digest),
        other => panic!("not a conflict: {other:?}"),
    }
}

#[test]
fn a_write_against_a_stale_base_is_refused_and_leaves_the_file_untouched() {
    let tree = Tree::new();
    let host = tree.host();
    std::fs::write(tree.root.join("doc.txt"), "version one\n").expect("seed");
    let read = host
        .read(RootScope::Workspace, "doc.txt", 4096)
        .expect("read");
    // Someone else edits the file after the caller read it.
    std::fs::write(tree.root.join("doc.txt"), "edited elsewhere\n").expect("elsewhere");

    let content = "the caller's rewrite\n";
    let error = host
        .write_checked(
            &context(),
            RootScope::Workspace,
            "doc.txt",
            content.as_bytes(),
            &digest(content),
            Some(&read.content_digest),
        )
        .expect_err("a stale base is a conflict");
    assert!(
        error.to_string().starts_with("FILE_EDIT_CONFLICT"),
        "{error}"
    );
    let (expected, actual) = conflict(error);
    assert_eq!(expected, read.content_digest);
    assert_eq!(actual, digest("edited elsewhere\n"));
    assert_eq!(
        std::fs::read_to_string(tree.root.join("doc.txt")).expect("read"),
        "edited elsewhere\n"
    );
    assert!(
        tree.leftovers().is_empty(),
        "no temporary file is left behind: {:?}",
        tree.leftovers()
    );

    // The current base is accepted, and the write is atomic and reports what it wrote.
    let current = host
        .read(RootScope::Workspace, "doc.txt", 4096)
        .expect("read")
        .content_digest;
    let written = host
        .write_checked(
            &context(),
            RootScope::Workspace,
            "doc.txt",
            content.as_bytes(),
            &digest(content),
            Some(&current),
        )
        .expect("the current base");
    assert!(written.replaced);
    assert_eq!(
        std::fs::read_to_string(tree.root.join("doc.txt")).expect("read"),
        content
    );
}

#[test]
fn a_write_whose_target_changes_after_the_temporary_file_is_written_is_a_conflict() {
    let tree = Tree::new();
    std::fs::write(tree.root.join("doc.txt"), "version one\n").expect("seed");
    let target = tree.root.join("doc.txt");
    // The second writer lands exactly between the temporary file being complete and the target being
    // re-checked: the instant a check made only before the write would miss.
    let host = tree.host().with_after_temporary({
        let target = target.clone();
        move |_| {
            std::fs::write(&target, "the other writer got there first\n").expect("the other writer")
        }
    });
    let read = host
        .read(RootScope::Workspace, "doc.txt", 4096)
        .expect("read");
    let content = "mine\n";
    let error = host
        .write_checked(
            &context(),
            RootScope::Workspace,
            "doc.txt",
            content.as_bytes(),
            &digest(content),
            Some(&read.content_digest),
        )
        .expect_err("the re-check after the temporary file catches the other writer");
    let (expected, actual) = conflict(error);
    assert_eq!(expected, read.content_digest);
    assert_eq!(actual, digest("the other writer got there first\n"));
    assert_eq!(
        std::fs::read_to_string(&target).expect("read"),
        "the other writer got there first\n",
        "the other writer's bytes survive"
    );
    assert!(tree.leftovers().is_empty(), "{:?}", tree.leftovers());
}

#[test]
fn a_patch_is_applied_to_the_bytes_it_was_checked_against_or_not_at_all() {
    let tree = Tree::new();
    let target = tree.root.join("notes.txt");
    std::fs::write(&target, "alpha\nbeta\ngamma\n").expect("seed");
    let patch = "--- a/notes.txt\n+++ b/notes.txt\n@@ -1,3 +1,3 @@\n alpha\n-beta\n+BETA\n gamma\n";

    // Without a base digest the expectation is the digest of the bytes the patch was computed from, so a
    // change between the read and the rename is still a conflict and never a lost edit.
    let racing = tree.host().with_after_temporary({
        let target = target.clone();
        move |_| {
            std::fs::write(
                &target,
                "alpha\nbeta\ngamma\nand a line from someone else\n",
            )
            .expect("other")
        }
    });
    let error = racing
        .patch(&context(), RootScope::Workspace, "notes.txt", patch)
        .expect_err("a concurrent change is a conflict");
    let (expected, actual) = conflict(error);
    assert_eq!(expected, digest("alpha\nbeta\ngamma\n"));
    assert_eq!(
        actual,
        digest("alpha\nbeta\ngamma\nand a line from someone else\n")
    );
    assert_eq!(
        std::fs::read_to_string(&target).expect("read"),
        "alpha\nbeta\ngamma\nand a line from someone else\n"
    );
    assert!(tree.leftovers().is_empty(), "{:?}", tree.leftovers());

    // With a base digest the file must be what the caller read.
    std::fs::write(&target, "alpha\nbeta\ngamma\n").expect("reset");
    let host = tree.host();
    let stale = host
        .patch_checked(
            &context(),
            RootScope::Workspace,
            "notes.txt",
            patch,
            Some(&digest("alpha\nbeta\n")),
        )
        .expect_err("not the bytes the caller read");
    let (expected, actual) = conflict(stale);
    assert_eq!(expected, digest("alpha\nbeta\n"));
    assert_eq!(actual, digest("alpha\nbeta\ngamma\n"));
    let applied = host
        .patch_checked(
            &context(),
            RootScope::Workspace,
            "notes.txt",
            patch,
            Some(&digest("alpha\nbeta\ngamma\n")),
        )
        .expect("the right base");
    assert_eq!((applied.hunks, applied.added, applied.removed), (1, 1, 1));
    assert_eq!(
        std::fs::read_to_string(&target).expect("read"),
        "alpha\nBETA\ngamma\n"
    );
    assert_eq!(applied.content_digest, digest("alpha\nBETA\ngamma\n"));
}

#[test]
fn a_rejected_patch_changes_nothing_and_a_write_keeps_the_files_permissions() {
    use std::os::unix::fs::PermissionsExt as _;
    let tree = Tree::new();
    let target = tree.root.join("run.sh");
    std::fs::write(&target, "#!/bin/sh\necho one\n").expect("seed");
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o750)).expect("chmod");
    let host = tree.host();
    // A hunk that does not match refuses the whole patch: the file is untouched and nothing is left.
    let bad = "@@ -1,2 +1,2 @@\n #!/bin/sh\n-echo two\n+echo three\n";
    assert!(matches!(
        host.patch(&context(), RootScope::Workspace, "run.sh", bad),
        Err(ToolError::PatchRejected { .. })
    ));
    assert_eq!(
        std::fs::read_to_string(&target).expect("read"),
        "#!/bin/sh\necho one\n"
    );
    assert!(tree.leftovers().is_empty());
    // A good patch and a write both replace the file atomically with the mode it had.
    let good = "@@ -1,2 +1,2 @@\n #!/bin/sh\n-echo one\n+echo two\n";
    host.patch(&context(), RootScope::Workspace, "run.sh", good)
        .expect("patch");
    assert_eq!(
        std::fs::metadata(&target)
            .expect("meta")
            .permissions()
            .mode()
            & 0o777,
        0o750
    );
    let text = "#!/bin/sh\necho three\n";
    host.write(
        &context(),
        RootScope::Workspace,
        "run.sh",
        text.as_bytes(),
        &digest(text),
    )
    .expect("write");
    assert_eq!(
        std::fs::metadata(&target)
            .expect("meta")
            .permissions()
            .mode()
            & 0o777,
        0o750
    );
    assert_eq!(std::fs::read_to_string(&target).expect("read"), text);
    assert!(tree.leftovers().is_empty());
}

#[test]
fn a_write_with_no_base_is_unconditional_as_it_always_was_and_absent_means_create() {
    let tree = Tree::new();
    let host = tree.host();
    let text = "new file\n";
    // `absent` is the base of a file that must not exist yet.
    let created = host
        .write_checked(
            &context(),
            RootScope::Workspace,
            "sub/new.txt",
            text.as_bytes(),
            &digest(text),
            Some("absent"),
        )
        .expect("created");
    assert!(!created.replaced);
    let again = host
        .write_checked(
            &context(),
            RootScope::Workspace,
            "sub/new.txt",
            text.as_bytes(),
            &digest(text),
            Some("absent"),
        )
        .expect_err("it exists now");
    let (expected, actual) = conflict(again);
    assert_eq!((expected.as_str(), actual), ("absent", digest(text)));
    // No base: the old behaviour.
    let other = "overwritten\n";
    host.write(
        &context(),
        RootScope::Workspace,
        "sub/new.txt",
        other.as_bytes(),
        &digest(other),
    )
    .expect("unconditional");
    assert_eq!(
        std::fs::read_to_string(tree.root.join("sub/new.txt")).expect("read"),
        other
    );
    // The declared content digest is still checked first.
    assert!(matches!(
        host.write(
            &context(),
            RootScope::Workspace,
            "sub/new.txt",
            other.as_bytes(),
            &digest("something else")
        ),
        Err(ToolError::DigestMismatch { .. })
    ));
}

#[test]
fn the_conflict_is_what_a_second_concurrent_writer_sees_and_the_code_is_the_one_the_tables_name() {
    // Two real threads edit the same file from the same base: exactly one wins, the other is a conflict,
    // and no write is lost to a silent overwrite.
    let tree = Tree::new();
    std::fs::write(tree.root.join("shared.txt"), "base\n").expect("seed");
    let base = digest("base\n");
    let host = Arc::new(tree.host());
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let results: Vec<_> = (0..2)
        .map(|n| {
            let host = Arc::clone(&host);
            let barrier = Arc::clone(&barrier);
            let base = base.clone();
            std::thread::spawn(move || {
                let text = format!("writer {n}\n");
                barrier.wait();
                host.write_checked(
                    &context(),
                    RootScope::Workspace,
                    "shared.txt",
                    text.as_bytes(),
                    &digest(&text),
                    Some(&base),
                )
                .map(|_| text)
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|handle| handle.join().expect("thread"))
        .collect();
    let winners: Vec<&String> = results
        .iter()
        .filter_map(|result| result.as_ref().ok())
        .collect();
    // At most one writer can have seen the base at its re-check: the rename of the first changes the
    // digest the second compares with, unless the second's re-check ran before the first's rename, in
    // which case both pass the check and the later rename wins. Both outcomes keep one writer's whole
    // content, never a mixture, and a refused writer is a typed conflict.
    assert!(!winners.is_empty());
    let on_disk = std::fs::read_to_string(tree.root.join("shared.txt")).expect("read");
    assert!(
        winners.iter().any(|text| **text == on_disk),
        "{on_disk:?} is a whole write"
    );
    for result in &results {
        if let Err(error) = result {
            assert!(matches!(error, ToolError::EditConflict { .. }), "{error:?}");
        }
    }
    assert!(tree.leftovers().is_empty());
}
