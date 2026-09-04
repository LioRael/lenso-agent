use super::*;
use std::collections::BTreeMap;
use tokio::net::TcpListener;
use tokio_tungstenite::accept_async;

mod continuation_tests;

fn config(base_url: &str) -> DirectModelConfig {
    serde_json::from_value(serde_json::json!({
        "base_url": base_url, "model": "test-model", "reasoning_effort": "medium",
        "max_event_bytes": 4096, "transport": "websocket"
    }))
    .unwrap()
}

fn credential() -> AccessResponse {
    serde_json::from_value(serde_json::json!({"access_token":"secret-token", "account_id":"account-a", "expires_at":"18446744073709551615"})).unwrap()
}

fn request(text: &str) -> ResponsesRequest {
    ResponsesRequest {
        continuation_scope: None,
        body: serde_json::json!({
            "model":"test-model", "store":false, "stream":true,
            "input":[{"role":"user","content":text}], "tools":[]
        }),
        provider_to_lenso_tool_names: BTreeMap::new(),
    }
}

async fn listener() -> (TcpListener, DirectModelConfig) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = config(&format!("http://{}", listener.local_addr().unwrap()));
    (listener, config)
}

async fn finish(stream: &ResponseStream) {
    stream.close_send().await.unwrap();
    loop {
        if let NativeStreamItem::Terminal(result) = stream.receive().await.unwrap() {
            result.unwrap();
            break;
        }
    }
}

fn completed() -> Message {
    Message::Text(r#"{"type":"response.completed","response":{"id":"response-1"}}"#.into())
}

#[test]
fn wire_request_removes_http_streaming_and_starts_an_independent_response() {
    let message = create_message(&request("hello")).unwrap();
    let json: serde_json::Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
    assert_eq!(json["type"], "response.create");
    assert!(json.get("stream").is_none());
    assert!(json.get("previous_response_id").is_none());
    assert_eq!(json["store"], false);
}

#[test]
fn omitted_transport_defaults_to_websocket_and_matches_schema() {
    let config: DirectModelConfig = serde_json::from_value(serde_json::json!({
        "base_url":"http://localhost", "model":"test", "reasoning_effort":"medium", "max_event_bytes":4096
    })).unwrap();
    assert_eq!(config.transport, Transport::Websocket);
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../../config.schema.json")).unwrap();
    assert_eq!(schema["properties"]["transport"]["default"], "websocket");
}

#[test]
fn explicit_transport_overrides_the_default() {
    for (name, expected) in [("sse", Transport::Sse), ("auto", Transport::Auto)] {
        let config: DirectModelConfig = serde_json::from_value(serde_json::json!({
            "base_url":"http://localhost", "model":"test", "reasoning_effort":"medium",
            "max_event_bytes":4096, "transport":name
        }))
        .unwrap();
        assert_eq!(config.transport, expected);
    }
}

#[test]
fn pool_admission_is_bounded_and_dropping_a_lease_releases_capacity() {
    let pool = Pool::default();
    let mut leases: Vec<_> = (0..MAX_CONNECTIONS)
        .map(|_| pool.reserve([1; 32], None).ok().unwrap())
        .collect();
    assert!(matches!(
        pool.reserve([1; 32], None),
        Err(OpenError::Model(ModelCompleteInvocationError::Domain(
            CompleteError::Overloaded
        )))
    ));
    leases.pop();
    assert!(pool.reserve([1; 32], None).is_ok());
}

#[tokio::test]
async fn completed_responses_reuse_one_socket_without_reusing_context() {
    let (listener, config) = listener().await;
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(tcp).await.unwrap();
        for expected in ["first", "second"] {
            let text = socket.next().await.unwrap().unwrap().into_text().unwrap();
            let body: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(body["input"][0]["content"], expected);
            assert!(body.get("previous_response_id").is_none());
            socket.send(completed()).await.unwrap();
        }
    });
    let pool = Pool::default();
    for text in ["first", "second"] {
        let stream = pool
            .open(&config, &credential(), &request(text))
            .await
            .unwrap();
        finish(&stream).await;
        assert_eq!(pool.0.borrow().active, 0);
        assert_eq!(pool.0.borrow().idle.len(), 1);
    }
    server.await.unwrap();
}

