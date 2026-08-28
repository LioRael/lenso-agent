//! Bounded, profile-scoped MCP stdio client projected as Agent Tools.

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::PathBuf,
    process::Stdio,
    rc::Rc,
    time::Duration,
};

use lenso::prelude::*;
use lenso_capability_agent_tool_provider::{
    self as tool_contract, CatalogRequest, CatalogResponse, ContentType, ExecuteError,
    ExecuteRequest, ExecuteResponse, ExecutionFailedPayload, ToolDefinition, ToolExecutionClass,
};
use lenso_kernel::RuntimeFailure;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
};

const MODERN_VERSION: &str = "2026-07-28";
const LEGACY_VERSION: &str = "2025-06-18";
const SUPPORTED_LEGACY_VERSIONS: &[&str] =
    &["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];
const MAX_TOOLS: usize = 256;
const MAX_PAGES: usize = 16;
const MAX_SCHEMA_BYTES: usize = 65_536;
const MAX_OUTPUT_BYTES: usize = 1_048_576;
const MAX_MESSAGE_BYTES: usize = 1_310_720;

#[derive(Clone, Copy, Debug, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ProtocolMode {
    Auto,
    Modern,
    Legacy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Protocol {
    Modern,
    Legacy,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct McpClientConfig {
    program: PathBuf,
    arguments: Vec<String>,
    working_directory: PathBuf,
    environment_allowlist: Vec<String>,
    protocol: ProtocolMode,
    tool_namespace: String,
    startup_timeout_ms: u64,
    request_timeout_ms: u64,
}

#[derive(Clone, Debug)]
struct ReadyClient {
    protocol: Protocol,
    program: PathBuf,
    working_directory: PathBuf,
    environment: BTreeMap<String, String>,
    catalog: CatalogResponse,
    exposed_to_remote: BTreeMap<String, String>,
    session: Rc<Mutex<Option<Session>>>,
}

fn validate_config(config: &McpClientConfig) -> Result<(), RuntimeFailure> {
    let arguments_bytes = config.arguments.iter().map(String::len).sum::<usize>();
    let namespace_bytes = config.tool_namespace.as_bytes();
    let namespace_valid = matches!(namespace_bytes.first(), Some(b'a'..=b'z'))
        && namespace_bytes.len() <= 32
        && namespace_bytes[1..]
            .iter()
            .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_'));
    let environment_valid = config.environment_allowlist.len() <= 64
        && config.environment_allowlist.iter().all(|name| {
            let mut bytes = name.bytes();
            name.len() <= 128
                && bytes
                    .next()
                    .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
                && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        });
    if !config.program.is_absolute()
        || config.working_directory.as_os_str().is_empty()
        || config.arguments.len() > 64
        || arguments_bytes > 131_072
        || config
            .arguments
            .iter()
            .any(|argument| argument.len() > 16_384)
        || !namespace_valid
        || !environment_valid
        || !(1..=60_000).contains(&config.startup_timeout_ms)
        || !(1..=3_600_000).contains(&config.request_timeout_ms)
    {
        return Err(invalid_plan(
            "MCP stdio configuration requires an absolute program, bounded arguments, a safe Tool namespace, an environment allowlist, and bounded timeouts",
        ));
    }
    Ok(())
}

#[lenso::plugin(
    lifecycle,
    configuration_schema = "config.schema.json",
    validate = validate_config
)]
#[derive(Clone, Debug)]
struct McpClientPlugin {
    #[config]
    config: McpClientConfig,
    ready: Rc<RefCell<Option<ReadyClient>>>,
}

#[lenso::provides(tool_contract::ToolProvider)]
impl McpClientPlugin {
    fn catalog(
        &self,
        _context: Ctx,
        _request: CatalogRequest,
    ) -> impl std::future::Future<Output = PluginResult<CatalogResponse, tool_contract::CatalogError>>
    {
        let result = self
            .ready
            .borrow()
            .as_ref()
            .map(|ready| ready.catalog.clone())
            .ok_or(RuntimeFailure::Unavailable {
                capability: tool_contract::CAPABILITY_ID,
            });
        futures::future::ready(result.map_err(PluginError::runtime))
    }

