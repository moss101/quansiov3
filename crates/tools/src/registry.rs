//! The control-plane Tool Registry (DOMAIN.md §7.5).
//!
//! Declarations are loaded from `config/tools.yaml` and checked against the generated
//! tool catalog (`schemas/catalog/tools.yaml`, derived from DOMAIN.md). The registry
//! exposes tools to a model **only** through capability filtering: a run never sees a tool
//! its projection cannot cover.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, PoisonError, RwLock};

use chrono::{DateTime, Utc};
use quansio_capability::CapabilityProjection;
use serde::{Deserialize, Serialize};

use crate::declaration::{RawToolDeclaration, ToolDeclaration, ToolOrigin};
use crate::error::ToolError;
use crate::mcp::{self, ToolListChange};

/// Where `config/tools.yaml` lives relative to this source file.
const TOOLS_CONFIG: &str = include_str!("../../../config/tools.yaml");
/// The generated tool catalog this registry must agree with (GOV-004).
const TOOL_CATALOG: &str = include_str!("../../../schemas/catalog/tools.yaml");

/// The registry YAML shape.
#[derive(Debug, Deserialize)]
struct RegistryDocument {
    #[allow(dead_code)]
    #[serde(default)]
    version: u32,
    tools: Vec<RawToolDeclaration>,
    /// Catalog families (`connector.<id>.<op>`) reserved for later materialization.
    #[serde(default)]
    families: Vec<RawFamilyReservation>,
}

/// A catalog family this registry reserves but does not make callable.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RawFamilyReservation {
    /// The family name, with `<…>` segments.
    pub name: String,
    /// Which task materializes concrete operations.
    pub materialized_by: String,
}

/// Who may propose one tool (ADP-008).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolVisibility {
    /// The tool's name.
    pub name: String,
    /// A model may be offered it and may propose it.
    pub model_visible: bool,
    /// The host may call it with no model proposing it.
    pub host_callable: bool,
}

/// The generated catalog YAML shape.
#[derive(Debug, Deserialize)]
struct CatalogDocument {
    tools: Vec<String>,
}

/// The versioned Tool Registry.
#[derive(Debug, Clone)]
pub struct ToolRegistry {
    declarations: Vec<ToolDeclaration>,
    families: Vec<RawFamilyReservation>,
    /// External tool names whose server stopped offering them (ADP-009). No call resolves to one
    /// and no projection exposes it; an older version is still found by `get_version`, so a call that
    /// parked for approval before the tool went away is answered rather than lost.
    retired: BTreeSet<String>,
}

