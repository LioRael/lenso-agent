// Adapted from lenso-wasm-component-adapter 0.2.12 integration tests (MIT OR Apache-2.0).
use std::{
    any::Any,
    process::Command,
    rc::Rc,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures::FutureExt;
use lenso_app_plan::{
    AppComposition, CapabilityBinding, CapabilityEndpointPlan, CapabilityRequirementPlan,
    ExecutionClassId, PluginInstancePlan, ResolvedAppPlan,
};
use lenso_kernel::{
    CancellationToken, DeterministicDriver, ExecutionAdapter, ExecutionAdapterCatalog,
    InvocationContext, Kernel, NativeEventEndpoint, NativeStreamItem, NoopPluginLifecycle,
    PluginDependencyHandle, PluginEventDependencyHandle, RequestCapability, RuntimeFailure,
};
use lenso_plugin_bundle::{
    SourcePluginBuild, build_source_plugin_bundle, extract_plugin_descriptor, sha256_digest,
    verify_bundle_directory,
};
use lenso_runtime_codec::{
    ArtifactCatalog, ArtifactHandle, JsonCapabilityCodec, JsonHostRequestFuture,
    JsonInvocationOutcome,
};
use lenso_runtime_conformance::{
    ConformanceExecutionAdapter, ConformancePlugin, ConformancePluginFactory, PROBE_CAPABILITY_ID,
    PROBE_DESCRIPTOR_VERSION, PROBE_OPERATION, PROBE_PROVIDER_PACKAGE_ID, Probe, ProbeError,
    ProbeProviderFactory, ProbeRequest,
};
use lenso_wasm_component_adapter::{EXECUTION_CLASS, WasmComponentAdapter, WasmComponentLimits};
use serde_json::Value;
use sha2::{Digest, Sha256};

static RUST_GUEST: OnceLock<Vec<u8>> = OnceLock::new();
static RUST_HOST_IMPORT_GUEST: OnceLock<Vec<u8>> = OnceLock::new();
static RUST_STREAM_GUEST: OnceLock<Vec<u8>> = OnceLock::new();
static FIXTURE_TARGET: OnceLock<tempfile::TempDir> = OnceLock::new();

const NOTIFICATIONS_CAPABILITY_ID: &str = "test.notifications@1";
const NOTIFICATIONS_VERSION: &str = "1.0.0";

fn fixture_target() -> &'static std::path::Path {
    FIXTURE_TARGET
        .get_or_init(|| tempfile::tempdir().unwrap())
        .path()
}

fn wasm_fixture(
    slot: &'static OnceLock<Vec<u8>>,
    manifest: &'static str,
    artifact: &'static str,
) -> &'static [u8] {
    slot.get_or_init(|| {
        let status = Command::new(env!("CARGO"))
            .args([
                "build",
                "--locked",
                "--release",
                "--target",
                "wasm32-unknown-unknown",
                "--manifest-path",
                manifest,
                "--target-dir",
            ])
            .arg(fixture_target())
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::read(
            fixture_target()
                .join("wasm32-unknown-unknown/release")
                .join(artifact),
        )
        .unwrap()
    })
}

fn rust_guest() -> &'static [u8] {
    wasm_fixture(
        &RUST_GUEST,
        "tests/fixtures/rust-guest/Cargo.toml",
        "lenso_wasm_test_guest.wasm",
    )
}

fn rust_host_import_guest() -> &'static [u8] {
    wasm_fixture(
        &RUST_HOST_IMPORT_GUEST,
        "tests/fixtures/rust-host-import-guest/Cargo.toml",
        "lenso_wasm_host_import_test_guest.wasm",
    )
}

fn rust_stream_guest() -> &'static [u8] {
    wasm_fixture(
        &RUST_STREAM_GUEST,
        "tests/fixtures/rust-stream-guest/Cargo.toml",
        "lenso_wasm_stream_test_guest.wasm",
    )
}

#[test]
fn source_descriptor_survives_component_encoding_without_execution() {
    let component = wit_component::ComponentEncoder::default()
        .module(rust_guest())
        .unwrap()
        .validate(true)
        .encode()
        .unwrap();

    assert_eq!(
        extract_plugin_descriptor(&component).unwrap(),
        br#"{"abi":"lenso.json-request@1","capabilities":[{"capability_id":"test.echo@1","descriptor_version":"1.0.0","request_operations":["echo","fail","trap","loop"]}]}"#
    );
}