    async fn execute(
        &self,
        context: Ctx,
        request: ExecuteRequest,
    ) -> PluginResult<ExecuteResponse, ExecuteError> {
        let ready = self.ready.borrow().clone().ok_or_else(|| {
            PluginError::runtime(RuntimeFailure::Unavailable {
                capability: tool_contract::CAPABILITY_ID,
            })
        })?;
        let Some(remote_name) = ready.exposed_to_remote.get(&request.name).cloned() else {
            return Err(PluginError::domain(ExecuteError::NotFound));
        };
        let arguments: Value = serde_json::from_str(request.arguments_json.as_str())
            .map_err(|_| PluginError::domain(ExecuteError::InvalidArguments))?;
        if !arguments.is_object() {
            return Err(PluginError::domain(ExecuteError::InvalidArguments));
        }
        ensure_program_identity(&self.config.program, &ready.program)
            .map_err(PluginError::runtime)?;
        ensure_working_directory_identity(&self.config.working_directory, &ready.working_directory)
            .map_err(PluginError::runtime)?;
        let mut session_slot = ready.session.lock().await;
        if session_slot.is_none() {
            *session_slot = Some(
                connect(&self.config, &ready)
                    .await
                    .map_err(PluginError::runtime)?,
            );
        }
        let session = session_slot.as_mut().expect("session was connected");
        let request_id = session.next_request_id();
        let params = json!({"name": remote_name, "arguments": arguments});
        let cancellation = context.cancellation();
        let outcome = tokio::select! {
            () = cancellation.cancelled() => {
                let _ = session.cancel(request_id, ready.protocol).await;
                Err(RuntimeFailure::Cancelled { request_id: context.request_id() })
            }
            result = session.request_with_id(request_id, "tools/call", params, ready.protocol, self.config.request_timeout_ms) => {
                result
            }
        };
        let response = match outcome {
            Ok(response) => response,
            Err(error) => {
                if let Some(session) = session_slot.take() {
                    session.shutdown().await;
                }
                return Err(PluginError::runtime(error));
            }
        };
        map_tool_result(&response).map_err(PluginError::domain)
    }
}

impl Lifecycle for McpClientPlugin {
    async fn activate(&self, _context: ActivateContext) -> Result<(), RuntimeFailure> {
        let program = canonical_regular_file(&self.config.program, "MCP program")?;
        let working_directory =
            fs::canonicalize(&self.config.working_directory).map_err(|error| {
                invalid_plan(format!("MCP working directory is unavailable: {error}"))
            })?;
        if !working_directory.is_dir() {
            return Err(invalid_plan("MCP working directory is not a directory"));
        }
        let environment = self
            .config
            .environment_allowlist
            .iter()
            .filter_map(|name| env::var(name).ok().map(|value| (name.clone(), value)))
            .collect::<BTreeMap<_, _>>();
        let seed = ReadyClient {
            protocol: Protocol::Legacy,
            program,
            working_directory,
            environment,
            catalog: CatalogResponse { tools: Vec::new() },
            exposed_to_remote: BTreeMap::new(),
            session: Rc::new(Mutex::new(None)),
        };
        let protocol = select_protocol(&self.config, &seed).await?;
        let mut selected = seed.clone();
        selected.protocol = protocol;
        let mut session = connect(&self.config, &selected).await?;
        let remote_tools =
            match list_tools(&mut session, protocol, self.config.startup_timeout_ms).await {
                Ok(tools) => tools,
                Err(error) => {
                    session.shutdown().await;
                    return Err(error);
                }
            };
        let (catalog, exposed_to_remote) =
            match project_catalog(&self.config.tool_namespace, remote_tools) {
                Ok(projected) => projected,
                Err(error) => {
                    session.shutdown().await;
                    return Err(error);
                }
            };
        let session = Rc::new(Mutex::new(Some(session)));
        self.ready.replace(Some(ReadyClient {
            protocol,
            catalog,
            exposed_to_remote,
            session,
            ..seed
        }));
        Ok(())
    }

