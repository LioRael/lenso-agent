//! Host-enforced bounded HTTP GET provider for reviewed Plugin imports.

use std::time::Duration;

use futures::{StreamExt, future::LocalBoxFuture};
use lenso::prelude::*;
use lenso_capability_agent_http_fetch::{
    self as http_fetch_contract, GetError, GetErrorExecutionFailedPayload, GetRequest, GetResponse,
    HttpFetchProvider,
};
use lenso_kernel::{InvocationContext, RuntimeFailure};

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HttpFetchConfig {
    allowed_origins: Vec<String>,
    max_response_bytes: usize,
    timeout_ms: u64,
}

fn validate_config(config: &HttpFetchConfig) -> Result<(), RuntimeFailure> {
    if config.allowed_origins.len() > 32
        || config.max_response_bytes == 0
        || config.max_response_bytes > 1_048_576
        || config.timeout_ms == 0
        || config.timeout_ms > 30_000
    {
        return Err(invalid_plan(
            "HTTP fetch limits are outside the supported bounds",
        ));
    }
    let mut previous = None;
    for origin in &config.allowed_origins {
        let normalized = normalize_origin(origin)
            .ok_or_else(|| invalid_plan(format!("HTTP fetch origin `{origin}` is invalid")))?;
        if normalized != *origin || previous.is_some_and(|value: &str| value >= origin.as_str()) {
            return Err(invalid_plan(
                "HTTP fetch origins must be normalized, sorted, and unique",
            ));
        }
        previous = Some(origin);
    }
    Ok(())
}

#[lenso::module(
    lifecycle,
    configuration_schema = "config.schema.json",
    validate = validate_config
)]
#[derive(Clone, Debug)]
struct HttpFetcher {
    #[config]
    config: HttpFetchConfig,
}

#[lenso::provides(http_fetch_contract::HttpFetch)]
impl HttpFetchProvider for HttpFetcher {
    fn get(
        &self,
        _context: InvocationContext,
        request: GetRequest,
    ) -> LocalBoxFuture<'static, Result<Result<GetResponse, GetError>, RuntimeFailure>> {
        let config = self.config.clone();
        Box::pin(async move {
            validate_config(&config)?;
            let url = match reqwest::Url::parse(&request.url) {
                Ok(url) => url,
                Err(_) => return Ok(Err(GetError::InvalidUrl)),
            };
            if !url.username().is_empty() || url.password().is_some() {
                return Ok(Err(GetError::InvalidUrl));
            }
            let Some(origin) = normalize_url_origin(&url) else {
                return Ok(Err(GetError::InvalidUrl));
            };
            if config.allowed_origins.binary_search(&origin).is_err() {
                return Ok(Err(GetError::PermissionDenied));
            }

            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_millis(config.timeout_ms))
                .build()
                .map_err(|error| RuntimeFailure::ModuleFailure {
                    detail: format!("HTTP fetch client is unavailable: {error}"),
                })?;
            let response = match client.get(url).send().await {
                Ok(response) => response,
                Err(error) => return Ok(Err(execution_failed("request_failed", error))),
            };
            if response
                .content_length()
                .is_some_and(|length| length > config.max_response_bytes as u64)
            {
                return Ok(Err(GetError::OutputLimitExceeded));
            }
            let status_code = i64::from(response.status().as_u16());
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            let mut body = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    Err(error) => return Ok(Err(execution_failed("response_failed", error))),
                };
                if body.len().saturating_add(chunk.len()) > config.max_response_bytes {
                    return Ok(Err(GetError::OutputLimitExceeded));
                }
                body.extend_from_slice(&chunk);
            }
            let body = match String::from_utf8(body) {
                Ok(body) => body,
                Err(_) => return Ok(Err(GetError::ResponseNotUtf8)),
            };
            Ok(Ok(GetResponse {
                status_code,
                content_type,
                body,
                metadata_json: serde_json::json!({"origin": origin}).to_string(),
            }))
        })
    }
}

impl Lifecycle for HttpFetcher {
    #[allow(clippy::unused_async_trait_impl)]
    async fn prepare(&self, _context: PrepareContext) -> Result<(), RuntimeFailure> {
        validate_config(&self.config)?;
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_millis(self.config.timeout_ms))
            .build()
            .map(|_| ())
            .map_err(|error| RuntimeFailure::ModuleFailure {
                detail: format!("HTTP fetch client is unavailable: {error}"),
            })
    }
}

fn normalize_origin(origin: &str) -> Option<String> {
    let url = reqwest::Url::parse(origin).ok()?;
    if url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    normalize_url_origin(&url)
}

fn normalize_url_origin(url: &reqwest::Url) -> Option<String> {
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return None;
    }
    Some(url.origin().ascii_serialization())
}

fn execution_failed(reason_code: &str, error: impl std::fmt::Display) -> GetError {
    GetError::ExecutionFailed {
        payload: GetErrorExecutionFailedPayload {
            reason_code: reason_code.to_owned(),
            message: error.to_string(),
            details_json: "{}".to_owned(),
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
    use lenso_kernel::CancellationToken;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn context() -> InvocationContext {
        InvocationContext::new(1, None, CancellationToken::new())
    }

    fn provider(allowed_origins: Vec<String>) -> HttpFetcher {
        HttpFetcher {
            config: HttpFetchConfig {
                allowed_origins,
                max_response_bytes: 4096,
                timeout_ms: 5_000,
            },
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fetches_only_an_allowed_origin() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let origin = format!("http://{address}");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await.unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 7\r\nConnection: close\r\n\r\nfixture",
                )
                .await
                .unwrap();
        });
        let response = provider(vec![origin.clone()])
            .get(
                context(),
                GetRequest {
                    url: format!("{origin}/data"),
                },
            )
            .await
            .unwrap()
            .unwrap();
        server.await.unwrap();
        assert_eq!(response.status_code, 200);
        assert_eq!(response.body, "fixture");

        let denied = provider(Vec::new())
            .get(
                context(),
                GetRequest {
                    url: format!("{origin}/data"),
                },
            )
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(denied, GetError::PermissionDenied);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invalid_host_configuration_is_a_runtime_failure() {
        let error = provider(vec![
            "https://b.example".to_owned(),
            "https://a.example".to_owned(),
        ])
        .get(
            context(),
            GetRequest {
                url: "https://a.example/data".to_owned(),
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, RuntimeFailure::InvalidResolvedPlan { .. }));
    }
}
