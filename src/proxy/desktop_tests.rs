use super::*;
use crate::config::{AccountConfig, ProxyConfig};
use std::collections::BTreeMap;

struct Fixture {
    address: std::net::SocketAddr,
    app: Arc<App>,
    seen: Arc<StdMutex<Vec<(String, hyper::HeaderMap, bytes::Bytes)>>>,
    tasks: Vec<tokio::task::AbortHandle>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

async fn fixture(dir: &std::path::Path) -> Fixture {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let seen = Arc::new(StdMutex::new(Vec::new()));
    let upstream_seen = seen.clone();
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = upstream.accept().await.unwrap();
            let seen = upstream_seen.clone();
            tokio::spawn(async move {
                let service = service_fn(move |mut request: Request<Incoming>| {
                    let seen = seen.clone();
                    async move {
                        if request.uri().path() == "/backend-api/socket" {
                            let accept =
                                tokio_tungstenite::tungstenite::handshake::derive_accept_key(
                                    request.headers()[SEC_WEBSOCKET_KEY].as_bytes(),
                                );
                            let upgrade = hyper::upgrade::on(&mut request);
                            seen.lock().unwrap().push((
                                request.uri().to_string(),
                                request.headers().clone(),
                                bytes::Bytes::new(),
                            ));
                            tokio::spawn(async move {
                                let mut socket = WebSocketStream::from_raw_socket(
                                    TokioIo::new(upgrade.await.unwrap()),
                                    Role::Server,
                                    None,
                                )
                                .await;
                                while let Some(Ok(message)) = socket.next().await {
                                    if message.is_close() {
                                        break;
                                    }
                                    socket.send(message).await.unwrap();
                                }
                            });
                            return Ok::<_, Infallible>(
                                Response::builder()
                                    .status(StatusCode::SWITCHING_PROTOCOLS)
                                    .header(CONNECTION, "upgrade")
                                    .header(UPGRADE, "websocket")
                                    .header(SEC_WEBSOCKET_ACCEPT, accept)
                                    .body(empty_body())
                                    .unwrap(),
                            );
                        }
                        let (parts, body) = request.into_parts();
                        let bytes = body.collect().await.unwrap().to_bytes();
                        let path = parts.uri.path().to_owned();
                        let response_case = parts.uri.query().unwrap_or("").to_owned();
                        seen.lock().unwrap().push((
                            parts.uri.to_string(),
                            parts.headers,
                            bytes.clone(),
                        ));
                        if path == "/backend-api/redirect" {
                            return Ok::<_, Infallible>(
                                Response::builder()
                                    .status(StatusCode::TEMPORARY_REDIRECT)
                                    .header(LOCATION, "https://example.invalid/target")
                                    .body(empty_body())
                                    .unwrap(),
                            );
                        }
                        let body = if path == "/backend-api/wham/usage"
                            && response_case == "malformed"
                        {
                            bytes_body(bytes::Bytes::from_static(
                                b"not-json: original upstream response\n",
                            ))
                        } else if path == "/backend-api/wham/usage" && response_case == "oversized"
                        {
                            bytes_body(bytes::Bytes::from(vec![
                                b'x';
                                MAX_USAGE_RESPONSE_BYTES + 123
                            ]))
                        } else if path == "/backend-api/wham/usage" {
                            json_body(serde_json::json!({
                                "plan_type":"plus", "identity":"native-user",
                                "rate_limit":{"allowed":false,"limit_reached":true,"primary_window":{"used_percent":100,"reset_at":123}},
                                "credits":{"has_credits":false,"balance":"0","unlimited":false},
                                "rate_limit_reached_type":"weekly", "rate_limit_upsell":{"type":"reserve"}
                            }))
                        } else {
                            bytes_body(bytes)
                        };
                        Ok::<_, Infallible>(
                            Response::builder()
                                .header(CONTENT_TYPE, "application/json")
                                .header("etag", "upstream-etag")
                                .body(body)
                                .unwrap(),
                        )
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .with_upgrades()
                    .await;
            });
        }
    });
    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = tcp.local_addr().unwrap();
    let listener = ListenerConfig {
        address,
        pool: "default".into(),
    };
    let mut accounts = BTreeMap::new();
    for name in ["a", "b"] {
        let home = dir.join(name);
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("auth.json"),
            format!(r#"{{"tokens":{{"access_token":"token-{name}"}}}}"#),
        )
        .unwrap();
        accounts.insert(name.into(), AccountConfig::CodexHome { path: home });
    }
    let config = Arc::new(Config {
        proxy: ProxyConfig {
            upstream: format!("http://{upstream_address}/backend-api/codex"),
            state_dir: Some(dir.join("state")),
            installation_secret: "0123456789abcdef".into(),
            affinity_key: "0123456789abcdef0123456789abcdef".into(),
            ..Default::default()
        },
        accounts,
        listeners: BTreeMap::from([("default".into(), listener.clone())]),
        pools: BTreeMap::from([(
            "default".into(),
            PoolConfig {
                members: vec!["a".into(), "b".into()],
                ..Default::default()
            },
        )]),
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
    let mut app = App::new_unvalidated(config, router, Arc::new(Stats::default())).unwrap();
    Arc::get_mut(&mut app).unwrap().service_nonce = Some("desktop-test-health-nonce".into());
    let serving = tokio::spawn(app.clone().serve_desktop_tcp(
        listener,
        tcp,
        format!("http://{upstream_address}"),
    ));
    Fixture {
        address,
        app,
        seen,
        tasks: vec![task.abort_handle(), serving.abort_handle()],
    }
}

async fn json(response: reqwest::Response) -> serde_json::Value {
    serde_json::from_slice(&response.bytes().await.unwrap()).unwrap()
}

#[tokio::test]
async fn desktop_usage_tracks_real_pool_exhaustion_without_selecting_or_changing_identity() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = fixture(dir.path()).await;
    let client = reqwest::Client::new();
    let url = format!(
        "http://{}/0123456789abcdef/backend-api/wham/usage",
        fixture.address
    );
    let read = || {
        client
            .get(&url)
            .bearer_auth("desktop-identity")
            .header("chatgpt-account-id", "desktop-account")
            .send()
    };
    let healthy = json(read().await.unwrap()).await;
    assert_eq!(healthy["rate_limit"]["allowed"], true);
    assert_eq!(healthy["plan_type"], "plus");
    assert_eq!(healthy["credits"]["balance"], "0");
    assert_eq!(healthy["rate_limit"]["primary_window"]["reset_at"], 123);
    assert_eq!(fixture.app.router.wired_account("default").await, None);
    let mut quota = hyper::HeaderMap::new();
    quota.insert("retry-after", "600".parse().unwrap());
    fixture.app.router.quota_failure("a", &quota).await;
    let alternate = json(read().await.unwrap()).await;
    assert_eq!(alternate["rate_limit"]["allowed"], true);
    fixture.app.router.quota_failure("b", &quota).await;
    let exhausted = json(read().await.unwrap()).await;
    assert_eq!(exhausted["rate_limit"]["allowed"], false);
    assert_eq!(
        exhausted["rate_limit"]["primary_window"]["used_percent"],
        100
    );
    let seen = fixture.seen.lock().unwrap();
    assert_eq!(seen.len(), 3);
    for (_, headers, _) in seen.iter() {
        assert_eq!(headers[AUTHORIZATION], "Bearer desktop-identity");
        assert_eq!(headers["chatgpt-account-id"], "desktop-account");
    }
}

#[tokio::test]
async fn desktop_backend_preserves_headers_body_and_query_and_requires_authentication() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = fixture(dir.path()).await;
    let client = reqwest::Client::new();
    let url = format!(
        "http://{}/0123456789abcdef/backend-api/conversations?cursor=a%2Fb",
        fixture.address
    );
    let response = client
        .post(&url)
        .bearer_auth("desktop")
        .header("chatgpt-account-id", "identity")
        .header("x-desktop-feature", "native")
        .body("payload")
        .send()
        .await
        .unwrap();
    assert_eq!(response.text().await.unwrap(), "payload");
    let unauthorized = client.get(&url).send().await.unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    let unknown = client
        .get(format!("http://{}/outside", fixture.address))
        .bearer_auth("desktop")
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    for path in [
        "/backend-api/codex/models",
        "/backend-api/wham/usage",
        "/wrong-secret/backend-api/wham/usage",
    ] {
        let response = client
            .get(format!("http://{}{path}", fixture.address))
            .bearer_auth("fake-bearer")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(!response.text().await.unwrap().contains("native-user"));
    }
    let seen = fixture.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, "/backend-api/conversations?cursor=a%2Fb");
    assert_eq!(seen[0].1["x-desktop-feature"], "native");
    assert_eq!(seen[0].1[AUTHORIZATION], "Bearer desktop");
    assert_eq!(seen[0].2, "payload");
}

