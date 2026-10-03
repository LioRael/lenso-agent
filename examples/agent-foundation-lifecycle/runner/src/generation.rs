//! Canonical provenance for this executable's one immutable native Generation.
//!
//! This closes the public resolver's Plan over the actual executable, linked
//! factories, Host Catalog and Plugin Root. It does not claim durable Host
//! supervision, online switching, or a cross-process Generation lease.

use std::{fs, io::Write, path::Path};

use lenso_app_plan::{
    ExecutionClassId, ResolvedAppPlan,
    authoring::{HostCatalog, PluginRootSnapshot},
};
use lenso_native_adapter::NativePluginRegistry;
use lenso_plugin_control_plane::{
    AdapterProfile, CanonicalDocument, EmbeddedPlugin, HostBuildManifest, HostExecutionPolicy,
    PlanGenerationInput, ResolvedGeneration, resolve_plan_generation, sha256_digest,
};
use lenso_runtime_codec::InstanceResourceCatalog;
use serde_json::{Value, json};

const APP_ID: &str = "example.agent-foundation-lifecycle";

pub fn resolve(
    home: &Path,
    catalog: &HostCatalog,
    root: &PluginRootSnapshot,
    plan: &ResolvedAppPlan,
) -> ResolvedGeneration {
    plan.validate().expect("resolved fixture Plan is valid");
    let native = ExecutionClassId::native_rust();
    assert!(
        plan.plugin_instances()
            .iter()
            .all(|instance| { instance.execution_class() == &native }),
        "this fixture only admits linked native Plugins"
    );

    let registry = NativePluginRegistry::new().with_linked_factories();
    let mut embedded_plugins = registry
        .factories()
        .map(|factory| EmbeddedPlugin {
            package_id: factory.package_id().to_owned(),
            factory_identity: factory.factory_identity(),
            execution_class: native.as_str().to_owned(),
        })
        .collect::<Vec<_>>();
    embedded_plugins.sort_by(|left, right| {
        (&left.package_id, &left.factory_identity)
            .cmp(&(&right.package_id, &right.factory_identity))
    });
    for instance in plan.plugin_instances() {
        assert_eq!(
            registry
                .factories()
                .filter(|factory| {
                    factory.package_id() == instance.package_id()
                        && factory.runtime_profile() == instance.runtime_profile()
                        && (factory.package_version() == instance.package_revision()
                            || factory.factory_identity() == instance.package_revision())
                })
                .count(),
            1,
            "selected Plugin has exactly one linked factory identity"
        );
    }
    let mut profiles = registry
        .factories()
        .map(|factory| factory.runtime_profile().to_owned())
        .collect::<Vec<_>>();
    profiles.sort();
    profiles.dedup();

    let executable = std::env::current_exe().expect("locate the running fixture executable");
    let executable_digest =
        sha256_digest(&fs::read(executable).expect("hash the fixture executable"));
    let target = format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS);
    let host_build = CanonicalDocument::from_value(
        "lenso-host-build.json",
        HostBuildManifest {
            schema_version: 1,
            app_id: APP_ID.into(),
            host_executable_digest: executable_digest.clone(),
            target: target.clone(),
            embedded_plugins,
            adapter_profiles: vec![AdapterProfile {
                execution_class: native.as_str().to_owned(),
                // The statically linked Adapter's exact build is closed by these executable bytes.
                adapter_build_identity: executable_digest,
                targets: vec![target.clone()],
                profiles,
            }],
        },
    )
    .expect("canonical Host Build identity");
    let policy = CanonicalDocument::from_value(
        "lenso-host-execution-policy.json",
        HostExecutionPolicy {
            schema_version: 1,
            app_id: APP_ID.into(),
            host_build_manifest_digest: host_build.digest().to_owned(),
            target,
            preference: vec![native.as_str().to_owned()],
        },
    )
    .expect("canonical native-only execution policy");

    // Catalogs contain JSON Schemas, whose numbers need not satisfy the stricter
    // authority-document integer profile. Canonicalize their JSON separately,
    // then close only their content digests into the authority document.
    let catalog_bytes = canonical_json(serde_json::to_value(catalog).unwrap());
    let root_bytes = canonical_json(serde_json::to_value(root).unwrap());
    let authority = CanonicalDocument::from_value(
        "fixture-resolution-authority.json",
        json!({
            "schema_version": 1,
            "app_id": APP_ID,
            "host_catalog_digest": sha256_digest(&catalog_bytes),
            "plugin_root_digest": sha256_digest(&root_bytes),
        }),
    )
    .expect("canonical Host Catalog and Plugin Root authority");
    let generation = resolve_plan_generation(PlanGenerationInput {
        app_id: APP_ID,
        authority_digest: authority.digest(),
        plan,
        host_build: &host_build,
        policy: &policy,
        artifacts: Vec::new(),
        resources: InstanceResourceCatalog::new(),
    })
    .expect("public canonical Generation resolution");
    assert!(
        generation.grants.value().grants.is_empty(),
        "no synthetic grants are issued"
    );

    let evidence = home.join("generation-evidence");
    fs::create_dir_all(&evidence).expect("create Generation evidence directory");
    for bytes in [
        catalog_bytes.as_slice(),
        root_bytes.as_slice(),
        authority.bytes(),
        host_build.bytes(),
        policy.bytes(),
        generation.artifact_set.bytes(),
        generation.grants.bytes(),
        generation.spec.bytes(),
        &serde_json::to_vec(plan).expect("serialize exact resolved Plan"),
    ] {
        persist_document(&evidence, bytes);
    }
    generation
}

fn canonical_json(value: Value) -> Vec<u8> {
    fn sorted(value: Value) -> Value {
        match value {
            Value::Object(values) => {
                let mut fields = values.into_iter().collect::<Vec<_>>();
                fields.sort_by(|left, right| left.0.cmp(&right.0));
                Value::Object(
                    fields
                        .into_iter()
                        .map(|(key, value)| (key, sorted(value)))
                        .collect(),
                )
            }
            Value::Array(values) => Value::Array(values.into_iter().map(sorted).collect()),
            value => value,
        }
    }
    serde_json::to_vec(&sorted(value)).expect("serialize canonical fixture input")
}

fn persist_document(directory: &Path, bytes: &[u8]) {
    let digest = sha256_digest(bytes);
    let path = directory.join(format!("{}.json", digest.strip_prefix("sha256:").unwrap()));
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => {
            file.write_all(bytes).expect("write canonical evidence");
            file.sync_all().expect("sync canonical evidence");
            fs::File::open(directory)
                .and_then(|dir| dir.sync_all())
                .expect("sync evidence directory");
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            assert!(
                fs::symlink_metadata(&path).unwrap().file_type().is_file(),
                "existing Generation evidence must be a regular file"
            );
            assert_eq!(
                fs::read(path).unwrap(),
                bytes,
                "canonical evidence must match its digest"
            );
        }
        Err(error) => panic!("cannot persist Generation evidence: {error}"),
    }
}
