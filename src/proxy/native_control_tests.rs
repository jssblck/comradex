use super::tests::{connect_test_websocket, start_caller_proxy};
use super::*;
use crate::config::{AccountConfig, ProxyConfig};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path};

async fn start_two_account_proxy(
    dir: &Path,
    upstream: String,
) -> (
    std::net::SocketAddr,
    tokio::task::JoinHandle<Result<()>>,
    Arc<Router>,
) {
    let mut accounts = BTreeMap::new();
    for name in ["a", "b"] {
        let path = dir.join(name);
        fs::create_dir_all(&path).unwrap();
        fs::write(
            path.join("auth.json"),
            format!(r#"{{"tokens":{{"access_token":"token-{name}"}}}}"#),
        )
        .unwrap();
        accounts.insert(name.into(), AccountConfig::CodexHome { path });
    }
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = proxy.local_addr().unwrap();
    let listener = ListenerConfig {
        address,
        pool: "default".into(),
    };
    let config = Arc::new(Config {
        proxy: ProxyConfig {
            upstream,
            responses_websocket_mode: ResponsesWebsocketMode::Direct,
            installation_secret: "0123456789abcdef".into(),
            affinity_key: "0123456789abcdef0123456789abcdef".into(),
            state_dir: Some(dir.join("state")),
            ..Default::default()
        },
        listeners: BTreeMap::from([("default".into(), listener.clone())]),
        pools: BTreeMap::from([(
            "default".into(),
            PoolConfig {
                members: vec!["a".into(), "b".into()],
                preferred: Some("a".into()),
                ..Default::default()
            },
        )]),
        accounts,
    });
    let affinity = Arc::new(
        AffinityStore::load(
            dir.join("affinity.json"),
            &config.proxy.affinity_key,
            Duration::from_secs(60),
        )
        .unwrap(),
    );
    let router = Arc::new(Router::new(&config, affinity));
    let app = App::new_unvalidated(config, router.clone(), Arc::new(Stats::default())).unwrap();
    let task = tokio::spawn(app.serve_tcp("default".into(), listener, proxy));
    (address, task, router)
}

struct Backend {
    address: std::net::SocketAddr,
    received: mpsc::UnboundedReceiver<(usize, String)>,
    send: mpsc::UnboundedSender<Message>,
    connections: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

struct AbortTask(tokio::task::JoinHandle<()>);

impl Drop for AbortTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn backend() -> Backend {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (received_tx, received) = mpsc::unbounded_channel();
    let (send, mut commands) = mpsc::unbounded_channel();
    let connections = Arc::new(AtomicUsize::new(0));
    let count = connections.clone();
    let task = tokio::spawn(async move {
        // Concurrent accepts make an accidental replacement observable, even
        // while the original physical socket remains alive.
        let (socket_tx, mut sockets) = mpsc::unbounded_channel();
        let accept_task = AbortTask(tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let id = count.fetch_add(1, Ordering::SeqCst);
                let socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                socket_tx.send((id, socket)).unwrap();
            }
        }));
        let Some((id, mut socket)) = sockets.recv().await else {
            return;
        };
        loop {
            tokio::select! {
                command = commands.recv() => {
                    let Some(command) = command else { break; };
                    if socket.send(command).await.is_err() { break; }
                }
                message = socket.next() => {
                    match message {
                        Some(Ok(Message::Text(text))) => { received_tx.send((id, text.to_string())).unwrap(); }
                        Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                        _ => {}
                    }
                }
            }
        }
        drop(accept_task);
    });
    Backend {
        address,
        received,
        send,
        connections,
        task,
    }
}

async fn receive(backend: &mut Backend) -> (usize, String) {
    tokio::time::timeout(Duration::from_secs(3), backend.received.recv())
        .await
        .unwrap()
        .unwrap()
}

fn emit(backend: &Backend, event: Value) {
    backend
        .send
        .send(Message::Text(event.to_string().into()))
        .unwrap();
}

