use std::{
    hash::{DefaultHasher, Hasher},
    path::PathBuf,
};

const ENGINE_FILES: [(&str, &str); 3] = [
    ("spotdl.bin", "SPOTDL_STUDIO_SPOTDL"),
    ("ffmpeg.gz", "SPOTDL_STUDIO_FFMPEG"),
    ("deno.zip", "SPOTDL_STUDIO_DENO"),
];

fn main() {
    println!("cargo::rustc-check-cfg=cfg(embedded_engines)");
    println!("cargo:rerun-if-changed=engines");

    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("engines");
    let paths: Vec<PathBuf> = ENGINE_FILES.iter().map(|(f, _)| dir.join(f)).collect();

    if paths.iter().all(|p| p.is_file()) {
        // The fingerprint names the folder the engines are unpacked into, so a
        // new engine build never reuses files left by an older one.
        let mut hasher = DefaultHasher::new();
        for ((file, var), path) in ENGINE_FILES.iter().zip(&paths) {
            println!("cargo:rerun-if-changed={}", path.display());
            println!("cargo:rustc-env={var}={}", path.display());
            hasher.write(file.as_bytes());
            hasher.write(&std::fs::read(path).unwrap());
        }
        println!("cargo:rustc-env=SPOTDL_STUDIO_ENGINE_ID={:016x}", hasher.finish());
        println!("cargo:rustc-cfg=embedded_engines");
    } else {
        println!(
            "cargo:warning=No engines in src-tauri/engines; this build will look for spotdl and ffmpeg on PATH. Run scripts/fetch-engines.sh to pack them in."
        );
    }

    tauri_build::build()
}