impl ToolRegistry {
    /// The shipped registry from `config/tools.yaml`, checked against the tool catalog.
    ///
    /// # Panics
    /// Panics only if the repository's own configuration is invalid, which the
    /// `config/tools.yaml` contract tests catch first.
    #[must_use]
    pub fn builtin() -> &'static Self {
        static REGISTRY: std::sync::OnceLock<ToolRegistry> = std::sync::OnceLock::new();
        REGISTRY.get_or_init(|| {
            let registry = Self::from_yaml(TOOLS_CONFIG)
                .expect("config/tools.yaml is a valid DOMAIN §7.5 Tool registry");
            let catalog: CatalogDocument = serde_yaml::from_str(TOOL_CATALOG)
                .expect("schemas/catalog/tools.yaml is a generated catalog");
            registry
                .check_catalog(&catalog.tools)
                .expect("config/tools.yaml covers the generated tool catalog");
            registry
                .check_side_effects(crate::side_effect::SideEffectMapping::builtin())
                .expect("config/tools.yaml agrees with the browser_side_effects mapping");
            registry
                .check_receipts()
                .expect("config/tools.yaml gives every computer Tool the canonical receipt");
            registry
        })
    }

    /// Build a registry from YAML source.
    ///
    /// # Errors
    /// Returns [`ToolError::Config`] for unparseable YAML and
    /// [`ToolError::MalformedDeclaration`] for a declaration that breaks the contract.
    pub fn from_yaml(source: &str) -> Result<Self, ToolError> {
        let document: RegistryDocument =
            serde_yaml::from_str(source).map_err(|error| ToolError::Config(error.to_string()))?;
        let mut declarations = Vec::with_capacity(document.tools.len());
        let mut seen: BTreeMap<(String, u32), ()> = BTreeMap::new();
        let mut declared_names: Vec<String> = Vec::with_capacity(document.tools.len());
        for raw in document.tools {
            let declaration = ToolDeclaration::from_raw(raw)?;
            let key = (declaration.name.clone(), declaration.version);
            if seen.insert(key, ()).is_some() {
                return Err(ToolError::Config(format!(
                    "tool {} version {} is declared twice",
                    declaration.name, declaration.version
                )));
            }
            declared_names.push(declaration.name.clone());
            declarations.push(declaration);
        }
        for family in &document.families {
            if !family.name.contains('<') || !family.name.contains('>') {
                return Err(ToolError::Config(format!(
                    "family reservation {} must contain a <…> segment",
                    family.name
                )));
            }
            if declared_names.iter().any(|name| name == &family.name) {
                return Err(ToolError::Config(format!(
                    "family reservation {} collides with a tool declaration",
                    family.name
                )));
            }
        }
        Ok(Self {
            declarations,
            families: document.families,
            retired: BTreeSet::new(),
        })
    }

    /// The registry with the external tools `external` declares (ADP-009).
    ///
    /// `external` is the **complete** set the MCP client currently offers, so the result is this registry's
    /// built-in tools plus exactly those: an external tool that is not in it is retired (no call resolves to
    /// it and no projection exposes it), one that is is registered or kept, and a name that was retired and is
    /// offered again is live again, which is the only way a tool comes back. Registering never widens
    /// anything: a declaration's grant template names the class and resource the configuration gave it, and a
    /// projection still has to hold a grant for it.
    ///
    /// # Errors
    /// Returns [`ToolError::MalformedDeclaration`] for a declaration that is not an external `mcp.<server>.<tool>`
    /// tool, whose name collides with a built-in tool, whose schema leaves the vocabulary, that is not
    /// host-`qworkerd` with untrusted results, or that reuses a registered version with different content.
    pub fn with_external(&self, external: Vec<ToolDeclaration>) -> Result<Self, ToolError> {
        let mut next = self.clone();
        let mut offered: BTreeSet<String> = BTreeSet::new();
        for declaration in external {
            let malformed = |detail: String| ToolError::MalformedDeclaration {
                tool: declaration.name.clone(),
                detail,
            };
            let ToolOrigin::External { server, .. } = &declaration.origin else {
                return Err(malformed(
                    "only an external declaration may be registered here".to_string(),
                ));
            };
            let prefix = format!("{}.{server}.", mcp::NAMESPACE);
            if !declaration.name.starts_with(&prefix) || declaration.name.len() == prefix.len() {
                return Err(malformed(format!(
                    "an external tool of {server} is named {prefix}<tool>"
                )));
            }
            if self
                .declarations
                .iter()
                .any(|d| d.name == declaration.name && matches!(d.origin, ToolOrigin::Builtin))
            {
                return Err(malformed("the name belongs to a built-in tool".to_string()));
            }
            if declaration.host != crate::declaration::ToolHost::Qworkerd
                || declaration.source_trust != crate::declaration::SourceTrust::UntrustedExternal
                || declaration.host_callable
                || declaration.grant_templates.len() != 1
            {
                return Err(malformed(
                    "an external tool runs in the worker, returns untrusted data, is never host-callable and has one grant template".to_string(),
                ));
            }
            crate::schema::check_vocabulary(&declaration.name, &declaration.input_schema)?;
            crate::schema::check_vocabulary(&declaration.name, &declaration.output_schema)?;
            if let Some(existing) = next.get_version(&declaration.name, declaration.version) {
                if existing != &declaration {
                    return Err(malformed(format!(
                        "version {} is already registered with different content",
                        declaration.version
                    )));
                }
            } else {
                next.declarations.push(declaration.clone());
            }
            offered.insert(declaration.name.clone());
        }
        let known_external: BTreeSet<String> = next
            .declarations
            .iter()
            .filter(|d| matches!(d.origin, ToolOrigin::External { .. }))
            .map(|d| d.name.clone())
            .collect();
        next.retired = known_external.difference(&offered).cloned().collect();
        Ok(next)
    }

    /// The registry with one server's tools replaced by `declarations` (the complete set it now offers),
    /// every other server's tools untouched. See [`ToolRegistry::with_external`].
    ///
    /// # Errors
    /// The same as [`ToolRegistry::with_external`]; a declaration of another server is
    /// [`ToolError::MalformedDeclaration`].
    pub fn with_server_tools(
        &self,
        server: &str,
        declarations: Vec<ToolDeclaration>,
    ) -> Result<Self, ToolError> {
        if let Some(stray) = declarations.iter().find(|declaration| {
            !matches!(&declaration.origin, ToolOrigin::External { server: s, .. } if s == server)
        }) {
            return Err(ToolError::MalformedDeclaration {
                tool: stray.name.clone(),
                detail: format!("the tool does not belong to the server {server}"),
            });
        }
        let mut all: Vec<ToolDeclaration> = self
            .external()
            .filter(|declaration| {
                !matches!(&declaration.origin, ToolOrigin::External { server: s, .. } if s == server)
            })
            .cloned()
            .collect();
        all.extend(declarations);
        self.with_external(all)
    }

    /// What a replacement of one server's tools changes, for the report of a refresh.
    #[must_use]
    pub fn change_for(&self, server: &str, current: &[ToolDeclaration]) -> ToolListChange {
        let latest: Vec<&ToolDeclaration> = self
            .external()
            .filter(|declaration| {
                matches!(&declaration.origin, ToolOrigin::External { server: s, .. } if s == server)
            })
            .collect();
        mcp::diff_tools(&latest, current)
    }

    /// The newest declaration of every external tool that is not retired, in registration order.
    pub fn external(&self) -> impl Iterator<Item = &ToolDeclaration> {
        self.latest()
            .filter(|declaration| matches!(declaration.origin, ToolOrigin::External { .. }))
    }

    /// Every version of the external tools of one server, retired or not. The MCP client builds the next
    /// version of a tool from these.
    #[must_use]
    pub fn external_of(&self, server: &str) -> Vec<&ToolDeclaration> {
        self.declarations
            .iter()
            .filter(|declaration| {
                matches!(&declaration.origin, ToolOrigin::External { server: s, .. } if s == server)
            })
            .collect()
    }

    /// Whether a name is an external tool whose server stopped offering it.
    #[must_use]
    pub fn is_retired(&self, name: &str) -> bool {
        self.retired.contains(name)
    }

    /// Build a registry from a YAML file on disk.
    ///
    /// # Errors
    /// Returns [`ToolError::Config`] when the file cannot be read, and the same errors as
    /// [`ToolRegistry::from_yaml`].
    pub fn from_config_file(path: &Path) -> Result<Self, ToolError> {
        let source = std::fs::read_to_string(path)
            .map_err(|error| ToolError::Config(format!("{}: {error}", path.display())))?;
        Self::from_yaml(&source)
    }

    /// Every registered declaration, in configuration order.
    pub fn declarations(&self) -> impl Iterator<Item = &ToolDeclaration> {
        self.declarations.iter()
    }

    /// The latest registered version of a tool.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ToolDeclaration> {
        if self.retired.contains(name) {
            return None;
        }
        self.declarations
            .iter()
            .filter(|declaration| declaration.name == name)
            .max_by_key(|declaration| declaration.version)
    }

    /// One exact tool version.
    #[must_use]
    pub fn get_version(&self, name: &str, version: u32) -> Option<&ToolDeclaration> {
        self.declarations
            .iter()
            .find(|declaration| declaration.name == name && declaration.version == version)
    }

    /// Every registered tool name, deduplicated and sorted.
    ///
    /// Includes reserved catalog families, so it is the set the catalog must cover.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .declarations
            .iter()
            .map(|declaration| declaration.name.as_str())
            .chain(self.families.iter().map(|family| family.name.as_str()))
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// Every declared tool name, deduplicated and sorted, whoever may call it. This is the set the
    /// catalog and the contract tests cover; it is **not** a list to show a model: use
    /// [`ToolRegistry::model_visible_names`] or [`ToolRegistry::exposed_names`] for that.
    #[must_use]
    pub fn callable_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .declarations
            .iter()
            .map(|declaration| declaration.name.as_str())
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// The newest declaration of every tool, in configuration order. Visibility is judged on this one:
    /// a call resolves to the latest version, so an older version must never make a name visible.
    fn latest(&self) -> impl Iterator<Item = &ToolDeclaration> {
        self.declarations.iter().filter(|declaration| {
            self.get(&declaration.name)
                .is_some_and(|newest| newest.version == declaration.version)
        })
    }

    /// Every tool a model may be offered by *any* projection, deduplicated and sorted: the ceiling a
    /// model-facing list (a recording compile's tool summaries, a schema export, a tool search, an MCP
    /// or SDK export) must stay inside. A projection narrows it further and never widens it.
    #[must_use]
    pub fn model_visible_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .latest()
            .filter(|declaration| declaration.model_visible)
            .map(|declaration| declaration.name.as_str())
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// Every tool the host may call without a model, deduplicated and sorted.
    #[must_use]
    pub fn host_callable_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .latest()
            .filter(|declaration| declaration.host_callable)
            .map(|declaration| declaration.name.as_str())
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// Who may propose each tool, for an **administrator-facing** check such as compiling a pack (ADP-008): the
    /// newest declaration of every tool, sorted by name. It names host-only tools, so it is never a list to show
    /// a model: use [`ToolRegistry::model_visible_names`] or [`ToolRegistry::exposed_names`] for that.
    #[must_use]
    pub fn visibility(&self) -> Vec<ToolVisibility> {
        let mut table: Vec<ToolVisibility> = self
            .latest()
            .map(|declaration| ToolVisibility {
                name: declaration.name.clone(),
                model_visible: declaration.model_visible,
                host_callable: declaration.host_callable,
            })
            .collect();
        table.sort_by(|a, b| a.name.cmp(&b.name));
        table
    }

    /// The catalog families this registry reserves but does not make callable, with the
    /// task that materializes their concrete operations.
    #[must_use]
    pub fn families(&self) -> &[RawFamilyReservation] {
        &self.families
    }

    /// How many declarations are registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.declarations.len()
    }

    /// Whether the registry holds no declarations.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.declarations.is_empty()
    }

    /// The tools a projection exposes **to a model**, in configuration order.
    ///
    /// A tool is exposed when the projection's grants cover it *and* it is model-visible (ADP-008). The
    /// second condition is checked after the first and no input changes it: a host-only tool is absent
    /// whatever the projection holds, which is how "capability only narrows" holds across the split.
    #[must_use]
    pub fn exposed_to(
        &self,
        projection: &CapabilityProjection,
        now: DateTime<Utc>,
    ) -> Vec<&ToolDeclaration> {
        self.latest()
            .filter(|declaration| {
                declaration.model_visible && declaration.covered_by(&projection.grants, now)
            })
            .collect()
    }

    /// The tools a projection exposes, deduplicated to the latest version of each name.
    #[must_use]
    pub fn exposed_names(
        &self,
        projection: &CapabilityProjection,
        now: DateTime<Utc>,
    ) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .exposed_to(projection, now)
            .into_iter()
            .map(|declaration| declaration.name.as_str())
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// Whether a projection exposes a named tool.
    #[must_use]
    pub fn is_exposed(
        &self,
        name: &str,
        projection: &CapabilityProjection,
        now: DateTime<Utc>,
    ) -> bool {
        self.exposed_to(projection, now)
            .into_iter()
            .any(|declaration| declaration.name == name)
    }

    /// Prove every declaration agrees with the side-effect mapping (ADP-001): a browser tool declares
    /// its side-effect class, and its effect class is the one that class maps to.
    ///
    /// # Errors
    /// Returns [`ToolError::MalformedDeclaration`] naming the first disagreement.
    pub fn check_side_effects(
        &self,
        mapping: &crate::side_effect::SideEffectMapping,
    ) -> Result<(), ToolError> {
        self.declarations
            .iter()
            .try_for_each(|declaration| mapping.check(declaration))
    }

    /// Prove every `computer.*` declaration carries the canonical, required action receipt in its
    /// output (ADP-002).
    ///
    /// # Errors
    /// Returns [`ToolError::MalformedDeclaration`] naming the first Tool without it.
    pub fn check_receipts(&self) -> Result<(), ToolError> {
        self.declarations
            .iter()
            .try_for_each(crate::receipt::check_declaration)
    }

    /// Prove the registry and the generated tool catalog cover each other exactly.
    ///
    /// Catalog entries containing `<…>` segments are families (`connector.<id>.<op>`); a
    /// declared concrete name matches a family when its segments line up.
    ///
    /// # Errors
    /// Returns [`ToolError::CatalogDivergence`] naming the first divergence in either
    /// direction.
    pub fn check_catalog(&self, catalog: &[String]) -> Result<(), ToolError> {
        for name in self.names() {
            if !catalog.iter().any(|entry| family_matches(entry, name)) {
                return Err(ToolError::CatalogDivergence {
                    detail: format!("declared tool {name} is not in the generated tool catalog"),
                });
            }
        }
        for entry in catalog {
            if !self.names().iter().any(|name| family_matches(entry, name)) {
                return Err(ToolError::CatalogDivergence {
                    detail: format!("catalog tool {entry} has no Tool declaration"),
                });
            }
        }
        Ok(())
    }
}

