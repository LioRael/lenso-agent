//! Constrained Code Mode Tool Provider over one explicitly composed Tools Runtime.

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

use futures::{StreamExt, stream};
use lenso::prelude::*;
use lenso_capability_agent_tool_provider::{
    self as provider_contract, CatalogRequest, CatalogResponse, ContentType, ExecuteError,
    ExecuteRequest, ExecuteResponse, ExecutionFailedPayload, ToolDefinition, ToolExecutionClass,
    ToolProviderProvider,
};
use lenso_capability_agent_tools::{
    self as tools_contract, CatalogResponseToolsItem, CatalogResponseToolsItemExecution,
    ToolsExecuteInvocationError,
};
use lenso_kernel::{InvocationContext, RuntimeFailure};
use mlua::{ChunkMode, HookTriggers, Lua, LuaOptions, LuaSerdeExt, StdLib, Value, VmState};

/// Stable model-visible Tool name.
pub const RUN_CODE_TOOL: &str = "run_code";

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::struct_field_names,
    reason = "limit names are the public configuration contract"
)]
struct CodeModeConfig {
    max_code_bytes: usize,
    max_instructions: u64,
    max_memory_bytes: usize,
    max_output_bytes: usize,
    max_parallel_subcalls: usize,
    max_subcalls: usize,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RunCodeArguments {
    code: String,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NestedCall {
    name: String,
    #[serde(default = "empty_object")]
    arguments: serde_json::Value,
}

#[derive(Clone, Debug, serde::Serialize)]
struct NestedCallFact {
    sequence: usize,
    name: String,
    arguments: serde_json::Value,
    outcome: &'static str,
    provider_code: Option<String>,
}

#[derive(Clone)]
struct CodeRuntime {
    tools: Port<tools_contract::ToolsClient>,
    context: InvocationContext,
    catalog: BTreeMap<String, CatalogResponseToolsItem>,
    allowed_tools: BTreeSet<String>,
    max_parallel: usize,
    max_subcalls: usize,
    next_sequence: Rc<Cell<usize>>,
    facts: Rc<RefCell<BTreeMap<usize, NestedCallFact>>>,
}

fn empty_object() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

fn validate_config(config: &CodeModeConfig) -> Result<(), RuntimeFailure> {
    if config.max_code_bytes == 0
        || config.max_code_bytes > 262_144
        || config.max_instructions == 0
        || config.max_instructions > 10_000_000
        || !(65_536..=67_108_864).contains(&config.max_memory_bytes)
        || config.max_output_bytes == 0
        || config.max_output_bytes > 1_048_576
        || !(1..=16).contains(&config.max_parallel_subcalls)
        || !(1..=64).contains(&config.max_subcalls)
    {
        return Err(RuntimeFailure::InvalidResolvedPlan {
            detail: "Code Mode limits are invalid".to_owned(),
        });
    }
    Ok(())
}

#[lenso::plugin(
    configuration_schema = "config.schema.json",
    validate = validate_config
)]
#[derive(Clone, Debug)]
struct CodeModeToolsPlugin {
    #[config]
    config: CodeModeConfig,
    tools: Port<tools_contract::ToolsClient>,
}

