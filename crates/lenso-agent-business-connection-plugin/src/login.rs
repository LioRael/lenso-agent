//! Consent completion belongs to the Plugin lifetime, not a browser request.
use super::{
    client, failure, response,
    state::{Attempt, Grant, Poll, State},
};
use lenso_kernel::CancellationToken;
use std::{sync::Arc, time::Duration};
use tokio::sync::Mutex;

pub(super) fn complete(
    origin: &str,
    state: &Arc<Mutex<State>>,
    attempt: &Attempt,
    shutdown: CancellationToken,
) -> impl Future<Output = ()> + 'static {
    let origin = origin.to_owned();
    let state = Arc::downgrade(state);
    let id = attempt.id.clone();
    let remote_id = attempt.remote_id.clone();
    let secret = attempt.secret.clone();
    let cancellation = attempt.cancellation.clone();
    let expires = attempt.expires;
    async move {
        let deadline = Duration::from_millis(expires.saturating_sub(super::state::now()));
        let work = async {
            loop {
                // Only this worker consumes the one-shot grant. UI polling is read-only.
                let result = async {
                    let reply = client()?.post(format!("{origin}/auth/agent/connection/poll"))
                        .json(&serde_json::json!({"attempt_id":remote_id,"polling_secret":secret.as_str()}))
                        .send().await.map_err(|_| failure())?;
                    response::<Poll>(reply, None).await
                }.await;
                let Some(state) = state.upgrade() else {
                    return;
                };
                let mut state = state.lock().await;
                if !state
                    .attempt
                    .as_ref()
                    .is_some_and(|a| a.id == id && a.valid())
                {
                    return;
                }
                match result {
                    Ok(reply) if reply.state == "pending" => {}
                    Ok(reply) if reply.state == "connected" => {
                        match reply.grant.and_then(|grant| Grant::new(grant).ok()) {
                            Some(grant) => {
                                state.grant = Some(Arc::new(grant));
                                state.attempt.as_mut().unwrap().connected = true;
                            }
                            None => state.attempt.as_mut().unwrap().failed = true,
                        }
                        return;
                    }
                    _ => {
                        // A transport failure may follow grant consumption: never blindly retry.
                        state.attempt.as_mut().unwrap().failed = true;
                        return;
                    }
                }
                drop(state);
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        };
        tokio::select! {
            () = shutdown.cancelled() => {},
            () = cancellation.cancelled() => {},
            () = tokio::time::sleep(deadline) => {},
            () = work => {},
        }
    }
}
