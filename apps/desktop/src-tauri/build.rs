//! Writes a placeholder page when the frontend is not built, then runs the Tauri build.
//!
//! `tauri::generate_context!` panics at compile time when `frontendDist` does not exist, under `clippy` and `doc`
//! too, so a fresh clone could not build the workspace before `bun run build`. `tauri build` always replaces the
//! placeholder through `beforeBuildCommand` (ADR 0015).

use std::path::Path;
use std::{env, fs};

const PLACEHOLDER: &str = "<!doctype html>\n\
<html lang=\"en\">\n\
<head><meta charset=\"utf-8\"><title>Procly</title></head>\n\
<body><p>The frontend is not built. Run <code>bun run build</code> in <code>apps/desktop</code>.</p></body>\n\
</html>\n";

fn main() {
    let manifest_dir = env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR for a build script");
    let dist = Path::new(&manifest_dir).join("../dist");
    if !dist.join("index.html").exists() {
        fs::create_dir_all(&dist).expect("the placeholder directory can be created");
        fs::write(dist.join("index.html"), PLACEHOLDER).expect("the placeholder page can be written");
    }
    tauri_build::build();
}