    async fn deactivate(&self, _context: DeactivateContext) -> Result<(), RuntimeFailure> {
        let ready = self.ready.replace(None);
        if let Some(ready) = ready
            && let Some(session) = ready.session.lock().await.take()
        {
            session.shutdown().await;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct Session {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Session {
    fn spawn(config: &McpClientConfig, ready: &ReadyClient) -> Result<Self, RuntimeFailure> {
        let mut command = Command::new(&ready.program);
        command
            .args(&config.arguments)
            .current_dir(&ready.working_directory)
            .env_clear()
            .envs(&ready.environment)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(process_failure)?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| protocol_failure("MCP process has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| protocol_failure("MCP process has no stdout"))?;
        Ok(Self {
            child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
            next_id: 1,
        })
    }

    fn next_request_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    async fn request(
        &mut self,
        method: &str,
        params: Value,
        protocol: Protocol,
        timeout_ms: u64,
    ) -> Result<Value, RuntimeFailure> {
        let id = self.next_request_id();
        self.request_with_id(id, method, params, protocol, timeout_ms)
            .await
    }

    async fn request_with_id(
        &mut self,
        id: u64,
        method: &str,
        mut params: Value,
        protocol: Protocol,
        timeout_ms: u64,
    ) -> Result<Value, RuntimeFailure> {
        add_modern_metadata(&mut params, protocol);
        self.write(&json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}))
            .await?;
        let receive = self.receive_response(id);
        match tokio::time::timeout(Duration::from_millis(timeout_ms), receive).await {
            Ok(result) => result,
            Err(_) => {
                if method != "initialize" {
                    let _ = self.cancel(id, protocol).await;
                }
                Err(protocol_failure(format!(
                    "MCP request `{method}` timed out"
                )))
            }
        }
    }

    async fn notify(
        &mut self,
        method: &str,
        mut params: Value,
        protocol: Protocol,
    ) -> Result<(), RuntimeFailure> {
        add_modern_metadata(&mut params, protocol);
        self.write(&json!({"jsonrpc":"2.0", "method":method, "params":params}))
            .await
    }

    async fn cancel(&mut self, request_id: u64, protocol: Protocol) -> Result<(), RuntimeFailure> {
        self.notify(
            "notifications/cancelled",
            json!({"requestId": request_id}),
            protocol,
        )
        .await
    }

    async fn write(&mut self, value: &Value) -> Result<(), RuntimeFailure> {
        let mut bytes =
            serde_json::to_vec(value).map_err(|error| protocol_failure(error.to_string()))?;
        if bytes.len() > MAX_MESSAGE_BYTES {
            return Err(protocol_failure(
                "outbound MCP message exceeded the byte limit",
            ));
        }
        bytes.push(b'\n');
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| protocol_failure("MCP stdin is closed"))?;
        stdin.write_all(&bytes).await.map_err(process_failure)?;
        stdin.flush().await.map_err(process_failure)
    }

    async fn receive_response(&mut self, expected_id: u64) -> Result<Value, RuntimeFailure> {
        loop {
            let line = self.read_message_line().await?;
            let message: Value = serde_json::from_slice(&line).map_err(|error| {
                protocol_failure(format!("MCP stdout was not JSON-RPC: {error}"))
            })?;
            if message["jsonrpc"].as_str() != Some("2.0") {
                return Err(protocol_failure("MCP message did not declare JSON-RPC 2.0"));
            }
            if let Some(id) = message.get("id") {
                if message.get("method").is_some() {
                    return Err(protocol_failure(
                        "MCP server sent an unsupported client request",
                    ));
                }
                if id.as_u64() != Some(expected_id) {
                    return Err(protocol_failure(
                        "MCP response ID did not match the request",
                    ));
                }
                return Ok(message);
            }
            if message.get("method").is_none() {
                return Err(protocol_failure("MCP message was not a valid notification"));
            }
        }
    }

    async fn read_message_line(&mut self) -> Result<Vec<u8>, RuntimeFailure> {
        let mut line = Vec::new();
        loop {
            let available = self.stdout.fill_buf().await.map_err(process_failure)?;
            if available.is_empty() {
                return Err(protocol_failure(
                    "MCP process closed stdout before replying",
                ));
            }
            let newline = available.iter().position(|byte| *byte == b'\n');
            let take = newline.unwrap_or(available.len());
            if line.len().saturating_add(take) > MAX_MESSAGE_BYTES {
                return Err(protocol_failure(
                    "inbound MCP message exceeded the byte limit",
                ));
            }
            line.extend_from_slice(&available[..take]);
            let consumed = take + usize::from(newline.is_some());
            self.stdout.consume(consumed);
            if newline.is_some() {
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(line);
            }
        }
    }

    async fn shutdown(mut self) {
        self.stdin.take();
        if tokio::time::timeout(Duration::from_millis(250), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.kill().await;
            let _ = self.child.wait().await;
        }
    }
}

fn add_modern_metadata(params: &mut Value, protocol: Protocol) {
    if protocol != Protocol::Modern {
        return;
    }
    let Some(object) = params.as_object_mut() else {
        return;
    };
    object.insert(
        "_meta".to_owned(),
        json!({
            "io.modelcontextprotocol/protocolVersion": MODERN_VERSION,
            "io.modelcontextprotocol/clientInfo": {"name":"lenso-agent", "version":env!("CARGO_PKG_VERSION")},
            "io.modelcontextprotocol/clientCapabilities": {}
        }),
    );
}

async fn select_protocol(
    config: &McpClientConfig,
    ready: &ReadyClient,
) -> Result<Protocol, RuntimeFailure> {
    match config.protocol {
        ProtocolMode::Modern => Ok(Protocol::Modern),
        ProtocolMode::Legacy => Ok(Protocol::Legacy),
        ProtocolMode::Auto => {
            let mut probe = Session::spawn(config, ready)?;
            let response = probe
                .request(
                    "server/discover",
                    json!({}),
                    Protocol::Modern,
                    config.startup_timeout_ms,
                )
                .await;
            probe.shutdown().await;
            match response {
                Ok(message) if message.get("result").is_some() => {
                    if !message["result"]["capabilities"]["tools"].is_object() {
                        return Err(protocol_failure(
                            "MCP server does not declare the Tools capability",
                        ));
                    }
                    let versions = message["result"]["supportedVersions"]
                        .as_array()
                        .ok_or_else(|| {
                            protocol_failure("MCP discovery omitted supportedVersions")
                        })?;
                    if versions
                        .iter()
                        .any(|version| version.as_str() == Some(MODERN_VERSION))
                    {
                        Ok(Protocol::Modern)
                    } else {
                        Err(protocol_failure(
                            "MCP server has no mutually supported modern protocol version",
                        ))
                    }
                }
                Ok(message) if message["error"]["code"].as_i64() == Some(-32022) => {
                    let supported = message["error"]["data"]["supported"]
                        .as_array()
                        .ok_or_else(|| {
                            protocol_failure("MCP version error omitted supported versions")
                        })?;
                    if supported
                        .iter()
                        .any(|version| version.as_str() == Some(MODERN_VERSION))
                    {
                        Ok(Protocol::Modern)
                    } else {
                        Err(protocol_failure(
                            "MCP server has no mutually supported modern protocol version",
                        ))
                    }
                }
                Ok(_) | Err(_) => Ok(Protocol::Legacy),
            }
        }
    }
}

async fn initialize(
    session: &mut Session,
    protocol: Protocol,
    timeout_ms: u64,
) -> Result<(), RuntimeFailure> {
    if protocol == Protocol::Modern {
        return Ok(());
    }
    let response = session
        .request(
            "initialize",
            json!({
                "protocolVersion": LEGACY_VERSION,
                "capabilities": {},
                "clientInfo": {"name":"lenso-agent", "version":env!("CARGO_PKG_VERSION")}
            }),
            protocol,
            timeout_ms,
        )
        .await?;
    let result = rpc_result(&response)?;
    let version = result["protocolVersion"]
        .as_str()
        .ok_or_else(|| protocol_failure("MCP initialize omitted protocolVersion"))?;
    if !SUPPORTED_LEGACY_VERSIONS.contains(&version) {
        return Err(protocol_failure(format!(
            "MCP server selected unsupported legacy protocol version `{version}`"
        )));
    }
    if !result["capabilities"]["tools"].is_object() {
        return Err(protocol_failure(
            "MCP server does not declare the Tools capability",
        ));
    }
    session
        .notify("notifications/initialized", json!({}), protocol)
        .await
}

async fn connect(config: &McpClientConfig, ready: &ReadyClient) -> Result<Session, RuntimeFailure> {
    let mut session = Session::spawn(config, ready)?;
    if let Err(error) = initialize(&mut session, ready.protocol, config.startup_timeout_ms).await {
        session.shutdown().await;
        return Err(error);
    }
    Ok(session)
}

async fn list_tools(
    session: &mut Session,
    protocol: Protocol,
    timeout_ms: u64,
) -> Result<Vec<Value>, RuntimeFailure> {
    let mut tools = Vec::new();
    let mut cursor: Option<String> = None;
    let mut seen_cursors = BTreeSet::new();
    for _ in 0..MAX_PAGES {
        let params = cursor
            .as_ref()
            .map_or_else(|| json!({}), |cursor| json!({"cursor": cursor}));
        let response = session
            .request("tools/list", params, protocol, timeout_ms)
            .await?;
        let result = rpc_result(&response)?;
        let page = result["tools"]
            .as_array()
            .ok_or_else(|| protocol_failure("MCP tools/list omitted tools"))?;
        if tools.len().saturating_add(page.len()) > MAX_TOOLS {
            return Err(protocol_failure("MCP server exposed more than 256 tools"));
        }
        tools.extend(page.iter().cloned());
        cursor = result
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let Some(next) = cursor.as_ref() else {
            return Ok(tools);
        };
        if next.is_empty() || !seen_cursors.insert(next.clone()) {
            return Err(protocol_failure(
                "MCP tools/list returned an invalid pagination cursor",
            ));
        }
    }
    Err(protocol_failure("MCP tools/list exceeded the page limit"))
}

fn project_catalog(
    namespace: &str,
    remote_tools: Vec<Value>,
) -> Result<(CatalogResponse, BTreeMap<String, String>), RuntimeFailure> {
    let mut tools = Vec::with_capacity(remote_tools.len());
    let mut mapping = BTreeMap::new();
    for remote in remote_tools {
        let remote_name = remote["name"]
            .as_str()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| protocol_failure("MCP Tool has no name"))?;
        let normalized_name = normalize_remote_tool_name(remote_name)
            .ok_or_else(|| protocol_failure("MCP Tool name cannot be represented safely"))?;
        let exposed_name = format!("mcp__{namespace}__{normalized_name}");
        if exposed_name.len() > 64
            || mapping
                .insert(exposed_name.clone(), remote_name.to_owned())
                .is_some()
        {
            return Err(protocol_failure(
                "MCP Tool names are duplicate or exceed the Host limit after namespacing",
            ));
        }
        let description = remote
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("");
        if description.len() > 4_096 {
            return Err(protocol_failure(
                "MCP Tool description exceeded the Host limit",
            ));
        }
        let schema = remote
            .get("inputSchema")
            .cloned()
            .unwrap_or_else(|| json!({"type":"object"}));
        if !schema.is_object() {
            return Err(protocol_failure(
                "MCP Tool inputSchema is not a JSON object",
            ));
        }
        let input_schema_json =
            serde_json::to_string(&schema).map_err(|error| protocol_failure(error.to_string()))?;
        if !(2..=MAX_SCHEMA_BYTES).contains(&input_schema_json.len()) {
            return Err(protocol_failure(
                "MCP Tool inputSchema exceeded the Host limit",
            ));
        }
        tools.push(ToolDefinition {
            name: exposed_name,
            description: description.to_owned(),
            input_schema_json: input_schema_json.try_into().expect("validated schema JSON"),
            execution: ToolExecutionClass::Exclusive,
        });
    }
    Ok((CatalogResponse { tools }, mapping))
}

fn normalize_remote_tool_name(name: &str) -> Option<String> {
    let mut normalized = String::with_capacity(name.len());
    let mut previous_was_separator = false;
    for byte in name.bytes() {
        let lowered = byte.to_ascii_lowercase();
        if lowered.is_ascii_lowercase() || lowered.is_ascii_digit() {
            normalized.push(char::from(lowered));
            previous_was_separator = false;
        } else if !previous_was_separator && !normalized.is_empty() {
            normalized.push('_');
            previous_was_separator = true;
        }
    }
    while normalized.ends_with('_') {
        normalized.pop();
    }
    (!normalized.is_empty()).then_some(normalized)
}

fn map_tool_result(response: &Value) -> Result<ExecuteResponse, ExecuteError> {
    let result = rpc_result(response).map_err(|error| {
        execution_failed(
            "mcp_rpc_error",
            &format!("MCP request failed: {error:?}"),
            response,
        )
    })?;
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err(execution_failed(
            "mcp_tool_error",
            "MCP Tool reported an error",
            result,
        ));
    }
    let content = result["content"].as_array().ok_or_else(|| {
        execution_failed("invalid_result", "MCP Tool result omitted content", result)
    })?;
    let all_text = content
        .iter()
        .all(|item| item["type"].as_str() == Some("text") && item["text"].is_string());
    if !all_text {
        return Err(execution_failed(
            "unsupported_content",
            "MCP Tool returned content that the text-only Tool Provider contract cannot represent",
            result,
        ));
    }
    let output = content
        .iter()
        .filter_map(|item| item["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if output.len() > MAX_OUTPUT_BYTES {
        return Err(ExecuteError::OutputLimitExceeded);
    }
    let metadata = json!({
        "mcp": true,
        "structured_content": result.get("structuredContent").cloned().unwrap_or(Value::Null)
    });
    Ok(ExecuteResponse {
        content_type: ContentType::Text,
        content: output,
        metadata_json: metadata
            .to_string()
            .try_into()
            .expect("metadata is valid JSON"),
    })
}

fn rpc_result(message: &Value) -> Result<&Value, RuntimeFailure> {
    let result = message.get("result");
    let error = message.get("error");
    if result.is_some() == error.is_some() {
        return Err(protocol_failure(
            "MCP response must contain exactly one of result or error",
        ));
    }
    if let Some(result) = result {
        return Ok(result);
    }
    if let Some(error) = error {
        return Err(protocol_failure(format!("MCP JSON-RPC error: {error}")));
    }
    unreachable!("exactly one response field was present")
}

fn execution_failed(reason_code: &str, message: &str, details: &Value) -> ExecuteError {
    let details_json = serde_json::to_string(details).unwrap_or_else(|_| "{}".to_owned());
    let bounded_details = if details_json.len() <= MAX_SCHEMA_BYTES {
        details_json
    } else {
        "{\"truncated\":true}".to_owned()
    };
    ExecuteError::ExecutionFailed {
        payload: ExecutionFailedPayload {
            reason_code: reason_code.to_owned(),
            message: message.chars().take(4_096).collect(),
            details_json: bounded_details.try_into().expect("details are valid JSON"),
        },
    }
}

fn ensure_program_identity(configured: &PathBuf, expected: &PathBuf) -> Result<(), RuntimeFailure> {
    let current = canonical_regular_file(configured, "MCP program")?;
    if &current != expected {
        return Err(RuntimeFailure::PluginFailure {
            detail: "configured MCP executable identity changed after activation".to_owned(),
        });
    }
    Ok(())
}

fn ensure_working_directory_identity(
    configured: &PathBuf,
    expected: &PathBuf,
) -> Result<(), RuntimeFailure> {
    let current = fs::canonicalize(configured)
        .map_err(|error| invalid_plan(format!("MCP working directory is unavailable: {error}")))?;
    if &current != expected || !current.is_dir() {
        return Err(RuntimeFailure::PluginFailure {
            detail: "configured MCP working directory identity changed after activation".to_owned(),
        });
    }
    Ok(())
}

fn canonical_regular_file(path: &PathBuf, label: &str) -> Result<PathBuf, RuntimeFailure> {
    let canonical = fs::canonicalize(path)
        .map_err(|error| invalid_plan(format!("{label} is unavailable: {error}")))?;
    let metadata = fs::metadata(&canonical)
        .map_err(|error| invalid_plan(format!("{label} is unavailable: {error}")))?;
    if !metadata.is_file() {
        return Err(invalid_plan(format!("{label} is not a regular file")));
    }
    Ok(canonical)
}

fn invalid_plan(detail: impl Into<String>) -> RuntimeFailure {
    RuntimeFailure::InvalidResolvedPlan {
        detail: detail.into(),
    }
}

fn protocol_failure(detail: impl Into<String>) -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
        detail: detail.into(),
    }
}

