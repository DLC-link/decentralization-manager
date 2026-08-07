use std::{path::Path, process::Command};

fn main() {
    // The demo UI is only compiled into the `decman-wallet-demo` binary, which is
    // behind the `demo` feature. A wallet provider depending on this crate for the
    // client library never pays for an npm build.
    println!("cargo:rerun-if-env-changed=DECMAN_SKIP_FRONTEND");
    if std::env::var_os("CARGO_FEATURE_DEMO").is_none() {
        return;
    }

    // `DECMAN_SKIP_FRONTEND=1` skips the (slow) npm build while iterating on the
    // Rust side. Same flag the `decman` crate honors, so one env var covers both.
    if std::env::var_os("DECMAN_SKIP_FRONTEND").is_some() {
        ensure_placeholder_bundle();
        return;
    }
    build_frontend();
}

/// rust-embed (`#[folder = "frontend/dist"]` in `demo/assets.rs`) needs the folder
/// to exist at compile time even when the real build is skipped.
fn ensure_placeholder_bundle() {
    let dist = Path::new("frontend/dist");
    std::fs::create_dir_all(dist).ok();
    let index = dist.join("index.html");
    if !index.exists() {
        std::fs::write(index, "<!doctype html>\n").ok();
    }
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
        println!("cargo:info=Installing demo wallet frontend dependencies...");
        let status = Command::new("npm")
            .args(["install"])
            .current_dir(frontend_dir)
            .status()
            .expect("Failed to run npm install");

        assert!(status.success(), "npm install failed");
    }

    println!("cargo:info=Building demo wallet frontend...");
    let status = Command::new("npm")
        .args(["run", "build"])
        .current_dir(frontend_dir)
        .status()
        .expect("Failed to run npm build");

    assert!(status.success(), "Frontend build failed");
}
