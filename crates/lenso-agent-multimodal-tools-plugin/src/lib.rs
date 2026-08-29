//! Root-bounded image understanding and audio transcription Tools.

use std::{
    path::{Component, Path, PathBuf},
    time::Duration,
};

use base64::Engine as _;
use lenso::prelude::*;
use lenso_capability_agent_tool_provider::{
    self as tool_contract, CatalogRequest, CatalogResponse, ContentType, ExecuteError,
    ExecuteRequest, ExecuteResponse, ExecutionFailedPayload, ToolDefinition, ToolExecutionClass,
};
use lenso_capability_secrets::{self as secrets_contract, ResolveRequest};
use lenso_kernel::RuntimeFailure;

pub const READ_IMAGE_TOOL: &str = "read_image";
pub const TRANSCRIBE_AUDIO_TOOL: &str = "transcribe_audio";

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MultimodalConfig {
    api_key_ref: String,
    audio_model: String,
    base_url: String,
    image_model: String,
    max_file_bytes: usize,
    root: String,
    timeout_ms: u64,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MediaArguments {
    path: String,
    #[serde(default)]
    prompt: Option<String>,
}

fn validate_config(config: &MultimodalConfig) -> Result<(), RuntimeFailure> {
    if config.api_key_ref.trim().is_empty()
        || !valid_model(&config.image_model)
        || !valid_model(&config.audio_model)
        || !(1..=20 * 1024 * 1024).contains(&config.max_file_bytes)
        || !(1..=120_000).contains(&config.timeout_ms)
        || config.root.is_empty()
    {
        return Err(invalid_plan(
            "Multimodal Tool configuration is invalid or unbounded",
        ));
    }
    let endpoint = endpoint(config)?;
    let secure = endpoint.scheme() == "https";
    let loopback = endpoint.scheme() == "http"
        && endpoint
            .host_str()
            .is_some_and(|host| matches!(host, "127.0.0.1" | "localhost" | "::1"));
    if (!secure && !loopback)
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
    {
        return Err(invalid_plan(
            "Multimodal base_url must use HTTPS or loopback HTTP without credentials",
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
struct MultimodalTools {
    #[config]
    config: MultimodalConfig,
    secrets: Port<secrets_contract::SecretsClient>,
}

impl Lifecycle for MultimodalTools {
    #[allow(clippy::unused_async_trait_impl)]
    async fn prepare(&self, _context: PrepareContext) -> Result<(), RuntimeFailure> {
        validate_config(&self.config)
    }
}

#[lenso::provides(tool_contract::ToolProvider)]
impl MultimodalTools {
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
        let arguments = decode::<MediaArguments>(&request)?;
        validate_prompt(arguments.prompt.as_deref())?;
        let (path, media_type, format) =
            resolve_media(&self.config, &arguments.path, request.name.as_str())?;
        let bytes = std::fs::read(&path).map_err(|error| {
            PluginError::domain(match error.kind() {
                std::io::ErrorKind::NotFound => ExecuteError::NotFound,
                std::io::ErrorKind::PermissionDenied => ExecuteError::PermissionDenied,
                _ => execution_failed("media_read_failed", "Media file could not be read."),
            })
        })?;
        if bytes.is_empty() || bytes.len() > self.config.max_file_bytes {
            return Err(PluginError::domain(ExecuteError::OutputLimitExceeded));
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        let credential = self
            .secrets
            .resolve_with_context(
                context,
                ResolveRequest {
                    reference: self.config.api_key_ref.clone(),
                },
            )
            .await
            .map_err(|_| {
                PluginError::domain(execution_failed(
                    "authentication_required",
                    "Configured multimodal credential is unavailable.",
                ))
            })?;
        let body = request_body(
            &self.config,
            request.name.as_str(),
            arguments.prompt.as_deref(),
            media_type,
            format,
            &encoded,
        );
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_millis(self.config.timeout_ms))
            .build()
            .map_err(|_| {
                PluginError::domain(execution_failed(
                    "multimodal_client_unavailable",
                    "Multimodal HTTP client is unavailable.",
                ))
            })?;
        let response = client
            .post(endpoint(&self.config).map_err(PluginError::runtime)?)
            .bearer_auth(credential.value)
            .json(&body)
            .send()
            .await
            .map_err(|_| {
                PluginError::domain(execution_failed(
                    "multimodal_request_failed",
                    "Multimodal provider request failed.",
                ))
            })?;
        if !response.status().is_success() {
            return Err(PluginError::domain(execution_failed(
                "multimodal_provider_rejected",
                "Multimodal provider rejected the request.",
            )));
        }
        let response: serde_json::Value = response.json().await.map_err(|_| {
            PluginError::domain(execution_failed(
                "multimodal_response_invalid",
                "Multimodal provider returned invalid JSON.",
            ))
        })?;
        let derived_text = response["choices"][0]["message"]["content"]
            .as_str()
            .filter(|content| !content.is_empty() && content.len() <= 1_048_576)
            .ok_or_else(|| {
                PluginError::domain(execution_failed(
                    "multimodal_response_invalid",
                    "Multimodal provider returned no bounded text result.",
                ))
            })?
            .to_owned();
        Ok(ExecuteResponse {
            content: derived_text,
            content_type: ContentType::Text,
            metadata_json: serde_json::json!({
                "tool": request.name,
                "path": arguments.path,
                "media_type": media_type,
                "provider_origin": endpoint(&self.config).map_err(PluginError::runtime)?.origin().ascii_serialization(),
            })
            .to_string()
            .try_into()
            .expect("Multimodal metadata must be valid JSON"),
        })
    }
}

fn request_body(
    config: &MultimodalConfig,
    tool: &str,
    prompt: Option<&str>,
    media_type: &str,
    format: &str,
    data: &str,
) -> serde_json::Value {
    if tool == READ_IMAGE_TOOL {
        serde_json::json!({
            "model": config.image_model,
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": prompt.unwrap_or("Describe this image precisely and mention any visible text.") },
                    { "type": "image_url", "image_url": { "url": format!("data:{media_type};base64,{data}") } }
                ]
            }]
        })
    } else {
        serde_json::json!({
            "model": config.audio_model,
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": prompt.unwrap_or("Transcribe this audio faithfully.") },
                    { "type": "input_audio", "input_audio": { "data": data, "format": format } }
                ]
            }]
        })
    }
}

