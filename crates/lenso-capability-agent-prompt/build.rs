use std::path::Path;

use lenso_contract_codegen::{ProjectionLanguage, check_projection};

fn main() {
    println!("cargo:rerun-if-changed=capability.json");
    println!("cargo:rerun-if-changed=schemas");
    println!("cargo:rerun-if-changed=src/generated.rs");

    check_projection(
        Path::new("capability.json"),
        ProjectionLanguage::RustRuntime,
        Path::new("src/generated.rs"),
    )
    .unwrap_or_else(|error| panic!("Agent Prompt generated artifacts are stale: {error}"));
}