#[tokio::test]
async fn cancellation_wakes_a_pending_receive_and_drops_the_socket() {
    let (listener, config) = listener().await;
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(tcp).await.unwrap();
        socket.next().await.unwrap().unwrap();
        assert!(matches!(
            socket.next().await,
            None | Some(Err(_) | Ok(Message::Close(_)))
        ));
    });
    let pool = Pool::default();
    let stream = pool
        .open(&config, &credential(), &request("cancel"))
        .await
        .unwrap();
    let (result, ()) = tokio::join!(stream.receive(), async {
        tokio::task::yield_now().await;
        stream.cancel();
    });
    assert!(matches!(result, Err(RuntimeFailure::AdmissionClosed)));
    assert_eq!(pool.0.borrow().active, 0);
    assert!(pool.0.borrow().idle.is_empty());
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn truncated_stream_is_not_recycled_or_replayed() {
    let (listener, config) = listener().await;
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(tcp).await.unwrap();
        socket.next().await.unwrap().unwrap();
        socket
            .send(Message::Text(
                r#"{"type":"response.output_text.delta","delta":"partial"}"#.into(),
            ))
            .await
            .unwrap();
        socket.close(None).await.unwrap();
    });
    let pool = Pool::default();
    let stream = pool
        .open(&config, &credential(), &request("truncate"))
        .await
        .unwrap();
    assert!(matches!(
        stream.receive().await.unwrap(),
        NativeStreamItem::Message(_)
    ));
    assert!(stream.receive().await.is_err());
    assert_eq!(pool.0.borrow().active, 0);
    assert!(pool.0.borrow().idle.is_empty());
    server.await.unwrap();
}

#[tokio::test]
async fn token_rotation_never_reuses_an_authenticated_socket() {
    let (listener, config) = listener().await;
    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(tcp).await.unwrap();
            socket.next().await.unwrap().unwrap();
            socket.send(completed()).await.unwrap();
            // Keep it open until the client explicitly discards the old identity.
            let _ = socket.next().await;
        }
    });
    let pool = Pool::default();
    let first = pool
        .open(&config, &credential(), &request("one"))
        .await
        .unwrap();
    finish(&first).await;
    let mut rotated = credential();
    rotated.access_token = "rotated-secret".to_owned();
    let second = pool.open(&config, &rotated, &request("two")).await.unwrap();
    finish(&second).await;
    pool.shutdown();
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn oversized_message_fails_without_returning_socket_to_pool() {
    let (listener, mut config) = listener().await;
    config.max_event_bytes = 128;
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(tcp).await.unwrap();
        socket.next().await.unwrap().unwrap();
        socket
            .send(Message::Text("x".repeat(1024).into()))
            .await
            .unwrap();
    });
    let pool = Pool::default();
    let stream = pool
        .open(&config, &credential(), &request("large"))
        .await
        .unwrap();
    assert!(stream.receive().await.is_err());
    assert!(pool.0.borrow().idle.is_empty());
    server.await.unwrap();
}

#[tokio::test]
async fn only_explicit_upgrade_rejection_allows_transport_fallback() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for status in [426, 401, 403, 429] {
        let (listener, config) = listener().await;
        let server = tokio::spawn(async move {
            let (mut tcp, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 4096];
            let _ = tcp.read(&mut buffer).await.unwrap();
            tcp.write_all(
                format!(
                    "HTTP/1.1 {status} Rejected\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        });
        let error = Pool::default()
            .open(&config, &credential(), &request("test"))
            .await
            .unwrap_err();
        assert_eq!(matches!(error, OpenError::Unsupported), status == 426);
        let diagnostic = format!("{error:?}");
        assert!(!diagnostic.contains("secret-token"));
        assert!(!diagnostic.contains("account-a"));
        server.await.unwrap();
    }
}
