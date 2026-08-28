use std::{env, path::Path};

use lenso_contract_codegen::{ProjectionLanguage, check_projection, write_projection};

fn main() {
    println!("cargo:rerun-if-changed=capability.json");
    println!("cargo:rerun-if-changed=schemas");
    println!("cargo:rerun-if-changed=src/generated.rs");
    println!("cargo:rerun-if-env-changed=LENSO_UPDATE_CONTRACT_SNAPSHOT");

    if env::var_os("LENSO_UPDATE_CONTRACT_SNAPSHOT").is_some() {
        write_projection(
            Path::new("capability.json"),
            ProjectionLanguage::RustRuntime,
            Path::new("src/generated.rs"),
        )
        .unwrap_or_else(|error| panic!("failed to update Agent Session projection: {error}"));
    }

    check_projection(
        Path::new("capability.json"),
        ProjectionLanguage::RustRuntime,
        Path::new("src/generated.rs"),
    )
    .unwrap_or_else(|error| panic!("Agent Session generated artifacts are stale: {error}"));
}
