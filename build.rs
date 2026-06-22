use std::{path::Path, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=frontend/src");
    println!("cargo:rerun-if-changed=frontend/index.html");
    println!("cargo:rerun-if-changed=frontend/package.json");

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
    // Surface the crate version to the frontend build so the UI can display
    // the build version (see vite.config.ts `__APP_VERSION__`).
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "dev".to_string());
    let status = Command::new("npm")
        .args(["run", "build"])
        .env("APP_VERSION", &version)
        .current_dir(frontend_dir)
        .status()
        .expect("Failed to run npm build");

    assert!(status.success(), "Frontend build failed");
}
