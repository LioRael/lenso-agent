//! A third-party Model that makes model-visible input observable in V04.

use futures::future::{LocalBoxFuture, ready};
use lenso::prelude::*;
use lenso_capability_agent_model::{
    self as model, CatalogControl, CatalogControlStatus, CatalogFreshness, CatalogInputModality,
    CatalogModel, CatalogModelLimits, CatalogProvenance, CatalogRequest, CatalogResponse,
    CatalogSource, CompleteError, CompleteMessage, CompleteMessageKind, CompleteOpen, ModelCatalog,
    ModelComplete, ModelCompleteInvocationError, ModelProvider,
};
use lenso_kernel::{InvocationContext, NativeStreamSession, RuntimeFailure};

pub const MODEL_ID: &str = "example/inspect-v1";

#[derive(Clone, Debug, Default, serde::Deserialize, lenso::PluginConfig)]
#[serde(deny_unknown_fields)]
struct InspectModelConfig {}

#[lenso::plugin]
#[derive(Clone, Debug)]
struct InspectModel {
    #[config]
    config: InspectModelConfig,
    #[tasks]
    tasks: ManagedTasks,
}

pub fn link() {
    link_plugin();
}

#[lenso::provides(model::Model)]
impl ModelProvider for InspectModel {
    fn catalog(
        &self,
        _context: InvocationContext,
        _request: CatalogRequest,
    ) -> lenso_kernel::NativeRequestFuture<ModelCatalog> {
        let _ = &self.config;
        Box::pin(ready(Ok(Ok(CatalogResponse {
            models: vec![CatalogModel {
                id: MODEL_ID.to_owned(),
                display_name: "External inspection model".to_owned(),
                description: "Returns the exact model-visible request for fixture assertions."
                    .to_owned(),
                hidden: false,
                limits: CatalogModelLimits {
                    context_window_tokens: Some(Some("8192".to_owned())),
                    max_input_tokens: Some(Some("4096".to_owned())),
                    max_output_tokens: Some(Some("1024".to_owned())),
                },
                input_modalities: vec![CatalogInputModality::Text],
                text_output: true,
                tool_calls: false,
                parallel_tool_calls: false,
                reasoning: unsupported_control(),
                service_tiers: unsupported_control(),
                wire_protocol: "example.inspect-model@1".to_owned(),
                compaction_compatibility: "generic-text-v1".to_owned(),
            }],
            provenance: CatalogProvenance {
                source: CatalogSource::Configured,
                freshness: CatalogFreshness::Fresh,
                fetched_at_unix_seconds: None,
                validated_at_unix_seconds: None,
                revision: Some(Some("fixture-v1".to_owned())),
                max_stale_seconds: None,
            },
        }))))
    }

    fn complete(
        &self,
        context: InvocationContext,
        request: CompleteOpen,
    ) -> LocalBoxFuture<'static, Result<Box<dyn NativeStreamSession>, ModelCompleteInvocationError>>
    {
        let plugin = self.clone();
        Box::pin(async move {
            if context.is_cancelled() {
                return Err(ModelCompleteInvocationError::Runtime(
                    RuntimeFailure::Cancelled {
                        request_id: context.request_id(),
                    },
                ));
            }
            if request.model != MODEL_ID {
                return Err(ModelCompleteInvocationError::Domain(
                    CompleteError::UnsupportedModel,
                ));
            }
            let text = request
                .messages
                .iter()
                .map(|message| message.content.as_str())
                .collect::<Vec<_>>()
                .join(" | ");
            let (stream, channel) = ProviderStream::<ModelComplete>::channel(&context, 8);
            let task_plugin = plugin.clone();
            plugin
                .tasks
                .spawn_local(async move {
                    task_plugin.produce(text, channel).await;
                })
                .map_err(|error| {
                    ModelCompleteInvocationError::Runtime(RuntimeFailure::PluginFailure {
                        detail: format!("external Model task failed to start: {error:?}"),
                    })
                })?;
            Ok(Box::new(stream) as Box<dyn NativeStreamSession>)
        })
    }
}

impl InspectModel {
    async fn produce(&self, text: String, mut channel: ProviderStreamChannel<ModelComplete>) {
        let _ = &self.config;
        let terminal: PluginResult<(), CompleteError> = match channel.receive().await {
            Ok(StreamInput::PeerHalfClosed) => channel
                .send(CompleteMessage {
                    sequence: "1".to_owned(),
                    kind: CompleteMessageKind::TextDelta,
                    text,
                    tool_call_id: String::new(),
                    tool_name: String::new(),
                    arguments_json: "{}".try_into().expect("fixture arguments are valid JSON"),
                    input_tokens: "0".to_owned(),
                    output_tokens: "1".to_owned(),
                })
                .await
                .map_err(PluginError::runtime),
            Ok(StreamInput::Message(_)) => {
                Err(PluginError::runtime(RuntimeFailure::ProtocolViolation {
                    capability: model::CAPABILITY_ID,
                }))
            }
            Err(error) => Err(PluginError::runtime(error)),
        };
        let _ = channel.complete(terminal).await;
    }
}

fn unsupported_control() -> CatalogControl {
    CatalogControl {
        status: CatalogControlStatus::Unsupported,
        mode: None,
        options: Vec::new(),
        default: None,
        budget_tokens: None,
    }
}
