//! Opt-in projection of one bound application's accepted management operations.

use lenso::{PluginError, Port};
use lenso_capability_agent_tool_provider as tools;
use lenso_capability_management as management;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest as _, Sha256};

const STATUS_TOOL: &str = "management__status";

#[lenso::plugin]
#[derive(Clone, Debug)]
struct ManagementTools {
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

#[lenso::provides(tools::ToolProvider)]
impl ManagementTools {
    async fn catalog(
        &self,
        context: lenso::Ctx,
        _request: tools::CatalogRequest,
    ) -> lenso::PluginResult<tools::CatalogResponse, tools::CatalogError> {
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
                .find(|entry| tool_name(&entry.id) == request.name)
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

fn tool_name(entry: &str) -> String {
    format!("management__{:x}", Sha256::digest(entry.as_bytes()))
}

fn definition(entry: &management::Entry) -> Result<tools::ToolDefinition, serde_json::Error> {
    let input: Value = serde_json::from_str(entry.input_schema_json.as_str())?;
    let mut required = vec!["input"];
    if entry.effect == management::Effect::Write {
        required.push("idempotency_key");
    }
    let schema = serde_json::json!({"type":"object","additionalProperties":false,"properties":{"input":input,"idempotency_key":{"type":"string","minLength":1,"maxLength":128},"expected_revision":{"type":["string","null"],"minLength":1,"maxLength":128}},"required":required});
    Ok(tools::ToolDefinition {
        name: tool_name(&entry.id),
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
