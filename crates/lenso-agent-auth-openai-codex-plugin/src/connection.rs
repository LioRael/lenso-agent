use std::time::Duration;

use lenso_capability_agent_auth_connection as contract;
use lenso_kernel::RuntimeFailure;
use tokio::{sync::Mutex, task::JoinHandle};

use super::{
    DirectAuthOptions, begin_device_login, complete_device_login, direct_logout, now_millis,
    plugin_failure,
};

const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Default)]
pub(super) struct ConnectionManager {
    attempt: Mutex<Option<Attempt>>,
}

struct Attempt {
    id: String,
    presentation: Option<contract::BeginResponse>,
    task: Option<JoinHandle<bool>>,
    state: contract::LoginState,
}

impl std::fmt::Debug for Attempt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Attempt")
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl Drop for Attempt {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

impl Attempt {
    async fn settle(&mut self) {
        if self.task.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(task) = self.task.take()
        {
            self.state = if matches!(task.await, Ok(true)) {
                contract::LoginState::Connected
            } else {
                contract::LoginState::Failed
            };
        }
    }

    async fn cancel(&mut self) -> bool {
        if let Some(task) = self.task.take() {
            task.abort();
            // Join before acknowledging cancellation or deleting credentials.
            // A completion already inside synchronous persistence must finish first.
            if matches!(task.await, Ok(true)) {
                self.state = contract::LoginState::Connected;
                return false;
            }
            self.state = contract::LoginState::Cancelled;
            true
        } else {
            matches!(self.state, contract::LoginState::Cancelled)
        }
    }
}

impl ConnectionManager {
    pub(super) async fn begin(
        &self,
        options: DirectAuthOptions,
        request: contract::BeginRequest,
    ) -> Result<Result<contract::BeginResponse, contract::BeginError>, RuntimeFailure> {
        if !matches!(request.method, contract::LoginMethod::DeviceCode) {
            return Ok(Err(contract::BeginError::UnsupportedMethod));
        }
        let mut slot = self.attempt.lock().await;
        if let Some(attempt) = slot.as_mut() {
            attempt.settle().await;
            if attempt.task.is_some() {
                if let Some(presentation) = &attempt.presentation {
                    return Ok(Ok(presentation.clone()));
                }
                return Ok(Err(contract::BeginError::AttemptInProgress));
            }
        }
        let pending =
            tokio::time::timeout(Duration::from_secs(30), begin_device_login(options.clone()))
                .await
                .map_err(|_| plugin_failure("authentication start timed out"))?
                .map_err(|_| plugin_failure("authentication could not be started"))?;
        let response = contract::BeginResponse {
            attempt_id: uuid::Uuid::new_v4().to_string(),
            authorization_url: pending.verification_url.clone(),
            user_code: pending.user_code.clone(),
            expires_at_millis: now_millis().saturating_add(300_000).to_string(),
        };
        let task = tokio::spawn(async move {
            matches!(
                tokio::time::timeout(ATTEMPT_TIMEOUT, complete_device_login(options, pending))
                    .await,
                Ok(Ok(_))
            )
        });
        *slot = Some(Attempt {
            id: response.attempt_id.clone(),
            presentation: Some(response.clone()),
            task: Some(task),
            state: contract::LoginState::Pending,
        });
        Ok(Ok(response))
    }

    pub(super) async fn poll(
        &self,
        request: contract::AttemptRequest,
    ) -> Result<Result<contract::PollResponse, contract::PollError>, RuntimeFailure> {
        let mut slot = self.attempt.lock().await;
        let Some(attempt) = slot
            .as_mut()
            .filter(|attempt| attempt.id == request.attempt_id)
        else {
            return Ok(Err(contract::PollError::UnknownAttempt));
        };
        attempt.settle().await;
        Ok(Ok(contract::PollResponse {
            state: attempt.state.clone(),
        }))
    }

    pub(super) async fn cancel(
        &self,
        request: contract::AttemptRequest,
    ) -> Result<Result<contract::CancelResponse, contract::CancelError>, RuntimeFailure> {
        let mut slot = self.attempt.lock().await;
        let Some(attempt) = slot
            .as_mut()
            .filter(|attempt| attempt.id == request.attempt_id)
        else {
            return Ok(Err(contract::CancelError::UnknownAttempt));
        };
        attempt.settle().await;
        Ok(Ok(contract::CancelResponse {
            cancelled: attempt.cancel().await,
        }))
    }

    pub(super) async fn disconnect(
        &self,
        options: DirectAuthOptions,
    ) -> Result<Result<contract::DisconnectResponse, contract::DisconnectError>, RuntimeFailure>
    {
        let mut slot = self.attempt.lock().await;
        if let Some(attempt) = slot.as_mut() {
            attempt.cancel().await;
        }
        direct_logout(options)
            .map_err(|_| plugin_failure("failed to disconnect authentication"))?;
        *slot = None;
        Ok(Ok(contract::DisconnectResponse { disconnected: true }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_joins_the_attempt_and_rejects_stale_handles() {
        let manager = ConnectionManager::default();
        let task = tokio::spawn(async { std::future::pending::<bool>().await });
        *manager.attempt.lock().await = Some(Attempt {
            id: "attempt-a".into(),
            presentation: None,
            task: Some(task),
            state: contract::LoginState::Pending,
        });
        let unknown_result = manager
            .cancel(contract::AttemptRequest {
                attempt_id: "attempt-b".into(),
            })
            .await
            .unwrap();
        assert!(matches!(
            unknown_result,
            Err(contract::CancelError::UnknownAttempt)
        ));
        let cancelled = manager
            .cancel(contract::AttemptRequest {
                attempt_id: "attempt-a".into(),
            })
            .await
            .unwrap()
            .unwrap();
        assert!(cancelled.cancelled);
        let state = manager
            .poll(contract::AttemptRequest {
                attempt_id: "attempt-a".into(),
            })
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(state.state, contract::LoginState::Cancelled));
        assert!(
            manager
                .attempt
                .lock()
                .await
                .as_ref()
                .unwrap()
                .task
                .is_none()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn generated_provider_reports_status_without_secrets_and_disconnects() {
        use lenso_capability_agent_auth_connection::AuthConnectionProvider;
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("auth.json");
        let provider = crate::CodexAuth {
            config: crate::AuthConfig {
                issuer: "https://auth.openai.com".into(),
                profile: "test".into(),
                credential_file: Some(path.clone()),
                refresh_margin_seconds: 60,
            },
            client: reqwest::Client::new(),
            connection: std::sync::Arc::default(),
        };
        let context = || {
            lenso_kernel::InvocationContext::new(1, None, lenso_kernel::CancellationToken::new())
        };
        let status =
            AuthConnectionProvider::status(&provider, context(), contract::StatusRequest {})
                .await
                .unwrap()
                .unwrap();
        assert!(!status.connected);
        assert_eq!(status.methods, [contract::LoginMethod::DeviceCode]);
        let rejected = AuthConnectionProvider::begin(
            &provider,
            context(),
            contract::BeginRequest {
                method: contract::LoginMethod::BrowserLoopback,
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            rejected,
            Err(contract::BeginError::UnsupportedMethod)
        ));
        let disconnected = AuthConnectionProvider::disconnect(
            &provider,
            context(),
            contract::DisconnectRequest {},
        )
        .await
        .unwrap()
        .unwrap();
        assert!(disconnected.disconnected);
        assert!(!format!("{status:?}").contains(path.to_str().unwrap()));
    }
}
