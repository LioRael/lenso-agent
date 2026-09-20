//! Protocol-neutral framing validation for duplex interaction Providers.

use crate::{ConnectOpen, FrameDirection, FrameKind, InteractionFrame};

/// Default maximum when a Provider's configuration has no narrower bound.
pub const DEFAULT_MAX_PENDING_FRAMES: usize = 8;
const MAX_FRAME_BYTES: usize = 65_536;

/// A deterministic state machine that tracks both stream directions without
/// confusing a transport disconnect with an application terminal result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InteractionFlow {
    protocol: String,
    next_client_sequence: u64,
    next_agent_sequence: u64,
    max_pending_frames: usize,
    pending_agent_frames: usize,
    client_closed: bool,
    agent_closed: bool,
    interrupted: bool,
}

/// Stable invalid-frame outcome used by a Provider to map malformed inputs to
/// `InteractionError::InvalidFrame` rather than treating them as text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InteractionValidationError {
    InvalidOpen(String),
    InvalidFrame(String),
    CapacityExceeded,
    ClientDirectionClosed,
    AgentDirectionClosed,
    Interrupted,
}

impl InteractionFlow {
    /// Creates flow state after validating a negotiated open request.
    pub fn new(open: &ConnectOpen) -> Result<Self, InteractionValidationError> {
        validate_connect_open(open)?;
        Ok(Self {
            protocol: open.protocol.clone(),
            next_client_sequence: 1,
            next_agent_sequence: 1,
            max_pending_frames: usize::try_from(open.max_pending_frames)
                .unwrap_or(DEFAULT_MAX_PENDING_FRAMES),
            pending_agent_frames: 0,
            client_closed: false,
            agent_closed: false,
            interrupted: false,
        })
    }

    /// Validates a client-to-Agent frame and advances only the client cursor.
    pub fn accept_client(
        &mut self,
        frame: &InteractionFrame,
    ) -> Result<(), InteractionValidationError> {
        if self.client_closed {
            return Err(InteractionValidationError::ClientDirectionClosed);
        }
        self.validate_direction_and_sequence(
            frame,
            &FrameDirection::ClientToAgent,
            self.next_client_sequence,
        )?;
        self.next_client_sequence = self.next_client_sequence.saturating_add(1);
        if frame.kind == FrameKind::Interrupt {
            self.interrupted = true;
        }
        Ok(())
    }

    /// Validates an Agent-to-client frame and retains one bounded pending slot
    /// until the client acknowledges or consumes it.
    pub fn emit_agent(
        &mut self,
        frame: &InteractionFrame,
    ) -> Result<(), InteractionValidationError> {
        if self.agent_closed {
            return Err(InteractionValidationError::AgentDirectionClosed);
        }
        if self.interrupted && frame.kind != FrameKind::Notice {
            return Err(InteractionValidationError::Interrupted);
        }
        if self.pending_agent_frames >= self.max_pending_frames {
            return Err(InteractionValidationError::CapacityExceeded);
        }
        self.validate_direction_and_sequence(
            frame,
            &FrameDirection::AgentToClient,
            self.next_agent_sequence,
        )?;
        self.next_agent_sequence = self.next_agent_sequence.saturating_add(1);
        self.pending_agent_frames = self.pending_agent_frames.saturating_add(1);
        Ok(())
    }

    /// Releases bounded output capacity after a client-side acknowledgement.
    pub fn acknowledge_agent_frame(&mut self) {
        self.pending_agent_frames = self.pending_agent_frames.saturating_sub(1);
    }

    /// Records independent half-closes. A Provider still has to send one
    /// terminal outcome through the underlying stream API.
    pub fn close_client(&mut self) {
        self.client_closed = true;
    }

    /// Records the Provider half-close after it has finished producing data.
    pub fn close_agent(&mut self) {
        self.agent_closed = true;
    }

    #[must_use]
    pub const fn is_interrupted(&self) -> bool {
        self.interrupted
    }

    fn validate_direction_and_sequence(
        &self,
        frame: &InteractionFrame,
        expected_direction: &FrameDirection,
        expected_sequence: u64,
    ) -> Result<(), InteractionValidationError> {
        validate_frame(frame, &self.protocol)?;
        if &frame.direction != expected_direction {
            return Err(InteractionValidationError::InvalidFrame(
                "frame direction does not match this stream direction".to_owned(),
            ));
        }
        let sequence = frame.sequence.parse::<u64>().map_err(|_| {
            InteractionValidationError::InvalidFrame("frame sequence is not a u64".to_owned())
        })?;
        if sequence != expected_sequence {
            return Err(InteractionValidationError::InvalidFrame(format!(
                "frame sequence {sequence} does not equal expected {expected_sequence}"
            )));
        }
        Ok(())
    }
}