#[tokio::test]
async fn desktop_native_models_use_comradex_account_routing() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = fixture(dir.path()).await;
    let response = reqwest::Client::new()
        .get(format!(
            "http://{}/0123456789abcdef/backend-api/codex/models",
            fixture.address
        ))
        .bearer_auth("desktop")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let seen = fixture.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, "/backend-api/codex/models");
    assert_ne!(seen[0].1[AUTHORIZATION], "Bearer desktop");
}

#[tokio::test]
async fn desktop_missing_credentials_cannot_report_healthy_pool() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = fixture(dir.path()).await;
    std::fs::remove_file(dir.path().join("a/auth.json")).unwrap();
    std::fs::remove_file(dir.path().join("b/auth.json")).unwrap();
    assert!(
        !fixture
            .app
            .desktop_pool_available("default", &hyper::HeaderMap::new())
            .await
    );
}

#[tokio::test]
async fn desktop_unprojectable_usage_preserves_success_status_headers_and_exact_body() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = fixture(dir.path()).await;
    let client = reqwest::Client::new();
    for (case, expected) in [
        (
            "malformed",
            b"not-json: original upstream response\n".to_vec(),
        ),
        ("oversized", vec![b'x'; MAX_USAGE_RESPONSE_BYTES + 123]),
    ] {
        let response = client
            .get(format!(
                "http://{}/0123456789abcdef/backend-api/wham/usage?{case}",
                fixture.address
            ))
            .bearer_auth("desktop")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["etag"], "upstream-etag");
        assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
        assert_eq!(
            response.bytes().await.unwrap().as_ref(),
            expected.as_slice()
        );
    }
}

