use std::{path::Path, process::Command};

fn main() {
    // The frontend's TypeScript wire types are generated from the Rust DTOs by
    // the `gen-types` binary (ts-rs) and committed to the repo — see
    // `crates/decman/src/bin/gen_types.rs` and `just gen-types`. This build
    // script only compiles and embeds the frontend bundle.
    //
    // `DECMAN_SKIP_FRONTEND=1` skips the (slow) npm build while iterating on the
    // Rust side locally. CI and release builds leave it unset.
    println!("cargo:rerun-if-env-changed=DECMAN_SKIP_FRONTEND");
    if std::env::var_os("DECMAN_SKIP_FRONTEND").is_some() {
        // rust-embed (`#[folder = "frontend/dist"]` in server/assets.rs) needs the
        // folder to exist at compile time, even when we skip the actual build.
        // Ensure a placeholder so the crate still compiles — e.g. for the
        // `gen-types` binary, which never serves assets. A real build overwrites it.
        let dist = Path::new("frontend/dist");
        std::fs::create_dir_all(dist).ok();
        let index = dist.join("index.html");
        if !index.exists() {
            std::fs::write(index, "<!doctype html>\n").ok();
        }
        return;
    }
    build_frontend();
}

/// Install frontend deps (first run only) and build the Vite bundle that
/// `rust-embed` embeds from `frontend/dist`.
fn build_frontend() {
    println!("cargo:rerun-if-changed=frontend/src");
    println!("cargo:rerun-if-changed=frontend/index.html");
    println!("cargo:rerun-if-changed=frontend/package.json");
    println!("cargo:rerun-if-changed=frontend/package-lock.json");
    println!("cargo:rerun-if-changed=frontend/vite.config.ts");
    println!("cargo:rerun-if-changed=frontend/tsconfig.json");
    println!("cargo:rerun-if-changed=frontend/tsconfig.app.json");
    println!("cargo:rerun-if-changed=frontend/tsconfig.node.json");

    let frontend_dir = Path::new("frontend");

    if !frontend_dir.join("node_modules").exists() {
        println!("cargo:info=Installing frontend dependencies...");
        let status = Command::new("npm")
            .args(["install"])
            .current_dir(frontend_dir)
            .status()
            .expect("Failed to run npm install");

        assert!(status.success(), "npm install failed");
    }

    println!("cargo:info=Building frontend...");
    let status = Command::new("npm")
        .args(["run", "build"])
        .current_dir(frontend_dir)
        .status()
        .expect("Failed to run npm build");

    assert!(status.success(), "Frontend build failed");
}