#[lenso::provides(provider_contract::ToolProvider)]
impl ToolProviderProvider for CodeModeToolsPlugin {
    fn catalog(
        &self,
        _context: InvocationContext,
        _request: CatalogRequest,
    ) -> lenso_kernel::NativeRequestFuture<provider_contract::ToolProviderCatalog> {
        let max_code_bytes = self.config.max_code_bytes;
        Box::pin(futures::future::ready(Ok(Ok(CatalogResponse {
            tools: vec![ToolDefinition {
                name: RUN_CODE_TOOL.to_owned(),
                description: "Run bounded Lua code that can transform values and call only the narrow Tools explicitly granted to Code Mode. Use tool(name, arguments) for one call or parallel(calls) for an ordered bounded batch. Return one JSON-compatible value.".to_owned(),
                input_schema_json: serde_json::json!({
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "code": {
                            "type": "string",
                            "minLength": 1,
                            "maxLength": max_code_bytes
                        }
                    },
                    "required": ["code"]
                })
                .to_string()
                .try_into()
                .expect("Code Mode Tool schema must be valid JSON"),
                execution: ToolExecutionClass::Exclusive,
            }],
        }))))
    }

    fn execute(
        &self,
        context: InvocationContext,
        request: ExecuteRequest,
    ) -> lenso_kernel::NativeRequestFuture<provider_contract::ToolProviderExecute> {
        if request.name != RUN_CODE_TOOL {
            return Box::pin(futures::future::ready(Ok(Err(ExecuteError::NotFound))));
        }
        let Ok(arguments) =
            serde_json::from_str::<RunCodeArguments>(request.arguments_json.as_str())
        else {
            return Box::pin(futures::future::ready(Ok(Err(
                ExecuteError::InvalidArguments,
            ))));
        };
        if arguments.code.trim().is_empty() || arguments.code.len() > self.config.max_code_bytes {
            return Box::pin(futures::future::ready(Ok(Err(
                ExecuteError::InvalidArguments,
            ))));
        }
        let plugin = self.clone();
        Box::pin(async move { plugin.run_code(context, arguments.code).await })
    }
}

impl CodeModeToolsPlugin {
    async fn run_code(
        &self,
        context: InvocationContext,
        code: String,
    ) -> Result<Result<ExecuteResponse, ExecuteError>, RuntimeFailure> {
        let catalog = self
            .tools
            .catalog_with_context(context.clone(), tools_contract::CatalogRequest {})
            .await
            .map_err(|error| RuntimeFailure::PluginFailure {
                detail: format!("Code Mode Tool catalog failed: {error:?}"),
            })?;
        let catalog = catalog
            .tools
            .into_iter()
            .map(|tool| (tool.name.clone(), tool))
            .collect::<BTreeMap<_, _>>();
        let runtime = CodeRuntime {
            tools: self.tools.clone(),
            context,
            allowed_tools: catalog.keys().cloned().collect(),
            catalog,
            max_parallel: self.config.max_parallel_subcalls,
            max_subcalls: self.config.max_subcalls,
            next_sequence: Rc::new(Cell::new(0)),
            facts: Rc::new(RefCell::new(BTreeMap::new())),
        };
        match evaluate_lua(&code, &self.config, runtime.clone()).await {
            Ok(value) => {
                let response_content =
                    serde_json::to_string(&value).map_err(|error| RuntimeFailure::Internal {
                        detail: format!("Code Mode output encoding failed: {error}"),
                    })?;
                if response_content.len() > self.config.max_output_bytes {
                    return Ok(Err(ExecuteError::OutputLimitExceeded));
                }
                let nested_calls = runtime.facts.borrow().values().cloned().collect::<Vec<_>>();
                Ok(Ok(ExecuteResponse {
                    content: response_content,
                    content_type: ContentType::Text,
                    metadata_json: serde_json::json!({
                        "language": "lua54",
                        "nested_calls": nested_calls,
                    })
                    .to_string()
                    .try_into()
                    .expect("Code Mode metadata must be valid JSON"),
                }))
            }
            Err(error) => Ok(Err(execution_failed("code_failed", &error))),
        }
    }
}

async fn evaluate_lua(
    code: &str,
    config: &CodeModeConfig,
    runtime: CodeRuntime,
) -> Result<serde_json::Value, String> {
    let lua = constrained_lua(config)?;
    let single_runtime = runtime.clone();
    let tool = lua
        .create_async_function(move |lua, (name, arguments): (String, Value)| {
            let runtime = single_runtime.clone();
            async move {
                let arguments = lua.from_value(arguments)?;
                let content = runtime
                    .execute_one(NestedCall { name, arguments })
                    .await
                    .map_err(mlua::Error::runtime)?;
                lua.to_value(&content)
            }
        })
        .map_err(|error| error.to_string())?;
    lua.globals()
        .set("tool", tool)
        .map_err(|error| error.to_string())?;
    let parallel_runtime = runtime;
    let parallel = lua
        .create_async_function(move |lua, calls: Value| {
            let runtime = parallel_runtime.clone();
            async move {
                let calls = lua.from_value::<Vec<NestedCall>>(calls)?;
                let content = runtime
                    .execute_calls(calls)
                    .await
                    .map_err(mlua::Error::runtime)?;
                lua.to_value(&content)
            }
        })
        .map_err(|error| error.to_string())?;
    lua.globals()
        .set("parallel", parallel)
        .map_err(|error| error.to_string())?;
    let value = lua
        .load(code)
        .set_mode(ChunkMode::Text)
        .set_name("run_code")
        .eval_async::<Value>()
        .await
        .map_err(|error| bounded_error(&error.to_string()))?;
    lua.from_value(value)
        .map_err(|error| bounded_error(&error.to_string()))
}

