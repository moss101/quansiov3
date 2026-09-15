//! quansio CLI over the public API v1.
//!
//! Canonical owner (DOSSIER.md §17): `crates/cli`. Every mutating verb is a public
//! `/v1/commands/<Name>` call. There is no flag that bypasses API/runtime policy.

#![forbid(unsafe_code)]

/// Repository path of this crate's canonical owner.
pub const CANONICAL_OWNER: &str = "crates/cli";

/// Supported CLI verbs. Each maps to a public command or read projection.
pub const VERBS: &[&str] = &[
    "login",
    "workspaces",
    "threads",
    "messages",
    "runs",
    "approvals",
    "artifacts",
    "routines",
    "targets",
    "diagnostics",
];

/// Build the public API path for a verb. Never a private/internal route.
///
/// # Errors
/// Returns a message when the verb is unknown.
pub fn api_path(verb: &str, json: bool) -> Result<String, String> {
    let _ = json;
    match verb {
        "login" => Ok("/v1/commands/CreateWorkspace".to_string()),
        "workspaces" => Ok("/v1/read-projections".to_string()),
        "threads" => Ok("/v1/messages".to_string()),
        "messages" => Ok("/v1/messages".to_string()),
        "runs" => Ok("/v1/runs/{id}".to_string()),
        "approvals" => Ok("/v1/approvals".to_string()),
        "artifacts" => Ok("/v1/artifacts".to_string()),
        "routines" => Ok("/v1/routines".to_string()),
        "targets" => Ok("/v1/targets".to_string()),
        "diagnostics" => Ok("/v1/health".to_string()),
        "bypass-policy" | "--force" => Err("CLI never bypasses API/runtime policy".to_string()),
        other => Err(format!("unknown verb {other}")),
    }
}

/// Access tokens are tenant-scoped; a token without a tenant is unusable.
#[must_use]
pub fn token_is_scoped(token: &str) -> bool {
    token.contains("tn_") && !token.contains("unscoped")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_verb_is_a_public_v1_path() {
        for verb in VERBS {
            let path = api_path(verb, true).expect("path");
            assert!(path.starts_with("/v1/"), "{verb} -> {path}");
        }
        assert!(api_path("bypass-policy", true)
            .unwrap_err()
            .contains("never bypasses"));
        assert!(token_is_scoped("access.tn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"));
        assert!(!token_is_scoped("unscoped-root"));
    }
}