#[tokio::test]
async fn desktop_usage_projection_timeout_replays_prefix_and_remaining_stream() {
    let first = futures_util::stream::iter([Ok::<_, std::io::Error>(Frame::data(
        bytes::Bytes::from_static(b"prefix"),
    ))]);
    let rest = futures_util::stream::once(async {
        tokio::time::sleep(Duration::from_millis(20)).await;
        Ok::<_, std::io::Error>(Frame::data(bytes::Bytes::from_static(b"-remaining")))
    });
    let body = BodyExt::boxed(http_body_util::StreamBody::new(first.chain(rest)));
    let (body, projected) =
        desktop::project_usage_body(body, true, 1024, Duration::from_millis(1)).await;
    assert!(!projected);
    assert_eq!(body.collect().await.unwrap().to_bytes(), "prefix-remaining");
}

#[tokio::test]
async fn desktop_usage_only_exhaustion_and_elapsed_reset_control_pool_availability() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = fixture(dir.path()).await;
    let mut headers = hyper::HeaderMap::new();
    headers.insert("x-codex-primary-used-percent", "100".parse().unwrap());
    let future_reset = chrono::Utc::now().timestamp() + 600;
    headers.insert(
        "x-codex-primary-reset-at",
        future_reset.to_string().parse().unwrap(),
    );
    fixture.app.router.observe_headers("a", &headers).await;
    assert!(
        fixture
            .app
            .desktop_pool_available("default", &hyper::HeaderMap::new())
            .await
    );
    fixture.app.router.observe_headers("b", &headers).await;
    assert!(
        !fixture
            .app
            .desktop_pool_available("default", &hyper::HeaderMap::new())
            .await
    );
    // No quota rejection occurred. A reported elapsed reset restores availability.
    headers.insert(
        "x-codex-primary-reset-at",
        (chrono::Utc::now().timestamp() - 1)
            .to_string()
            .parse()
            .unwrap(),
    );
    fixture.app.router.observe_headers("a", &headers).await;
    assert!(
        fixture
            .app
            .desktop_pool_available("default", &hyper::HeaderMap::new())
            .await
    );
}

