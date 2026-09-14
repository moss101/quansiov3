//! File, directory and patch tools (EXEC-006, `fs.read` / `fs.write` / `fs.patch`).
//!
//! Every operation takes a [`SandboxPath`], so the path policy is not something a call site can skip,
//! and every mutating operation takes a [`ToolContext`], so the effect it settles and the capability it
//! runs under are carried with it rather than reconstructed later. Two further rules come from the tool
//! declarations in `config/tools.yaml` and are enforced here rather than trusted:
//!
//! * **a declared digest is checked.** `fs.write` requires `content_digest`, and writing content that
//!   does not hash to it is refused. That is what makes the call idempotent in the Effect Ledger's
//!   sense: the record says which bytes were written, and a retry that claims different bytes is a
//!   different call, not a replay.
//! * **a patch is all-or-nothing.** `fs.patch` applies a unified diff, and a hunk whose context does not
//!   match the file is refused with the line named, leaving the file untouched. A partially applied
//!   patch is worse than a refused one: the caller cannot tell what the file now is.

use std::path::Path;

use quansio_core::Digest;

use super::sandbox::{PathRefusal, SandboxPath};
use super::{ToolContext, ToolError};

/// A file's contents, bounded by the tool declaration's `max_output_bytes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRead {
    /// The scope it was read under.
    pub scope: &'static str,
    /// The path relative to its root.
    pub path: String,
    /// The bytes, truncated to the bound if the file is larger.
    pub content: Vec<u8>,
    /// Whether the content was truncated.
    pub truncated: bool,
    /// The file's size on disk, whether or not it was truncated.
    pub size_bytes: u64,
    /// SHA-256 of the whole file, so a caller can tell whether it is looking at all of it.
    pub content_digest: String,
}

/// One directory entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    /// The entry's name.
    pub name: String,
    /// Whether it is a directory.
    pub is_dir: bool,
    /// Whether it is a symbolic link.
    pub is_symlink: bool,
    /// Its size, for a regular file.
    pub size_bytes: u64,
}

/// A directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirListing {
    /// The scope.
    pub scope: &'static str,
    /// The directory, relative to its root.
    pub path: String,
    /// The entries, sorted by name so a listing is deterministic.
    pub entries: Vec<DirEntry>,
    /// Whether the listing was truncated by the bound.
    pub truncated: bool,
}

/// What a write did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileWrite {
    /// The scope.
    pub scope: &'static str,
    /// The path relative to its root.
    pub path: String,
    /// Bytes written.
    pub bytes_written: u64,
    /// SHA-256 of what was written.
    pub content_digest: String,
    /// Whether the file existed before.
    pub replaced: bool,
    /// The effect this write settles.
    pub effect_id: String,
    /// The idempotency key the runtime derived for the call.
    pub idempotency_key: String,
}

/// What a patch did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePatch {
    /// The scope.
    pub scope: &'static str,
    /// The path relative to its root.
    pub path: String,
    /// Hunks applied.
    pub hunks: usize,
    /// Lines added.
    pub added: usize,
    /// Lines removed.
    pub removed: usize,
    /// SHA-256 of the file after the patch.
    pub content_digest: String,
    /// The effect this patch settles.
    pub effect_id: String,
}

/// File, directory and patch operations over a sandbox.
#[derive(Debug, Clone)]
pub struct FileHost {
    sandbox: super::sandbox::Sandbox,
}

impl FileHost {
    /// Build a host over a sandbox.
    #[must_use]
    pub const fn new(sandbox: super::sandbox::Sandbox) -> Self {
        Self { sandbox }
    }

    /// The sandbox in force.
    #[must_use]
    pub const fn sandbox(&self) -> &super::sandbox::Sandbox {
        &self.sandbox
    }

