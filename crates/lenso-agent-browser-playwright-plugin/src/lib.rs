//! Stateful browser Tools over a Host-selected Playwright/CDP Process boundary.

use std::path::{Component, Path};

use lenso::prelude::*;
use lenso_capability_agent_process::{
    self as process_contract, CatalogRequest as ProcessCatalogRequest, ProcessRunInvocationError,
    RunError, RunRequest, RunResponse,
};
use lenso_capability_agent_tool_provider::{
    self as tool_contract, CatalogRequest, CatalogResponse, ContentType, ExecuteError,
    ExecuteRequest, ExecuteResponse, ExecutionFailedPayload, ToolDefinition, ToolExecutionClass,
};
use lenso_kernel::RuntimeFailure;

pub const NAVIGATE_TOOL: &str = "browser_navigate";
pub const SNAPSHOT_TOOL: &str = "browser_snapshot";
pub const CLICK_TOOL: &str = "browser_click";
pub const TYPE_TOOL: &str = "browser_type";
pub const SCREENSHOT_TOOL: &str = "browser_screenshot";

const PLAYWRIGHT_DRIVER: &str = r"
const [payload] = process.argv.slice(1);
const request = JSON.parse(payload);
const { chromium } = await import('playwright');
const browser = await chromium.connectOverCDP(request.cdpEndpoint);
const contexts = browser.contexts();
if (contexts.length !== 1) throw new Error('expected exactly one CDP browser context');
const context = contexts[0];
await context.route('**/*', async route => {
  const url = new URL(route.request().url());
  if (request.allowedOrigins.includes(url.origin)) await route.continue();
  else await route.abort('blockedbyclient');
});
const pages = context.pages();
const page = pages.at(-1) ?? await context.newPage();
page.setDefaultTimeout(request.timeoutMs);
let result;
switch (request.action.kind) {
  case 'navigate':
    await page.goto(request.action.url, { waitUntil: 'domcontentloaded' });
    result = { url: page.url(), title: await page.title() };
    break;
  case 'snapshot': {
    const text = await page.locator('body').innerText();
    result = { url: page.url(), title: await page.title(), text: text.slice(0, request.maxSnapshotBytes) };
    break;
  }
  case 'click':
    await page.locator(request.action.selector).click();
    result = { url: page.url(), title: await page.title() };
    break;
  case 'type':
    await page.locator(request.action.selector).fill(request.action.text);
    result = { url: page.url(), title: await page.title() };
    break;
  case 'screenshot':
    await page.screenshot({ path: request.action.path, fullPage: request.action.fullPage });
    result = { url: page.url(), title: await page.title(), path: request.action.path };
    break;
  default: throw new Error('unsupported browser action');
}
if (!request.allowedOrigins.includes(new URL(page.url()).origin)) throw new Error('browser left the allowed origin set');
console.log(JSON.stringify(result));
process.exit(0);
";

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserConfig {
    allowed_origins: Vec<String>,
    cdp_endpoint: String,
    max_snapshot_bytes: usize,
    screenshot_directory: String,
    timeout_ms: u64,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NavigateArguments {
    url: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArguments {}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectorArguments {
    selector: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TypeArguments {
    selector: String,
    text: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ScreenshotArguments {
    name: String,
    #[serde(default)]
    full_page: bool,
}

fn validate_config(config: &BrowserConfig) -> Result<(), RuntimeFailure> {
    if config.allowed_origins.is_empty()
        || config.allowed_origins.len() > 32
        || config
            .allowed_origins
            .iter()
            .any(|origin| normalized_origin(origin).as_deref() != Some(origin))
        || config
            .allowed_origins
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        || !(1..=1_048_576).contains(&config.max_snapshot_bytes)
        || !(1..=120_000).contains(&config.timeout_ms)
        || !safe_relative_path(&config.screenshot_directory)
        || !valid_cdp_endpoint(&config.cdp_endpoint)
    {
        return Err(RuntimeFailure::InvalidResolvedPlan {
            detail: "Playwright browser configuration is invalid or unbounded".to_owned(),
        });
    }
    Ok(())
}

#[lenso::plugin(
    lifecycle,
    configuration_schema = "config.schema.json",
    validate = validate_config
)]
#[derive(Clone, Debug)]
struct PlaywrightBrowser {
    #[config]
    config: BrowserConfig,
    process: Port<process_contract::ProcessClient>,
}

impl Lifecycle for PlaywrightBrowser {
    async fn activate(&self, _context: ActivateContext) -> Result<(), RuntimeFailure> {
        let catalog = self
            .process
            .catalog(ProcessCatalogRequest {})
            .await
            .map_err(|error| RuntimeFailure::PluginFailure {
                detail: format!("Process catalog is unavailable: {error:?}"),
            })?;
        if !catalog
            .programs
            .iter()
            .any(|program| program.name == "node")
        {
            return Err(RuntimeFailure::InvalidResolvedPlan {
                detail: "Playwright Browser requires its Process Provider to authorize `node`"
                    .to_owned(),
            });
        }
        Ok(())
    }
}

#[lenso::provides(tool_contract::ToolProvider)]
impl PlaywrightBrowser {
    fn catalog(
        &self,
        _context: Ctx,
        _request: CatalogRequest,
    ) -> impl std::future::Future<Output = PluginResult<CatalogResponse, tool_contract::CatalogError>>
    {
        let _ = self;
        futures::future::ready(Ok(CatalogResponse {
            tools: tool_definitions(),
        }))
    }

    async fn execute(
        &self,
        context: Ctx,
        request: ExecuteRequest,
    ) -> PluginResult<ExecuteResponse, ExecuteError> {
        let action = action_for(&self.config, &request)?;
        let payload = serde_json::json!({
            "action": action,
            "allowedOrigins": self.config.allowed_origins,
            "cdpEndpoint": self.config.cdp_endpoint,
            "maxSnapshotBytes": self.config.max_snapshot_bytes,
            "timeoutMs": self.config.timeout_ms,
        });
        let response = self
            .process
            .run_with_context(
                context,
                RunRequest {
                    program: "node".to_owned(),
                    arguments: vec![
                        "--input-type=module".to_owned(),
                        "--eval".to_owned(),
                        PLAYWRIGHT_DRIVER.to_owned(),
                        payload.to_string(),
                    ],
                    cwd: ".".to_owned(),
                    timeout_ms: self.config.timeout_ms.to_string(),
                },
            )
            .await
            .map_err(map_process_error)?;
        map_response(request.name.as_str(), response)
    }
}

fn action_for(
    config: &BrowserConfig,
    request: &ExecuteRequest,
) -> PluginResult<serde_json::Value, ExecuteError> {
    match request.name.as_str() {
        NAVIGATE_TOOL => {
            let arguments = decode::<NavigateArguments>(request)?;
            let origin = normalized_origin(&arguments.url)
                .ok_or_else(|| PluginError::domain(ExecuteError::InvalidArguments))?;
            if config.allowed_origins.binary_search(&origin).is_err() {
                return Err(PluginError::domain(ExecuteError::PermissionDenied));
            }
            Ok(serde_json::json!({ "kind": "navigate", "url": arguments.url }))
        }
        SNAPSHOT_TOOL => {
            decode::<EmptyArguments>(request)?;
            Ok(serde_json::json!({ "kind": "snapshot" }))
        }
        CLICK_TOOL => {
            let arguments = decode::<SelectorArguments>(request)?;
            validate_text(&arguments.selector, 1024, true)?;
            Ok(serde_json::json!({ "kind": "click", "selector": arguments.selector }))
        }
        TYPE_TOOL => {
            let arguments = decode::<TypeArguments>(request)?;
            validate_text(&arguments.selector, 1024, true)?;
            validate_text(&arguments.text, 65_536, false)?;
            Ok(serde_json::json!({
                "kind": "type",
                "selector": arguments.selector,
                "text": arguments.text,
            }))
        }
        SCREENSHOT_TOOL => {
            let arguments = decode::<ScreenshotArguments>(request)?;
            if arguments.name.is_empty()
                || arguments.name.len() > 128
                || !arguments
                    .name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            {
                return Err(PluginError::domain(ExecuteError::InvalidArguments));
            }
            let path = format!("{}/{}.png", config.screenshot_directory, arguments.name);
            Ok(serde_json::json!({
                "kind": "screenshot",
                "path": path,
                "fullPage": arguments.full_page,
            }))
        }
        _ => Err(PluginError::domain(ExecuteError::NotFound)),
    }
}

fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        tool(
            NAVIGATE_TOOL,
            "Navigate the attached browser to one explicitly allowed URL.",
            &serde_json::json!({
                "type": "object", "additionalProperties": false,
                "properties": { "url": { "type": "string", "minLength": 1, "maxLength": 4096 } },
                "required": ["url"]
            }),
        ),
        tool(
            SNAPSHOT_TOOL,
            "Read the attached page title, URL, and bounded visible text snapshot.",
            &empty_schema(),
        ),
        tool(
            CLICK_TOOL,
            "Click one CSS selector in the attached page.",
            &serde_json::json!({
                "type": "object", "additionalProperties": false,
                "properties": { "selector": { "type": "string", "minLength": 1, "maxLength": 1024 } },
                "required": ["selector"]
            }),
        ),
        tool(
            TYPE_TOOL,
            "Fill one CSS selector with bounded text in the attached page.",
            &serde_json::json!({
                "type": "object", "additionalProperties": false,
                "properties": {
                    "selector": { "type": "string", "minLength": 1, "maxLength": 1024 },
                    "text": { "type": "string", "maxLength": 65536 }
                },
                "required": ["selector", "text"]
            }),
        ),
        tool(
            SCREENSHOT_TOOL,
            "Capture the attached page into the configured Workspace screenshot directory.",
            &serde_json::json!({
                "type": "object", "additionalProperties": false,
                "properties": {
                    "name": { "type": "string", "pattern": "^[A-Za-z0-9_-]{1,128}$" },
                    "full_page": { "type": "boolean", "default": false }
                },
                "required": ["name"]
            }),
        ),
    ]
}

