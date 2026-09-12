//! Resource selectors (DOMAIN.md §6.1).
//!
//! A selector names the resource region a [`crate::Grant`] covers. This module owns the
//! nine canonical selector kinds and the *containment* relation used by the narrowing
//! algebra: `a.covers(b)` means every resource matched by `b` is also matched by `a`, so
//! `b` is a narrowing of `a`.
//!
//! Containment is deliberately **conservative**: when the implementation cannot prove
//! that `a` contains `b`, it answers `false`. A layer that offers a selector the algebra
//! cannot prove narrower is treated as a widening attempt and rejected, so an
//! unprovable glob can only remove authority — never add it.

use core::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::CapabilityError;

/// The canonical selector kinds (DOMAIN.md §6.1).
pub const SELECTOR_KINDS: [&str; 9] = [
    "fs",
    "domain",
    "connector",
    "app",
    "artifact",
    "model",
    "work",
    "secret",
    "target",
];

/// A resource selector: a canonical kind plus the region it covers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ResourceSelector {
    /// A path glob under a filesystem root.
    Fs {
        /// The path glob.
        glob: String,
    },
    /// A host glob with an optional port set (`ports` empty means every port).
    Domain {
        /// The host glob.
        host_glob: String,
        /// The allowed ports; empty means every port.
        ports: Vec<u16>,
    },
    /// A connector identity plus a resource glob inside it.
    Connector {
        /// The connector id (`cnx_…`) or `*`.
        connector_id: String,
        /// The resource glob inside the connector.
        resource_glob: String,
    },
    /// A native application identity.
    App {
        /// The app identity, or `*`.
        identity: String,
    },
    /// An artifact/role glob.
    Artifact {
        /// The artifact role glob.
        role_glob: String,
    },
    /// A model route or provider glob.
    Model {
        /// The route or provider selector.
        route_glob: String,
    },
    /// A work node subtree.
    Work {
        /// The node id/path, a `…/**` subtree, or `*`.
        node_selector: String,
    },
    /// A secret handle id.
    Secret {
        /// The handle id (`sec_…`) or `*`.
        handle_id: String,
    },
    /// An execution target class or id.
    Target {
        /// The target class/id, or `*`.
        target_selector: String,
    },
}

impl ResourceSelector {
    /// The canonical kind string.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Fs { .. } => "fs",
            Self::Domain { .. } => "domain",
            Self::Connector { .. } => "connector",
            Self::App { .. } => "app",
            Self::Artifact { .. } => "artifact",
            Self::Model { .. } => "model",
            Self::Work { .. } => "work",
            Self::Secret { .. } => "secret",
            Self::Target { .. } => "target",
        }
    }

    /// The wire `selector` string from DOMAIN.md §6.1.
    #[must_use]
    pub fn selector(&self) -> String {
        match self {
            Self::Fs { glob } => glob.clone(),
            Self::Domain { host_glob, .. } => host_glob.clone(),
            Self::Connector {
                connector_id,
                resource_glob,
            } => format!("{connector_id}/{resource_glob}"),
            Self::App { identity } => identity.clone(),
            Self::Artifact { role_glob } => role_glob.clone(),
            Self::Model { route_glob } => route_glob.clone(),
            Self::Work { node_selector } => node_selector.clone(),
            Self::Secret { handle_id } => handle_id.clone(),
            Self::Target { target_selector } => target_selector.clone(),
        }
    }

    /// Build a selector from its canonical parts.
    ///
    /// # Errors
    /// Returns [`CapabilityError::MalformedSelector`] when the kind is not canonical or
    /// the selector is empty.
    pub fn from_parts(kind: &str, selector: &str, ports: &[u16]) -> Result<Self, CapabilityError> {
        let malformed = |reason: &str| CapabilityError::MalformedSelector {
            kind: kind.to_string(),
            reason: reason.to_string(),
        };
        if selector.is_empty() {
            return Err(malformed("selector must not be empty"));
        }
        match kind {
            "fs" => Ok(Self::Fs {
                glob: selector.to_string(),
            }),
            "domain" => {
                if selector.contains("://") || selector.contains('/') {
                    return Err(malformed("domain selector is a host glob, not a URL"));
                }
                Ok(Self::Domain {
                    host_glob: selector.to_string(),
                    ports: ports.to_vec(),
                })
            }
            "connector" => match selector.split_once('/') {
                Some((connector_id, resource_glob)) => Ok(Self::Connector {
                    connector_id: connector_id.to_string(),
                    resource_glob: resource_glob.to_string(),
                }),
                None => Ok(Self::Connector {
                    connector_id: selector.to_string(),
                    resource_glob: "*".to_string(),
                }),
            },
            "app" => Ok(Self::App {
                identity: selector.to_string(),
            }),
            "artifact" => Ok(Self::Artifact {
                role_glob: selector.to_string(),
            }),
            "model" => Ok(Self::Model {
                route_glob: selector.to_string(),
            }),
            "work" => Ok(Self::Work {
                node_selector: selector.to_string(),
            }),
            "secret" => Ok(Self::Secret {
                handle_id: selector.to_string(),
            }),
            "target" => Ok(Self::Target {
                target_selector: selector.to_string(),
            }),
            other => Err(malformed(&format!(
                "kind '{other}' is not one of {}",
                SELECTOR_KINDS.join(", ")
            ))),
        }
    }

    /// Whether every resource matched by `other` is also matched by `self`.
    ///
    /// Conservative: `false` means "not provably contained", which the algebra treats as
    /// a widening attempt.
    #[must_use]
    pub fn covers(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Fs { glob: wider }, Self::Fs { glob: narrower }) => glob_covers(wider, narrower),
            (
                Self::Domain {
                    host_glob: wider,
                    ports: wider_ports,
                },
                Self::Domain {
                    host_glob: narrower,
                    ports: narrower_ports,
                },
            ) => glob_covers(wider, narrower) && ports_cover(wider_ports, narrower_ports),
            (
                Self::Connector {
                    connector_id: wider_id,
                    resource_glob: wider_glob,
                },
                Self::Connector {
                    connector_id: narrower_id,
                    resource_glob: narrower_glob,
                },
            ) => {
                (wider_id == narrower_id || wider_id == "*")
                    && glob_covers(wider_glob, narrower_glob)
            }
            (Self::App { identity: wider }, Self::App { identity: narrower }) => {
                wider == narrower || wider == "*"
            }
            (
                Self::Artifact { role_glob: wider },
                Self::Artifact {
                    role_glob: narrower,
                },
            ) => glob_covers(wider, narrower),
            (
                Self::Model { route_glob: wider },
                Self::Model {
                    route_glob: narrower,
                },
            ) => glob_covers(wider, narrower),
            (
                Self::Work {
                    node_selector: wider,
                },
                Self::Work {
                    node_selector: narrower,
                },
            ) => path_covers(wider, narrower),
            (
                Self::Secret { handle_id: wider },
                Self::Secret {
                    handle_id: narrower,
                },
            ) => wider == narrower || wider == "*",
            (
                Self::Target {
                    target_selector: wider,
                },
                Self::Target {
                    target_selector: narrower,
                },
            ) => wider == narrower || wider == "*" || path_covers(wider, narrower),
            _ => false,
        }
    }

    /// Whether the two selectors can match at least one common resource.
    ///
    /// Used by policy and user-rule matching, where a rule applies to a grant when
    /// either region contains the other.
    #[must_use]
    pub fn overlaps(&self, other: &Self) -> bool {
        self.covers(other) || other.covers(self)
    }

    /// The narrower of two overlapping selectors, or `None` when neither contains the
    /// other (the intersection is not representable, so the algebra drops it).
    #[must_use]
    pub fn intersect(&self, other: &Self) -> Option<Self> {
        if self.covers(other) {
            Some(other.clone())
        } else if other.covers(self) {
            Some(self.clone())
        } else {
            None
        }
    }

    fn to_wire(&self) -> SelectorWire {
        let ports = match self {
            Self::Domain { ports, .. } => ports.clone(),
            _ => Vec::new(),
        };
        SelectorWire {
            kind: self.kind().to_string(),
            selector: self.selector(),
            ports,
        }
    }
}

