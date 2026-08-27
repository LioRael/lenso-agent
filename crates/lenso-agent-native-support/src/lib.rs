//! Small native-runtime helpers shared by Agent Harness Plugins.

use std::{
    any::Any,
    cell::{Cell, RefCell},
    collections::VecDeque,
};

use futures::future::{LocalBoxFuture, ready};
use lenso_kernel::{NativeStreamItem, NativeStreamSession, RuntimeFailure};

/// Finite server-output stream used by deterministic and fully computed turns.
#[derive(Debug)]
pub struct FiniteOutputStream {
    capability: &'static str,
    events: RefCell<VecDeque<NativeStreamItem>>,
    cancelled: Cell<bool>,
    send_closed: Cell<bool>,
}

impl FiniteOutputStream {
    /// Builds an ordered stream and appends peer-half-close plus terminal success.
    pub fn successful<M: Any>(capability: &'static str, messages: Vec<M>) -> Self {
        let mut events = messages
            .into_iter()
            .map(|message| NativeStreamItem::Message(Box::new(message) as Box<dyn Any>))
            .collect::<VecDeque<_>>();
        events.push_back(NativeStreamItem::PeerHalfClosed);
        events.push_back(NativeStreamItem::Terminal(Ok(())));
        Self {
            capability,
            events: RefCell::new(events),
            cancelled: Cell::new(false),
            send_closed: Cell::new(false),
        }
    }
}

impl NativeStreamSession for FiniteOutputStream {
    fn send(&self, _message: Box<dyn Any>) -> LocalBoxFuture<'static, Result<(), RuntimeFailure>> {
        Box::pin(ready(Err(RuntimeFailure::ProtocolViolation {
            capability: self.capability,
        })))
    }

    fn receive(&self) -> LocalBoxFuture<'static, Result<NativeStreamItem, RuntimeFailure>> {
        let result = if self.cancelled.get() {
            Err(RuntimeFailure::AdmissionClosed)
        } else {
            self.events
                .borrow_mut()
                .pop_front()
                .ok_or(RuntimeFailure::ProtocolViolation {
                    capability: self.capability,
                })
        };
        Box::pin(ready(result))
    }

    fn close_send(&self) -> LocalBoxFuture<'static, Result<(), RuntimeFailure>> {
        let result = if self.send_closed.replace(true) {
            Err(RuntimeFailure::ProtocolViolation {
                capability: self.capability,
            })
        } else {
            Ok(())
        };
        Box::pin(ready(result))
    }

    fn cancel(&self) {
        self.cancelled.set(true);
        self.events.borrow_mut().clear();
    }
}