#[test]
fn source_bundle_builds_one_v2_entry_without_a_manifest_template() {
    let fixture_target = tempfile::tempdir().unwrap();
    let wasm_module = fixture_target.path().join("lenso_wasm_test_guest.wasm");
    std::fs::write(&wasm_module, rust_guest()).unwrap();
    let bundle_root = fixture_target.path().join("bundle");
    let built = build_source_plugin_bundle(&SourcePluginBuild {
        package_manifest: "tests/fixtures/rust-guest/Cargo.toml".into(),
        wasm_module,
        output: bundle_root.clone(),
    })
    .unwrap();
    let component = std::fs::read(bundle_root.join("plugin.wasm")).unwrap();
    let manifest = std::fs::read_to_string(bundle_root.join("lenso-plugin.json")).unwrap();
    let parsed: Value = serde_json::from_str(&manifest).unwrap();
    assert_eq!(parsed["schema_version"], 2);
    assert_eq!(parsed["plugin_id"], "test.echo");
    assert_eq!(parsed["artifact"]["digest"], sha256_digest(&component));
    assert_eq!(parsed["artifact"]["size"], component.len());
    assert_eq!(parsed["entry"]["descriptor"]["plugin_id"], "test.echo");
    assert_eq!(
        parsed["entry"]["descriptor"]["runtime_package_revision"],
        sha256_digest(&component)
    );
    assert_eq!(
        parsed["entry"]["descriptor"]["provided_capabilities"][0]["operations"],
        serde_json::json!(["echo", "fail", "trap", "loop"])
    );
    assert_eq!(built, verify_bundle_directory(&bundle_root).unwrap());
    assert!(!manifest.contains("plugin_contributions"));

    let mut conflicting: Value = serde_json::from_str(&manifest).unwrap();
    conflicting["plugin_contributions"] = serde_json::json!([]);
    std::fs::write(
        bundle_root.join("lenso-plugin.json"),
        serde_json::to_vec(&conflicting).unwrap(),
    )
    .unwrap();
    assert!(verify_bundle_directory(&bundle_root).is_err());

    let mut mismatched: Value = serde_json::from_str(&manifest).unwrap();
    mismatched["entry"]["descriptor"]["provided_capabilities"][0]["operations"] =
        serde_json::json!(["different"]);
    std::fs::write(
        bundle_root.join("lenso-plugin.json"),
        serde_json::to_vec(&mismatched).unwrap(),
    )
    .unwrap();
    assert!(verify_bundle_directory(&bundle_root).is_err());

    std::fs::write(bundle_root.join("lenso-plugin.json"), &manifest).unwrap();
    std::fs::write(bundle_root.join("plugin.wasm"), b"tampered").unwrap();
    assert!(verify_bundle_directory(&bundle_root).is_err());
}

#[derive(Debug)]
struct EchoCodec;

#[derive(Debug)]
struct EchoCapability;

impl RequestCapability for EchoCapability {
    type Request = u64;
    type Response = u64;
    type DomainError = String;

    const ID: &'static str = "test.echo@1";
    const DESCRIPTOR_VERSION: &'static str = "1.0.0";
}

#[derive(Debug)]
struct ProbeCodec;