fn constrained_lua(config: &CodeModeConfig) -> Result<Lua, String> {
    let lua = Lua::new_with(
        StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::UTF8,
        LuaOptions::default(),
    )
    .map_err(|error| error.to_string())?;
    lua.set_memory_limit(config.max_memory_bytes)
        .map_err(|error| error.to_string())?;
    let instructions = Rc::new(Cell::new(0_u64));
    let max_instructions = config.max_instructions;
    lua.set_hook(
        HookTriggers::new().every_nth_instruction(1_000),
        move |_lua, _debug| {
            let next = instructions.get().saturating_add(1_000);
            instructions.set(next);
            if next > max_instructions {
                Err(mlua::Error::runtime("Code Mode instruction limit exceeded"))
            } else {
                Ok(VmState::Continue)
            }
        },
    )
    .map_err(|error| error.to_string())?;
    for name in [
        "collectgarbage",
        "debug",
        "dofile",
        "io",
        "load",
        "loadfile",
        "os",
        "package",
        "print",
        "require",
        "warn",
    ] {
        lua.globals()
            .set(name, Value::Nil)
            .map_err(|error| error.to_string())?;
    }
    Ok(lua)
}

impl CodeRuntime {
    async fn execute_calls(&self, calls: Vec<NestedCall>) -> Result<Vec<String>, String> {
        if calls.is_empty() {
            return Ok(Vec::new());
        }
        let mut waves: Vec<(bool, Vec<NestedCall>)> = Vec::new();
        for call in calls {
            let parallel_safe = self.parallel_safe(&call)?;
            if parallel_safe && let Some((true, wave)) = waves.last_mut() {
                wave.push(call);
            } else {
                waves.push((parallel_safe, vec![call]));
            }
        }
        let mut output = Vec::new();
        for (parallel_safe, wave) in waves {
            if parallel_safe {
                let runtime = self.clone();
                let mut results =
                    stream::iter(wave.into_iter().enumerate().map(|(index, call)| {
                        let runtime = runtime.clone();
                        async move { (index, runtime.execute_one(call).await) }
                    }))
                    .buffer_unordered(self.max_parallel)
                    .collect::<Vec<_>>()
                    .await;
                results.sort_by_key(|(index, _)| *index);
                for (_, result) in results {
                    output.push(result?);
                }
            } else {
                output.push(
                    self.execute_one(wave.into_iter().next().expect("wave is non-empty"))
                        .await?,
                );
            }
        }
        Ok(output)
    }

    fn parallel_safe(&self, call: &NestedCall) -> Result<bool, String> {
        let Some(tool) = self.catalog.get(&call.name) else {
            return Err(format!(
                "Tool `{}` is outside the Code Mode catalog",
                call.name
            ));
        };
        Ok(matches!(
            tool.execution,
            CatalogResponseToolsItemExecution::ParallelSafe
        ))
    }

