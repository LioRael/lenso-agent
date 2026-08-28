use std::sync::Arc;

use lenso_agent_loop_plugin::RunScope;
use lenso_capability_agent::{RUN_TURN_OPERATION, RunTurnRequest};
use lenso_kernel::StreamEvent;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError};

use crate::generation::TurnGeneration;

const MAX_QUEUED_TURNS: usize = 1_024;

/// Bounds concurrent Channel ingress before it reaches the single-turn Agent.
#[derive(Clone, Debug)]
pub struct TurnGate {
    admitted: Arc<Semaphore>,
    active: Arc<Semaphore>,
}

impl TurnGate {
    pub fn new(queue_capacity: usize) -> Result<Self, String> {
        if queue_capacity > MAX_QUEUED_TURNS {
            return Err(format!(
                "Channel queue capacity must not exceed {MAX_QUEUED_TURNS}"
            ));
        }
        let admitted_capacity = queue_capacity
            .checked_add(1)
            .ok_or_else(|| "Channel queue capacity overflowed".to_owned())?;
        Ok(Self {
            admitted: Arc::new(Semaphore::new(admitted_capacity)),
            active: Arc::new(Semaphore::new(1)),
        })
    }

    pub(crate) async fn enter(&self) -> Result<TurnPermit, TurnGateFull> {
        let admitted = self
            .admitted
            .clone()
            .try_acquire_owned()
            .map_err(|error| match error {
                TryAcquireError::NoPermits | TryAcquireError::Closed => TurnGateFull,
            })?;
        let active = self
            .active
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| TurnGateFull)?;
        Ok(TurnPermit {
            _admitted: admitted,
            _active: active,
        })
    }
}

impl Default for TurnGate {
    fn default() -> Self {
        Self::new(0).expect("zero-capacity Channel queue is valid")
    }
}

#[derive(Debug)]
pub(crate) struct TurnPermit {
    _admitted: OwnedSemaphorePermit,
    _active: OwnedSemaphorePermit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TurnGateFull;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn zero_queue_capacity_rejects_a_second_turn() {
        let gate = TurnGate::new(0).unwrap();
        let _first = gate.enter().await.unwrap();
        assert_eq!(gate.enter().await.unwrap_err(), TurnGateFull);
    }

    #[tokio::test]
    async fn admitted_turn_waits_while_later_turn_is_rejected() {
        let gate = TurnGate::new(1).unwrap();
        let first = gate.enter().await.unwrap();
        let waiting_gate = gate.clone();
        let waiting = tokio::spawn(async move { waiting_gate.enter().await });
        tokio::task::yield_now().await;

        assert_eq!(gate.enter().await.unwrap_err(), TurnGateFull);
        drop(first);
        assert!(waiting.await.unwrap().is_ok());
    }

    #[test]
    fn queue_capacity_is_bounded() {
        assert!(TurnGate::new(MAX_QUEUED_TURNS).is_ok());
        assert!(TurnGate::new(MAX_QUEUED_TURNS + 1).is_err());
    }
}

pub(crate) async fn run_agent_turn(
    turn: TurnGeneration,
    prompt: String,
    session_id: Option<&str>,
    allowed_tools: &[String],
) -> Result<AgentTurnResult, String> {
    let context = RunScope::new(allowed_tools.to_vec())?.attach(turn.invocation_context()?)?;
    let stream = turn
        .handle()
        .open_with_context(
            RUN_TURN_OPERATION,
            context,
            RunTurnRequest {
                input: prompt,
                session_id: session_id.map(str::to_owned),
            },
        )
        .await
        .map_err(|error| format!("Agent stream failed to open: {error:?}"))?
        .map_err(|error| format!("Agent rejected the Turn: {error:?}"))?;
    stream
        .close_send()
        .await
        .map_err(|error| format!("failed to half-close Agent input: {error:?}"))?;
    let mut output = String::new();
    let mut returned_session_id = session_id.map(str::to_owned);
    loop {
        match stream
            .receive()
            .await
            .map_err(|error| format!("Agent stream failed: {error:?}"))?
        {
            StreamEvent::Message(message) => {
                returned_session_id = message.session_id.clone().or(returned_session_id);
                if message.is_text_delta() {
                    output.push_str(&message.text);
                }
            }
            StreamEvent::PeerHalfClosed => {}
            StreamEvent::Terminal(Ok(())) => break,
            StreamEvent::Terminal(Err(error)) => {
                return Err(format!("Agent Turn failed: {error:?}"));
            }
        }
    }
    let session_id = returned_session_id
        .ok_or_else(|| "Agent Turn completed without a Session identity".to_owned())?;
    let text = if output.trim().is_empty() {
        "The Agent completed without a text response.".to_owned()
    } else {
        output
    };
    Ok(AgentTurnResult { text, session_id })
}

#[derive(Debug)]
pub(crate) struct AgentTurnResult {
    pub(crate) text: String,
    pub(crate) session_id: String,
}