impl JsonCapabilityCodec for ProbeCodec {
    fn capability_id(&self) -> &'static str {
        PROBE_CAPABILITY_ID
    }

    fn descriptor_version(&self) -> &'static str {
        PROBE_DESCRIPTOR_VERSION
    }

    fn request_operations(&self) -> &'static [&'static str] {
        &[PROBE_OPERATION]
    }

    fn encode_request(&self, _: &str, _: &dyn Any) -> Result<Value, RuntimeFailure> {
        Err(RuntimeFailure::ProtocolViolation {
            capability: PROBE_CAPABILITY_ID,
        })
    }

    fn decode_response(&self, _: &str, _: Value) -> Result<Box<dyn Any>, RuntimeFailure> {
        Err(RuntimeFailure::ProtocolViolation {
            capability: PROBE_CAPABILITY_ID,
        })
    }

    fn decode_domain_error(&self, _: &str, _: Value) -> Result<Box<dyn Any>, RuntimeFailure> {
        Err(RuntimeFailure::ProtocolViolation {
            capability: PROBE_CAPABILITY_ID,
        })
    }

    fn invoke_host_request(
        &self,
        dependency: PluginDependencyHandle,
        operation: String,
        request: Value,
        context: InvocationContext,
    ) -> JsonHostRequestFuture {
        Box::pin(async move {
            if operation != PROBE_OPERATION {
                return Err(RuntimeFailure::UnknownOperation {
                    capability: PROBE_CAPABILITY_ID,
                    operation,
                });
            }
            let value = request
                .get("value")
                .and_then(Value::as_str)
                .ok_or(RuntimeFailure::ProtocolViolation {
                    capability: PROBE_CAPABILITY_ID,
                })?
                .to_owned();
            match dependency
                .typed::<Probe>()?
                .invoke_with_context(PROBE_OPERATION, context, ProbeRequest { value })
                .await?
            {
                Ok(response) => Ok(JsonInvocationOutcome::Success(
                    serde_json::json!({ "value": response.value }),
                )),
                Err(ProbeError::EmptyValue) => Ok(JsonInvocationOutcome::DomainError(
                    serde_json::json!({ "kind": "empty_value" }),
                )),
            }
        })
    }
}

#[derive(Debug)]
struct NotificationsCodec;

impl JsonCapabilityCodec for NotificationsCodec {
    fn capability_id(&self) -> &'static str {
        NOTIFICATIONS_CAPABILITY_ID
    }

    fn descriptor_version(&self) -> &'static str {
        NOTIFICATIONS_VERSION
    }

    fn request_operations(&self) -> &'static [&'static str] {
        &[]
    }

    fn event_operations(&self) -> &'static [&'static str] {
        &["notify"]
    }

    fn encode_request(&self, operation: &str, _: &dyn Any) -> Result<Value, RuntimeFailure> {
        Err(RuntimeFailure::UnknownOperation {
            capability: NOTIFICATIONS_CAPABILITY_ID,
            operation: operation.to_owned(),
        })
    }

    fn decode_response(&self, operation: &str, _: Value) -> Result<Box<dyn Any>, RuntimeFailure> {
        Err(RuntimeFailure::UnknownOperation {
            capability: NOTIFICATIONS_CAPABILITY_ID,
            operation: operation.to_owned(),
        })
    }

    fn decode_domain_error(
        &self,
        operation: &str,
        _: Value,
    ) -> Result<Box<dyn Any>, RuntimeFailure> {
        Err(RuntimeFailure::UnknownOperation {
            capability: NOTIFICATIONS_CAPABILITY_ID,
            operation: operation.to_owned(),
        })
    }

    fn publish_host_event(
        &self,
        dependency: PluginEventDependencyHandle,
        operation: String,
        event: Value,
        context: InvocationContext,
    ) -> futures::future::LocalBoxFuture<'static, Result<(), RuntimeFailure>> {
        Box::pin(async move {
            let results = dependency
                .typed::<Notifications>()?
                .publish_with_context(&operation, context, event)
                .await;
            let [result] = results.as_slice() else {
                return Err(RuntimeFailure::ProtocolViolation {
                    capability: NOTIFICATIONS_CAPABILITY_ID,
                });
            };
            match result.admission() {
                lenso_kernel::EventAdmission::Accepted => Ok(()),
                lenso_kernel::EventAdmission::Unavailable => Err(RuntimeFailure::Unavailable {
                    capability: NOTIFICATIONS_CAPABILITY_ID,
                }),
                lenso_kernel::EventAdmission::Exhausted => Err(RuntimeFailure::ResourceExhausted {
                    capability: NOTIFICATIONS_CAPABILITY_ID,
                    operation,
                }),
            }
        })
    }
}

#[derive(Debug)]
struct Notifications;

impl lenso_kernel::EventCapability for Notifications {
    type Event = Value;

    const ID: &'static str = NOTIFICATIONS_CAPABILITY_ID;
    const DESCRIPTOR_VERSION: &'static str = NOTIFICATIONS_VERSION;
}

#[derive(Debug)]
struct NotificationsEndpoint {
    publications: Arc<AtomicUsize>,
}

