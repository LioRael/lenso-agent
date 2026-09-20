#[allow(dead_code)]
#[path = "src/contract.rs"]
mod contract;
fn main() {
    for path in [
        "src/contract.rs",
        "capability.json",
        "schemas",
        "src/generated.rs",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    lenso_contract_codegen::check_source_snapshot(
        &contract::__lenso_capability_snapshot(),
        std::path::Path::new("capability.json"),
    )
    .expect("run lenso app build/dev to synchronize the contract");
    lenso_contract_codegen::check_projection(
        std::path::Path::new("capability.json"),
        lenso_contract_codegen::ProjectionLanguage::RustRuntime,
        std::path::Path::new("src/generated.rs"),
    )
    .expect("stale generated projection");
}