    async fn execute_one(&self, call: NestedCall) -> Result<String, String> {
        if !self.allowed_tools.contains(&call.name) {
            return Err(format!(
                "Tool `{}` is outside the Code Mode catalog",
                call.name
            ));
        }
        let sequence = self.next_sequence.get();
        if sequence >= self.max_subcalls {
            return Err("Code Mode nested Tool call limit exceeded".to_owned());
        }
        self.next_sequence.set(sequence + 1);
        let arguments_json = serde_json::to_string(&call.arguments)
            .map_err(|error| format!("nested Tool arguments are invalid: {error}"))?;
        let result = self
            .tools
            .execute_with_context(
                self.context.clone(),
                tools_contract::ExecuteRequest {
                    name: call.name.clone(),
                    arguments_json: arguments_json
                        .try_into()
                        .map_err(|error| format!("nested Tool arguments are invalid: {error}"))?,
                },
            )
            .await;
        match result {
            Ok(response) => {
                self.record(sequence, call, "success", None);
                Ok(response.content)
            }
            Err(ToolsExecuteInvocationError::Domain(error)) => {
                let code = domain_error_code(&error);
                self.record(sequence, call, "domain_error", Some(code.clone()));
                Err(format!("nested Tool returned Domain Error `{code}`"))
            }
            Err(ToolsExecuteInvocationError::Runtime(error)) => {
                self.record(sequence, call, "runtime_failure", None);
                Err(format!("nested Tool Runtime Failure: {error:?}"))
            }
        }
    }

    fn record(
        &self,
        sequence: usize,
        call: NestedCall,
        outcome: &'static str,
        provider_code: Option<String>,
    ) {
        self.facts.borrow_mut().insert(
            sequence,
            NestedCallFact {
                sequence,
                name: call.name,
                arguments: call.arguments,
                outcome,
                provider_code,
            },
        );
    }
}

fn domain_error_code(error: &tools_contract::ExecuteError) -> String {
    match error {
        tools_contract::ExecuteError::InvalidArguments => "invalid_arguments".to_owned(),
        tools_contract::ExecuteError::ToolError { payload } => payload.provider_code.clone(),
        tools_contract::ExecuteError::UnknownTool => "unknown_tool".to_owned(),
        tools_contract::ExecuteError::Unknown(unknown) => unknown.code.clone(),
    }
}

fn execution_failed(reason_code: &str, message: &str) -> ExecuteError {
    ExecuteError::ExecutionFailed {
        payload: ExecutionFailedPayload {
            reason_code: reason_code.to_owned(),
            message: bounded_error(message),
            details_json: "{}"
                .to_owned()
                .try_into()
                .expect("static Code Mode error details must be valid JSON"),
        },
    }
}

fn bounded_error(message: &str) -> String {
    const MAX_ERROR_BYTES: usize = 4_096;
    if message.len() <= MAX_ERROR_BYTES {
        return message.to_owned();
    }
    let mut end = MAX_ERROR_BYTES;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &message[..end])
}

#[cfg(test)]
mod tests {
    use mlua::Value;

    use super::{CodeModeConfig, constrained_lua, validate_config};

    fn config() -> CodeModeConfig {
        CodeModeConfig {
            max_code_bytes: 32_768,
            max_instructions: 100_000,
            max_memory_bytes: 8_388_608,
            max_output_bytes: 262_144,
            max_parallel_subcalls: 4,
            max_subcalls: 16,
        }
    }

    #[test]
    fn rejects_zero_or_unbounded_limits() {
        assert!(validate_config(&config()).is_ok());
        let mut invalid = config();
        invalid.max_subcalls = 65;
        assert!(validate_config(&invalid).is_err());
        invalid = config();
        invalid.max_memory_bytes = 1;
        assert!(validate_config(&invalid).is_err());
    }

    #[test]
    fn removes_ambient_filesystem_process_and_debug_libraries() {
        let lua = constrained_lua(&config()).unwrap();
        for name in [
            "debug", "dofile", "io", "load", "loadfile", "os", "package", "print", "require",
            "warn",
        ] {
            assert!(matches!(
                lua.globals().get::<Value>(name).unwrap(),
                Value::Nil
            ));
        }
    }

    #[test]
    fn interrupts_an_infinite_program_at_the_instruction_limit() {
        let mut limited = config();
        limited.max_instructions = 2_000;
        let lua = constrained_lua(&limited).unwrap();
        let error = lua.load("while true do end").exec().unwrap_err();
        assert!(error.to_string().contains("instruction limit"));
    }
}