impl NativeEventEndpoint for NotificationsEndpoint {
    fn capability_id(&self) -> &'static str {
        NOTIFICATIONS_CAPABILITY_ID
    }

    fn descriptor_version(&self) -> &'static str {
        NOTIFICATIONS_VERSION
    }

    fn operations(&self) -> &'static [&'static str] {
        &["notify"]
    }

    fn publish(
        &self,
        operation: &str,
        event: Box<dyn Any>,
        _: InvocationContext,
    ) -> futures::future::LocalBoxFuture<'static, Result<(), RuntimeFailure>> {
        let result = if operation != "notify" || event.downcast::<Value>().is_err() {
            Err(RuntimeFailure::ProtocolViolation {
                capability: NOTIFICATIONS_CAPABILITY_ID,
            })
        } else {
            self.publications.fetch_add(1, Ordering::Relaxed);
            Ok(())
        };
        Box::pin(futures::future::ready(result))
    }
}

#[derive(Debug)]
struct NotificationsFactory {
    publications: Arc<AtomicUsize>,
}

impl ConformancePluginFactory for NotificationsFactory {
    fn package_id(&self) -> &'static str {
        "test.notifications-provider"
    }

    fn instantiate(&self, _: &PluginInstancePlan) -> Result<ConformancePlugin, RuntimeFailure> {
        Ok(ConformancePlugin::with_event_endpoints(
            vec![Rc::new(NotificationsEndpoint {
                publications: self.publications.clone(),
            })],
            NoopPluginLifecycle,
        ))
    }
}

#[derive(Debug)]
struct EmptyConsumerFactory;

impl ConformancePluginFactory for EmptyConsumerFactory {
    fn package_id(&self) -> &'static str {
        "test.echo-consumer"
    }

    fn instantiate(&self, _: &PluginInstancePlan) -> Result<ConformancePlugin, RuntimeFailure> {
        Ok(ConformancePlugin::default())
    }
}

impl JsonCapabilityCodec for EchoCodec {
    fn capability_id(&self) -> &'static str {
        "test.echo@1"
    }
    fn descriptor_version(&self) -> &'static str {
        "1.0.0"
    }
    fn request_operations(&self) -> &'static [&'static str] {
        &["echo", "fail", "trap", "loop"]
    }
    fn encode_request(&self, _operation: &str, request: &dyn Any) -> Result<Value, RuntimeFailure> {
        request
            .downcast_ref::<u64>()
            .copied()
            .map(Value::from)
            .ok_or(RuntimeFailure::ProtocolViolation {
                capability: self.capability_id(),
            })
    }
    fn decode_response(
        &self,
        _operation: &str,
        value: Value,
    ) -> Result<Box<dyn Any>, RuntimeFailure> {
        value
            .as_u64()
            .map(|value| Box::new(value) as Box<dyn Any>)
            .ok_or(RuntimeFailure::ProtocolViolation {
                capability: self.capability_id(),
            })
    }
    fn decode_domain_error(
        &self,
        _operation: &str,
        value: Value,
    ) -> Result<Box<dyn Any>, RuntimeFailure> {
        value
            .as_str()
            .map(|value| Box::new(value.to_owned()) as Box<dyn Any>)
            .ok_or(RuntimeFailure::ProtocolViolation {
                capability: self.capability_id(),
            })
    }
}

#[derive(Debug)]
struct NarrowCodec;

impl JsonCapabilityCodec for NarrowCodec {
    fn capability_id(&self) -> &'static str {
        "test.echo@1"
    }
    fn descriptor_version(&self) -> &'static str {
        "1.0.0"
    }
    fn request_operations(&self) -> &'static [&'static str] {
        &["echo"]
    }
    fn encode_request(&self, operation: &str, request: &dyn Any) -> Result<Value, RuntimeFailure> {
        EchoCodec.encode_request(operation, request)
    }
    fn decode_response(
        &self,
        operation: &str,
        value: Value,
    ) -> Result<Box<dyn Any>, RuntimeFailure> {
        EchoCodec.decode_response(operation, value)
    }
    fn decode_domain_error(
        &self,
        operation: &str,
        value: Value,
    ) -> Result<Box<dyn Any>, RuntimeFailure> {
        EchoCodec.decode_domain_error(operation, value)
    }
}

