//! Contract binding generator (GOV-004).
//!
//! Generates Rust bindings for every `schemas/proto/**/*.proto` file into an output
//! directory (default `crates/contracts/src/generated`). The generated tree is checked
//! in and CI fails on any regeneration diff, so hand edits are never silently kept.
//!
//! Usage: `cargo run -p quansio-contract-gen -- [--out <dir>]`

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    // scripts/dev/contract-gen -> repository root
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("generator lives at scripts/dev/contract-gen")
        .to_path_buf()
}

fn collect_protos(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            collect_protos(&path, out);
        } else if path.extension().map(|e| e == "proto").unwrap_or(false) {
            out.push(path);
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut out_dir = repo_root().join("crates/contracts/src/generated");
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => out_dir = PathBuf::from(args.next().expect("--out requires a directory")),
            other => panic!("unknown argument: {other}"),
        }
    }

    let proto_root = repo_root().join("schemas/proto");
    let mut protos = Vec::new();
    collect_protos(&proto_root, &mut protos);
    protos.sort();
    assert!(!protos.is_empty(), "no .proto sources found under {}", proto_root.display());

    if out_dir.exists() {
        std::fs::remove_dir_all(&out_dir).expect("clear previous generated tree");
    }
    std::fs::create_dir_all(&out_dir).expect("create output directory");

    tonic_build::configure()
        .out_dir(&out_dir)
        .btree_map(["."])
        .build_server(true)
        .build_client(true)
        .compile_protos(&protos, &[proto_root.clone()])
        .expect("prost/tonic codegen failed");

    // A stable module tree: one module per `quansio.v1.<area>` package, so that
    // `crates/contracts/src/lib.rs` can include the generated file list verbatim.
    let mut packages: Vec<String> = std::fs::read_dir(&out_dir)
        .expect("read generated tree")
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let name = path.file_name()?.to_str()?.to_string();
            name.strip_suffix(".rs").map(|s| s.to_string())
        })
        .collect();
    packages.sort();

    // Generated code is not hand-written: silence documentation/style lints inside it
    // so `clippy -D warnings` stays meaningful for the hand-written crates.
    let mut modules = String::from(
        "//! Generated Rust bindings (prost/tonic) for Quansio contracts. Do not hand edit.\n\
         //! Regenerate with: cargo run -p quansio-contract-gen -- --out crates/contracts/src/generated\n\
         #![allow(missing_docs, clippy::all, clippy::pedantic, rustdoc::all)]\n\n",
    );
    for package in &packages {
        let area = package.rsplit('.').next().expect("package has an area segment");
        modules.push_str(&format!("pub mod {area} {{\n    include!(\"{package}.rs\");\n}}\n"));
    }
    std::fs::write(out_dir.join("mod.rs"), modules).expect("write generated mod.rs");

    println!(
        "generated {} package(s) into {}",
        packages.len(),
        out_dir.display()
    );
}
