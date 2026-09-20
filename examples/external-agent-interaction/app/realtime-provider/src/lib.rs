//! A third-party, bounded duplex interaction Provider used by V07.
//!
//! The Provider has no Agent Loop dependency. It accepts a small example
//! protocol, tracks direction-local sequences through the public runtime
//! helper, and maps an interrupt to a Capability-defined terminal outcome.

use lenso::prelude::*;
use lenso_capability_agent_interaction::{
    self as interaction, ConnectOpen, FrameDirection, FrameKind, InteractionFlow, InteractionFrame,
    InteractionValidationError,
};

const PROTOCOL: &str = "example.realtime-audio@1";

#[lenso::plugin]
#[derive(Clone, Debug)]
struct RealtimeProvider {
    #[tasks]
    tasks: ManagedTasks,
}

#[lenso::provides(interaction::Interaction)]
impl RealtimeProvider {
    async fn connect(
        &self,
        context: Ctx,
        request: ConnectOpen,
    ) -> PluginResult<ProviderStream<interaction::Interaction>, interaction::ConnectError> {
        if request.protocol != PROTOCOL {
            return Err(PluginError::domain(
                interaction::ConnectError::UnsupportedProtocol,
            ));
        }
        if request.resume_from.is_some() {
            return Err(PluginError::domain(
                interaction::ConnectError::ResumeUnsupported,
            ));
        }
        interaction::validate_connect_open(&request).map_err(validation_error)?;
        let capacity = usize::try_from(request.max_pending_frames)
            .map_err(|_| PluginError::domain(interaction::ConnectError::CapacityExceeded))?;
        let (stream, mut channel) =
            ProviderStream::<interaction::Interaction>::channel(&context, capacity);
        self.tasks
            .clone()
            .spawn_local(async move {
                let result = serve(&mut channel, request).await;
                let _ = channel.complete(result).await;
            })
            .map_err(|error| {
                PluginError::runtime(RuntimeFailure::PluginFailure {
                    detail: format!(
                        "external realtime interaction task failed to start: {error:?}"
                    ),
                })
            })?;
        Ok(stream)
    }
}

async fn serve(
    channel: &mut ProviderStreamChannel<interaction::Interaction>,
    open: ConnectOpen,
) -> PluginResult<(), interaction::ConnectError> {
    let mut flow = InteractionFlow::new(&open).map_err(validation_error)?;
    let mut next_agent_sequence = 1_u64;
    loop {
        match channel.receive().await {
            Ok(StreamInput::Message(frame)) => {
                flow.accept_client(&frame).map_err(validation_error)?;
                match frame.kind {
                    FrameKind::Acknowledgement => flow.acknowledge_agent_frame(),
                    FrameKind::Interrupt => {
                        send_agent(
                            channel,
                            &mut flow,
                            &open,
                            &mut next_agent_sequence,
                            FrameKind::Notice,
                            "interrupted",
                        )
                        .await?;
                        flow.close_agent();
                        return Err(PluginError::domain(interaction::ConnectError::Interrupted));
                    }
                    FrameKind::Input => {
                        send_agent(
                            channel,
                            &mut flow,
                            &open,
                            &mut next_agent_sequence,
                            FrameKind::Output,
                            "sample accepted",
                        )
                        .await?;
                    }
                    FrameKind::Output | FrameKind::Notice => {
                        return Err(PluginError::domain(interaction::ConnectError::InvalidFrame));
                    }
                }
            }
            Ok(StreamInput::PeerHalfClosed) => {
                flow.close_client();
                send_agent(
                    channel,
                    &mut flow,
                    &open,
                    &mut next_agent_sequence,
                    FrameKind::Notice,
                    "client half closed",
                )
                .await?;
                flow.close_agent();
                return Ok(());
            }
            Err(error) => return Err(PluginError::runtime(error)),
        }
    }
}

async fn send_agent(
    channel: &mut ProviderStreamChannel<interaction::Interaction>,
    flow: &mut InteractionFlow,
    open: &ConnectOpen,
    next_sequence: &mut u64,
    kind: FrameKind,
    text: &str,
) -> PluginResult<(), interaction::ConnectError> {
    let frame = InteractionFrame {
        protocol: open.protocol.clone(),
        sequence: next_sequence.to_string(),
        direction: FrameDirection::AgentToClient,
        kind,
        correlation_id: open.interaction_id.clone(),
        payload_json: format!(r#"{{"transcript":"{text}"}}"#)
            .try_into()
            .expect("fixture payload is valid JSON"),
    };
    flow.emit_agent(&frame).map_err(validation_error)?;
    *next_sequence = next_sequence.saturating_add(1);
    channel.send(frame).await.map_err(PluginError::runtime)
}

fn validation_error(error: InteractionValidationError) -> PluginError<interaction::ConnectError> {
    let domain = match error {
        InteractionValidationError::CapacityExceeded => interaction::ConnectError::CapacityExceeded,
        InteractionValidationError::Interrupted => interaction::ConnectError::Interrupted,
        InteractionValidationError::InvalidOpen(_)
        | InteractionValidationError::InvalidFrame(_)
        | InteractionValidationError::ClientDirectionClosed
        | InteractionValidationError::AgentDirectionClosed => {
            interaction::ConnectError::InvalidFrame
        }
    };
    PluginError::domain(domain)
}

pub fn link() {
    link_plugin();
}
