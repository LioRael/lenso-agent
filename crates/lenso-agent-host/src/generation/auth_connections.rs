//! Auth management through exact, Plan-bound Web consumer dependencies.

use lenso_capability_agent_auth_connection as contract;
use lenso_kernel::{
    CancellationToken, InvocationContext, PluginDependencyHandle, RequestCapability,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

use super::AgentApp;

#[derive(Debug, Serialize)]
pub struct ConnectionCatalog {
    pub generation: String,
    pub connections: Vec<Connection>,
}

#[derive(Debug, Serialize)]
pub struct Connection {
    pub provider: String,
    pub status: contract::StatusResponse,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionRequest {
    pub generation: String,
    pub provider: String,
    pub action: ConnectionAction,
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "kind",
    content = "request",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ConnectionAction {
    Begin(contract::BeginRequest),
    Poll(contract::AttemptRequest),
    Cancel(contract::AttemptRequest),
    Disconnect(contract::DisconnectRequest),
}

impl AgentApp {
    pub async fn web_auth_connections(&self) -> Result<ConnectionCatalog, String> {
        let route = self.host.route().await.map_err(super::control_error)?;
        let dependencies = route
            .target()
            .dependencies("lenso.agent.web/web")
            .map_err(|_| "Authentication connections are unavailable")?;
        let mut connections = Vec::new();
        for binding in dependencies
            .bindings()
            .iter()
            .filter(|binding| binding.capability_id() == contract::CAPABILITY_ID)
        {
            let handle = binding
                .handle()
                .ok_or("Authentication provider is unavailable")?;
            let context = route
                .target()
                .invocation_context_after(Duration::from_secs(35), CancellationToken::new());
            let status = invoke::<contract::AuthConnectionStatus>(
                &handle,
                context,
                contract::STATUS_OPERATION,
                contract::StatusRequest {},
            )
            .await?;
            connections.push(Connection {
                provider: binding.provider_instance().to_owned(),
                status,
            });
        }
        Ok(ConnectionCatalog {
            generation: route.generation_spec_digest().to_owned(),
            connections,
        })
    }

    pub async fn web_auth_connection_action(
        &self,
        request: ConnectionRequest,
    ) -> Result<Value, String> {
        let route = self.host.route().await.map_err(super::control_error)?;
        if request.generation != route.generation_spec_digest() {
            return Err("Authentication settings changed. Refresh before retrying.".into());
        }
        let dependencies = route
            .target()
            .dependencies("lenso.agent.web/web")
            .map_err(|_| "Authentication connections are unavailable")?;
        let handle = dependencies
            .bindings()
            .iter()
            .find(|binding| {
                binding.capability_id() == contract::CAPABILITY_ID
                    && binding.provider_instance() == request.provider
            })
            .and_then(lenso_kernel::PluginDependency::handle)
            .ok_or("Authentication provider is not bound to this Agent")?;
        let context = route
            .target()
            .invocation_context_after(Duration::from_secs(35), CancellationToken::new());
        let result = match request.action {
            ConnectionAction::Begin(request) => serde_json::to_value(
                invoke::<contract::AuthConnectionBegin>(
                    &handle,
                    context,
                    contract::BEGIN_OPERATION,
                    request,
                )
                .await?,
            ),
            ConnectionAction::Poll(request) => serde_json::to_value(
                invoke::<contract::AuthConnectionPoll>(
                    &handle,
                    context,
                    contract::POLL_OPERATION,
                    request,
                )
                .await?,
            ),
            ConnectionAction::Cancel(request) => serde_json::to_value(
                invoke::<contract::AuthConnectionCancel>(
                    &handle,
                    context,
                    contract::CANCEL_OPERATION,
                    request,
                )
                .await?,
            ),
            ConnectionAction::Disconnect(request) => serde_json::to_value(
                invoke::<contract::AuthConnectionDisconnect>(
                    &handle,
                    context,
                    contract::DISCONNECT_OPERATION,
                    request,
                )
                .await?,
            ),
        };
        result.map_err(|_| "Authentication response could not be encoded".into())
    }
}

async fn invoke<C: RequestCapability>(
    dependency: &PluginDependencyHandle,
    context: InvocationContext,
    operation: &str,
    request: C::Request,
) -> Result<C::Response, String> {
    let cancellation = context.cancellation();
    let handle = dependency
        .typed::<C>()
        .map_err(|_| "Authentication provider is unavailable")?;
    let result = tokio::time::timeout(
        Duration::from_secs(35),
        handle.invoke_with_context(operation, context, request),
    )
    .await
    .map_err(|_| {
        cancellation.cancel();
        "Authentication request timed out"
    })?
    .map_err(|_| "Authentication provider is unavailable")?;
    result.map_err(|_| {
        "Authentication request was rejected. Refresh the connection and try again.".into()
    })
}
