use std::path::Path;

use lenso_contract_codegen::{ProjectionLanguage, check_projection};

fn main() {
    println!("cargo:rerun-if-changed=capability.json");
    println!("cargo:rerun-if-changed=schemas");
    println!("cargo:rerun-if-changed=src/generated.rs");
    check_projection(
        Path::new("capability.json"),
        ProjectionLanguage::Rust,
        Path::new("src/generated.rs"),
    )
    .unwrap_or_else(|error| panic!("Process generated artifacts are stale: {error}"));
}