/// The registry a running process plans, exposes and dispatches against (ADP-009).
///
/// The built-in registry never changes; the external tools an MCP server offers do (a server adds, changes
/// or removes a tool while a run is going). The handle holds the registry as one immutable snapshot that a
/// refresh replaces whole, so a call is planned against one consistent registry and every holder of the
/// handle (the dispatcher, the model's tool offer) sees the same one. Cloning the handle shares it.
#[derive(Debug, Clone)]
pub struct RegistryHandle {
    current: Arc<RwLock<Arc<ToolRegistry>>>,
}

impl RegistryHandle {
    /// A handle on the shipped registry with no external tool.
    #[must_use]
    pub fn builtin() -> Self {
        Self::fixed(ToolRegistry::builtin().clone())
    }

    /// A handle on this registry.
    #[must_use]
    pub fn fixed(registry: ToolRegistry) -> Self {
        Self {
            current: Arc::new(RwLock::new(Arc::new(registry))),
        }
    }

    /// The registry as it is now. A snapshot: a later refresh does not change it.
    #[must_use]
    pub fn current(&self) -> Arc<ToolRegistry> {
        Arc::clone(&self.current.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// Replace one server's external tools with the complete set it now offers.
    ///
    /// # Errors
    /// The same as [`ToolRegistry::with_server_tools`]; on an error the registry is unchanged.
    pub fn replace_server_tools(
        &self,
        server: &str,
        declarations: Vec<ToolDeclaration>,
    ) -> Result<ToolListChange, ToolError> {
        let mut guard = self.current.write().unwrap_or_else(PoisonError::into_inner);
        let change = guard.change_for(server, &declarations);
        let next = guard.with_server_tools(server, declarations)?;
        *guard = Arc::new(next);
        Ok(change)
    }
}

/// Whether a catalog entry (possibly a `<…>` family) covers a concrete tool name.
fn family_matches(entry: &str, name: &str) -> bool {
    let mut entry_segments = entry.split('.');
    let mut name_segments = name.split('.');
    loop {
        match (entry_segments.next(), name_segments.next()) {
            (None, None) => return true,
            (Some(_), None) | (None, Some(_)) => return false,
            (Some(pattern), Some(segment)) => {
                let is_family = pattern.starts_with('<') && pattern.ends_with('>');
                if !is_family && pattern != segment {
                    return false;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::family_matches;

    #[test]
    fn families_match_only_their_own_shape() {
        assert!(family_matches(
            "connector.<id>.<op>",
            "connector.github.create_issue"
        ));
        assert!(family_matches("scm.git.<op>", "scm.git.push"));
        assert!(!family_matches("scm.git.<op>", "scm.pr.create"));
        assert!(!family_matches("scm.git.<op>", "scm.git"));
        assert!(!family_matches("fs.read", "fs.write"));
        assert!(family_matches("fs.read", "fs.read"));
    }
}
