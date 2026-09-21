//! `sqlx::migrate!` embeds the migration files it finds at macro expansion; a file added
//! afterwards is invisible until the crate recompiles. Watching the directory makes an
//! added migration a rebuild, so a test binary never runs against a stale schema.
fn main() {
    println!("cargo:rerun-if-changed=../../migrations");
}