async fn event<S>(socket: &mut WebSocketStream<S>) -> Value
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let text = tokio::time::timeout(Duration::from_secs(3), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .into_text()
        .unwrap();
    serde_json::from_str(&text).unwrap()
}

#[tokio::test]
async fn steering_successor_and_saved_results_keep_exact_socket_and_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let mut backend = backend().await;
    let (address, task, router) = start_two_account_proxy(
        dir.path(),
        format!("http://{}/backend-api/codex", backend.address),
    )
    .await;
    let mut socket = connect_test_websocket(address).await;
    socket
        .send(Message::Text(
            r#"{"type":"response.create","model":"native","prompt_cache_key":"native-cohort","input":[]}"#.into(),
        ))
        .await
        .unwrap();
    let (physical, _) = receive(&mut backend).await;
    emit(
        &backend,
        json!({"type":"response.created","response":{"id":"parent"}}),
    );
    assert_eq!(event(&mut socket).await["type"], "response.created");
    let cohort = router.affinity.key("prompt-cache:native-cohort");
    assert_eq!(router.affinity.get(&cohort).await.unwrap().account_id, "a");
    socket
        .send(Message::Text(
            json!({"type":"response.steer","previous_response_id":"foreign","input":"wrong owner"})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    assert_eq!(
        event(&mut socket).await["error"]["type"],
        "native_control_invalid"
    );
    assert!(backend.received.try_recv().is_err());
    let steer = "{ \"type\": \"response.steer\", \"previous_response_id\": \"parent\", \"input\": \"change course\" }";
    socket.send(Message::Text(steer.into())).await.unwrap();
    assert_eq!(receive(&mut backend).await, (physical, steer.into()));
    emit(
        &backend,
        json!({"type":"response.steer.accepted","steer":{"id":"steer_1","previous_response_id":"parent"}}),
    );
    emit(
        &backend,
        json!({"type":"response.incomplete","response":{"id":"parent","incomplete_details":{"reason":"steered"}}}),
    );
    // The normal router now prefers B. Native successors/results still belong
    // to the physical connection admitted on A, even after parent settlement.
    for _ in 0..3 {
        router.capacity_failure("a").await;
    }
    router.bind(cohort.clone(), "b").await;
    emit(
        &backend,
        json!({"type":"response.created","response":{"id":"successor"}}),
    );
    emit(
        &backend,
        json!({"type":"response.output_item.done","response_id":"successor","item":{"type":"custom_tool_call","call_id":"call_custom"}}),
    );
    emit(
        &backend,
        json!({"type":"response.completed","response":{"id":"successor","output":[]}}),
    );
    for kind in [
        "response.steer.accepted",
        "response.incomplete",
        "response.created",
        "response.output_item.done",
        "response.completed",
    ] {
        assert_eq!(event(&mut socket).await["type"], kind);
    }
    assert_eq!(router.affinity.get(&cohort).await.unwrap().account_id, "b");
    assert_eq!(
        router
            .affinity
            .get(&router.affinity.key("previous-response:successor"))
            .await
            .unwrap()
            .account_id,
        "a"
    );
    let result = "{ \"type\":\"response.create\", \"model\":\"native\", \"prompt_cache_key\":\"native-cohort\", \"previous_response_id\":\"successor\", \"input\":[{\"type\":\"custom_tool_call_output\",\"call_id\":\"call_custom\",\"output\":[{\"type\":\"input_text\",\"text\":\"done\"}]}] }";
    socket.send(Message::Text(result.into())).await.unwrap();
    assert_eq!(receive(&mut backend).await, (physical, result.into()));
    emit(
        &backend,
        json!({"type":"response.created","response":{"id":"continued"}}),
    );
    assert_eq!(event(&mut socket).await["type"], "response.created");
    assert_eq!(router.affinity.get(&cohort).await.unwrap().account_id, "b");
    assert_eq!(backend.connections.load(Ordering::SeqCst), 1);
    socket.close(None).await.unwrap();
    task.abort();
}

#[tokio::test]
async fn late_injection_failure_keeps_saved_result_on_same_socket_without_replay() {
    let dir = tempfile::tempdir().unwrap();
    let mut backend = backend().await;
    let (address, task, _) = start_two_account_proxy(
        dir.path(),
        format!("http://{}/backend-api/codex", backend.address),
    )
    .await;
    let mut socket = connect_test_websocket(address).await;
    socket
        .send(Message::Text(
            r#"{"type":"response.create","multi_agent":{"enabled":true},"input":[]}"#.into(),
        ))
        .await
        .unwrap();
    let (physical, _) = receive(&mut backend).await;
    emit(
        &backend,
        json!({"type":"response.created","response":{"id":"parent"}}),
    );
    emit(
        &backend,
        json!({"type":"response.output_item.done","response_id":"parent","item":{"type":"function_call","call_id":"call_1"}}),
    );
    emit(
        &backend,
        json!({"type":"response.completed","response":{"id":"parent","output":[]}}),
    );
    for _ in 0..3 {
        event(&mut socket).await;
    }
    let injection = r#"{ "type":"response.inject", "response_id":"parent", "input":[{"type":"function_call_output","call_id":"call_1","output":"saved"}] }"#;
    socket.send(Message::Text(injection.into())).await.unwrap();
    assert_eq!(receive(&mut backend).await, (physical, injection.into()));
    emit(
        &backend,
        json!({"type":"response.inject.failed","response_id":"parent","input":[{"type":"function_call_output","call_id":"call_1","output":"saved"}],"error":{"code":"response_already_completed"}}),
    );
    assert_eq!(event(&mut socket).await["type"], "response.inject.failed");
    let continuation = r#"{ "type":"response.create", "previous_response_id":"parent", "multi_agent":{"enabled":true}, "input":[{"type":"function_call_output","call_id":"call_1","output":"saved"}] }"#;
    socket
        .send(Message::Text(continuation.into()))
        .await
        .unwrap();
    assert_eq!(receive(&mut backend).await, (physical, continuation.into()));
    emit(
        &backend,
        json!({"type":"response.failed","response":{"id":"failed","error":{"code":"server_is_overloaded"}}}),
    );
    assert_eq!(event(&mut socket).await["type"], "response.failed");
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(3), socket.next())
            .await
            .unwrap(),
        Some(Ok(Message::Close(_))) | None
    ));
    assert_eq!(backend.connections.load(Ordering::SeqCst), 1);
    task.abort();
}

