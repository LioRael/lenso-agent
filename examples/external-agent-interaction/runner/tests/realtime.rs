use std::time::Duration;

use external_realtime_caller as caller_plugin;
use external_realtime_provider as provider_plugin;
use lenso_app_plan::authoring::{
    HostBinding, HostCatalog, HostSlot, PluginInstanceId, PluginRootInstance, PluginRootSnapshot,
    resolve_plugin_root,
};
use lenso_capability_agent_interaction::{
    self as interaction, ConnectOpen, FrameDirection, FrameKind, InteractionFrame,
};
use lenso_kernel::{CancellationToken, Kernel, RuntimeFailure, ShutdownOutcome, StreamEvent};
use lenso_native_adapter::NativePluginRegistry;
use lenso_runner::TokioDriver;

const CALLER: &str = "example.realtime-caller";
const PROVIDER: &str = "example.realtime-provider";
const CALLER_INSTANCE: &str = "example.realtime-caller/console";
const PROTOCOL: &str = "example.realtime-audio@1";

fn host() -> HostCatalog {
    caller_plugin::link();
    provider_plugin::link();
    NativePluginRegistry::host_catalog(
        [
            HostSlot::one("interaction-callers"),
            HostSlot::one("interaction-providers"),
        ],
        [],
    )
    .unwrap()
    .with_bindings([HostBinding::to_instance(
        PluginInstanceId::new(CALLER, "console"),
        interaction::CAPABILITY_ID,
        PluginInstanceId::new(PROVIDER, "realtime"),
    )])
}

fn root() -> PluginRootSnapshot {
    PluginRootSnapshot::new(
        [],
        [
            PluginRootInstance::new(CALLER, "console"),
            PluginRootInstance::new(PROVIDER, "realtime"),
        ],
        [],
    )
}

fn open(id: &str) -> ConnectOpen {
    ConnectOpen {
        protocol: PROTOCOL.to_owned(),
        interaction_id: id.to_owned(),
        resume_from: None,
        max_pending_frames: 2,
    }
}

fn client_frame(id: &str, sequence: u64, kind: FrameKind) -> InteractionFrame {
    InteractionFrame {
        protocol: PROTOCOL.to_owned(),
        sequence: sequence.to_string(),
        direction: FrameDirection::ClientToAgent,
        kind,
        correlation_id: id.to_owned(),
        payload_json: r#"{"samples":320,"sample_rate_hz":16000}"#.try_into().unwrap(),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn copied_external_plugin_proves_bounded_duplex_interaction() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let resolved = resolve_plugin_root(&host(), &root()).unwrap();
            let app = Kernel::start_native(
                resolved.plan().clone(),
                TokioDriver::new(),
                NativePluginRegistry::new().with_linked_factories(),
            )
            .await
            .unwrap();
            let handle = app
                .stream_handle::<interaction::Interaction>(CALLER_INSTANCE)
                .unwrap();

            let stream = handle
                .open(interaction::CONNECT_OPERATION, open("audio-1"))
                .await
                .unwrap()
                .unwrap();
            stream
                .send(client_frame("audio-1", 1, FrameKind::Input))
                .await
                .unwrap();
            let first = match stream.receive().await.unwrap() {
                StreamEvent::Message(frame) => frame,
                other => panic!("expected first duplex output, got {other:?}"),
            };
            assert_eq!(first.direction, FrameDirection::AgentToClient);
            assert_eq!(first.sequence, "1");
            assert_eq!(first.kind, FrameKind::Output);
            stream
                .send(client_frame("audio-1", 2, FrameKind::Acknowledgement))
                .await
                .unwrap();
            stream.close_send().await.unwrap();
            let mut notice = None;
            loop {
                match stream.receive().await.unwrap() {
                    StreamEvent::Message(frame) => notice = Some(frame),
                    StreamEvent::PeerHalfClosed => {}
                    StreamEvent::Terminal(Ok(())) => break,
                    StreamEvent::Terminal(Err(error)) => panic!("normal duplex failed: {error:?}"),
                }
            }
            let notice = notice.unwrap();
            assert_eq!(notice.sequence, "2");
            assert_eq!(notice.kind, FrameKind::Notice);
            // The immutable descriptor intentionally admits one long-lived
            // interaction at a time. Dropping its completed Stream releases
            // that lease before opening the independent interrupt case.
            drop(stream);

            let interrupted = handle
                .open(interaction::CONNECT_OPERATION, open("audio-interrupt"))
                .await
                .unwrap()
                .unwrap();
            interrupted
                .send(client_frame("audio-interrupt", 1, FrameKind::Interrupt))
                .await
                .unwrap();
            assert!(matches!(
                interrupted.receive().await.unwrap(),
                StreamEvent::Message(InteractionFrame {
                    kind: FrameKind::Notice,
                    sequence,
                    ..
                }) if sequence == "1"
            ));
            assert!(matches!(
                interrupted.receive().await.unwrap(),
                StreamEvent::PeerHalfClosed
            ));
            assert!(matches!(
                interrupted.receive().await.unwrap(),
                StreamEvent::Terminal(Err(interaction::ConnectError::Interrupted))
            ));
            drop(interrupted);

            let unsupported_resume = handle
                .open(
                    interaction::CONNECT_OPERATION,
                    ConnectOpen {
                        resume_from: Some(Some("opaque-cursor".to_owned())),
                        ..open("audio-resume")
                    },
                )
                .await
                .unwrap();
            assert!(matches!(
                unsupported_resume,
                Err(interaction::ConnectError::ResumeUnsupported)
            ));

            let cancelled = CancellationToken::new();
            cancelled.cancel();
            let cancelled_open = handle
                .open_with_context(
                    interaction::CONNECT_OPERATION,
                    app.invocation_context_after(Duration::from_secs(1), cancelled),
                    open("audio-cancelled"),
                )
                .await;
            assert!(matches!(
                cancelled_open,
                Err(RuntimeFailure::Cancelled { .. })
            ));

            assert_eq!(
                app.shutdown(Duration::from_secs(1)).await,
                ShutdownOutcome::Clean
            );
        })
        .await;
}
