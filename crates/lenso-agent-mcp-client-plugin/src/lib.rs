//! Bounded, profile-scoped MCP client projected as Agent Tools.

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::PathBuf,
    process::Stdio,
    rc::Rc,
    time::Duration,
};

use base64::Engine as _;
use futures::StreamExt;
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
struct McpClientConfig {
    #[serde(flatten)]
    transport: TransportConfig,
    protocol: ProtocolMode,
    tool_namespace: String,
    startup_timeout_ms: u64,
    request_timeout_ms: u64,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case")]
enum TransportConfig {
    Stdio {
        program: PathBuf,
        arguments: Vec<String>,
        working_directory: PathBuf,
        environment_allowlist: Vec<String>,
    },
    StreamableHttp {
        endpoint: String,
        authorization_environment: Option<String>,
    },
}

#[derive(Clone, Debug)]
struct ReadyClient {
    protocol: Protocol,
    transport: ReadyTransport,
    catalog: Rc<RefCell<CatalogResponse>>,
    exposed_to_remote: Rc<RefCell<BTreeMap<String, String>>>,
    session: Rc<Mutex<Option<Session>>>,
}

#[derive(Clone, Debug)]
enum ReadyTransport {
    Stdio {
        program: PathBuf,
        working_directory: PathBuf,
        environment: BTreeMap<String, String>,
    },
    StreamableHttp {
        endpoint: reqwest::Url,
        authorization: Option<String>,
        client: reqwest::Client,
    },
}

fn validate_config(config: &McpClientConfig) -> Result<(), RuntimeFailure> {
    let namespace_bytes = config.tool_namespace.as_bytes();
    let namespace_valid = matches!(namespace_bytes.first(), Some(b'a'..=b'z'))
        && namespace_bytes.len() <= 32
        && namespace_bytes[1..]
            .iter()
            .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_'));
    let transport_valid = match &config.transport {
        TransportConfig::Stdio {
            program,
            arguments,
            working_directory,
            environment_allowlist,
        } => {
            let arguments_bytes = arguments.iter().map(String::len).sum::<usize>();
            program.is_absolute()
                && !working_directory.as_os_str().is_empty()
                && arguments.len() <= 64
                && arguments_bytes <= 131_072
                && arguments.iter().all(|argument| argument.len() <= 16_384)
                && valid_environment_names(environment_allowlist)
        }
        TransportConfig::StreamableHttp {
            endpoint,
            authorization_environment,
        } => {
            matches!(config.protocol, ProtocolMode::Auto | ProtocolMode::Modern)
                && reqwest::Url::parse(endpoint).is_ok_and(|url| {
                    url.scheme() == "https"
                        || (url.scheme() == "http"
                            && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")))
                })
                && authorization_environment
                    .as_ref()
                    .is_none_or(|name| valid_environment_names(std::slice::from_ref(name)))
        }
    };
    if !transport_valid
        || !namespace_valid
        || !(1..=60_000).contains(&config.startup_timeout_ms)
        || !(1..=3_600_000).contains(&config.request_timeout_ms)
    {
        return Err(invalid_plan(
            "MCP configuration requires a safe stdio or Streamable HTTP transport, a Tool namespace, and bounded timeouts",
        ));
    }
    Ok(())
}

fn valid_environment_names(names: &[String]) -> bool {
    names.len() <= 64
        && names.iter().all(|name| {
            let mut bytes = name.bytes();
            name.len() <= 128
                && bytes
                    .next()
                    .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
                && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        })
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
    async fn catalog(
        &self,
        _context: Ctx,
        _request: CatalogRequest,
    ) -> PluginResult<CatalogResponse, tool_contract::CatalogError> {
        let ready = self
            .ready
            .borrow()
            .as_ref()
            .cloned()
            .ok_or(RuntimeFailure::Unavailable {
                capability: tool_contract::CAPABILITY_ID,
            })
            .map_err(PluginError::runtime)?;
        refresh_catalog(&self.config, &ready)
            .await
            .map_err(PluginError::runtime)?;
        Ok(ready.catalog.borrow().clone())
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
        let Some(remote_name) = ready.exposed_to_remote.borrow().get(&request.name).cloned() else {
            return Err(PluginError::domain(ExecuteError::NotFound));
        };
        let arguments: Value = serde_json::from_str(request.arguments_json.as_str())
            .map_err(|_| PluginError::domain(ExecuteError::InvalidArguments))?;
        if !arguments.is_object() {
            return Err(PluginError::domain(ExecuteError::InvalidArguments));
        }
        let headers = if matches!(ready.transport, ReadyTransport::StreamableHttp { .. }) {
            let schema_json = ready
                .catalog
                .borrow()
                .tools
                .iter()
                .find(|tool| tool.name == request.name)
                .map(|tool| tool.input_schema_json.as_str().to_owned())
                .ok_or_else(|| PluginError::domain(ExecuteError::NotFound))?;
            let schema = serde_json::from_str::<Value>(&schema_json).map_err(|error| {
                PluginError::runtime(protocol_failure(format!(
                    "projected MCP Tool schema became invalid: {error}"
                )))
            })?;
            parameter_headers(&schema, &arguments).map_err(PluginError::runtime)?
        } else {
            BTreeMap::new()
        };
        let params = json!({"name": remote_name, "arguments": arguments});
        let response = ready
            .request_with_context(&self.config, &context, "tools/call", params, &headers)
            .await
            .map_err(PluginError::runtime)?;
        map_tool_result(&response).map_err(PluginError::domain)
    }
}

impl ReadyClient {
    async fn request_with_context(
        &self,
        config: &McpClientConfig,
        context: &Ctx,
        method: &str,
        params: Value,
        headers: &BTreeMap<String, String>,
    ) -> Result<Value, RuntimeFailure> {
        match &self.transport {
            ReadyTransport::Stdio {
                program,
                working_directory,
                ..
            } => {
                let TransportConfig::Stdio {
                    program: configured_program,
                    working_directory: configured_directory,
                    ..
                } = &config.transport
                else {
                    return Err(protocol_failure("MCP transport changed after activation"));
                };
                ensure_program_identity(configured_program, program)?;
                ensure_working_directory_identity(configured_directory, working_directory)?;
                let mut session_slot = self.session.lock().await;
                if session_slot.is_none() {
                    *session_slot = Some(connect(config, self).await?);
                }
                let session = session_slot.as_mut().expect("session was connected");
                let request_id = session.next_request_id();
                let cancellation = context.cancellation();
                let outcome = tokio::select! {
                    () = cancellation.cancelled() => {
                        let _ = session.cancel(request_id, self.protocol).await;
                        Err(RuntimeFailure::Cancelled { request_id: context.request_id() })
                    }
                    result = session.request_with_id(request_id, method, params, self.protocol, config.request_timeout_ms) => result,
                };
                if outcome.is_err()
                    && let Some(session) = session_slot.take()
                {
                    session.shutdown().await;
                }
                outcome
            }
            ReadyTransport::StreamableHttp { .. } => {
                http_request(
                    self,
                    context,
                    method,
                    params,
                    headers,
                    config.request_timeout_ms,
                )
                .await
            }
        }
    }
}

impl Lifecycle for McpClientPlugin {
    async fn activate(&self, _context: ActivateContext) -> Result<(), RuntimeFailure> {
        let transport = prepare_transport(&self.config)?;
        let seed = ReadyClient {
            protocol: Protocol::Legacy,
            transport,
            catalog: Rc::new(RefCell::new(CatalogResponse { tools: Vec::new() })),
            exposed_to_remote: Rc::new(RefCell::new(BTreeMap::new())),
            session: Rc::new(Mutex::new(None)),
        };
        let protocol = select_protocol(&self.config, &seed).await?;
        let mut selected = seed.clone();
        selected.protocol = protocol;
        let (remote_tools, session) = match &selected.transport {
            ReadyTransport::Stdio { .. } => {
                let mut session = connect(&self.config, &selected).await?;
                match list_tools_stdio(&mut session, protocol, self.config.startup_timeout_ms).await
                {
                    Ok(tools) => (tools, Some(session)),
                    Err(error) => {
                        session.shutdown().await;
                        return Err(error);
                    }
                }
            }
            ReadyTransport::StreamableHttp { .. } => {
                let tools = list_tools_http(&selected, self.config.startup_timeout_ms).await?;
                (tools, None)
            }
        };
        let enforce_http_headers =
            matches!(selected.transport, ReadyTransport::StreamableHttp { .. });
        let (catalog, exposed_to_remote) = match project_catalog(
            &self.config.tool_namespace,
            remote_tools,
            enforce_http_headers,
        ) {
            Ok(projected) => projected,
            Err(error) => {
                if let Some(session) = session {
                    session.shutdown().await;
                }
                return Err(error);
            }
        };
        let session = Rc::new(Mutex::new(session));
        self.ready.replace(Some(ReadyClient {
            protocol,
            catalog: Rc::new(RefCell::new(catalog)),
            exposed_to_remote: Rc::new(RefCell::new(exposed_to_remote)),
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

async fn refresh_catalog(
    config: &McpClientConfig,
    ready: &ReadyClient,
) -> Result<(), RuntimeFailure> {
    let remote_tools = match &ready.transport {
        ReadyTransport::Stdio { .. } => {
            let mut session_slot = ready.session.lock().await;
            if session_slot.is_none() {
                *session_slot = Some(connect(config, ready).await?);
            }
            let result = list_tools_stdio(
                session_slot.as_mut().expect("session was connected"),
                ready.protocol,
                config.request_timeout_ms,
            )
            .await;
            if result.is_err()
                && let Some(session) = session_slot.take()
            {
                session.shutdown().await;
            }
            result?
        }
        ReadyTransport::StreamableHttp { .. } => {
            list_tools_http(ready, config.request_timeout_ms).await?
        }
    };
    let (catalog, exposed_to_remote) = project_catalog(
        &config.tool_namespace,
        remote_tools,
        matches!(ready.transport, ReadyTransport::StreamableHttp { .. }),
    )?;
    ready.catalog.replace(catalog);
    ready.exposed_to_remote.replace(exposed_to_remote);
    Ok(())
}

fn prepare_transport(config: &McpClientConfig) -> Result<ReadyTransport, RuntimeFailure> {
    match &config.transport {
        TransportConfig::Stdio {
            program,
            working_directory,
            environment_allowlist,
            ..
        } => {
            let program = canonical_regular_file(program, "MCP program")?;
            let working_directory = fs::canonicalize(working_directory).map_err(|error| {
                invalid_plan(format!("MCP working directory is unavailable: {error}"))
            })?;
            if !working_directory.is_dir() {
                return Err(invalid_plan("MCP working directory is not a directory"));
            }
            let environment = environment_allowlist
                .iter()
                .filter_map(|name| env::var(name).ok().map(|value| (name.clone(), value)))
                .collect::<BTreeMap<_, _>>();
            Ok(ReadyTransport::Stdio {
                program,
                working_directory,
                environment,
            })
        }
        TransportConfig::StreamableHttp {
            endpoint,
            authorization_environment,
        } => {
            let endpoint = reqwest::Url::parse(endpoint)
                .map_err(|error| invalid_plan(format!("MCP endpoint is invalid: {error}")))?;
            let authorization = authorization_environment
                .as_ref()
                .map(|name| {
                    env::var(name).map_err(|_| {
                        invalid_plan(format!("MCP authorization environment `{name}` is missing"))
                    })
                })
                .transpose()?;
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|error| {
                    invalid_plan(format!("failed to build MCP HTTP client: {error}"))
                })?;
            Ok(ReadyTransport::StreamableHttp {
                endpoint,
                authorization,
                client,
            })
        }
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
        let TransportConfig::Stdio { arguments, .. } = &config.transport else {
            return Err(protocol_failure(
                "cannot spawn stdio for an HTTP MCP transport",
            ));
        };
        let ReadyTransport::Stdio {
            program,
            working_directory,
            environment,
        } = &ready.transport
        else {
            return Err(protocol_failure(
                "cannot spawn stdio for an HTTP MCP transport",
            ));
        };
        let mut command = Command::new(program);
        command
            .args(arguments)
            .current_dir(working_directory)
            .env_clear()
            .envs(environment)
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
    if matches!(ready.transport, ReadyTransport::StreamableHttp { .. }) {
        let response = http_request_raw(
            ready,
            "server/discover",
            json!({}),
            config.startup_timeout_ms,
        )
        .await?;
        validate_modern_discovery(&response)?;
        return Ok(Protocol::Modern);
    }
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
                    validate_modern_discovery(&message).map(|()| Protocol::Modern)
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

fn validate_modern_discovery(message: &Value) -> Result<(), RuntimeFailure> {
    let result = rpc_result(message)?;
    if !result["capabilities"]["tools"].is_object() {
        return Err(protocol_failure(
            "MCP server does not declare the Tools capability",
        ));
    }
    let versions = result["supportedVersions"]
        .as_array()
        .ok_or_else(|| protocol_failure("MCP discovery omitted supportedVersions"))?;
    if versions
        .iter()
        .any(|version| version.as_str() == Some(MODERN_VERSION))
    {
        Ok(())
    } else {
        Err(protocol_failure(
            "MCP server has no mutually supported modern protocol version",
        ))
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

async fn list_tools_stdio(
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

async fn list_tools_http(
    ready: &ReadyClient,
    timeout_ms: u64,
) -> Result<Vec<Value>, RuntimeFailure> {
    let mut tools = Vec::new();
    let mut cursor: Option<String> = None;
    let mut seen_cursors = BTreeSet::new();
    for _ in 0..MAX_PAGES {
        let params = cursor
            .as_ref()
            .map_or_else(|| json!({}), |cursor| json!({"cursor": cursor}));
        let response = http_request_raw(ready, "tools/list", params, timeout_ms).await?;
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

async fn http_request(
    ready: &ReadyClient,
    context: &Ctx,
    method: &str,
    params: Value,
    headers: &BTreeMap<String, String>,
    timeout_ms: u64,
) -> Result<Value, RuntimeFailure> {
    let cancellation = context.cancellation();
    tokio::select! {
        () = cancellation.cancelled() => Err(RuntimeFailure::Cancelled { request_id: context.request_id() }),
        result = http_request_raw_with_headers(ready, method, params, headers, timeout_ms) => result,
    }
}

async fn http_request_raw(
    ready: &ReadyClient,
    method: &str,
    params: Value,
    timeout_ms: u64,
) -> Result<Value, RuntimeFailure> {
    http_request_raw_with_headers(ready, method, params, &BTreeMap::new(), timeout_ms).await
}

async fn http_request_raw_with_headers(
    ready: &ReadyClient,
    method: &str,
    mut params: Value,
    parameter_headers: &BTreeMap<String, String>,
    timeout_ms: u64,
) -> Result<Value, RuntimeFailure> {
    let ReadyTransport::StreamableHttp {
        endpoint,
        authorization,
        client,
    } = &ready.transport
    else {
        return Err(protocol_failure("MCP HTTP request used a stdio transport"));
    };
    add_modern_metadata(&mut params, Protocol::Modern);
    let request_id = 1_u64;
    let body = json!({"jsonrpc":"2.0", "id":request_id, "method":method, "params":params});
    let mut request = client
        .post(endpoint.clone())
        .header(
            reqwest::header::ACCEPT,
            "application/json, text/event-stream",
        )
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header("MCP-Protocol-Version", MODERN_VERSION)
        .header("Mcp-Method", method)
        .timeout(Duration::from_millis(timeout_ms));
    if matches!(method, "tools/call" | "resources/read" | "prompts/get")
        && let Some(name) = body["params"]
            .get(if method == "resources/read" {
                "uri"
            } else {
                "name"
            })
            .and_then(Value::as_str)
    {
        request = request.header("Mcp-Name", encode_header_value(name));
    }
    if let Some(authorization) = authorization {
        request = request.header(reqwest::header::AUTHORIZATION, authorization);
    }
    for (name, value) in parameter_headers {
        request = request.header(name, value);
    }
    let response = request.json(&body).send().await.map_err(http_failure)?;
    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_owned();
    let bytes = read_http_body(response).await?;
    if !status.is_success() {
        return Err(protocol_failure(format!(
            "MCP HTTP request `{method}` failed with {status}: {}",
            String::from_utf8_lossy(&bytes)
        )));
    }
    match content_type.as_str() {
        "application/json" => parse_http_json(&bytes, request_id),
        "text/event-stream" => parse_sse_response(&bytes, request_id),
        _ => Err(protocol_failure(format!(
            "MCP HTTP response used unsupported Content-Type `{content_type}`"
        ))),
    }
}

async fn read_http_body(response: reqwest::Response) -> Result<Vec<u8>, RuntimeFailure> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(http_failure)?;
        if body.len().saturating_add(chunk.len()) > MAX_MESSAGE_BYTES {
            return Err(protocol_failure(
                "MCP HTTP response exceeded the byte limit",
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn parse_http_json(bytes: &[u8], expected_id: u64) -> Result<Value, RuntimeFailure> {
    let message = serde_json::from_slice::<Value>(bytes).map_err(|error| {
        protocol_failure(format!("MCP HTTP response was invalid JSON: {error}"))
    })?;
    validate_response_id(message, expected_id)
}

fn parse_sse_response(bytes: &[u8], expected_id: u64) -> Result<Value, RuntimeFailure> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| protocol_failure("MCP SSE response was not UTF-8"))?;
    let normalized = text.replace("\r\n", "\n");
    for event in normalized.split("\n\n") {
        let data = event
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim_start)
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() {
            continue;
        }
        let message = serde_json::from_str::<Value>(&data)
            .map_err(|error| protocol_failure(format!("MCP SSE data was invalid JSON: {error}")))?;
        if message.get("id").and_then(Value::as_u64) == Some(expected_id) {
            return validate_response_id(message, expected_id);
        }
    }
    Err(protocol_failure(
        "MCP SSE stream ended without a final response",
    ))
}

fn validate_response_id(message: Value, expected_id: u64) -> Result<Value, RuntimeFailure> {
    if message["jsonrpc"].as_str() != Some("2.0")
        || message.get("id").and_then(Value::as_u64) != Some(expected_id)
    {
        return Err(protocol_failure(
            "MCP HTTP response ID did not match the request",
        ));
    }
    Ok(message)
}

fn encode_header_value(value: &str) -> String {
    let bytes = value.as_bytes();
    let safe = !value.starts_with("=?base64?")
        && !value.ends_with("?=")
        && value.trim() == value
        && bytes
            .iter()
            .all(|byte| matches!(byte, 0x20..=0x7e) || *byte == b'\t');
    if safe {
        value.to_owned()
    } else {
        format!(
            "=?base64?{}?=",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
    }
}

fn http_failure(error: impl std::fmt::Display) -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
        detail: format!("MCP Streamable HTTP failed: {error}"),
    }
}

fn project_catalog(
    namespace: &str,
    remote_tools: Vec<Value>,
    enforce_http_headers: bool,
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
        if enforce_http_headers {
            collect_header_bindings(&schema)?;
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

#[derive(Clone, Debug)]
struct HeaderBinding {
    header: String,
    path: Vec<String>,
    value_type: String,
}

fn collect_header_bindings(schema: &Value) -> Result<Vec<HeaderBinding>, RuntimeFailure> {
    let mut bindings = Vec::new();
    collect_header_bindings_at(schema, &mut Vec::new(), false, &mut bindings)?;
    let mut names = BTreeSet::new();
    if bindings
        .iter()
        .any(|binding| !names.insert(binding.header.to_ascii_lowercase()))
    {
        return Err(protocol_failure(
            "MCP Tool inputSchema contains duplicate x-mcp-header names",
        ));
    }
    Ok(bindings)
}

fn collect_header_bindings_at(
    schema: &Value,
    path: &mut Vec<String>,
    is_property: bool,
    bindings: &mut Vec<HeaderBinding>,
) -> Result<(), RuntimeFailure> {
    let Some(object) = schema.as_object() else {
        return Ok(());
    };
    if let Some(annotation) = object.get("x-mcp-header") {
        let name = annotation.as_str().filter(|name| valid_header_token(name));
        let value_type = object.get("type").and_then(Value::as_str);
        if !is_property
            || name.is_none()
            || !matches!(value_type, Some("string" | "integer" | "boolean"))
        {
            return Err(protocol_failure(
                "MCP Tool inputSchema contains an invalid x-mcp-header annotation",
            ));
        }
        bindings.push(HeaderBinding {
            header: format!("Mcp-Param-{}", name.expect("validated")),
            path: path.clone(),
            value_type: value_type.expect("validated").to_owned(),
        });
    }
    for (key, value) in object {
        if key == "properties" {
            let properties = value.as_object().ok_or_else(|| {
                protocol_failure("MCP Tool inputSchema properties is not an object")
            })?;
            for (name, property) in properties {
                path.push(name.clone());
                collect_header_bindings_at(property, path, true, bindings)?;
                path.pop();
            }
        } else if key != "x-mcp-header" && contains_header_annotation(value) {
            return Err(protocol_failure(
                "MCP Tool inputSchema places x-mcp-header outside a reachable property",
            ));
        }
    }
    Ok(())
}

fn contains_header_annotation(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            object.contains_key("x-mcp-header") || object.values().any(contains_header_annotation)
        }
        Value::Array(values) => values.iter().any(contains_header_annotation),
        _ => false,
    }
}

fn valid_header_token(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn parameter_headers(
    schema: &Value,
    arguments: &Value,
) -> Result<BTreeMap<String, String>, RuntimeFailure> {
    let mut headers = BTreeMap::new();
    for binding in collect_header_bindings(schema)? {
        let value = binding
            .path
            .iter()
            .try_fold(arguments, |value, segment| value.get(segment));
        let Some(value) = value.filter(|value| !value.is_null()) else {
            continue;
        };
        let encoded = match binding.value_type.as_str() {
            "string" => value.as_str().map(encode_header_value),
            "integer" => safe_json_integer(value).map(|value| value.to_string()),
            "boolean" => value.as_bool().map(|value| value.to_string()),
            _ => None,
        }
        .ok_or_else(|| {
            protocol_failure("MCP Tool argument for x-mcp-header has the wrong primitive type")
        })?;
        headers.insert(binding.header, encoded);
    }
    Ok(headers)
}

fn safe_json_integer(value: &Value) -> Option<i64> {
    const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;
    value
        .as_i64()
        .filter(|value| (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(value))
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
    use axum::{
        Json, Router,
        body::Body,
        http::{HeaderMap, StatusCode},
        response::{IntoResponse, Response},
        routing::post,
    };
    use lenso_kernel::{CancellationToken, InvocationContext};
    use std::process::Command as StdCommand;

    const MODERN_SERVER: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *\"method\":\"server/discover\"*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}}}}\n' "$id"
      ;;
    *\"method\":\"tools/list\"*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"resultType":"complete","tools":[{"name":"ping","description":"Return pong.","inputSchema":{"type":"object","additionalProperties":false}}]}}\n' "$id"
      ;;
    *\"method\":\"tools/call\"*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"resultType":"complete","content":[{"type":"text","text":"pong"}],"isError":false}}\n' "$id"
      ;;
  esac
done
"#;

    const LEGACY_SERVER: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *\"method\":\"server/discover\"*)
      printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32601,"message":"Method not found"}}\n' "$id"
      ;;
    *\"method\":\"initialize\"*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}}\n' "$id"
      ;;
    *\"method\":\"tools/list\"*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"ping","description":"Return pong.","inputSchema":{"type":"object"}}]}}\n' "$id"
      ;;
    *\"method\":\"tools/call\"*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"legacy pong"}],"isError":false}}\n' "$id"
      ;;
  esac
done
"#;

    async fn http_fixture(headers: HeaderMap, Json(body): Json<Value>) -> Response {
        let method = body["method"].as_str().unwrap_or("");
        if headers
            .get("mcp-protocol-version")
            .and_then(|value| value.to_str().ok())
            != Some(MODERN_VERSION)
            || headers
                .get("mcp-method")
                .and_then(|value| value.to_str().ok())
                != Some(method)
        {
            return StatusCode::BAD_REQUEST.into_response();
        }
        match method {
            "server/discover" => Json(json!({
                "jsonrpc":"2.0", "id":1,
                "result":{"resultType":"complete","supportedVersions":[MODERN_VERSION],"capabilities":{"tools":{}}}
            }))
            .into_response(),
            "tools/list" => Json(json!({
                "jsonrpc":"2.0", "id":1,
                "result":{"resultType":"complete","tools":[{"name":"ping","description":"Return pong.","inputSchema":{"type":"object","properties":{"region":{"type":"string","x-mcp-header":"Region"}},"required":["region"],"additionalProperties":false}}]}
            }))
            .into_response(),
            "tools/call" => {
                if headers
                    .get("mcp-name")
                    .and_then(|value| value.to_str().ok())
                    != Some("ping")
                    || headers
                        .get("mcp-param-region")
                        .and_then(|value| value.to_str().ok())
                        != Some("us-west1")
                {
                    return StatusCode::BAD_REQUEST.into_response();
                }
                Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(Body::from(
                        "event: message\r\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{}}\r\n\r\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"resultType\":\"complete\",\"content\":[{\"type\":\"text\",\"text\":\"http pong\"}],\"isError\":false}}\r\n\r\n",
                    ))
                    .unwrap()
            }
            _ => StatusCode::NOT_FOUND.into_response(),
        }
    }

    async fn http_ready() -> (McpClientConfig, ReadyClient, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/mcp", post(http_fixture)))
                .await
                .unwrap();
        });
        let config = McpClientConfig {
            transport: TransportConfig::StreamableHttp {
                endpoint: format!("http://{address}/mcp"),
                authorization_environment: None,
            },
            protocol: ProtocolMode::Modern,
            tool_namespace: "fixture".to_owned(),
            startup_timeout_ms: 1_000,
            request_timeout_ms: 1_000,
        };
        let ready = ReadyClient {
            protocol: Protocol::Modern,
            transport: prepare_transport(&config).unwrap(),
            catalog: Rc::new(RefCell::new(CatalogResponse { tools: Vec::new() })),
            exposed_to_remote: Rc::new(RefCell::new(BTreeMap::new())),
            session: Rc::new(Mutex::new(None)),
        };
        (config, ready, server)
    }

    fn config(script: &str, protocol: ProtocolMode) -> McpClientConfig {
        McpClientConfig {
            transport: TransportConfig::Stdio {
                program: PathBuf::from("/bin/sh"),
                arguments: vec!["-c".to_owned(), script.to_owned()],
                working_directory: env::current_dir().unwrap(),
                environment_allowlist: Vec::new(),
            },
            protocol,
            tool_namespace: "fixture".to_owned(),
            startup_timeout_ms: 1_000,
            request_timeout_ms: 1_000,
        }
    }

    fn seed(config: &McpClientConfig) -> ReadyClient {
        let TransportConfig::Stdio {
            program,
            working_directory,
            ..
        } = &config.transport
        else {
            unreachable!()
        };
        ReadyClient {
            protocol: Protocol::Legacy,
            transport: ReadyTransport::Stdio {
                program: fs::canonicalize(program).unwrap(),
                working_directory: fs::canonicalize(working_directory).unwrap(),
                environment: BTreeMap::new(),
            },
            catalog: Rc::new(RefCell::new(CatalogResponse { tools: Vec::new() })),
            exposed_to_remote: Rc::new(RefCell::new(BTreeMap::new())),
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
        let remote = list_tools_stdio(&mut session, ready.protocol, config.startup_timeout_ms)
            .await
            .unwrap();
        let (catalog, mapping) = project_catalog(&config.tool_namespace, remote, false).unwrap();
        ready.catalog.replace(catalog);
        ready.exposed_to_remote.replace(mapping);
        ready.session = Rc::new(Mutex::new(Some(session)));
        ready
    }

    #[tokio::test(flavor = "current_thread")]
    async fn streamable_http_sends_required_headers_and_accepts_sse() {
        let (config, ready, server) = http_ready().await;
        assert_eq!(
            select_protocol(&config, &ready).await.unwrap(),
            Protocol::Modern
        );
        let remote = list_tools_http(&ready, config.startup_timeout_ms)
            .await
            .unwrap();
        let (catalog, mapping) = project_catalog(&config.tool_namespace, remote, true).unwrap();
        assert_eq!(catalog.tools[0].name, "mcp__fixture__ping");
        assert_eq!(mapping["mcp__fixture__ping"], "ping");
        let schema =
            serde_json::from_str::<Value>(catalog.tools[0].input_schema_json.as_str()).unwrap();
        let arguments = json!({"region":"us-west1"});
        let headers = parameter_headers(&schema, &arguments).unwrap();
        let response = http_request_raw_with_headers(
            &ready,
            "tools/call",
            json!({"name":"ping","arguments":arguments}),
            &headers,
            config.request_timeout_ms,
        )
        .await
        .unwrap();
        assert_eq!(map_tool_result(&response).unwrap().content, "http pong");
        assert_eq!(
            encode_header_value("Hello, 世界"),
            "=?base64?SGVsbG8sIOS4lueVjA==?="
        );
        server.abort();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn auto_detects_modern_and_projects_namespaced_tools() {
        let config = config(MODERN_SERVER, ProtocolMode::Auto);
        let ready = discover(&config).await;
        assert_eq!(ready.protocol, Protocol::Modern);
        assert_eq!(ready.catalog.borrow().tools.len(), 1);
        assert_eq!(ready.catalog.borrow().tools[0].name, "mcp__fixture__ping");
        assert!(matches!(
            ready.catalog.borrow().tools[0].execution,
            ToolExecutionClass::Exclusive
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn auto_falls_back_to_a_fresh_legacy_session() {
        let config = config(LEGACY_SERVER, ProtocolMode::Auto);
        let ready = discover(&config).await;
        assert_eq!(ready.protocol, Protocol::Legacy);
        assert_eq!(ready.catalog.borrow().tools[0].name, "mcp__fixture__ping");
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
        ready.catalog.replace(CatalogResponse {
            tools: vec![ToolDefinition {
                name: "mcp__fixture__wait".to_owned(),
                description: String::new(),
                input_schema_json: "{\"type\":\"object\"}".to_owned().try_into().unwrap(),
                execution: ToolExecutionClass::Exclusive,
            }],
        });
        ready
            .exposed_to_remote
            .borrow_mut()
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
        ready.catalog.replace(CatalogResponse {
            tools: vec![ToolDefinition {
                name: "mcp__fixture__wait".to_owned(),
                description: String::new(),
                input_schema_json: "{\"type\":\"object\"}".to_owned().try_into().unwrap(),
                execution: ToolExecutionClass::Exclusive,
            }],
        });
        ready
            .exposed_to_remote
            .borrow_mut()
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
        assert!(project_catalog("fixture", duplicate, false).is_err());

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