#[tokio::test]
async fn multi_agent_saved_approval_without_prior_inject_keeps_socket() {
    let dir = tempfile::tempdir().unwrap();
    let mut backend = backend().await;
    let (address, task, router) = start_two_account_proxy(
        dir.path(),
        format!("http://{}/backend-api/codex", backend.address),
    )
    .await;
    let mut socket = connect_test_websocket(address).await;
    socket
        .send(Message::Text(
            r#"{"type":"response.create","multi_agent":{"enabled":true},"input":[]}"#.into(),
        ))
        .await
        .unwrap();
    let (physical, _) = receive(&mut backend).await;
    emit(
        &backend,
        json!({"type":"response.created","response":{"id":"parent"}}),
    );
    emit(
        &backend,
        json!({"type":"response.output_item.done","response_id":"parent","item":{"type":"mcp_approval_request","id":"approval_1"}}),
    );
    emit(
        &backend,
        json!({"type":"response.completed","response":{"id":"parent","output":[]}}),
    );
    for _ in 0..3 {
        event(&mut socket).await;
    }
    for _ in 0..3 {
        router.capacity_failure("a").await;
    }
    let continuation = r#"{ "type":"response.create", "previous_response_id":"parent", "multi_agent":{"enabled":true}, "input":[{"type":"mcp_approval_response","approval_request_id":"approval_1","approve":true}] }"#;
    socket
        .send(Message::Text(continuation.into()))
        .await
        .unwrap();
    assert_eq!(receive(&mut backend).await, (physical, continuation.into()));
    assert_eq!(backend.connections.load(Ordering::SeqCst), 1);
    socket.close(None).await.unwrap();
    task.abort();
}