impl fmt::Display for ResourceSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.kind(), self.selector())
    }
}

impl Serialize for ResourceSelector {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_wire().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ResourceSelector {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = SelectorWire::deserialize(deserializer)?;
        Self::from_parts(&wire.kind, &wire.selector, &wire.ports).map_err(D::Error::custom)
    }
}

#[derive(Serialize, Deserialize)]
struct SelectorWire {
    kind: String,
    selector: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    ports: Vec<u16>,
}

/// Whether the wider port set allows every port the narrower set allows.
///
/// An empty set means "every port", so only an empty wider set contains an empty
/// narrower set; a non-empty wider set never contains "every port".
fn ports_cover(wider: &[u16], narrower: &[u16]) -> bool {
    if wider.is_empty() {
        return true;
    }
    if narrower.is_empty() {
        return false;
    }
    narrower.iter().all(|port| wider.contains(port))
}

/// Whether every path matched by `narrower` is matched by `wider`.
fn glob_covers(wider: &str, narrower: &str) -> bool {
    let wider_segments: Vec<&str> = wider.split('/').collect();
    let narrower_segments: Vec<&str> = narrower.split('/').collect();
    segments_cover(&wider_segments, &narrower_segments)
}

fn segments_cover(wider: &[&str], narrower: &[&str]) -> bool {
    match wider.split_first() {
        None => narrower.is_empty(),
        Some((wider_segment, rest)) => {
            if *wider_segment == "**" {
                return segments_cover(rest, narrower)
                    || (!narrower.is_empty() && segments_cover(wider, &narrower[1..]));
            }
            match narrower.split_first() {
                None => false,
                Some((narrower_segment, narrower_rest)) => {
                    segment_cover(wider_segment, narrower_segment)
                        && segments_cover(rest, narrower_rest)
                }
            }
        }
    }
}

fn segment_cover(wider: &str, narrower: &str) -> bool {
    if wider == narrower {
        return true;
    }
    if wider == "*" {
        return true;
    }
    // A literal-language narrower segment is contained only when it satisfies the wider
    // pattern; two different wildcard patterns are never proven to nest.
    if !has_wildcard(narrower) && has_wildcard(wider) {
        return glob_segment_match(wider, narrower);
    }
    false
}

fn has_wildcard(segment: &str) -> bool {
    segment.contains('*') || segment.contains('?')
}

/// Standard `*`/`?` match of one path segment.
fn glob_segment_match(pattern: &str, value: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let value: Vec<char> = value.chars().collect();
    let (mut p, mut v) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut resume = 0usize;
    while v < value.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == value[v]) {
            p += 1;
            v += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            resume = v;
            p += 1;
        } else if let Some(star_index) = star {
            p = star_index + 1;
            resume += 1;
            v = resume;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

/// Whether the `wider` node selector covers the `narrower` one (subtree containment).
fn path_covers(wider: &str, narrower: &str) -> bool {
    if wider == "*" || wider == narrower {
        return true;
    }
    if let Some(root) = wider.strip_suffix("/**") {
        return narrower == root || narrower.starts_with(&format!("{root}/"));
    }
    narrower.starts_with(&format!("{wider}/"))
}