#[derive(Debug)]
struct ChatCodec;

impl JsonCapabilityCodec for ChatCodec {
    fn capability_id(&self) -> &'static str {
        "test.chat@1"
    }
    fn descriptor_version(&self) -> &'static str {
        "1.0.0"
    }
    fn request_operations(&self) -> &'static [&'static str] {
        &[]
    }
    fn stream_operations(&self) -> &'static [&'static str] {
        &["chat"]
    }
    fn encode_request(&self, operation: &str, _: &dyn Any) -> Result<Value, RuntimeFailure> {
        Err(RuntimeFailure::UnknownOperation {
            capability: self.capability_id(),
            operation: operation.to_owned(),
        })
    }
    fn decode_response(&self, operation: &str, _: Value) -> Result<Box<dyn Any>, RuntimeFailure> {
        Err(RuntimeFailure::UnknownOperation {
            capability: self.capability_id(),
            operation: operation.to_owned(),
        })
    }
    fn decode_domain_error(
        &self,
        operation: &str,
        _: Value,
    ) -> Result<Box<dyn Any>, RuntimeFailure> {
        Err(RuntimeFailure::UnknownOperation {
            capability: self.capability_id(),
            operation: operation.to_owned(),
        })
    }
    fn encode_stream_open(&self, _: &str, request: &dyn Any) -> Result<Value, RuntimeFailure> {
        request
            .downcast_ref::<u64>()
            .copied()
            .map(Value::from)
            .ok_or(RuntimeFailure::ProtocolViolation {
                capability: self.capability_id(),
            })
    }
    fn encode_stream_message(&self, _: &str, message: &dyn Any) -> Result<Value, RuntimeFailure> {
        message
            .downcast_ref::<String>()
            .cloned()
            .map(Value::from)
            .ok_or(RuntimeFailure::ProtocolViolation {
                capability: self.capability_id(),
            })
    }
    fn decode_stream_message(&self, _: &str, value: Value) -> Result<Box<dyn Any>, RuntimeFailure> {
        value
            .as_str()
            .map(|value| Box::new(value.to_owned()) as Box<dyn Any>)
            .ok_or(RuntimeFailure::ProtocolViolation {
                capability: self.capability_id(),
            })
    }
    fn decode_stream_domain_error(
        &self,
        _: &str,
        value: Value,
    ) -> Result<Box<dyn Any>, RuntimeFailure> {
        value
            .as_str()
            .map(|value| Box::new(value.to_owned()) as Box<dyn Any>)
            .ok_or(RuntimeFailure::ProtocolViolation {
                capability: self.capability_id(),
            })
    }
}

#[test]
fn real_component_import_invokes_only_the_plan_bound_host_capability() {
    assert!(!std::fs::read("/etc/hosts").unwrap().is_empty());
    assert!(std::env::var("PATH").is_ok());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let _host_connection = std::net::TcpStream::connect(address).unwrap();
    let port = u64::from(address.port());
    let component = wit_component::ComponentEncoder::default()
        .module(rust_host_import_guest())
        .unwrap()
        .validate(true)
        .encode()
        .unwrap();
    let artifact_file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(artifact_file.path(), &component).unwrap();
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(&component)));
    let artifact =
        ArtifactHandle::open(artifact_file.path(), &digest, component.len() as u64).unwrap();
    let publications = Arc::new(AtomicUsize::new(0));
    let wasm = WasmComponentAdapter::new(
        ArtifactCatalog::new()
            .with_artifact("plugin", artifact)
            .unwrap(),
    )
    .with_codec(NarrowCodec)
    .with_codec(ProbeCodec)
    .with_codec(NotificationsCodec);
    let adapters = ExecutionAdapterCatalog::new()
        .with_adapter(
            ConformanceExecutionAdapter::new()
                .with_factory(ProbeProviderFactory)
                .with_factory(NotificationsFactory {
                    publications: publications.clone(),
                })
                .with_factory(EmptyConsumerFactory),
        )
        .unwrap()
        .with_adapter(wasm)
        .unwrap();
    let driver = DeterministicDriver::new();
    let app = driver
        .run(Kernel::start(
            wasm_guest_import_plan(),
            driver.clone(),
            adapters,
        ))
        .expect("the Wasm Guest Import App should activate");
    let result = driver
        .run(
            app.handle::<EchoCapability>("consumer")
                .unwrap()
                .invoke("echo", port),
        )
        .expect("the Wasm guest should reach the host")
        .expect("the host should not return a Domain Error");
    assert_eq!(result, port);
    assert_eq!(publications.load(Ordering::Relaxed), 1);
}

