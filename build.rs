use std::error::Error;

use walkdir::WalkDir;

fn main() -> Result<(), Box<dyn Error>> {
    let mut protos = Vec::new();

    for entry in WalkDir::new("proto").into_iter().filter_map(Result::ok) {
        let path = entry.path();

        if let Some(ext) = path.extension()
            && ext == "proto"
        {
            protos.push(path.to_string_lossy().into_owned());
        }
    }

    for p in &protos {
        println!("cargo:rerun-if-changed={p}");
    }

    println!("cargo:rerun-if-changed=build.rs");

    tonic_prost_build::configure()
        .build_server(false)
        .compile_protos(
            &protos.iter().map(String::as_str).collect::<Vec<_>>(),
            &["proto/canton", "proto/googleapis"],
        )?;

    Ok(())
}