    /// Read a file, bounded.
    ///
    /// # Errors
    /// Returns [`ToolError::Path`] for a refused path, [`ToolError::NotFound`] when there is no such
    /// file, [`ToolError::NotAFile`] when it is not a regular file, and [`ToolError::Io`] when the read
    /// fails.
    pub fn read(
        &self,
        scope: super::sandbox::RootScope,
        requested: &str,
        max_bytes: usize,
    ) -> Result<FileRead, ToolError> {
        let path = self.sandbox.resolve(scope, requested)?;
        let metadata = std::fs::metadata(path.absolute()).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => ToolError::NotFound {
                path: path.relative().to_string(),
            },
            _ => ToolError::Io {
                path: path.relative().to_string(),
                detail: error.to_string(),
            },
        })?;
        if !metadata.is_file() {
            return Err(ToolError::NotAFile {
                path: path.relative().to_string(),
            });
        }
        let bytes = std::fs::read(path.absolute()).map_err(|error| ToolError::Io {
            path: path.relative().to_string(),
            detail: error.to_string(),
        })?;
        let content_digest = Digest::of(&bytes).as_str().to_string();
        let truncated = bytes.len() > max_bytes;
        let content = if truncated {
            bytes[..max_bytes].to_vec()
        } else {
            bytes
        };
        Ok(FileRead {
            scope: scope.as_str(),
            path: path.relative().to_string(),
            content,
            truncated,
            size_bytes: metadata.len(),
            content_digest,
        })
    }

    /// Write a file, checking the declared digest first.
    ///
    /// The write is atomic — a temporary file in the same directory is renamed over the target — so a
    /// reader never sees a half-written file and an interrupted write leaves the old contents.
    ///
    /// # Errors
    /// Returns [`ToolError::DigestMismatch`] when the content does not hash to `content_digest`, and the
    /// read/write errors above.
    pub fn write(
        &self,
        context: &ToolContext<'_>,
        scope: super::sandbox::RootScope,
        requested: &str,
        content: &[u8],
        content_digest: &str,
    ) -> Result<FileWrite, ToolError> {
        let path = self.sandbox.resolve(scope, requested)?;
        let actual = Digest::of(content).as_str().to_string();
        if actual != content_digest {
            return Err(ToolError::DigestMismatch {
                declared: content_digest.to_string(),
                actual,
            });
        }
        let existed = path.absolute().exists();
        if let Some(parent) = path.absolute().parent() {
            std::fs::create_dir_all(parent).map_err(|error| ToolError::Io {
                path: path.relative().to_string(),
                detail: error.to_string(),
            })?;
            // Creating the parents cannot take the write outside the root, but a parent that a
            // concurrent actor replaced with a link could, so the resolved path is re-checked.
            self.recheck(&path)?;
        }
        let temporary = path.absolute().with_extension(format!(
            "{}.qworkerd-partial",
            path.absolute()
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or("file")
        ));
        std::fs::write(&temporary, content).map_err(|error| ToolError::Io {
            path: path.relative().to_string(),
            detail: error.to_string(),
        })?;
        std::fs::rename(&temporary, path.absolute()).map_err(|error| ToolError::Io {
            path: path.relative().to_string(),
            detail: error.to_string(),
        })?;
        Ok(FileWrite {
            scope: scope.as_str(),
            path: path.relative().to_string(),
            bytes_written: content.len() as u64,
            content_digest: actual,
            replaced: existed,
            effect_id: context.effect_id.to_string(),
            idempotency_key: context.idempotency_key.to_string(),
        })
    }

    /// Apply a unified diff, all-or-nothing.
    ///
    /// # Errors
    /// Returns [`ToolError::PatchRejected`] naming the hunk and line that did not match, and the
    /// read/write errors above. Nothing is written when any hunk is rejected.
    pub fn patch(
        &self,
        context: &ToolContext<'_>,
        scope: super::sandbox::RootScope,
        requested: &str,
        patch: &str,
    ) -> Result<FilePatch, ToolError> {
        let path = self.sandbox.resolve(scope, requested)?;
        let original =
            std::fs::read_to_string(path.absolute()).map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => ToolError::NotFound {
                    path: path.relative().to_string(),
                },
                _ => ToolError::Io {
                    path: path.relative().to_string(),
                    detail: error.to_string(),
                },
            })?;
        let applied =
            apply_unified_diff(&original, patch).map_err(|detail| ToolError::PatchRejected {
                path: path.relative().to_string(),
                detail,
            })?;
        std::fs::write(path.absolute(), &applied.content).map_err(|error| ToolError::Io {
            path: path.relative().to_string(),
            detail: error.to_string(),
        })?;
        Ok(FilePatch {
            scope: scope.as_str(),
            path: path.relative().to_string(),
            hunks: applied.hunks,
            added: applied.added,
            removed: applied.removed,
            content_digest: Digest::of(applied.content.as_bytes()).as_str().to_string(),
            effect_id: context.effect_id.to_string(),
        })
    }

    /// List a directory, bounded and sorted.
    ///
    /// # Errors
    /// Returns [`ToolError::NotFound`] or [`ToolError::NotADirectory`], and the path refusals above.
    pub fn list(
        &self,
        scope: super::sandbox::RootScope,
        requested: &str,
        max_entries: usize,
    ) -> Result<DirListing, ToolError> {
        let path = self.sandbox.resolve(scope, requested)?;
        let reader = std::fs::read_dir(path.absolute()).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => ToolError::NotFound {
                path: path.relative().to_string(),
            },
            _ => ToolError::NotADirectory {
                path: path.relative().to_string(),
            },
        })?;
        let mut entries = Vec::new();
        for entry in reader {
            let entry = entry.map_err(|error| ToolError::Io {
                path: path.relative().to_string(),
                detail: error.to_string(),
            })?;
            let metadata = entry.metadata().ok();
            entries.push(DirEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                is_dir: metadata.as_ref().is_some_and(std::fs::Metadata::is_dir),
                is_symlink: entry
                    .file_type()
                    .map(|kind| kind.is_symlink())
                    .unwrap_or(false),
                size_bytes: metadata
                    .as_ref()
                    .filter(|metadata| metadata.is_file())
                    .map_or(0, std::fs::Metadata::len),
            });
        }
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        let truncated = entries.len() > max_entries;
        entries.truncate(max_entries);
        Ok(DirListing {
            scope: scope.as_str(),
            path: path.relative().to_string(),
            entries,
            truncated,
        })
    }

    /// Create a directory and any missing parents, inside the root.
    ///
    /// # Errors
    /// Returns the path refusals above, and [`ToolError::Io`] when the directory cannot be created.
    pub fn make_dir(
        &self,
        _context: &ToolContext<'_>,
        scope: super::sandbox::RootScope,
        requested: &str,
    ) -> Result<String, ToolError> {
        let path = self.sandbox.resolve(scope, requested)?;
        std::fs::create_dir_all(path.absolute()).map_err(|error| ToolError::Io {
            path: path.relative().to_string(),
            detail: error.to_string(),
        })?;
        Ok(path.relative().to_string())
    }

    /// Re-resolve a path that a moment ago did not exist, so a parent created between resolution and
    /// the write cannot have changed where the write lands.
    fn recheck(&self, path: &SandboxPath) -> Result<(), ToolError> {
        let again = self.sandbox.resolve(path.scope(), path.relative())?;
        if again.absolute() != path.absolute() {
            return Err(ToolError::Path(PathRefusal::EscapesRoot {
                scope: path.scope().as_str(),
                requested: path.relative().to_string(),
                resolved: again.absolute().display().to_string(),
            }));
        }
        Ok(())
    }
}