fn resolve_media<'a>(
    config: &MultimodalConfig,
    requested: &str,
    tool: &str,
) -> PluginResult<(PathBuf, &'a str, &'a str), ExecuteError> {
    if !safe_relative_path(requested) {
        return Err(PluginError::domain(ExecuteError::InvalidArguments));
    }
    let root = std::fs::canonicalize(&config.root)
        .map_err(|_| PluginError::domain(ExecuteError::PermissionDenied))?;
    let path = std::fs::canonicalize(root.join(requested)).map_err(|error| {
        PluginError::domain(if error.kind() == std::io::ErrorKind::NotFound {
            ExecuteError::NotFound
        } else {
            ExecuteError::PermissionDenied
        })
    })?;
    if !path.starts_with(&root) || !path.is_file() {
        return Err(PluginError::domain(ExecuteError::PermissionDenied));
    }
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| PluginError::domain(ExecuteError::InvalidArguments))?;
    match (tool, extension.as_str()) {
        (READ_IMAGE_TOOL, "png") => Ok((path, "image/png", "png")),
        (READ_IMAGE_TOOL, "jpg" | "jpeg") => Ok((path, "image/jpeg", "jpeg")),
        (READ_IMAGE_TOOL, "webp") => Ok((path, "image/webp", "webp")),
        (TRANSCRIBE_AUDIO_TOOL, "wav") => Ok((path, "audio/wav", "wav")),
        (TRANSCRIBE_AUDIO_TOOL, "mp3") => Ok((path, "audio/mpeg", "mp3")),
        (_, _) => Err(PluginError::domain(ExecuteError::InvalidArguments)),
    }
}

fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        tool(
            READ_IMAGE_TOOL,
            "Read one root-bounded PNG, JPEG, or WebP image through the configured vision model.",
            "Analyze the selected image for the current task.",
        ),
        tool(
            TRANSCRIBE_AUDIO_TOOL,
            "Transcribe one root-bounded WAV or MP3 file through the configured audio model.",
            "Transcribe the selected audio faithfully.",
        ),
    ]
}

fn tool(name: &str, description: &str, prompt_description: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema_json: serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "path": { "type": "string", "minLength": 1, "maxLength": 4096 },
                "prompt": { "type": "string", "maxLength": 4096, "description": prompt_description }
            },
            "required": ["path"]
        })
        .to_string()
        .try_into()
        .expect("Multimodal Tool schema must be valid JSON"),
        execution: ToolExecutionClass::ParallelSafe,
    }
}

fn endpoint(config: &MultimodalConfig) -> Result<reqwest::Url, RuntimeFailure> {
    reqwest::Url::parse(&format!(
        "{}/chat/completions",
        config.base_url.trim_end_matches('/')
    ))
    .map_err(|_| invalid_plan("Multimodal base_url is invalid"))
}

fn valid_model(value: &str) -> bool {
    value.trim() == value && !value.is_empty() && value.len() <= 256
}

fn safe_relative_path(value: &str) -> bool {
    let path = Path::new(value);
    !value.is_empty()
        && value.len() <= 4096
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn validate_prompt(value: Option<&str>) -> PluginResult<(), ExecuteError> {
    if value.is_some_and(|value| value.len() > 4096 || value.contains('\0')) {
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

fn execution_failed(reason_code: &str, message: &str) -> ExecuteError {
    ExecuteError::ExecutionFailed {
        payload: ExecutionFailedPayload {
            reason_code: reason_code.to_owned(),
            message: message.to_owned(),
            details_json: "{}".try_into().expect("empty JSON object must be valid"),
        },
    }
}

fn invalid_plan(detail: impl Into<String>) -> RuntimeFailure {
    RuntimeFailure::InvalidResolvedPlan {
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_paths_are_relative_and_extensions_are_explicit() {
        assert!(safe_relative_path("assets/screenshot.png"));
        assert!(!safe_relative_path("../secret.png"));
        assert!(!safe_relative_path("/tmp/secret.png"));
    }

    #[test]
    fn request_shapes_keep_media_in_typed_provider_blocks() {
        let config = MultimodalConfig {
            api_key_ref: "media/key".to_owned(),
            audio_model: "audio-model".to_owned(),
            base_url: "https://api.example.com/v1".to_owned(),
            image_model: "vision-model".to_owned(),
            max_file_bytes: 1024,
            root: ".".to_owned(),
            timeout_ms: 30_000,
        };
        let image = request_body(&config, READ_IMAGE_TOOL, None, "image/png", "png", "AA==");
        assert_eq!(image["messages"][0]["content"][1]["type"], "image_url");
        let audio = request_body(
            &config,
            TRANSCRIBE_AUDIO_TOOL,
            None,
            "audio/wav",
            "wav",
            "AA==",
        );
        assert_eq!(audio["messages"][0]["content"][1]["type"], "input_audio");
    }
}
