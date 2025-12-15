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

        if !status.success() {
            panic!("npm install failed");
        }
    }

    println!("cargo:info=Building frontend...");
    let status = Command::new("npm")
        .args(["run", "build"])
        .current_dir(frontend_dir)
        .status()
        .expect("Failed to run npm build");

    if !status.success() {
        panic!("Frontend build failed");
    }
}