/// Validates an interaction's open request before allocating any stream work.
pub fn validate_connect_open(open: &ConnectOpen) -> Result<(), InteractionValidationError> {
    validate_identifier("protocol", &open.protocol, 192)?;
    validate_identifier("interaction id", &open.interaction_id, 128)?;
    if let Some(Some(cursor)) = &open.resume_from {
        validate_identifier("resume cursor", cursor, 4_096)?;
    }
    if !(1..=128).contains(&open.max_pending_frames) {
        return Err(InteractionValidationError::InvalidOpen(
            "max pending frames must be in 1..=128".to_owned(),
        ));
    }
    Ok(())
}

/// Validates one portable structured frame without interpreting its business payload.
pub fn validate_frame(
    frame: &InteractionFrame,
    expected_protocol: &str,
) -> Result<(), InteractionValidationError> {
    if frame.protocol != expected_protocol {
        return Err(InteractionValidationError::InvalidFrame(
            "frame protocol does not match the negotiated open".to_owned(),
        ));
    }
    validate_identifier("frame protocol", &frame.protocol, 192)?;
    validate_identifier("correlation id", &frame.correlation_id, 128)?;
    if frame.payload_json.as_str().len() > MAX_FRAME_BYTES {
        return Err(InteractionValidationError::InvalidFrame(
            "frame payload exceeds the bounded size".to_owned(),
        ));
    }
    serde_json::from_str::<serde_json::Value>(frame.payload_json.as_str()).map_err(|_| {
        InteractionValidationError::InvalidFrame("frame payload is not valid JSON".to_owned())
    })?;
    if !frame.sequence.bytes().all(|byte| byte.is_ascii_digit())
        || frame.sequence.parse::<u64>().is_err()
    {
        return Err(InteractionValidationError::InvalidFrame(
            "frame sequence is not a decimal u64".to_owned(),
        ));
    }
    Ok(())
}

fn validate_identifier(
    label: &str,
    value: &str,
    max: usize,
) -> Result<(), InteractionValidationError> {
    if value.is_empty() || value.len() > max || value.chars().any(char::is_control) {
        return Err(InteractionValidationError::InvalidOpen(format!(
            "{label} is empty, too long, or contains control text"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{InteractionFlow, InteractionValidationError};
    use crate::{ConnectOpen, FrameDirection, FrameKind, InteractionFrame, RawJson};

    fn open() -> ConnectOpen {
        ConnectOpen {
            protocol: "example.realtime-audio@1".to_owned(),
            interaction_id: "audio-1".to_owned(),
            resume_from: None,
            max_pending_frames: 2,
        }
    }

    fn frame(sequence: u64, direction: FrameDirection, kind: FrameKind) -> InteractionFrame {
        InteractionFrame {
            protocol: "example.realtime-audio@1".to_owned(),
            sequence: sequence.to_string(),
            direction,
            kind,
            correlation_id: "sample-1".to_owned(),
            payload_json: RawJson::new(r#"{"sample_rate_hz":16000,"frames":320}"#).unwrap(),
        }
    }

    #[test]
    fn duplex_sequences_are_independent_and_output_is_bounded() {
        let mut flow = InteractionFlow::new(&open()).unwrap();
        flow.accept_client(&frame(1, FrameDirection::ClientToAgent, FrameKind::Input))
            .unwrap();
        flow.emit_agent(&frame(1, FrameDirection::AgentToClient, FrameKind::Output))
            .unwrap();
        flow.emit_agent(&frame(2, FrameDirection::AgentToClient, FrameKind::Output))
            .unwrap();
        assert_eq!(
            flow.emit_agent(&frame(3, FrameDirection::AgentToClient, FrameKind::Output)),
            Err(InteractionValidationError::CapacityExceeded)
        );
        flow.acknowledge_agent_frame();
        flow.emit_agent(&frame(3, FrameDirection::AgentToClient, FrameKind::Output))
            .unwrap();
    }

    #[test]
    fn interrupt_prevents_later_output_and_late_input_is_rejected_after_half_close() {
        let mut flow = InteractionFlow::new(&open()).unwrap();
        flow.accept_client(&frame(
            1,
            FrameDirection::ClientToAgent,
            FrameKind::Interrupt,
        ))
        .unwrap();
        assert!(flow.is_interrupted());
        assert_eq!(
            flow.emit_agent(&frame(1, FrameDirection::AgentToClient, FrameKind::Output)),
            Err(InteractionValidationError::Interrupted)
        );
        flow.close_client();
        assert_eq!(
            flow.accept_client(&frame(2, FrameDirection::ClientToAgent, FrameKind::Input)),
            Err(InteractionValidationError::ClientDirectionClosed)
        );
    }
}
