//! Append-only hash-chained audit log (OPS-002, DOMAIN.md §16).
//!
//! Entries are immutable: `append` is the only write. An in-place edit is refused.
//! Each tenant has its own chain (`prev_hash` / `hash`).

use sha2::{Digest, Sha256};

/// One AuditEntry (DOMAIN.md §16).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry {
    /// `aud_` id.
    pub id: String,
    /// Tenant.
    pub tenant_id: String,
    /// Optional workspace.
    pub workspace_id: Option<String>,
    /// Actor id.
    pub actor: String,
    /// Command or ops action.
    pub action: String,
    /// Target ref.
    pub target_ref: String,
    /// Decision.
    pub decision: String,
    /// Reason.
    pub reason: String,
    /// Correlation.
    pub correlation_id: String,
    /// When.
    pub occurred_at: String,
    /// Previous hash (empty for genesis).
    pub prev_hash: String,
    /// Hash of this entry.
    pub hash: String,
}

impl AuditEntry {
    /// Canonical bytes the hash covers. `hash` itself is excluded.
    #[must_use]
    pub fn canonical(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
            self.id,
            self.tenant_id,
            self.workspace_id.as_deref().unwrap_or(""),
            self.actor,
            self.action,
            self.target_ref,
            self.decision,
            self.reason,
            self.correlation_id,
            self.occurred_at,
            self.prev_hash
        )
    }
}

fn digest(canonical: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Per-tenant append-only log.
#[derive(Debug, Default, Clone)]
pub struct AuditLog {
    entries: Vec<AuditEntry>,
}

impl AuditLog {
    /// Append an entry. Computes hash over canonical fields chained from the tenant tail.
    pub fn append(&mut self, mut entry: AuditEntry) -> Result<&AuditEntry, String> {
        if !entry.id.starts_with("aud_") {
            return Err("audit id must start with aud_".to_string());
        }
        let prev = self
            .entries
            .iter()
            .rev()
            .find(|item| item.tenant_id == entry.tenant_id);
        entry.prev_hash = prev.map(|item| item.hash.clone()).unwrap_or_default();
        entry.hash = digest(&entry.canonical());
        self.entries.push(entry);
        Ok(self.entries.last().expect("just pushed"))
    }

    /// In-place edit is forbidden.
    ///
    /// # Errors
    /// Always. Audit entries cannot be edited in place.
    pub fn edit_in_place(&mut self, _id: &str, _patch: AuditEntry) -> Result<(), String> {
        Err("audit entries cannot be edited in place".to_string())
    }

    /// Export the tenant chain in order.
    #[must_use]
    pub fn export(&self, tenant_id: &str) -> Vec<AuditEntry> {
        self.entries
            .iter()
            .filter(|item| item.tenant_id == tenant_id)
            .cloned()
            .collect()
    }

    /// Verify the tenant chain.
    #[must_use]
    pub fn chain_holds(&self, tenant_id: &str) -> bool {
        let mut prev = String::new();
        for entry in self.export(tenant_id) {
            if entry.prev_hash != prev {
                return false;
            }
            if digest(&entry.canonical()) != entry.hash {
                return false;
            }
            prev = entry.hash;
        }
        true
    }
}

/// Repository path.
pub const AUDIT_OWNER: &str = "crates/server/src/audit";
