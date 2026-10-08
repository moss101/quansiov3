//! Side-effect classes: how a tool's nature maps onto the Effect Ledger's classes (ADP-001).
//!
//! A tool declares what *kind* of thing it does to the world; `config/effects.yaml`
//! (`browser_side_effects`) says which effect class and tier that kind settles as, and whether the host
//! must journal it for idempotency. The registry refuses a declaration whose `effect_class` disagrees
//! with the mapping, so a tool cannot claim to be a read while declaring a mutation's class, and a new
//! tool cannot lower its tier by choosing a different class than its kind allows.
//!
//! The mapping adds no effect class and no tier: every row names a class of the existing taxonomy
//! (DOMAIN.md §7.1), and the Effect Ledger keeps recording against that taxonomy.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use quansio_capability::EffectClass;
use serde::Deserialize;

use crate::declaration::ToolDeclaration;
use crate::error::ToolError;

/// What a tool does to the world, independent of how it does it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SideEffectClass {
    /// Reads the world and changes nothing.
    Read,
    /// Reads what may expose sensitive on-screen content (a screenshot, a field's value).
    ReadSensitive,
    /// Changes the tool's own sandbox and nothing outside it (typing, answering a dialog).
    LocalMutation,
    /// Reaches a destination outside the sandbox (navigation).
    OpenWorldMutation,
    /// May cause an external effect the page decides (a click that submits, sends or pays).
    PotentialExternalEffect,
    /// Brings another browser's session data (cookies) into the managed browser (ADP-009). Not the page's doing: the person's
    /// own act, approved at tier 4 every time.
    SessionImport,
}

impl SideEffectClass {
    /// Every class, in ladder order.
    pub const ALL: [Self; 6] = [
        Self::Read,
        Self::ReadSensitive,
        Self::LocalMutation,
        Self::OpenWorldMutation,
        Self::PotentialExternalEffect,
        Self::SessionImport,
    ];

    /// The canonical spelling used in `config/tools.yaml` and `config/effects.yaml`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::ReadSensitive => "read_sensitive",
            Self::LocalMutation => "local_mutation",
            Self::OpenWorldMutation => "open_world_mutation",
            Self::PotentialExternalEffect => "potential_external_effect",
            Self::SessionImport => "session_import",
        }
    }

    /// Parse the canonical spelling.
    ///
    /// # Errors
    /// Returns the offending value when it is not a side-effect class.
    pub fn parse(value: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|class| class.as_str() == value)
            .ok_or_else(|| {
                format!("side_effect {value:?} is not one of the six side-effect classes")
            })
    }
}

#[derive(Debug, Deserialize)]
struct EffectsDocument {
    #[serde(default)]
    browser_side_effects: BTreeMap<String, RawRow>,
}

#[derive(Debug, Deserialize)]
struct RawRow {
    /// The effect class the kind settles as. Named `settles_as`, not `effect_class`, so the taxonomy
    /// rows below stay the only lines of the file that say `effect_class:`.
    settles_as: String,
    journaled: bool,
}

/// One row of the mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SideEffectRow {
    /// The effect class the Effect Ledger records for a tool of this kind.
    pub effect_class: EffectClass,
    /// Whether the host journals the action for idempotency (keyed by call id and action id).
    pub journaled: bool,
}

/// The side-effect mapping from `config/effects.yaml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SideEffectMapping {
    rows: BTreeMap<SideEffectClass, SideEffectRow>,
}

/// Where `config/effects.yaml` lives relative to this source file.
const EFFECTS_CONFIG: &str = include_str!("../../../config/effects.yaml");

