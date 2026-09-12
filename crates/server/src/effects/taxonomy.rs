//! The effect-class registry (`config/effects.yaml`, DOMAIN.md §7.1).
//!
//! One row per consequential effect class: consequence tier, default policy decision and
//! reconciliation strategy. The registry is loaded from `config/effects.yaml` and
//! validated against the DOMAIN §7.1 taxonomy, and it fails closed: a missing class, an
//! unknown class, an out-of-range tier or an unparseable strategy is an error, never a
//! silently permissive default.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

use quansio_capability::{EffectClass, Tier};
use serde::Deserialize;

use crate::effects::error::EffectError;
use crate::effects::model::ReconciliationStrategy;

/// The effect classes of DOMAIN.md §7.1, with `skill.promote` and `pack.publish` split
/// out of their shared table row. The registry must cover exactly these classes.
pub const DOMAIN_EFFECT_CLASSES: &[&str] = &[
    "read.internal",
    "read.external",
    "fs.write.workspace",
    "fs.write.host",
    "process.exec.sandboxed",
    "process.exec.host",
    "network.egress.new_destination",
    "content.upload",
    "record.create",
    "record.update",
    "record.delete",
    "message.send",
    "content.publish",
    "scm.remote.write",
    "computer.input.privileged",
    "browser.session.import",
    "payment.execute",
    "data.upload.protected",
    "credential.access",
    "identity.change",
    "memory.write",
    "knowledge.write",
    "skill.promote",
    "pack.publish",
    "runtime.control",
];

/// One registered effect class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectTaxonomyEntry {
    /// The effect class.
    pub effect_class: EffectClass,
    /// Consequence tier.
    pub tier: Tier,
    /// Default policy decision applied before reservation.
    pub default_decision: String,
    /// Strategy the ledger follows when the outcome is unknown.
    pub reconciliation: ReconciliationStrategy,
}

/// The validated effect taxonomy.
#[derive(Debug, Clone)]
pub struct EffectTaxonomy {
    entries: BTreeMap<String, EffectTaxonomyEntry>,
}

#[derive(Debug, Deserialize)]
struct TaxonomyFile {
    version: u32,
    effect_classes: Vec<TaxonomyRow>,
}

#[derive(Debug, Deserialize)]
struct TaxonomyRow {
    effect_class: String,
    tier: u8,
    default_decision: String,
    reconciliation: String,
}

/// Where `config/effects.yaml` lives relative to this source file.
const EFFECTS_CONFIG: &str = include_str!("../../../../config/effects.yaml");

