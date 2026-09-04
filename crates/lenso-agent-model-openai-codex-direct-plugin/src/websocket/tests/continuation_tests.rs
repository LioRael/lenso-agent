use super::*;

fn scoped_request(text: &str, scope: &str) -> ResponsesRequest {
    let mut request = request(text);
    request.continuation_scope = Some(scope.to_owned());
    request
        .provider_to_lenso_tool_names
        .insert("read".into(), "read".into());
    request
}

fn tool_event() -> serde_json::Value {
    serde_json::json!({"type":"response.output_item.done", "item":{
        "type":"function_call", "call_id":"call-1", "name":"read", "arguments":"{}"
    }})
}

fn continued_request(scope: &str) -> ResponsesRequest {
    let mut request = scoped_request("start", scope);
    let input = request.body["input"].as_array_mut().unwrap();
    input.push(tool_event()["item"].clone());
    input.push(
        serde_json::json!({"type":"function_call_output", "call_id":"call-1", "output":"result"}),
    );
    request
}

#[test]
fn continuation_requires_same_scope_and_exact_context_and_controls() {
    let mut checkpoint = Checkpoint::start(&scoped_request("start", "turn-a")).unwrap();
    checkpoint.observe(&tool_event());
    checkpoint.observe(&serde_json::from_str(completed().to_text().unwrap()).unwrap());
    let next = continued_request("turn-a");
    let incremental = checkpoint.incremental_body(&next).unwrap();
    assert_eq!(incremental["previous_response_id"], "response-1");
    assert_eq!(incremental["input"].as_array().unwrap().len(), 1);
    assert_eq!(incremental["input"][0]["type"], "function_call_output");
    assert!(
        checkpoint
            .incremental_body(&continued_request("turn-b"))
            .is_none()
    );
    let mut compacted = continued_request("turn-a");
    compacted.body["input"][0]["content"] = "compacted context".into();
    assert!(checkpoint.incremental_body(&compacted).is_none());
    let mut changed = continued_request("turn-a");
    changed.body["tools"] = serde_json::json!([{"type":"function","name":"new_tool"}]);
    assert!(checkpoint.incremental_body(&changed).is_none());
    assert!(
        checkpoint
            .incremental_body(&scoped_request("start", "turn-a"))
            .is_none()
    );
}

#[tokio::test]
async fn tool_continuation_sends_only_new_results_on_the_same_socket() {
    let (listener, config) = listener().await;
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(tcp).await.unwrap();
        socket.next().await.unwrap().unwrap();
        socket
            .send(Message::Text(tool_event().to_string().into()))
            .await
            .unwrap();
        socket.send(completed()).await.unwrap();
        let body: serde_json::Value =
            serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(body["previous_response_id"], "response-1");
        assert_eq!(body["input"].as_array().unwrap().len(), 1);
        assert_eq!(body["input"][0]["output"], "result");
        socket.send(completed()).await.unwrap();
    });
    let pool = Pool::default();
    let first = pool
        .open(&config, &credential(), &scoped_request("start", "turn-a"))
        .await
        .unwrap();
    finish(&first).await;
    let second = pool
        .open(&config, &credential(), &continued_request("turn-a"))
        .await
        .unwrap();
    finish(&second).await;
    server.await.unwrap();
}

#[tokio::test]
async fn explicit_cache_miss_recovers_once_with_full_input() {
    let (listener, config) = listener().await;
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(tcp).await.unwrap();
        socket.next().await.unwrap().unwrap();
        socket
            .send(Message::Text(tool_event().to_string().into()))
            .await
            .unwrap();
        socket.send(completed()).await.unwrap();
        let incremental: serde_json::Value =
            serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(incremental["previous_response_id"], "response-1");
        socket
            .send(Message::Text(
                r#"{"type":"error","error":{"code":"previous_response_not_found"}}"#.into(),
            ))
            .await
            .unwrap();
        let full: serde_json::Value =
            serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert!(full.get("previous_response_id").is_none());
        assert_eq!(full["input"].as_array().unwrap().len(), 3);
        socket.send(completed()).await.unwrap();
    });
    let pool = Pool::default();
    let first = pool
        .open(&config, &credential(), &scoped_request("start", "turn-a"))
        .await
        .unwrap();
    finish(&first).await;
    let second = pool
        .open(&config, &credential(), &continued_request("turn-a"))
        .await
        .unwrap();
    finish(&second).await;
    server.await.unwrap();
}

#[test]
fn tls_configuration_does_not_depend_on_global_crypto_provider_selection() {
    assert!(matches!(tls_connector(), Ok(Connector::Rustls(_))));
}

#[tokio::test]
async fn a_second_cache_miss_is_terminal_not_an_unbounded_retry() {
    let (listener, config) = listener().await;
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(tcp).await.unwrap();
        socket.next().await.unwrap().unwrap();
        socket
            .send(Message::Text(tool_event().to_string().into()))
            .await
            .unwrap();
        socket.send(completed()).await.unwrap();
        for _ in 0..2 {
            socket.next().await.unwrap().unwrap();
            socket
                .send(Message::Text(
                    r#"{"type":"error","error":{"code":"previous_response_not_found"}}"#.into(),
                ))
                .await
                .unwrap();
        }
        assert!(matches!(
            socket.next().await,
            None | Some(Err(_) | Ok(Message::Close(_)))
        ));
    });
    let pool = Pool::default();
    let first = pool
        .open(&config, &credential(), &scoped_request("start", "turn-a"))
        .await
        .unwrap();
    finish(&first).await;
    let second = pool
        .open(&config, &credential(), &continued_request("turn-a"))
        .await
        .unwrap();
    assert!(second.receive().await.is_err());
    assert!(pool.0.borrow().idle.is_empty());
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}

#[test]
fn checkpoint_matches_parallel_tool_results_in_caller_order() {
    let mut checkpoint = Checkpoint::start(&scoped_request("start", "turn-a")).unwrap();
    checkpoint.observe(&tool_event());
    let mut second = tool_event();
    second["item"]["call_id"] = "call-2".into();
    checkpoint.observe(&second);
    checkpoint.observe(&serde_json::from_str(completed().to_text().unwrap()).unwrap());
    let mut next = continued_request("turn-a");
    let input = next.body["input"].as_array_mut().unwrap();
    input.push(second["item"].clone());
    input.push(
        serde_json::json!({"type":"function_call_output", "call_id":"call-2", "output":"second"}),
    );
    let delta = checkpoint.incremental_body(&next).unwrap();
    assert_eq!(delta["input"].as_array().unwrap().len(), 2);
    assert_eq!(delta["input"][0]["call_id"], "call-1");
    assert_eq!(delta["input"][1]["call_id"], "call-2");
}
