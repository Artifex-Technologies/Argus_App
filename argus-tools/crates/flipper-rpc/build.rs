// Generates the Rust message types from proto/*.proto with prost. Needs `protoc` on PATH
// (Arch: the `protobuf` package) or PROTOC pointing at it.
use std::path::PathBuf;

fn main() {
    let protos = ["flipper", "storage", "system", "application", "gui", "gpio", "property", "desktop"];
    let files: Vec<PathBuf> = protos.iter().map(|p| PathBuf::from(format!("proto/{p}.proto"))).collect();
    for f in &files {
        println!("cargo:rerun-if-changed={}", f.display());
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    prost_build::Config::new()
        .file_descriptor_set_path(out.join("flipper_descriptor.bin"))
        .compile_protos(&files, &["proto"])
        .expect("protoc failed on proto/*.proto");
}
