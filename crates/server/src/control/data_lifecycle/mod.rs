//! User data export, deletion and derived-store propagation (OPS-002).
//!
//! Deletion walks authoritative rows then derived planes (Tantivy, pgvector, memory,
//! caches). A legal hold blocks deletion of the held class. Audit remains.

use std::collections::BTreeSet;

/// Derived planes deletion must visit.
pub const DERIVED_PLANES: [&str; 4] = ["tantivy", "pgvector", "memory", "cache"];

/// One store plane and whether it still holds the subject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaneState {
    /// Plane name.
    pub name: String,
    /// Subject still present.
    pub present: bool,
}

/// A deletion/export subject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataSubject {
    /// Tenant.
    pub tenant_id: String,
    /// User.
    pub user_id: String,
    /// Legal hold classes that must not be deleted.
    pub legal_holds: BTreeSet<String>,
    /// Authoritative rows remaining.
    pub authoritative: bool,
    /// Derived planes.
    pub planes: Vec<PlaneState>,
}

impl DataSubject {
    /// Seed a subject that still has data on every plane.
    #[must_use]
    pub fn populated(tenant_id: impl Into<String>, user_id: impl Into<String>) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            user_id: user_id.into(),
            legal_holds: BTreeSet::new(),
            authoritative: true,
            planes: DERIVED_PLANES
                .iter()
                .map(|name| PlaneState {
                    name: (*name).to_string(),
                    present: true,
                })
                .collect(),
        }
    }

    /// Export a snapshot of remaining locations.
    #[must_use]
    pub fn export(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.authoritative {
            out.push("authoritative".to_string());
        }
        for plane in &self.planes {
            if plane.present {
                out.push(plane.name.clone());
            }
        }
        out
    }

    /// Delete unless a legal hold applies. Derived planes follow the authority.
    ///
    /// # Errors
    /// Returns when a legal hold covers `class`.
    pub fn delete(&mut self, class: &str) -> Result<Vec<String>, String> {
        if self.legal_holds.contains(class) {
            return Err(format!(
                "legal hold on {class} blocks deletion; derived planes left in place"
            ));
        }
        self.authoritative = false;
        let mut visited = vec!["authoritative".to_string()];
        for plane in &mut self.planes {
            plane.present = false;
            visited.push(plane.name.clone());
        }
        Ok(visited)
    }

    /// Whether any derived plane still holds the subject.
    #[must_use]
    pub fn derived_remaining(&self) -> bool {
        self.planes.iter().any(|plane| plane.present)
    }
}
