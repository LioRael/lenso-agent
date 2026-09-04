use lenso_contract_codegen::{
    ProjectionLanguage, check_projection, check_source_snapshot, write_projection,
    write_source_snapshot,
};
use std::{env, path::Path};

#[allow(dead_code)]
#[path = "src/contract.rs"]
mod contract_source;

fn main() {
    for path in [
        "capability.json",
        "schemas",
        "src/contract.rs",
        "src/generated.rs",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    println!("cargo:rerun-if-env-changed=LENSO_UPDATE_CONTRACT_SNAPSHOT");
    let snapshot = contract_source::__lenso_capability_snapshot();
    if env::var_os("LENSO_UPDATE_CONTRACT_SNAPSHOT").is_some() {
        write_source_snapshot(&snapshot, Path::new("capability.json"))
            .expect("update auth connection snapshot");
        write_projection(
            Path::new("capability.json"),
            ProjectionLanguage::RustRuntime,
            Path::new("src/generated.rs"),
        )
        .expect("generate auth connection projection");
    } else {
        check_source_snapshot(&snapshot, Path::new("capability.json"))
            .expect("auth connection snapshot is stale");
    }
    check_projection(
        Path::new("capability.json"),
        ProjectionLanguage::RustRuntime,
        Path::new("src/generated.rs"),
    )
    .expect("auth connection projection is stale");
}
