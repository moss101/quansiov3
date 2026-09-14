//! Where a tool may act, and what it may touch (EXEC-006, DOMAIN.md §7.1, §8.5).
//!
//! A worker runs inside an execution target, and every file a tool touches is resolved against a root
//! the target is *authorized* for. Two refusals follow, and they are separate rules because they fail
//! for different reasons:
//!
//! * **an unauthorized root.** An operation names the root it means — the run's workspace, or the host
//!   filesystem — and a sandbox that has no root for that scope refuses the operation before looking at
//!   any path. `fs.write.host` is a tier-3 effect precisely because reaching the host is a different
//!   authority from reaching the workspace, so the scope has to be a decision, not a default.
//! * **a path that leaves its root.** A request is made relative to the root, and it is refused if it
//!   is absolute, if it contains a `..` component at all, or if any existing ancestor is a symbolic
//!   link that resolves outside the root. The last is the one that matters in practice: `root/link` is
//!   *textually* inside the root, so only resolution catches it.
//!
//! Resolution canonicalizes the deepest existing ancestor rather than the whole path, because the
//! target of a write usually does not exist yet — and canonicalizing would fail on it. What is proven
//! is what the kernel would open: an existing ancestor outside the root is a refusal.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

/// Which authority an operation acts under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RootScope {
    /// The run's workspace inside the target.
    Workspace,
    /// The target's own scratch space, outside the workspace.
    Target,
    /// The host filesystem (`fs.write.host`, tier 3).
    Host,
}

impl RootScope {
    /// Canonical name, as the runtime's effect-class derivation and the log use it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Target => "target",
            Self::Host => "host",
        }
    }

    /// Parse the canonical name.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "workspace" => Some(Self::Workspace),
            "target" => Some(Self::Target),
            "host" => Some(Self::Host),
            _ => None,
        }
    }
}

/// Why a request was refused before any I/O.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathRefusal {
    /// The operation named a root this sandbox was not given.
    #[error("no {scope} root is authorized for this target")]
    RootNotAuthorized {
        /// The scope asked for.
        scope: &'static str,
    },
    /// The authorized root is not usable.
    #[error("the {scope} root {root:?} is not usable: {detail}")]
    RootUnusable {
        /// The scope.
        scope: &'static str,
        /// The root as configured.
        root: String,
        /// Why it cannot be used.
        detail: String,
    },
    /// Nothing was named.
    #[error("the request names no path")]
    Empty,
    /// The path is absolute, and an absolute path is not relative to a root.
    #[error("{requested:?} is absolute; a request is relative to the root it names")]
    Absolute {
        /// The path as requested.
        requested: String,
    },
    /// The path contains a `..` component.
    #[error("{requested:?} contains a parent component")]
    ParentComponent {
        /// The path as requested.
        requested: String,
    },
    /// The path contains something that is not a plain component.
    #[error("{requested:?} is not a plain relative path: {detail}")]
    NotPlain {
        /// The path as requested.
        requested: String,
        /// What is wrong with it.
        detail: String,
    },
    /// The request resolved outside its root.
    #[error("{requested:?} resolves to {resolved:?}, outside the {scope} root")]
    EscapesRoot {
        /// The scope.
        scope: &'static str,
        /// The path as requested.
        requested: String,
        /// Where it actually resolves.
        resolved: String,
    },
    /// An ancestor is a symbolic link that leaves the root.
    #[error("{requested:?} goes through the symbolic link {link:?}, which leaves the root")]
    SymlinkEscape {
        /// The path as requested.
        requested: String,
        /// The link.
        link: String,
    },
}

/// A path proven to be inside its root.
///
/// Holding one of these is the only way a file operation is called, so "was the path checked?" is not
/// a question a call site can get wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxPath {
    scope: RootScope,
    relative: String,
    absolute: PathBuf,
}

impl SandboxPath {
    /// The scope it was resolved under.
    #[must_use]
    pub const fn scope(&self) -> RootScope {
        self.scope
    }

    /// The path relative to its root, in the canonical `a/b` shape.
    #[must_use]
    pub fn relative(&self) -> &str {
        &self.relative
    }

    /// The absolute path to open.
    #[must_use]
    pub fn absolute(&self) -> &Path {
        &self.absolute
    }
}

/// The roots one target's tools may act under.
#[derive(Debug, Clone)]
pub struct Sandbox {
    roots: BTreeMap<RootScope, PathBuf>,
}