fn process_failure(error: impl std::fmt::Display) -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
        detail: format!("MCP stdio process failed: {error}"),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use lenso_kernel::{CancellationToken, InvocationContext};
    use std::process::Command as StdCommand;

    const MODERN_SERVER: &str = r#"
while IFS= read -r line; do
  case "$line" in
    *\"method\":\"server/discover\"*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}}}}'
      ;;
    *\"method\":\"tools/list\"*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"resultType":"complete","tools":[{"name":"ping","description":"Return pong.","inputSchema":{"type":"object","additionalProperties":false}}]}}'
      ;;
    *\"method\":\"tools/call\"*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"resultType":"complete","content":[{"type":"text","text":"pong"}],"isError":false}}'
      ;;
  esac
done
"#;

    const LEGACY_SERVER: &str = r#"
while IFS= read -r line; do
  case "$line" in
    *\"method\":\"server/discover\"*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Method not found"}}'
      ;;
    *\"method\":\"initialize\"*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}}'
      ;;
    *\"method\":\"tools/list\"*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"ping","description":"Return pong.","inputSchema":{"type":"object"}}]}}'
      ;;
    *\"method\":\"tools/call\"*)
      printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"legacy pong"}],"isError":false}}'
      ;;
  esac
done
"#;

    fn config(script: &str, protocol: ProtocolMode) -> McpClientConfig {
        McpClientConfig {
            program: PathBuf::from("/bin/sh"),
            arguments: vec!["-c".to_owned(), script.to_owned()],
            working_directory: env::current_dir().unwrap(),
            environment_allowlist: Vec::new(),
            protocol,
            tool_namespace: "fixture".to_owned(),
            startup_timeout_ms: 1_000,
            request_timeout_ms: 1_000,
        }
    }

    fn seed(config: &McpClientConfig) -> ReadyClient {
        ReadyClient {
            protocol: Protocol::Legacy,
            program: fs::canonicalize(&config.program).unwrap(),
            working_directory: fs::canonicalize(&config.working_directory).unwrap(),
            environment: BTreeMap::new(),
            catalog: CatalogResponse { tools: Vec::new() },
            exposed_to_remote: BTreeMap::new(),
            session: Rc::new(Mutex::new(None)),
        }
    }

    async fn discover(config: &McpClientConfig) -> ReadyClient {
        let mut ready = seed(config);
        ready.protocol = select_protocol(config, &ready).await.unwrap();
        let mut session = Session::spawn(config, &ready).unwrap();
        initialize(&mut session, ready.protocol, config.startup_timeout_ms)
            .await
            .unwrap();
        let remote = list_tools(&mut session, ready.protocol, config.startup_timeout_ms)
            .await
            .unwrap();
        (ready.catalog, ready.exposed_to_remote) =
            project_catalog(&config.tool_namespace, remote).unwrap();
        ready.session = Rc::new(Mutex::new(Some(session)));
        ready
    }

    #[tokio::test(flavor = "current_thread")]
    async fn auto_detects_modern_and_projects_namespaced_tools() {
        let config = config(MODERN_SERVER, ProtocolMode::Auto);
        let ready = discover(&config).await;
        assert_eq!(ready.protocol, Protocol::Modern);
        assert_eq!(ready.catalog.tools.len(), 1);
        assert_eq!(ready.catalog.tools[0].name, "mcp__fixture__ping");
        assert!(matches!(
            ready.catalog.tools[0].execution,
            ToolExecutionClass::Exclusive
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn auto_falls_back_to_a_fresh_legacy_session() {
        let config = config(LEGACY_SERVER, ProtocolMode::Auto);
        let ready = discover(&config).await;
        assert_eq!(ready.protocol, Protocol::Legacy);
        assert_eq!(ready.catalog.tools[0].name, "mcp__fixture__ping");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn projected_provider_invokes_a_real_modern_stdio_server() {
        let config = config(MODERN_SERVER, ProtocolMode::Auto);
        let ready = discover(&config).await;
        let plugin = McpClientPlugin {
            config,
            ready: Rc::new(RefCell::new(Some(ready))),
        };
        let response = plugin
            .execute(
                InvocationContext::new(7, None, CancellationToken::new()),
                ExecuteRequest {
                    name: "mcp__fixture__ping".to_owned(),
                    arguments_json: "{}".to_owned().try_into().unwrap(),
                },
            )
            .await
            .unwrap();
        assert_eq!(response.content, "pong");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_kills_and_reaps_the_stdio_process() {
        let directory = tempfile::tempdir().unwrap();
        let pid_path = directory.path().join("server.pid");
        let script = format!(
            r#"
echo $$ > '{}'
while IFS= read -r line; do
  case "$line" in
    *\"method\":\"tools/call\"*) exec sleep 10 ;;
  esac
done
"#,
            pid_path.display()
        );
        let config = config(&script, ProtocolMode::Modern);
        let mut ready = seed(&config);
        ready.protocol = Protocol::Modern;
        ready.catalog = CatalogResponse {
            tools: vec![ToolDefinition {
                name: "mcp__fixture__wait".to_owned(),
                description: String::new(),
                input_schema_json: "{\"type\":\"object\"}".to_owned().try_into().unwrap(),
                execution: ToolExecutionClass::Exclusive,
            }],
        };
        ready
            .exposed_to_remote
            .insert("mcp__fixture__wait".to_owned(), "wait".to_owned());
        let plugin = McpClientPlugin {
            config,
            ready: Rc::new(RefCell::new(Some(ready))),
        };
        let cancellation = CancellationToken::new();
        let context = InvocationContext::new(9, None, cancellation.clone());
        let execute = plugin.execute(
            context,
            ExecuteRequest {
                name: "mcp__fixture__wait".to_owned(),
                arguments_json: "{}".to_owned().try_into().unwrap(),
            },
        );
        let cancel = async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancellation.cancel();
        };
        let (result, ()) = tokio::join!(execute, cancel);
        assert!(matches!(
            result,
            Err(PluginError::Runtime(RuntimeFailure::Cancelled {
                request_id: 9
            }))
        ));
        let pid = fs::read_to_string(&pid_path).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let status = StdCommand::new("/bin/kill")
            .args(["-0", pid.trim()])
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(
            !status.success(),
            "cancelled MCP process must not remain alive"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn request_timeout_notifies_the_server_before_cleanup() {
        let directory = tempfile::tempdir().unwrap();
        let notice_path = directory.path().join("cancelled");
        let script = format!(
            r#"
while IFS= read -r line; do
  case "$line" in
    *\"method\":\"tools/call\"*) ;;
    *\"method\":\"notifications/cancelled\"*) echo yes > '{}';;
  esac
done
"#,
            notice_path.display()
        );
        let mut config = config(&script, ProtocolMode::Modern);
        config.request_timeout_ms = 50;
        let mut ready = seed(&config);
        ready.protocol = Protocol::Modern;
        ready.catalog = CatalogResponse {
            tools: vec![ToolDefinition {
                name: "mcp__fixture__wait".to_owned(),
                description: String::new(),
                input_schema_json: "{\"type\":\"object\"}".to_owned().try_into().unwrap(),
                execution: ToolExecutionClass::Exclusive,
            }],
        };
        ready
            .exposed_to_remote
            .insert("mcp__fixture__wait".to_owned(), "wait".to_owned());
        let plugin = McpClientPlugin {
            config,
            ready: Rc::new(RefCell::new(Some(ready))),
        };
        let result = plugin
            .execute(
                InvocationContext::new(10, None, CancellationToken::new()),
                ExecuteRequest {
                    name: "mcp__fixture__wait".to_owned(),
                    arguments_json: "{}".to_owned().try_into().unwrap(),
                },
            )
            .await;
        assert!(matches!(result, Err(PluginError::Runtime(_))));
        assert_eq!(fs::read_to_string(notice_path).unwrap().trim(), "yes");
    }

    #[test]
    fn invalid_catalog_and_tool_fail_closed() {
        let duplicate = vec![
            json!({"name":"same-tool","inputSchema":{"type":"object"}}),
            json!({"name":"same.tool","inputSchema":{"type":"object"}}),
        ];
        assert!(project_catalog("fixture", duplicate).is_err());

        let error = map_tool_result(&json!({
            "jsonrpc":"2.0",
            "id":1,
            "result":{"content":[{"type":"text","text":"denied"}],"isError":true}
        }))
        .unwrap_err();
        assert!(matches!(error, ExecuteError::ExecutionFailed { .. }));
    }

    #[test]
    fn descriptor_exposes_only_the_tool_provider_contract() {
        let descriptor: Value = serde_json::from_str(PLUGIN_DESCRIPTOR_JSON).unwrap();
        assert_eq!(descriptor["plugin_id"], "lenso.agent.mcp-client");
        assert_eq!(
            descriptor["provided_capabilities"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            descriptor["provided_capabilities"][0]["capability_id"],
            "lenso.agent.tool-provider@2"
        );
        assert_eq!(descriptor["required_capabilities"], json!([]));
    }
}
