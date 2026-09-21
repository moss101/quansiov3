//! APP-013: the CLI is a client and nothing else — a structural scan of the crate.
//!
//! Every HTTP path the crate spells is under `/v1/`; no database driver, no `.env`
//! reading, no worker channel and no process spawning is compiled into it; the only
//! credential it touches is its own credential file. A verb that could bypass API or
//! runtime policy would have to exist here first, and nothing does.

use std::path::Path;

fn sources() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    let mut stack = vec![root];
    while let Some(directory) = stack.pop() {
        for entry in std::fs::read_dir(&directory).expect("read src").flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                found.push((
                    path.display().to_string(),
                    std::fs::read_to_string(&path).expect("read source"),
                ));
            }
        }
    }
    found
}

#[test]
fn every_path_the_crate_spells_is_public_api_v1() {
    let mut paths = 0;
    for (file, text) in sources() {
        for (number, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            // Every string literal that looks like an absolute HTTP path.
            let mut rest = line;
            while let Some(start) = rest.find("\"/") {
                let candidate = &rest[start + 1..];
                let end = candidate.find('"').unwrap_or(candidate.len());
                let literal = &candidate[..end];
                rest = &candidate[end..];
                if literal.starts_with("/v1/") || literal == "/v1" {
                    paths += 1;
                    continue;
                }
                // Not an API path: a filesystem default or a test fixture only.
                let allowed = literal.starts_with("/tmp")
                    || literal.starts_with("/x")
                    || literal.starts_with("/home")
                    || literal.starts_with("/etc")
                    || literal.starts_with("/internal")
                    || literal.starts_with("/v2")
                    || literal.starts_with("/admin")
                    || literal.starts_with("/runs")
                    || literal == "/";
                assert!(
                    allowed,
                    "{file}:{}: {literal:?} is not a public API path",
                    number + 1
                );
            }
        }
    }
    assert!(
        paths >= 25,
        "the scan must actually see the crate's API paths ({paths})"
    );
}

#[test]
fn nothing_but_the_public_api_is_compiled_in() {
    let manifest =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .expect("manifest");
    for forbidden in [
        "sqlx",
        "postgres",
        "tokio-postgres",
        "redis",
        "nats",
        "quansio-server",
        "quansio-events",
        "quansio-machine",
        "dotenv",
    ] {
        assert!(
            !manifest.contains(forbidden),
            "the CLI must not depend on {forbidden}"
        );
    }
    for (file, text) in sources() {
        for forbidden in [
            "QUANSIO_DATABASE_URL",
            "Command::new(",
            "std::process::Command",
            "\".env\"",
            "x-quansio-tenant",
            "x-quansio-user",
            "QUANSIO_TRUST_SCOPE_HEADERS",
        ] {
            assert!(!text.contains(forbidden), "{file} mentions {forbidden}: the CLI reaches the server only as a bearer session over /v1");
        }
    }
}