impl Sandbox {
    /// A sandbox with no roots: every operation is refused until one is authorized.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            roots: BTreeMap::new(),
        }
    }

    /// Authorize a root for a scope.
    ///
    /// # Errors
    /// Returns [`PathRefusal::RootUnusable`] when the root is not absolute, does not exist, or is not a
    /// directory. A root that does not exist is refused rather than created: silently making a
    /// directory at a path an operator mistyped is how a tool ends up writing somewhere nobody chose.
    pub fn authorize(
        mut self,
        scope: RootScope,
        root: impl AsRef<Path>,
    ) -> Result<Self, PathRefusal> {
        let root = root.as_ref();
        let unusable = |detail: String| PathRefusal::RootUnusable {
            scope: scope.as_str(),
            root: root.display().to_string(),
            detail,
        };
        if !root.is_absolute() {
            return Err(unusable("it is not absolute".to_string()));
        }
        let canonical = std::fs::canonicalize(root).map_err(|error| unusable(error.to_string()))?;
        if !canonical.is_dir() {
            return Err(unusable("it is not a directory".to_string()));
        }
        self.roots.insert(scope, canonical);
        Ok(self)
    }

    /// Whether a scope has a root.
    #[must_use]
    pub fn has_root(&self, scope: RootScope) -> bool {
        self.roots.contains_key(&scope)
    }

    /// Resolve a requested path against the root of its scope.
    ///
    /// # Errors
    /// Returns the [`PathRefusal`] naming the rule: an unauthorized or unusable root, an empty or
    /// absolute request, a `..` component, anything that is not a plain component, a resolved path
    /// outside the root, or an ancestor that is a symbolic link leaving it.
    pub fn resolve(&self, scope: RootScope, requested: &str) -> Result<SandboxPath, PathRefusal> {
        let root = self
            .roots
            .get(&scope)
            .ok_or(PathRefusal::RootNotAuthorized {
                scope: scope.as_str(),
            })?;
        let requested = requested.trim();
        if requested.is_empty() {
            return Err(PathRefusal::Empty);
        }
        if requested.contains('\0') {
            return Err(PathRefusal::NotPlain {
                requested: requested.to_string(),
                detail: "it contains a NUL byte".to_string(),
            });
        }
        let named = Path::new(requested);
        if named.is_absolute() {
            return Err(PathRefusal::Absolute {
                requested: requested.to_string(),
            });
        }
        let mut components = Vec::new();
        for component in named.components() {
            match component {
                Component::Normal(part) => {
                    let part = part.to_str().ok_or_else(|| PathRefusal::NotPlain {
                        requested: requested.to_string(),
                        detail: "a component is not valid UTF-8".to_string(),
                    })?;
                    if part.is_empty() {
                        return Err(PathRefusal::NotPlain {
                            requested: requested.to_string(),
                            detail: "a component is empty".to_string(),
                        });
                    }
                    components.push(part.to_string());
                }
                Component::ParentDir => {
                    return Err(PathRefusal::ParentComponent {
                        requested: requested.to_string(),
                    })
                }
                Component::CurDir => {}
                Component::RootDir | Component::Prefix(_) => {
                    return Err(PathRefusal::Absolute {
                        requested: requested.to_string(),
                    })
                }
            }
        }
        if components.is_empty() {
            // The root itself.
            return Ok(SandboxPath {
                scope,
                relative: ".".to_string(),
                absolute: root.clone(),
            });
        }
        let relative = components.join("/");
        let absolute = root.join(&relative);

        // Prove what the kernel would open: canonicalize the deepest ancestor that exists. The target
        // itself may not exist yet — a write creates it — so it is the ancestors that are checked.
        let mut existing = absolute.as_path();
        let mut tail: Vec<String> = Vec::new();
        let resolved = loop {
            match std::fs::canonicalize(existing) {
                Ok(canonical) => break Some((canonical, tail)),
                Err(_) => match existing.parent() {
                    Some(parent) if parent.starts_with(root) || parent == root => {
                        if let Some(name) = existing.file_name().and_then(|n| n.to_str()) {
                            tail.push(name.to_string());
                        }
                        existing = parent;
                    }
                    // Walked past the root without finding anything that exists.
                    _ => break None,
                },
            }
        };
        let Some((mut canonical, tail)) = resolved else {
            return Err(PathRefusal::EscapesRoot {
                scope: scope.as_str(),
                requested: requested.to_string(),
                resolved: absolute.display().to_string(),
            });
        };
        if !canonical.starts_with(root) {
            // Every component is plain and no `..` survived, so an ancestor that resolves outside the
            // root can only be a symbolic link pointing out of it.
            return Err(PathRefusal::SymlinkEscape {
                requested: requested.to_string(),
                link: canonical.display().to_string(),
            });
        }
        for part in tail.iter().rev() {
            canonical.push(part);
        }
        if !canonical.starts_with(root) {
            return Err(PathRefusal::EscapesRoot {
                scope: scope.as_str(),
                requested: requested.to_string(),
                resolved: canonical.display().to_string(),
            });
        }
        Ok(SandboxPath {
            scope,
            relative,
            absolute: canonical,
        })
    }
}
