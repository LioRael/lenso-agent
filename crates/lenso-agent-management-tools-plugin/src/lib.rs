//! Opt-in projection of one bound application's accepted management operations.

use lenso::{CtxExt, PluginError, Port};
use lenso_capability_agent::AgentTaskBinding;
use lenso_capability_agent_tool_provider as tools;
use lenso_capability_management as management;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest as _, Sha256};

const STATUS_TOOL: &str = "management__status";

#[derive(Clone, Debug, Default, Deserialize, lenso::PluginConfig)]
#[serde(deny_unknown_fields)]
struct ManagementToolsConfig {
    task_id: Option<String>,
    agent_session_id: Option<String>,
    delegate_caller: Option<String>,
}

impl ManagementToolsConfig {
    fn binding(&self) -> Result<Option<AgentTaskBinding>, String> {
        match (&self.task_id, &self.agent_session_id, &self.delegate_caller) {
            (None, None, None) => Ok(None),
            (Some(task_id), Some(agent_session_id), Some(delegate_caller)) => {
                let binding = AgentTaskBinding {
                    task_id: task_id.clone(),
                    agent_session_id: agent_session_id.clone(),
                    delegate_caller: delegate_caller.clone(),
                };
                binding.validate()?;
                Ok(Some(binding))
            }
            _ => Err("management task binding must select all three labels".into()),
        }
    }
}

fn validate_config(config: &ManagementToolsConfig) -> Result<(), lenso::RuntimeFailure> {
    config
        .binding()
        .map(|_| ())
        .map_err(|detail| lenso::RuntimeFailure::PluginFailure { detail })
}

#[lenso::plugin(validate = validate_config)]
#[derive(Clone, Debug)]
struct ManagementTools {
    #[config]
    config: ManagementToolsConfig,
    management: Port<management::ManagementClient>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    input: Value,
    #[serde(default)]
    idempotency_key: Option<String>,
    #[serde(default)]
    expected_revision: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusArguments {
    operation_id: String,
}

impl ManagementTools {
    fn catalog_context(
        &self,
        context: lenso::Ctx,
    ) -> lenso::PluginResult<lenso::Ctx, tools::CatalogError> {
        let selected = self
            .config
            .binding()
            .map_err(|_| PluginError::domain(tools::CatalogError::CatalogInvalid))?;
        let Some(selected) = selected else {
            return Ok(context);
        };
        match context.typed_extension::<AgentTaskBinding>() {
            Ok(Some(current)) if current == selected => Ok(context),
            Ok(None) => selected
                .attach(context)
                .map_err(|_| PluginError::domain(tools::CatalogError::CatalogInvalid)),
            _ => Err(PluginError::domain(tools::CatalogError::CatalogInvalid)),
        }
    }
}

#[lenso::provides(tools::ToolProvider)]
impl ManagementTools {
    async fn catalog(
        &self,
        context: lenso::Ctx,
        _request: tools::CatalogRequest,
    ) -> lenso::PluginResult<tools::CatalogResponse, tools::CatalogError> {
        let context = self.catalog_context(context)?;
        let catalog = self
            .management
            .catalog_with_context(context, management::CatalogRequest {})
            .await
            .map_err(|error| match error {
                management::ManagementCatalogInvocationError::Runtime(error) => {
                    PluginError::runtime(error)
                }
                management::ManagementCatalogInvocationError::Domain(_) => {
                    PluginError::domain(tools::CatalogError::CatalogInvalid)
                }
            })?;
        let mut definitions = Vec::new();
        for entry in &catalog.entries {
            definitions.push(
                definition(entry)
                    .map_err(|_| PluginError::domain(tools::CatalogError::CatalogInvalid))?,
            );
        }
        if !definitions.is_empty() {
            definitions.push(tools::ToolDefinition {
                name:STATUS_TOOL.to_owned(),description:"Query an existing authorized management operation; never replay an unknown write.".to_owned(),execution:tools::ToolExecutionClass::ParallelSafe,
                input_schema_json:serde_json::json!({"type":"object","additionalProperties":false,"properties":{"operation_id":{"type":"string","minLength":1,"maxLength":128}},"required":["operation_id"]}).to_string().parse().map_err(|_|PluginError::domain(tools::CatalogError::CatalogInvalid))?,
            });
        }
        Ok(tools::CatalogResponse { tools: definitions })
    }