struct AppliedPatch {
    content: String,
    hunks: usize,
    added: usize,
    removed: usize,
}

/// Apply a unified diff to `original`, or refuse without changing anything.
///
/// This is deliberately strict: hunks must appear in order, the context must match exactly, and a hunk
/// that does not match refuses the whole patch. A lenient applier that guesses where a hunk goes is how
/// a patch lands on the wrong line.
fn apply_unified_diff(original: &str, patch: &str) -> Result<AppliedPatch, String> {
    // Whether the original ended with a newline decides how the last line is compared, so it is
    // remembered rather than assumed.
    let trailing_newline = original.ends_with('\n') || original.is_empty();
    let mut lines: Vec<String> = original.lines().map(str::to_string).collect();
    if lines.is_empty() && !original.is_empty() {
        lines.push(original.to_string());
    }

    let mut hunks: usize = 0;
    let mut added: usize = 0;
    let mut removed: usize = 0;
    let mut patched: Vec<String> = Vec::new();
    let mut consumed: usize = 0;
    let mut in_hunk = false;

    for raw in patch.lines() {
        if raw.starts_with("--- ") || raw.starts_with("+++ ") {
            if in_hunk {
                return Err("a file header appears inside a hunk".to_string());
            }
            continue;
        }
        if raw.starts_with("@@") {
            // Close the previous hunk: its lines are already in `patched`.
            in_hunk = true;
            hunks += 1;
            let header = raw
                .split("@@")
                .nth(1)
                .ok_or_else(|| format!("{raw:?} is not a hunk header"))?
                .trim();
            let old = header
                .split_whitespace()
                .next()
                .ok_or_else(|| format!("{raw:?} names no range"))?;
            let start = old
                .trim_start_matches('-')
                .split(',')
                .next()
                .ok_or_else(|| format!("{raw:?} names no start line"))?;
            let hunk_old_line = start
                .parse::<usize>()
                .map_err(|_| format!("{raw:?} does not name a line number"))?;
            // Position: copy the untouched lines before the hunk, then check the hunk's own lines.
            let target = hunk_old_line.saturating_sub(1);
            if target < consumed {
                return Err(format!(
                    "hunk at line {hunk_old_line} overlaps the previous one"
                ));
            }
            while consumed < target && consumed < lines.len() {
                patched.push(lines[consumed].clone());
                consumed += 1;
            }
            if consumed != target {
                return Err(format!(
                    "hunk at line {hunk_old_line} is past the end of the file"
                ));
            }
            continue;
        }
        if !in_hunk {
            if raw.trim().is_empty() {
                continue;
            }
            return Err(format!("{raw:?} appears before the first hunk"));
        }
        let (marker, body) = if raw.is_empty() {
            // An empty line is a context line whose single leading space was stripped by the writer.
            (" ", "")
        } else {
            let (marker, body) = raw.split_at(1);
            (marker, body)
        };
        let body = body.to_string();
        match marker {
            " " => {
                let actual = lines
                    .get(consumed)
                    .ok_or_else(|| format!("line {} is past the end of the file", consumed + 1))?;
                if actual != &body {
                    return Err(format!(
                        "line {} does not match the patch context: expected {body:?}, found {actual:?}",
                        consumed + 1
                    ));
                }
                patched.push(body);
                consumed += 1;
            }
            "-" => {
                let actual = lines
                    .get(consumed)
                    .ok_or_else(|| format!("line {} is past the end of the file", consumed + 1))?;
                if actual != &body {
                    return Err(format!(
                        "line {} does not match the line to remove: expected {body:?}, found {actual:?}",
                        consumed + 1
                    ));
                }
                consumed += 1;
                removed += 1;
            }
            "+" => {
                patched.push(body);
                added += 1;
            }
            "\\" => {}
            other => {
                return Err(format!("{other:?} is not a patch line marker"));
            }
        }
    }
    if hunks == 0 {
        return Err("the patch contains no hunks".to_string());
    }
    while consumed < lines.len() {
        patched.push(lines[consumed].clone());
        consumed += 1;
    }
    let mut content = patched.join("\n");
    if trailing_newline {
        content.push('\n');
    }
    Ok(AppliedPatch {
        content,
        hunks,
        added,
        removed,
    })
}

/// Whether a patch is even shaped like a diff, before any file is read. Used by the host to refuse a
/// malformed patch without touching the filesystem.
#[must_use]
pub fn patch_is_well_formed(patch: &str) -> bool {
    patch.lines().any(|line| line.starts_with("@@"))
}

/// A no-op reference to `Path`, so the import is used even when the module grows conditional code.
#[allow(dead_code)]
fn _path_marker(path: &Path) -> bool {
    path.is_absolute()
}
