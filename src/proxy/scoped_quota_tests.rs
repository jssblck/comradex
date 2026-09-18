async fn quota_refusal_upstream(
    status: StatusCode,
    payload: Bytes,
) -> (
    std::net::SocketAddr,
    tokio::task::JoinHandle<()>,
    Arc<Mutex<Vec<String>>>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let server_seen = seen.clone();
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let seen = server_seen.clone();
            let payload = payload.clone();
            tokio::spawn(async move {
                let service = service_fn(move |req: Request<Incoming>| {
                    let payload = payload.clone();
                    let authorization = req.headers()[AUTHORIZATION].to_str().unwrap().to_owned();
                    seen.lock().unwrap().push(authorization.clone());
                    async move {
                        // B can serve the request. A scoped refusal must still
                        // return A's original response instead of trying B.
                        let (status, payload) = if authorization == "Bearer token-b" {
                            (StatusCode::OK, Bytes::from_static(b"data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_b\",\"status\":\"completed\"}}\n\n"))
                        } else {
                            (status, payload)
                        };
                        Ok::<_, Infallible>(
                            Response::builder()
                                .status(status)
                                .header("retry-after", "17")
                                .header("x-upstream-marker", "unchanged")
                                .body(Full::new(payload))
                                .unwrap(),
                        )
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
    });
    (address, task, seen)
}

#[tokio::test]
async fn scoped_quota_http_preserves_refusal_without_replay_or_quarantine() {
    for code in [
        "credit_balance_exhausted",
        "organization_spend_limit_exceeded",
        "project_spend_limit_exceeded",
        "organization_usage_limit_exceeded",
    ] {
        let json =
            serde_json::json!({"error":{"code":code,"message":"The usage limit has been reached"}});
        let late = serde_json::json!({"type":"response.failed","response":{"id":"resp_scoped","status":"failed","error":{"code":code}}});
        for (status, payload) in [
            (StatusCode::TOO_MANY_REQUESTS, Bytes::from(json.to_string())),
            (StatusCode::PAYMENT_REQUIRED, Bytes::from(json.to_string())),
            (
                StatusCode::TOO_MANY_REQUESTS,
                Bytes::from(serde_json::json!({"type":code}).to_string()),
            ),
            (
                StatusCode::PAYMENT_REQUIRED,
                Bytes::from(serde_json::json!({"type":code}).to_string()),
            ),
            (StatusCode::OK, Bytes::from(late.to_string())),
            (
                StatusCode::OK,
                Bytes::from(format!(
                    "data: {{\"type\":\"response.created\",\"response\":{{\"id\":\"resp_scoped\"}}}}\n\ndata: {late}\n\n"
                )),
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let (upstream, server, seen) = quota_refusal_upstream(status, payload.clone()).await;
            let (address, proxy, router) =
                start_two_account_proxy(dir.path(), format!("http://{upstream}/backend-api/codex"))
                    .await;
            let client = TestClient::builder(TokioExecutor::new()).build_http::<Full<Bytes>>();
            let response = client
                .request(
                    Request::builder()
                        .method(Method::POST)
                        .uri(format!("http://{address}/0123456789abcdef/v1/responses"))
                        .body(Full::new(Bytes::from_static(
                            br#"{"input":[],"stream":true}"#,
                        )))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), status);
            assert_eq!(response.headers()["retry-after"], "17");
            assert_eq!(response.headers()["x-upstream-marker"], "unchanged");
            assert_eq!(
                response.into_body().collect().await.unwrap().to_bytes(),
                payload
            );
            assert_eq!(*seen.lock().unwrap(), ["Bearer token-a"]);
            let snapshot = router.routing_snapshot().await;
            assert!(snapshot.account_states.values().all(|state| state.available
                && state.quota_windows.is_empty()
                && state.capacity_backoff_until_unix.is_none()));
            assert!(
                router
                    .affinity
                    .get(&router.affinity.key("previous-response:resp_scoped"))
                    .await
                    .is_none()
            );
            proxy.abort();
            server.abort();
        }
    }
}

#[tokio::test]
async fn scoped_quota_bridge_does_not_replay_precreated_or_late_errors() {
    for status in [
        StatusCode::TOO_MANY_REQUESTS,
        StatusCode::PAYMENT_REQUIRED,
        StatusCode::OK,
    ] {
        let error = serde_json::json!({"code":"organization_usage_limit_exceeded","message":"usage limit reached"});
        let payload = if status.is_success() {
            Bytes::from(format!(
                "data: {{\"type\":\"error\",\"error\":{error}}}\n\n"
            ))
        } else {
            Bytes::from(serde_json::json!({"error":error}).to_string())
        };
        let dir = tempfile::tempdir().unwrap();
        let (upstream, server, seen) = quota_refusal_upstream(status, payload).await;
        let (address, proxy, router) =
            start_two_account_proxy(dir.path(), format!("http://{upstream}/backend-api/codex"))
                .await;
        let mut websocket = connect_test_websocket(address).await;
        websocket
            .send(Message::Text(
                r#"{"type":"response.create","input":[]}"#.into(),
            ))
            .await
            .unwrap();
        let message = tokio::time::timeout(Duration::from_secs(5), websocket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
        assert_eq!(value["error"], error);
        assert_eq!(*seen.lock().unwrap(), ["Bearer token-a"]);
        assert!(
            router
                .routing_snapshot()
                .await
                .account_states
                .values()
                .all(|state| state.available)
        );
        proxy.abort();
        server.abort();
    }
}

#[tokio::test]
async fn ordinary_account_quota_still_rotates_after_bounded_inspection() {
    for code in ["usage_limit_exceeded", "rate_limit_exceeded", "slow_down"] {
        let payload = Bytes::from(serde_json::json!({"error":{"code":code}}).to_string());
        let dir = tempfile::tempdir().unwrap();
        let (upstream, server, seen) =
            quota_refusal_upstream(StatusCode::TOO_MANY_REQUESTS, payload).await;
        let (address, proxy, router) =
            start_two_account_proxy(dir.path(), format!("http://{upstream}/backend-api/codex"))
                .await;
        let client = TestClient::builder(TokioExecutor::new()).build_http::<Full<Bytes>>();
        let response = client
            .request(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("http://{address}/0123456789abcdef/v1/responses"))
                    .body(Full::new(Bytes::from_static(br#"{"input":[]}"#)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        response.into_body().collect().await.unwrap();
        assert_eq!(*seen.lock().unwrap(), ["Bearer token-a", "Bearer token-b"]);
        assert!(!router.routing_snapshot().await.account_states["a"].available);
        proxy.abort();
        server.abort();
    }
}

#[tokio::test]
async fn uninspected_quota_body_is_forwarded_without_guessing_scope() {
    let payload = Bytes::from(serde_json::json!({"padding":"x".repeat(FILE_CREATE_RESPONSE_LIMIT + 1),"error":{"code":"project_spend_limit_exceeded"}}).to_string());
    let dir = tempfile::tempdir().unwrap();
    let (upstream, server, seen) =
        quota_refusal_upstream(StatusCode::TOO_MANY_REQUESTS, payload.clone()).await;
    let (address, proxy, router) =
        start_two_account_proxy(dir.path(), format!("http://{upstream}/backend-api/codex")).await;
    let client = TestClient::builder(TokioExecutor::new()).build_http::<Full<Bytes>>();
    let response = client
        .request(
            Request::builder()
                .method(Method::POST)
                .uri(format!("http://{address}/0123456789abcdef/v1/responses"))
                .body(Full::new(Bytes::from_static(br#"{"input":[]}"#)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        payload
    );
    assert_eq!(*seen.lock().unwrap(), ["Bearer token-a"]);
    assert!(
        router
            .routing_snapshot()
            .await
            .account_states
            .values()
            .all(|state| state.available)
    );
    proxy.abort();
    server.abort();
}

#[tokio::test]
async fn scoped_quota_upgrade_preserves_refusal_and_account_health() {
    for status in [StatusCode::TOO_MANY_REQUESTS, StatusCode::PAYMENT_REQUIRED] {
        let payload = Bytes::from_static(br#"{"error":{"code":"credit_balance_exhausted"}}"#);
        let dir = tempfile::tempdir().unwrap();
        let (upstream, server, seen) = quota_refusal_upstream(status, payload.clone()).await;
        let (address, proxy, router) = start_two_account_proxy_with_mode(
            dir.path(),
            format!("http://{upstream}/backend-api/codex"),
            ResponsesWebsocketMode::Direct,
        )
        .await;
        let stream = TcpStream::connect(address).await.unwrap();
        let request = format!("ws://{address}/0123456789abcdef/backend-api/codex/responses")
            .into_client_request()
            .unwrap();
        let error = tokio_tungstenite::client_async(request, stream)
            .await
            .unwrap_err();
        let tokio_tungstenite::tungstenite::Error::Http(response) = error else {
            panic!("unexpected upgrade error: {error}");
        };
        assert_eq!(response.status(), status);
        assert_eq!(response.headers()["retry-after"], "17");
        assert_eq!(response.body().as_ref().unwrap(), &payload.to_vec());
        assert_eq!(*seen.lock().unwrap(), ["Bearer token-a"]);
        assert!(
            router
                .routing_snapshot()
                .await
                .account_states
                .values()
                .all(|state| state.available)
        );
        proxy.abort();
        server.abort();
    }
}

#[tokio::test]
async fn scoped_quota_direct_precreated_error_keeps_owner_without_replay() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let server_connections = connections.clone();
    let server = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            server_connections.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut websocket = tokio_tungstenite::accept_async(stream).await.unwrap();
                while let Some(Ok(Message::Text(_))) = websocket.next().await {
                    websocket.send(Message::Text(r#"{ "type": "error", "error": {"code":"project_spend_limit_exceeded"} }"#.into())).await.unwrap();
                }
            });
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let (address, proxy, router) = start_two_account_proxy_with_mode(
        dir.path(),
        format!("http://{upstream}/backend-api/codex"),
        ResponsesWebsocketMode::Direct,
    )
    .await;
    let stream = TcpStream::connect(address).await.unwrap();
    let request = format!("ws://{address}/0123456789abcdef/backend-api/codex/responses")
        .into_client_request()
        .unwrap();
    let (mut websocket, _) = tokio_tungstenite::client_async(request, stream)
        .await
        .unwrap();
    for _ in 0..2 {
        websocket
            .send(Message::Text(
                r#"{"type":"response.create","input":[]}"#.into(),
            ))
            .await
            .unwrap();
        let message = tokio::time::timeout(Duration::from_secs(5), websocket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            message.to_text().unwrap(),
            r#"{ "type": "error", "error": {"code":"project_spend_limit_exceeded"} }"#
        );
        assert!(
            router
                .routing_snapshot()
                .await
                .account_states
                .values()
                .all(|state| state.available)
        );
    }
    assert_eq!(connections.load(Ordering::SeqCst), 1);
    proxy.abort();
    server.abort();
}