#[tokio::test]
async fn unacknowledged_control_close_never_reconnects() {
    let dir = tempfile::tempdir().unwrap();
    let mut backend = backend().await;
    let (address, task, _) = start_two_account_proxy(
        dir.path(),
        format!("http://{}/backend-api/codex", backend.address),
    )
    .await;
    let mut socket = connect_test_websocket(address).await;
    socket
        .send(Message::Text(
            r#"{"type":"response.create","input":[]}"#.into(),
        ))
        .await
        .unwrap();
    receive(&mut backend).await;
    emit(
        &backend,
        json!({"type":"response.created","response":{"id":"parent"}}),
    );
    event(&mut socket).await;
    socket
        .send(Message::Text(
            json!({"type":"response.steer","previous_response_id":"parent","input":"change"})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    receive(&mut backend).await;
    backend.send.send(Message::Close(None)).unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(3), socket.next())
            .await
            .unwrap(),
        Some(Ok(Message::Close(_)))
    ));
    assert_eq!(backend.connections.load(Ordering::SeqCst), 1);
    task.abort();
}

#[tokio::test]
async fn native_incomplete_failure_marks_only_account_scoped_health_and_closes() {
    for (reason, unavailable) in [
        ("usage_limit_reached", true),
        ("server_is_overloaded", false),
        ("organization_spend_limit_exceeded", false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut backend = backend().await;
        let (address, task, router) = start_two_account_proxy(
            dir.path(),
            format!("http://{}/backend-api/codex", backend.address),
        )
        .await;
        let mut socket = connect_test_websocket(address).await;
        socket
            .send(Message::Text(
                r#"{"type":"response.create","input":[]}"#.into(),
            ))
            .await
            .unwrap();
        receive(&mut backend).await;
        emit(
            &backend,
            json!({"type":"response.created","response":{"id":"parent"}}),
        );
        event(&mut socket).await;
        socket
            .send(Message::Text(
                json!({"type":"response.steer","previous_response_id":"parent","input":"change"})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        receive(&mut backend).await;
        if reason == "server_is_overloaded" {
            // Capacity is independent from auth/quota availability and trips
            // only after three observations. The wire terminal supplies #3.
            router.capacity_failure("a").await;
            router.capacity_failure("a").await;
        }
        let terminal = json!({"type":"response.incomplete","response":{"id":"parent","incomplete_details":{"reason":reason}}});
        emit(&backend, terminal.clone());
        assert_eq!(event(&mut socket).await, terminal);
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(3), socket.next())
                .await
                .unwrap(),
            Some(Ok(Message::Close(_))) | None
        ));
        let snapshot = router.routing_snapshot().await;
        assert_eq!(
            !snapshot.account_states["a"].available, unavailable,
            "{reason}"
        );
        assert!(snapshot.account_states["b"].available);
        assert_eq!(
            snapshot.account_states["a"]
                .capacity_backoff_until_unix
                .is_some(),
            reason == "server_is_overloaded"
        );
        assert_eq!(backend.connections.load(Ordering::SeqCst), 1);
        task.abort();
    }
}

#[tokio::test]
async fn bridge_controls_explicitly_require_direct_or_raw() {
    let dir = tempfile::tempdir().unwrap();
    let (address, task) = start_caller_proxy(
        dir.path(),
        "http://127.0.0.1:4999/backend-api/codex".into(),
        ResponsesWebsocketMode::HttpBridge,
    )
    .await;
    let mut socket = connect_test_websocket(address).await;
    for kind in ["response.steer", "response.inject"] {
        socket
            .send(Message::Text(json!({"type":kind}).to_string().into()))
            .await
            .unwrap();
        let error = event(&mut socket).await;
        assert_eq!(error["error"]["code"], "native_control_unsupported");
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("direct or raw")
        );
    }
    task.abort();
}
