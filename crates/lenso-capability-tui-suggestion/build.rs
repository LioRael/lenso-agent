use std::{env, path::Path};

use lenso_contract_codegen::{
    ProjectionLanguage, check_projection, check_source_snapshot, write_projection,
    write_source_snapshot,
};

#[allow(dead_code)]
#[path = "src/contract.rs"]
mod contract_source;

fn main() {
    println!("cargo:rerun-if-changed=capability.json");
    println!("cargo:rerun-if-changed=schemas");
    println!("cargo:rerun-if-changed=src/contract.rs");
    println!("cargo:rerun-if-changed=src/generated.rs");
    println!("cargo:rerun-if-env-changed=LENSO_UPDATE_CONTRACT_SNAPSHOT");

    let snapshot = contract_source::__lenso_capability_snapshot();
    if env::var_os("LENSO_UPDATE_CONTRACT_SNAPSHOT").is_some() {
        write_source_snapshot(&snapshot, Path::new("capability.json"))
            .unwrap_or_else(|error| panic!("failed to update TUI Suggestion snapshot: {error}"));
        write_projection(
            Path::new("capability.json"),
            ProjectionLanguage::RustRuntime,
            Path::new("src/generated.rs"),
        )
        .unwrap_or_else(|error| panic!("failed to update TUI Suggestion projection: {error}"));
    } else {
        check_source_snapshot(&snapshot, Path::new("capability.json"))
            .unwrap_or_else(|error| panic!("TUI Suggestion contract artifacts are stale: {error}"));
    }

    check_projection(
        Path::new("capability.json"),
        ProjectionLanguage::RustRuntime,
        Path::new("src/generated.rs"),
    )
    .unwrap_or_else(|error| panic!("TUI Suggestion generated artifacts are stale: {error}"));
}