#[test]
fn real_component_stream_preserves_messages_half_close_terminal_and_open_error() {
    let component = wit_component::ComponentEncoder::default()
        .module(rust_stream_guest())
        .unwrap()
        .validate(true)
        .encode()
        .unwrap();
    let artifact_file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(artifact_file.path(), &component).unwrap();
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(&component)));
    let artifact =
        ArtifactHandle::open(artifact_file.path(), &digest, component.len() as u64).unwrap();
    let adapter = WasmComponentAdapter::new(
        ArtifactCatalog::new()
            .with_artifact("plugin", artifact)
            .unwrap(),
    )
    .with_codec(ChatCodec);
    let plan = stream_plan();
    let generation = adapter.recreate(&plan, "plugin").unwrap();
    assert!(generation.endpoints().is_empty());
    let endpoint = generation.stream_endpoints()[0].clone();
    let context = InvocationContext::new(10, None, CancellationToken::new());
    let Ok(Ok(session)) =
        futures::executor::block_on(endpoint.open("chat", Box::new(7_u64), context))
    else {
        panic!("stream did not open")
    };
    futures::executor::block_on(session.send(Box::new("hello".to_owned()))).unwrap();
    let NativeStreamItem::Message(message) =
        futures::executor::block_on(session.receive()).unwrap()
    else {
        panic!("message missing")
    };
    assert_eq!(*message.downcast::<String>().unwrap(), "hello");
    futures::executor::block_on(session.close_send()).unwrap();
    assert!(matches!(
        futures::executor::block_on(session.receive()).unwrap(),
        NativeStreamItem::PeerHalfClosed
    ));
    assert!(matches!(
        futures::executor::block_on(session.receive()).unwrap(),
        NativeStreamItem::Terminal(Ok(()))
    ));

    let context = InvocationContext::new(11, None, CancellationToken::new());
    let Ok(Err(error)) =
        futures::executor::block_on(endpoint.open("chat", Box::new(0_u64), context))
    else {
        panic!("open error missing")
    };
    assert_eq!(*error.downcast::<String>().unwrap(), "rejected");
}

#[test]
fn dependency_free_authoring_v2_uses_the_same_wasm_request_profile() {
    let component = wit_component::ComponentEncoder::default()
        .module(rust_guest())
        .unwrap()
        .validate(true)
        .encode()
        .unwrap();
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), &component).unwrap();
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(&component)));
    let artifact = ArtifactHandle::open(file.path(), &digest, component.len() as u64).unwrap();
    let adapter = WasmComponentAdapter::new(
        ArtifactCatalog::new()
            .with_artifact("plugin", artifact)
            .unwrap(),
    )
    .with_codec(EchoCodec);
    assert!(adapter.supports_runtime_profile(2, EXECUTION_CLASS));
    assert!(!adapter.supports_runtime_profile(3, EXECUTION_CLASS));
    let instance = plan().plugin_instances()[0]
        .clone()
        .with_authoring(2, EXECUTION_CLASS);
    let modern = ResolvedAppPlan::new(vec![instance.clone()], Vec::new());
    let generation = adapter.recreate(&modern, "plugin").unwrap();
    let context = InvocationContext::new(1, None, CancellationToken::new());
    let response = futures::executor::block_on(generation.endpoints()[0].invoke(
        "echo",
        Box::new(7_u64),
        context,
    ));
    assert!(matches!(response, Ok(Ok(_))));
    let dependent = instance.with_requirement(
        CapabilityRequirementPlan::one("test.store@1", "1.0.0").with_requirement_id("store"),
    );
    let dependent = ResolvedAppPlan::new(vec![dependent], Vec::new());
    assert!(adapter.recreate(&dependent, "plugin").is_err());
}

