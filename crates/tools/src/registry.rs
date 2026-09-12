//! The control-plane Tool Registry (DOMAIN.md §7.5).
//!
//! Declarations are loaded from `config/tools.yaml` and checked against the generated
//! tool catalog (`schemas/catalog/tools.yaml`, derived from DOMAIN.md). The registry
//! exposes tools to a model **only** through capability filtering: a run never sees a tool
//! its projection cannot cover.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use quansio_capability::CapabilityProjection;
use serde::{Deserialize, Serialize};

use crate::declaration::{RawToolDeclaration, ToolDeclaration};
use crate::error::ToolError;

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
        })
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

    /// Every callable tool name, deduplicated and sorted.
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

    /// The tools a projection exposes, in configuration order.
    #[must_use]
    pub fn exposed_to(
        &self,
        projection: &CapabilityProjection,
        now: DateTime<Utc>,
    ) -> Vec<&ToolDeclaration> {
        self.declarations
            .iter()
            .filter(|declaration| declaration.covered_by(&projection.grants, now))
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