impl EffectTaxonomy {
    /// The shipped taxonomy from `config/effects.yaml`.
    ///
    /// Panics only if the committed configuration is invalid, which the taxonomy tests
    /// make impossible: the file is validated against DOMAIN §7.1 before release.
    #[must_use]
    pub fn builtin() -> &'static Self {
        static BUILTIN: OnceLock<EffectTaxonomy> = OnceLock::new();
        BUILTIN.get_or_init(|| {
            Self::from_yaml(EFFECTS_CONFIG)
                .expect("config/effects.yaml is a valid DOMAIN §7.1 taxonomy")
        })
    }

    /// Parse and validate a taxonomy document.
    ///
    /// # Errors
    /// Returns [`EffectError::Taxonomy`] when the document is malformed, when a class is
    /// missing or unknown, or when a tier is outside 0–4.
    pub fn from_yaml(source: &str) -> Result<Self, EffectError> {
        let file: TaxonomyFile = serde_yaml::from_str(source)
            .map_err(|error| EffectError::Taxonomy(error.to_string()))?;
        if file.version != 1 {
            return Err(EffectError::Taxonomy(format!(
                "unsupported taxonomy version {}",
                file.version
            )));
        }
        if file.effect_classes.is_empty() {
            return Err(EffectError::Taxonomy(
                "taxonomy registers no effect classes".to_string(),
            ));
        }
        let mut entries = BTreeMap::new();
        for row in file.effect_classes {
            let class = EffectClass::parse(row.effect_class.clone())?;
            if !DOMAIN_EFFECT_CLASSES.contains(&class.as_str()) {
                return Err(EffectError::UnknownEffectClass(class.to_string()));
            }
            let tier = Tier::new(row.tier)?;
            if row.default_decision.trim().is_empty() {
                return Err(EffectError::Taxonomy(format!(
                    "effect class {} has no default decision",
                    class
                )));
            }
            let reconciliation = ReconciliationStrategy::parse(&row.reconciliation)?;
            let entry = EffectTaxonomyEntry {
                effect_class: class.clone(),
                tier,
                default_decision: row.default_decision,
                reconciliation,
            };
            if entries.insert(class.to_string(), entry).is_some() {
                return Err(EffectError::Taxonomy(format!(
                    "effect class {class} is registered twice"
                )));
            }
        }
        let missing = DOMAIN_EFFECT_CLASSES
            .iter()
            .filter(|class| !entries.contains_key(**class))
            .copied()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(EffectError::Taxonomy(format!(
                "taxonomy is missing DOMAIN §7.1 classes: {}",
                missing.join(", ")
            )));
        }
        Ok(Self { entries })
    }

    /// Load and validate a taxonomy from a file.
    ///
    /// # Errors
    /// Returns [`EffectError::Taxonomy`] when the file cannot be read or is invalid.
    pub fn from_config_file(path: &Path) -> Result<Self, EffectError> {
        let source = std::fs::read_to_string(path)
            .map_err(|error| EffectError::Taxonomy(format!("{}: {error}", path.display())))?;
        Self::from_yaml(&source)
    }

    /// Look up one registered class.
    #[must_use]
    pub fn get(&self, effect_class: &str) -> Option<&EffectTaxonomyEntry> {
        self.entries.get(effect_class)
    }

    /// Look up one registered class, failing closed when it is not registered.
    ///
    /// # Errors
    /// Returns [`EffectError::UnknownEffectClass`] for an unregistered class.
    pub fn entry_for(
        &self,
        effect_class: &EffectClass,
    ) -> Result<&EffectTaxonomyEntry, EffectError> {
        self.get(effect_class.as_str())
            .ok_or_else(|| EffectError::UnknownEffectClass(effect_class.to_string()))
    }

    /// The registered tier for a class.
    ///
    /// # Errors
    /// Returns [`EffectError::UnknownEffectClass`] for an unregistered class.
    pub fn tier_for(&self, effect_class: &EffectClass) -> Result<Tier, EffectError> {
        Ok(self.entry_for(effect_class)?.tier)
    }

    /// The reconciliation strategy for a class.
    ///
    /// # Errors
    /// Returns [`EffectError::UnknownEffectClass`] for an unregistered class.
    pub fn strategy_for(
        &self,
        effect_class: &EffectClass,
    ) -> Result<ReconciliationStrategy, EffectError> {
        Ok(self.entry_for(effect_class)?.reconciliation)
    }

    /// Every registered entry, ordered by class.
    pub fn entries(&self) -> impl Iterator<Item = &EffectTaxonomyEntry> {
        self.entries.values()
    }

    /// Number of registered classes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the taxonomy registers no class.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::error::EffectError;
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    struct Catalog {
        effect_classes: Vec<CatalogRow>,
    }

    #[derive(Debug, Deserialize)]
    struct CatalogRow {
        effect_class: String,
        tier: String,
        reconciliation: String,
    }

    fn catalog() -> Catalog {
        serde_yaml::from_str(include_str!(
            "../../../../schemas/catalog/effect-classes.yaml"
        ))
        .expect("generated effect-class catalog parses")
    }

    #[test]
    fn builtin_covers_every_domain_class_exactly() {
        let taxonomy = EffectTaxonomy::builtin();
        assert_eq!(taxonomy.len(), DOMAIN_EFFECT_CLASSES.len());
        for class in DOMAIN_EFFECT_CLASSES {
            assert!(
                taxonomy.get(class).is_some(),
                "{class} must be registered in config/effects.yaml"
            );
        }
    }

    #[test]
    fn builtin_matches_the_generated_catalog() {
        let taxonomy = EffectTaxonomy::builtin();
        let catalog = catalog();
        assert_eq!(catalog.effect_classes.len(), 23);
        for row in &catalog.effect_classes {
            let entry = taxonomy.get(&row.effect_class).unwrap_or_else(|| {
                panic!("{} is missing from config/effects.yaml", row.effect_class)
            });
            let tier: u8 = row.tier.parse().expect("catalog tier is numeric");
            assert_eq!(
                entry.tier.get(),
                tier,
                "{} tier drifted from DOMAIN §7.1",
                row.effect_class
            );
            assert_eq!(
                entry.reconciliation,
                ReconciliationStrategy::parse(&row.reconciliation).expect("catalog strategy"),
                "{} reconciliation drifted from DOMAIN §7.1",
                row.effect_class
            );
        }
    }

    #[test]
    fn a_missing_class_fails_closed() {
        let source = "version: 1\neffect_classes:\n  - effect_class: read.internal\n    tier: 0\n    default_decision: allow\n    reconciliation: none\n";
        assert!(EffectTaxonomy::from_yaml(source).is_err());
    }

    #[test]
    fn an_unknown_class_fails_closed() {
        let source = "version: 1\neffect_classes:\n  - effect_class: read.internal\n    tier: 0\n    default_decision: allow\n    reconciliation: none\n  - effect_class: invented.effect\n    tier: 1\n    default_decision: allow\n    reconciliation: none\n";
        assert!(matches!(
            EffectTaxonomy::from_yaml(source),
            Err(EffectError::UnknownEffectClass(class)) if class == "invented.effect"
        ));
    }

    #[test]
    fn an_out_of_range_tier_fails_closed() {
        let source = "version: 1\neffect_classes:\n  - effect_class: read.internal\n    tier: 9\n    default_decision: allow\n    reconciliation: none\n";
        assert!(EffectTaxonomy::from_yaml(source).is_err());
    }
}
