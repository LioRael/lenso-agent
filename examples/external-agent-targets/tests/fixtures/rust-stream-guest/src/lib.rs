use std::{cell::RefCell, collections::VecDeque};

use lenso_guest_sdk::GuestSessions;

wit_bindgen::generate!({
    path: "wit",
    world: "plugin",
});

mod chat {
    pub const CAPABILITY_ID: &str = "test.chat@1";
    pub const DESCRIPTOR_VERSION: &str = "1.0.0";
    pub const CHAT: &str = "chat";
}

#[derive(Default)]
struct Session {
    messages: VecDeque<String>,
    closed: bool,
}

thread_local! {
    static SESSIONS: RefCell<GuestSessions<Session>> = const { RefCell::new(GuestSessions::new()) };
}

struct GuestComponent;

impl Guest for GuestComponent {
    fn describe() -> String {
        lenso_guest_sdk::guest_descriptor! {
            provides: [chat {
                requests: [],
                streams: [chat::CHAT],
            }],
            requires: [],
        }
    }

    fn invoke(_: String, _: String, _: String) -> Result<String, String> {
        Ok("null".to_owned())
    }

    fn stream_open(
        capability: String,
        operation: String,
        request_json: String,
    ) -> Result<u64, String> {
        assert_eq!(capability, "test.chat@1");
        assert_eq!(operation, "chat");
        if request_json == "0" {
            return Err("\"rejected\"".to_owned());
        }
        SESSIONS.with_borrow_mut(|sessions| {
            sessions
                .insert(Session::default())
                .map_err(|error| format!("failed to allocate stream: {error:?}"))
        })
    }

    fn stream_send(stream_id: u64, message_json: String) -> Result<(), String> {
        SESSIONS.with_borrow_mut(|sessions| {
            sessions
                .get_mut(stream_id)
                .map_err(|_| "unknown stream".to_owned())?
                .messages
                .push_back(message_json);
            Ok(())
        })
    }

    fn stream_receive(stream_id: u64) -> Result<String, String> {
        SESSIONS.with_borrow_mut(|sessions| {
            let session = sessions
                .get_mut(stream_id)
                .map_err(|_| "unknown stream".to_owned())?;
            if let Some(message) = session.messages.pop_front() {
                return Ok(format!(r#"{{"kind":"message","value":{message}}}"#));
            }
            if session.closed {
                session.closed = false;
                return Ok(r#"{"kind":"peer-half-closed"}"#.to_owned());
            }
            Ok(r#"{"kind":"terminal-success"}"#.to_owned())
        })
    }

    fn stream_close_send(stream_id: u64) -> Result<(), String> {
        SESSIONS.with_borrow_mut(|sessions| {
            sessions
                .get_mut(stream_id)
                .map_err(|_| "unknown stream".to_owned())?
                .closed = true;
            Ok(())
        })
    }

    fn stream_cancel(stream_id: u64) {
        SESSIONS.with_borrow_mut(|sessions| {
            let _ = sessions.remove(stream_id);
        });
    }
}

export!(GuestComponent);