#[test]
fn real_component_runs_without_wasi_and_retires_on_trap_or_cancellation() {
    let component = wit_component::ComponentEncoder::default()
        .module(rust_guest())
        .unwrap()
        .validate(true)
        .encode()
        .unwrap();
    let artifact_file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(artifact_file.path(), &component).unwrap();
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(&component)));
    let artifact =
        ArtifactHandle::open(artifact_file.path(), &digest, component.len() as u64).unwrap();
    let artifacts = ArtifactCatalog::new()
        .with_artifact("plugin", artifact)
        .unwrap();
    let mismatch = WasmComponentAdapter::new(artifacts.clone()).with_codec(NarrowCodec);
    assert!(mismatch.recreate(&narrow_plan(), "plugin").is_err());
    let duplicate = WasmComponentAdapter::new(artifacts.clone())
        .with_codec(EchoCodec)
        .with_codec(EchoCodec);
    assert!(duplicate.recreate(&plan(), "plugin").is_err());

    let adapter = WasmComponentAdapter::new(artifacts)
        .with_codec(EchoCodec)
        .with_limits(WasmComponentLimits {
            max_turn: Duration::from_millis(50),
            ..WasmComponentLimits::default()
        });
    let plan = plan();
    let generation = adapter.recreate(&plan, "plugin").unwrap();
    let endpoint = generation.endpoints()[0].clone();

    let context = InvocationContext::new(1, None, CancellationToken::new());
    let Ok(Ok(response)) =
        futures::executor::block_on(endpoint.invoke("echo", Box::new(9_u64), context))
    else {
        panic!("Component echo did not succeed");
    };
    assert_eq!(*response.downcast::<u64>().unwrap(), 9);

    let context = InvocationContext::new(2, None, CancellationToken::new());
    let Ok(Err(error)) =
        futures::executor::block_on(endpoint.invoke("fail", Box::new(0_u64), context))
    else {
        panic!("Component fail did not return a Domain Error");
    };
    assert_eq!(*error.downcast::<String>().unwrap(), "declared");

    let cancellation = CancellationToken::new();
    let cancel = cancellation.clone();
    let invocation = endpoint
        .invoke(
            "loop",
            Box::new(0_u64),
            InvocationContext::new(3, None, cancellation),
        )
        .fuse();
    let cancellation = async move {
        futures::future::ready(()).await;
        cancel.cancel();
    };
    let (failure, ()) =
        futures::executor::block_on(async { futures::join!(invocation, cancellation) });
    assert!(matches!(
        failure,
        Err(RuntimeFailure::Cancelled { request_id: 3 })
    ));
    assert!(adapter.recreate(&plan, "plugin").is_ok());
}

fn narrow_plan() -> ResolvedAppPlan {
    ResolvedAppPlan::new(
        vec![
            PluginInstancePlan::new("plugin", "test.component")
                .with_entrypoint("plugin")
                .with_execution_class(ExecutionClassId::new(EXECUTION_CLASS))
                .with_capability(CapabilityEndpointPlan::new(
                    "test.echo@1",
                    "1.0.0",
                    ["echo"],
                )),
        ],
        Vec::new(),
    )
}

fn plan() -> ResolvedAppPlan {
    ResolvedAppPlan::new(
        vec![
            PluginInstancePlan::new("plugin", "test.component")
                .with_entrypoint("plugin")
                .with_execution_class(ExecutionClassId::new(EXECUTION_CLASS))
                .with_capability(CapabilityEndpointPlan::new(
                    "test.echo@1",
                    "1.0.0",
                    ["echo", "fail", "trap", "loop"],
                )),
        ],
        Vec::new(),
    )
}

fn stream_plan() -> ResolvedAppPlan {
    ResolvedAppPlan::new(
        vec![
            PluginInstancePlan::new("plugin", "test.component")
                .with_entrypoint("plugin")
                .with_execution_class(ExecutionClassId::new(EXECUTION_CLASS))
                .with_capability(
                    CapabilityEndpointPlan::new("test.chat@1", "1.0.0", ["chat"])
                        .with_stream_operation("chat"),
                ),
        ],
        Vec::new(),
    )
}

