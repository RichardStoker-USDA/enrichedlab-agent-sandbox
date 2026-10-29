//! Embeds the result kind registry (`kinds/*.yaml`) so a broker binary carries exactly the registry it
//! was built with and reports its digest, and generates the evidence gRPC service from the contracts descriptor set.
use std::{env, fs, path::Path};

use prost::Message;

fn main() {
    println!("cargo:rerun-if-changed=proto/enrichedlab_evidence_v1.binpb");
    let manifest = env::var("CARGO_MANIFEST_DIR").expect("manifest dir");
    let descriptor = fs::read(Path::new(&manifest).join("proto/enrichedlab_evidence_v1.binpb"))
        .expect("evidence contract descriptor");
    let descriptor =
        prost_types::FileDescriptorSet::decode(descriptor.as_slice()).expect("descriptor set");
    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_fds(descriptor)
        .expect("generate the evidence gRPC service");
    println!("cargo:rerun-if-changed=kinds");
    let dir = Path::new(&env::var("CARGO_MANIFEST_DIR").expect("manifest dir")).join("kinds");
    let mut files: Vec<_> = fs::read_dir(&dir)
        .expect("kinds directory")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "yaml")
        })
        .collect();
    files.sort();
    let mut out = String::from("pub(crate) static KIND_FILES: &[(&str, &str)] = &[\n");
    for file in files {
        let name = file
            .file_name()
            .and_then(|name| name.to_str())
            .expect("UTF-8 kind file name");
        out.push_str(&format!(
            "    ({name:?}, include_str!({:?})),\n",
            file.display().to_string()
        ));
    }
    out.push_str("];\n");
    fs::write(
        Path::new(&env::var("OUT_DIR").expect("out dir")).join("kind_files.rs"),
        out,
    )
    .expect("write registry");
}