#[tokio::test]
async fn desktop_readiness_requires_service_nonce_and_never_calls_upstream() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = fixture(dir.path()).await;
    let client = reqwest::Client::new();
    let ready = client
        .get(format!(
            "http://{}/__comradex_health/desktop-test-health-nonce",
            fixture.address
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(ready.status(), StatusCode::OK);
    assert_eq!(json(ready).await["status"], "ok");
    let wrong = client
        .get(format!(
            "http://{}/__comradex_health/wrong",
            fixture.address
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::NOT_FOUND);
    assert!(fixture.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn desktop_backend_redirect_is_returned_without_forwarding_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = fixture(dir.path()).await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let response = client
        .get(format!(
            "http://{}/0123456789abcdef/backend-api/redirect",
            fixture.address
        ))
        .bearer_auth("desktop")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        response.headers()[LOCATION],
        "https://example.invalid/target"
    );
    assert_eq!(fixture.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn desktop_backend_websocket_preserves_identity_and_upgrades_bidirectionally() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let dir = tempfile::tempdir().unwrap();
    let fixture = fixture(dir.path()).await;
    let mut request = format!(
        "ws://{}/0123456789abcdef/backend-api/socket",
        fixture.address
    )
    .into_client_request()
    .unwrap();
    request
        .headers_mut()
        .insert(AUTHORIZATION, "Bearer desktop-socket".parse().unwrap());
    request
        .headers_mut()
        .insert("chatgpt-account-id", "desktop-id".parse().unwrap());
    let stream = tokio::net::TcpStream::connect(fixture.address)
        .await
        .unwrap();
    let (mut socket, _) = tokio_tungstenite::client_async(request, stream)
        .await
        .unwrap();
    socket
        .send(Message::Text("native message".into()))
        .await
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(3), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(response, Message::Text("native message".into()));
    socket.close(None).await.unwrap();
    let seen = fixture.seen.lock().unwrap();
    assert_eq!(seen[0].1[AUTHORIZATION], "Bearer desktop-socket");
    assert_eq!(seen[0].1["chatgpt-account-id"], "desktop-id");
}

#[tokio::test]
async fn desktop_rejects_oversized_upload_and_absolute_target_before_upstream() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let dir = tempfile::tempdir().unwrap();
    let fixture = fixture(dir.path()).await;
    for (target, length, expected) in [
        (
            "/0123456789abcdef/backend-api/upload",
            1024 * 1024 * 1024,
            "413",
        ),
        ("http://foreign.invalid/backend-api/usage", 0, "404"),
    ] {
        let mut stream = tokio::net::TcpStream::connect(fixture.address)
            .await
            .unwrap();
        stream.write_all(format!("POST {target} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer desktop\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
        let mut response = [0; 1024];
        let count = tokio::time::timeout(Duration::from_secs(3), stream.read(&mut response))
            .await
            .unwrap()
            .unwrap();
        let response = String::from_utf8_lossy(&response[..count]);
        assert!(
            response.starts_with(&format!("HTTP/1.1 {expected}")),
            "{response}"
        );
    }
    assert!(fixture.seen.lock().unwrap().is_empty());
}