fn wasm_guest_import_plan() -> ResolvedAppPlan {
    AppComposition::new(
        vec![
            PluginInstancePlan::new("provider", PROBE_PROVIDER_PACKAGE_ID).with_capability(
                CapabilityEndpointPlan::new(
                    PROBE_CAPABILITY_ID,
                    PROBE_DESCRIPTOR_VERSION,
                    [PROBE_OPERATION],
                ),
            ),
            PluginInstancePlan::new("~test.notifications@1", "test.notifications-provider")
                .with_capability(
                    CapabilityEndpointPlan::new(
                        NOTIFICATIONS_CAPABILITY_ID,
                        NOTIFICATIONS_VERSION,
                        ["notify"],
                    )
                    .with_event_operation("notify")
                    .with_event_capacity(4)
                    .with_limits(0, 4),
                ),
            PluginInstancePlan::new("plugin", "test.component")
                .with_entrypoint("plugin")
                .with_execution_class(ExecutionClassId::new(EXECUTION_CLASS))
                .with_requirement(CapabilityRequirementPlan::one(
                    PROBE_CAPABILITY_ID,
                    PROBE_DESCRIPTOR_VERSION,
                ))
                .with_requirement(
                    CapabilityRequirementPlan::one(
                        NOTIFICATIONS_CAPABILITY_ID,
                        NOTIFICATIONS_VERSION,
                    )
                    .with_requirement_id("~test.notifications@1"),
                )
                .with_capability(CapabilityEndpointPlan::new(
                    EchoCapability::ID,
                    EchoCapability::DESCRIPTOR_VERSION,
                    ["echo"],
                )),
            PluginInstancePlan::new("consumer", "test.echo-consumer").with_requirement(
                CapabilityRequirementPlan::one(
                    EchoCapability::ID,
                    EchoCapability::DESCRIPTOR_VERSION,
                ),
            ),
        ],
        vec![
            CapabilityBinding::new(
                "plugin",
                PROBE_CAPABILITY_ID,
                PROBE_DESCRIPTOR_VERSION,
                "provider",
            ),
            CapabilityBinding::new(
                "plugin",
                NOTIFICATIONS_CAPABILITY_ID,
                NOTIFICATIONS_VERSION,
                "~test.notifications@1",
            )
            .with_requirement_id("~test.notifications@1"),
            CapabilityBinding::new(
                "consumer",
                EchoCapability::ID,
                EchoCapability::DESCRIPTOR_VERSION,
                "plugin",
            ),
        ],
    )
    .resolve()
    .expect("the Wasm Guest Import fixture should resolve")
}

#[test]
fn unsupported_v2_imports_fail_before_artifact_loading_or_guest_activation() {
    let plan = ResolvedAppPlan::new(
        vec![
            PluginInstancePlan::new("provider", PROBE_PROVIDER_PACKAGE_ID).with_capability(
                CapabilityEndpointPlan::new(
                    PROBE_CAPABILITY_ID,
                    PROBE_DESCRIPTOR_VERSION,
                    [PROBE_OPERATION],
                ),
            ),
            PluginInstancePlan::new("plugin", "test.component")
                .with_entrypoint("plugin")
                .with_execution_class(ExecutionClassId::new(EXECUTION_CLASS))
                .with_authoring(2, EXECUTION_CLASS)
                .with_requirement(
                    CapabilityRequirementPlan::one(PROBE_CAPABILITY_ID, PROBE_DESCRIPTOR_VERSION)
                        .with_requirement_id("probe"),
                )
                .with_capability(CapabilityEndpointPlan::new(
                    EchoCapability::ID,
                    EchoCapability::DESCRIPTOR_VERSION,
                    ["echo"],
                )),
        ],
        vec![
            CapabilityBinding::new(
                "plugin",
                PROBE_CAPABILITY_ID,
                PROBE_DESCRIPTOR_VERSION,
                "provider",
            )
            .with_requirement_id("probe"),
        ],
    );
    plan.validate().expect("complete and valid v2 Plan");
    // No artifact exists: a target-support error must precede even loading one.
    let adapter = WasmComponentAdapter::new(ArtifactCatalog::new());
    match adapter.prepare(&plan) {
        Err(RuntimeFailure::InvalidResolvedPlan { detail }) => assert!(
            detail.contains("authoring v2 currently requires a dependency-free Contract"),
            "unexpected failure: {detail}"
        ),
        Err(other) => panic!("wrong rejection: {other:?}"),
        Ok(_) => panic!("unsupported imports reached preparation"),
    }
}
