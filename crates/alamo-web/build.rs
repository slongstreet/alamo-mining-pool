//! Make sure the dashboard output directory exists so `rust-embed` compiles even when the
//! frontend has not been built. The daemon then serves a "dashboard not built" page.

use std::path::Path;

fn main() {
    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/dist");
    std::fs::create_dir_all(&dist).expect("create web/dist");
    println!("cargo:rerun-if-changed={}", dist.display());
}