    async fn execute(
        &self,
        context: lenso::Ctx,
        request: tools::ExecuteRequest,
    ) -> lenso::PluginResult<tools::ExecuteResponse, tools::ExecuteError> {
        if let Some(selected) = self
            .config
            .binding()
            .map_err(|_| PluginError::domain(tools::ExecuteError::PermissionDenied))?
            && context
                .typed_extension::<AgentTaskBinding>()
                .ok()
                .flatten()
                .as_ref()
                != Some(&selected)
        {
            return Err(PluginError::domain(tools::ExecuteError::PermissionDenied));
        }
        let result = if request.name == STATUS_TOOL {
            let arguments: StatusArguments = serde_json::from_str(request.arguments_json.as_str())
                .map_err(|_| PluginError::domain(tools::ExecuteError::InvalidArguments))?;
            self.management
                .status_with_context(
                    context,
                    management::StatusRequest {
                        operation_id: arguments.operation_id,
                    },
                )
                .await
                .map_err(|error| match error {
                    management::ManagementStatusInvocationError::Runtime(error) => {
                        PluginError::runtime(error)
                    }
                    management::ManagementStatusInvocationError::Domain(error) => {
                        PluginError::domain(match error {
                            management::StatusError::PermissionDenied => {
                                tools::ExecuteError::PermissionDenied
                            }
                            management::StatusError::NotFound => tools::ExecuteError::NotFound,
                            management::StatusError::Conflict => failure("conflict"),
                            _ => failure("unavailable"),
                        })
                    }
                })?
        } else {
            // Every call refreshes the bound catalog; a previous session cannot retain a revoked entry.
            let catalog = self
                .management
                .catalog_with_context(context.clone(), management::CatalogRequest {})
                .await
                .map_err(|error| match error {
                    management::ManagementCatalogInvocationError::Runtime(error) => {
                        PluginError::runtime(error)
                    }
                    management::ManagementCatalogInvocationError::Domain(
                        management::CatalogError::PermissionDenied,
                    ) => PluginError::domain(tools::ExecuteError::PermissionDenied),
                    management::ManagementCatalogInvocationError::Domain(_) => {
                        PluginError::domain(failure("unavailable"))
                    }
                })?;
            let entry = catalog
                .entries
                .iter()
                .find(|entry| tool_name(entry).is_ok_and(|name| name == request.name))
                .ok_or_else(|| PluginError::domain(tools::ExecuteError::NotFound))?;
            let arguments: Arguments = serde_json::from_str(request.arguments_json.as_str())
                .map_err(|_| PluginError::domain(tools::ExecuteError::InvalidArguments))?;
            self.management
                .invoke_with_context(
                    context,
                    management::InvokeRequest {
                        entry_id: entry.id.clone(),
                        version: entry.version.clone(),
                        input_json: arguments.input.to_string().parse().map_err(|_| {
                            PluginError::domain(tools::ExecuteError::InvalidArguments)
                        })?,
                        idempotency_key: arguments.idempotency_key,
                        expected_revision: arguments.expected_revision,
                    },
                )
                .await
                .map_err(map_invoke)?
        };
        let output_text = serde_json::to_string(&result)
            .map_err(|_| PluginError::domain(failure("invalid_response")))?;
        if output_text.len() > 1_048_576 {
            return Err(PluginError::domain(
                tools::ExecuteError::OutputLimitExceeded,
            ));
        }
        Ok(tools::ExecuteResponse {
            content_type: tools::ContentType::Text,
            content: output_text,
            content_blocks: None,
            metadata_json: serde_json::json!({"management":true,"state":result.state})
                .to_string()
                .parse()
                .map_err(|_| PluginError::domain(failure("invalid_response")))?,
        })
    }
}

fn tool_name(entry: &management::Entry) -> Result<String, serde_json::Error> {
    let schema = canonical_schema(serde_json::from_str(entry.input_schema_json.as_str())?);
    let identity = serde_json::json!([
        entry.id,
        entry.version,
        entry.capability,
        entry.operation,
        entry.target_instance,
        schema,
        entry.effect,
        entry.requires_approval,
    ]);
    let digest = format!("{:x}", Sha256::digest(identity.to_string().as_bytes()));
    Ok(format!("management__{}", &digest[..48]))
}

fn canonical_schema(value: Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(key, value)| (key, canonical_schema(value)))
                .collect::<std::collections::BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.into_iter().map(canonical_schema).collect()),
        value => value,
    }
}

fn definition(entry: &management::Entry) -> Result<tools::ToolDefinition, serde_json::Error> {
    let input: Value = serde_json::from_str(entry.input_schema_json.as_str())?;
    let mut required = vec!["input"];
    if entry.effect == management::Effect::Write {
        required.push("idempotency_key");
    }
    let schema = serde_json::json!({"type":"object","additionalProperties":false,"properties":{"input":input,"idempotency_key":{"type":"string","minLength":1,"maxLength":128},"expected_revision":{"type":["string","null"],"minLength":1,"maxLength":128}},"required":required});
    Ok(tools::ToolDefinition {
        name: tool_name(entry)?,
        description: entry.description.clone(),
        execution: if entry.effect == management::Effect::Read {
            tools::ToolExecutionClass::ParallelSafe
        } else {
            tools::ToolExecutionClass::Exclusive
        },
        input_schema_json: schema
            .to_string()
            .parse()
            .map_err(serde::de::Error::custom)?,
    })
}

fn map_invoke(
    error: management::ManagementInvokeInvocationError,
) -> PluginError<tools::ExecuteError> {
    match error {
        management::ManagementInvokeInvocationError::Runtime(error) => PluginError::runtime(error),
        management::ManagementInvokeInvocationError::Domain(error) => {
            PluginError::domain(match error {
                management::InvokeError::PermissionDenied => tools::ExecuteError::PermissionDenied,
                management::InvokeError::NotFound => tools::ExecuteError::NotFound,
                management::InvokeError::InvalidInput => tools::ExecuteError::InvalidArguments,
                management::InvokeError::Conflict => failure("conflict"),
                management::InvokeError::Cancelled => failure("cancelled"),
                management::InvokeError::DeadlineExceeded => failure("deadline_exceeded"),
                _ => failure("unavailable"),
            })
        }
    }
}

fn failure(reason: &str) -> tools::ExecuteError {
    tools::ExecuteError::ExecutionFailed {
        payload: tools::ExecutionFailedPayload {
            reason_code: reason.to_owned(),
            message: "The bound management service did not complete the operation.".to_owned(),
            details_json: "{}".parse().expect("empty object is valid JSON"),
        },
    }
}

#[cfg(test)]
mod tests;