fn tool(name: &str, description: &str, schema: &serde_json::Value) -> ToolDefinition {
    ToolDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema_json: schema
            .to_string()
            .try_into()
            .expect("schema must be valid JSON"),
        execution: ToolExecutionClass::Exclusive,
    }
}

fn empty_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object", "additionalProperties": false, "properties": {}, "required": []
    })
}

fn normalized_origin(value: &str) -> Option<String> {
    let url = reqwest::Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    Some(url.origin().ascii_serialization())
}

fn valid_cdp_endpoint(value: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(value) else {
        return false;
    };
    matches!(url.scheme(), "http" | "ws")
        && url
            .host_str()
            .is_some_and(|host| matches!(host, "127.0.0.1" | "localhost" | "::1"))
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
}

fn safe_relative_path(value: &str) -> bool {
    let path = Path::new(value);
    !value.is_empty()
        && value.len() <= 1024
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn validate_text(value: &str, maximum: usize, nonempty: bool) -> PluginResult<(), ExecuteError> {
    if value.len() > maximum || value.contains('\0') || (nonempty && value.trim().is_empty()) {
        return Err(PluginError::domain(ExecuteError::InvalidArguments));
    }
    Ok(())
}

fn decode<T: serde::de::DeserializeOwned>(
    request: &ExecuteRequest,
) -> PluginResult<T, ExecuteError> {
    serde_json::from_str(request.arguments_json.as_str())
        .map_err(|_| PluginError::domain(ExecuteError::InvalidArguments))
}

fn map_response(
    tool_name: &str,
    response: RunResponse,
) -> PluginResult<ExecuteResponse, ExecuteError> {
    if response.exit_code != "0" {
        return Err(PluginError::domain(execution_failed(
            "browser_failed",
            &response.stderr,
        )));
    }
    Ok(ExecuteResponse {
        content: response.stdout,
        content_type: ContentType::Text,
        metadata_json:
            serde_json::json!({ "tool": tool_name, "duration_ms": response.duration_ms })
                .to_string()
                .try_into()
                .expect("metadata must be valid JSON"),
    })
}

fn map_process_error(error: ProcessRunInvocationError) -> PluginError<ExecuteError> {
    match error {
        ProcessRunInvocationError::Domain(error) => PluginError::domain(match error {
            RunError::InvalidRequest => ExecuteError::InvalidArguments,
            RunError::ProgramNotAllowed | RunError::InvalidWorkingDirectory => {
                ExecuteError::PermissionDenied
            }
            RunError::OutputLimitExceeded => ExecuteError::OutputLimitExceeded,
            RunError::Timeout => execution_failed("browser_timeout", "Browser action timed out."),
            RunError::Terminated => {
                execution_failed("browser_terminated", "Browser action was terminated.")
            }
            RunError::Unknown(_) => execution_failed("browser_unknown", "Browser action failed."),
        }),
        ProcessRunInvocationError::Runtime(error) => PluginError::runtime(error),
    }
}

fn execution_failed(reason_code: &str, detail: &str) -> ExecuteError {
    ExecuteError::ExecutionFailed {
        payload: ExecutionFailedPayload {
            reason_code: reason_code.to_owned(),
            message: "Browser execution failed.".to_owned(),
            details_json: serde_json::json!({ "detail": detail })
                .to_string()
                .try_into()
                .expect("details must be valid JSON"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> BrowserConfig {
        BrowserConfig {
            allowed_origins: vec!["https://example.com".to_owned()],
            cdp_endpoint: "http://127.0.0.1:9222".to_owned(),
            max_snapshot_bytes: 65_536,
            screenshot_directory: ".lenso/browser".to_owned(),
            timeout_ms: 30_000,
        }
    }

    fn request(name: &str, arguments: &serde_json::Value) -> ExecuteRequest {
        ExecuteRequest {
            name: name.to_owned(),
            arguments_json: arguments.to_string().try_into().unwrap(),
        }
    }

    #[test]
    fn navigation_requires_an_allowed_origin() {
        assert!(
            action_for(
                &config(),
                &request(
                    NAVIGATE_TOOL,
                    &serde_json::json!({ "url": "https://example.com/docs" })
                )
            )
            .is_ok()
        );
        assert!(
            action_for(
                &config(),
                &request(
                    NAVIGATE_TOOL,
                    &serde_json::json!({ "url": "https://other.example/docs" })
                )
            )
            .is_err()
        );
    }

    #[test]
    fn screenshot_path_is_derived_from_a_safe_name() {
        let action = action_for(
            &config(),
            &request(SCREENSHOT_TOOL, &serde_json::json!({ "name": "proof-1" })),
        )
        .unwrap();
        assert_eq!(action["path"], ".lenso/browser/proof-1.png");
        assert!(
            action_for(
                &config(),
                &request(SCREENSHOT_TOOL, &serde_json::json!({ "name": "../escape" }))
            )
            .is_err()
        );
    }
}