impl SideEffectMapping {
    /// The shipped mapping.
    ///
    /// # Panics
    /// Panics only if the repository's own configuration is invalid, which the contract tests catch
    /// first.
    #[must_use]
    pub fn builtin() -> &'static Self {
        static MAPPING: OnceLock<SideEffectMapping> = OnceLock::new();
        MAPPING.get_or_init(|| {
            Self::from_yaml(EFFECTS_CONFIG)
                .expect("config/effects.yaml carries a valid browser_side_effects mapping")
        })
    }

    /// Parse the mapping out of an effects document.
    ///
    /// # Errors
    /// Returns [`ToolError::Config`] when the document is malformed, a side-effect class is missing or
    /// unknown, or a row names a class that is not an effect class.
    pub fn from_yaml(source: &str) -> Result<Self, ToolError> {
        let document: EffectsDocument =
            serde_yaml::from_str(source).map_err(|error| ToolError::Config(error.to_string()))?;
        let mut rows = BTreeMap::new();
        for (name, raw) in document.browser_side_effects {
            let class = SideEffectClass::parse(&name).map_err(ToolError::Config)?;
            let effect_class = EffectClass::parse(raw.settles_as.clone())
                .map_err(|error| ToolError::Config(format!("{name}: {error}")))?;
            rows.insert(
                class,
                SideEffectRow {
                    effect_class,
                    journaled: raw.journaled,
                },
            );
        }
        for class in SideEffectClass::ALL {
            if !rows.contains_key(&class) {
                return Err(ToolError::Config(format!(
                    "browser_side_effects has no row for {}",
                    class.as_str()
                )));
            }
        }
        Ok(Self { rows })
    }

    /// The row for a class.
    ///
    /// # Panics
    /// Never: construction guarantees every class has a row.
    #[must_use]
    pub fn row(&self, class: SideEffectClass) -> &SideEffectRow {
        &self.rows[&class]
    }

    /// Prove a declaration agrees with the mapping.
    ///
    /// # Errors
    /// Returns [`ToolError::MalformedDeclaration`] when a browser tool declares no side-effect class,
    /// or when its fixed effect class is not the one its side-effect class maps to.
    pub fn check(&self, declaration: &ToolDeclaration) -> Result<(), ToolError> {
        let malformed = |detail: String| ToolError::MalformedDeclaration {
            tool: declaration.name.clone(),
            detail,
        };
        let Some(class) = declaration.side_effect else {
            return if declaration.host == crate::declaration::ToolHost::Browser {
                Err(malformed(
                    "a browser tool must declare its side_effect class".to_string(),
                ))
            } else {
                Ok(())
            };
        };
        let expected = &self.row(class).effect_class;
        match &declaration.effect_class {
            crate::declaration::EffectClassRule::Static(declared) if declared == expected => Ok(()),
            crate::declaration::EffectClassRule::Static(declared) => Err(malformed(format!(
                "side_effect {} maps to {expected}, but the declaration says {declared}",
                class.as_str()
            ))),
            crate::declaration::EffectClassRule::Derived(_) => Err(malformed(format!(
                "side_effect {} maps to the fixed class {expected}; a derived class cannot be checked against it",
                class.as_str()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_mapping_names_every_class() {
        let mapping = SideEffectMapping::builtin();
        for class in SideEffectClass::ALL {
            let row = mapping.row(class);
            assert!(!row.effect_class.as_str().is_empty());
        }
        assert!(!mapping.row(SideEffectClass::Read).journaled);
        assert!(!mapping.row(SideEffectClass::ReadSensitive).journaled);
        assert!(mapping.row(SideEffectClass::LocalMutation).journaled);
        assert!(mapping.row(SideEffectClass::OpenWorldMutation).journaled);
        assert!(
            mapping
                .row(SideEffectClass::PotentialExternalEffect)
                .journaled
        );
        // An import is journaled and settles as the one class that is tier 4 and never `always` (ADP-009).
        assert!(mapping.row(SideEffectClass::SessionImport).journaled);
        assert_eq!(
            mapping
                .row(SideEffectClass::SessionImport)
                .effect_class
                .as_str(),
            "browser.session.import"
        );
    }

    #[test]
    fn a_missing_or_unknown_class_is_refused() {
        let missing =
            "browser_side_effects:\n  read: { settles_as: read.external, journaled: false }\n";
        assert!(SideEffectMapping::from_yaml(missing).is_err());
        let unknown =
            "browser_side_effects:\n  shrug: { settles_as: read.external, journaled: false }\n";
        assert!(SideEffectMapping::from_yaml(unknown).is_err());
    }
}
