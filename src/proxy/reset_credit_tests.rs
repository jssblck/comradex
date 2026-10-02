fn reset_credit_token(workspace: &str) -> String {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&serde_json::json!({
        "exp": chrono::Utc::now().timestamp() + 3600,
        "https://api.openai.com/auth": {"chatgpt_account_id": workspace, "chatgpt_user_id": "reset-test-user"}
    })).unwrap());
    format!("e30.{payload}.sig")
}

// All credentials and endpoints in these tests are local fixtures. No live reset calls.
struct ResetCreditFixture {
    app: Arc<App>,
    router: Arc<Router>,
    calls: Arc<Mutex<Vec<(Method, String, serde_json::Value)>>>,
    server: tokio::task::JoinHandle<()>,
    home: std::path::PathBuf,
}

impl Drop for ResetCreditFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn reset_credit_fixture(
    dir: &std::path::Path,
    outcome: &'static str,
    expiry: &'static str,
) -> ResetCreditFixture {
    let home = dir.join("managed");
    fs::create_dir_all(&home).unwrap();
    fs::write(
        home.join("auth.json"),
        serde_json::to_vec(&serde_json::json!({"tokens": {
            "access_token": reset_credit_token("workspace-a"),
            "refresh_token": "fixture-refresh", "account_id": "workspace-a"
        }}))
        .unwrap(),
    )
    .unwrap();
    let (mut app, _, router) = managed_direct_test_app(dir, &home);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    Arc::get_mut(&mut app).unwrap().usage_url = format!(
        "http://{}/backend-api/wham/usage",
        listener.local_addr().unwrap()
    )
    .parse()
    .unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let consumed = Arc::new(AtomicBool::new(false));
    let server = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let recorded = recorded.clone();
            let consumed = consumed.clone();
            tokio::spawn(async move {
                let handler = service_fn(move |request: Request<Incoming>| {
                    let recorded = recorded.clone();
                    let consumed = consumed.clone();
                    async move {
                        assert_eq!(request.headers()["chatgpt-account-id"], "workspace-a");
                        assert!(
                            request.headers()[AUTHORIZATION]
                                .to_str()
                                .unwrap()
                                .starts_with("Bearer e30.")
                        );
                        let method = request.method().clone();
                        let path = request.uri().path().to_owned();
                        let bytes = request.into_body().collect().await.unwrap().to_bytes();
                        let body = if bytes.is_empty() {
                            serde_json::Value::Null
                        } else {
                            serde_json::from_slice(&bytes).unwrap()
                        };
                        recorded
                            .lock()
                            .unwrap()
                            .push((method.clone(), path.clone(), body));
                        let already_consumed = consumed.load(Ordering::SeqCst);
                        let available_count = if already_consumed { 0 } else { 1 };
                        let mut status = StatusCode::OK;
                        let response = match path.as_str() {
                            "/backend-api/wham/rate-limit-reset-credits/consume" => {
                                assert_eq!(method, Method::POST);
                                if outcome == "http_error" {
                                    status = StatusCode::INTERNAL_SERVER_ERROR;
                                }
                                let code = if outcome == "refresh_error" {
                                    "reset"
                                } else {
                                    outcome
                                };
                                if code == "reset" || code == "already_redeemed" {
                                    consumed.store(true, Ordering::SeqCst);
                                }
                                serde_json::json!({"code": code})
                            }
                            "/backend-api/wham/usage" => {
                                assert_eq!(method, Method::GET);
                                if outcome == "refresh_error" && already_consumed {
                                    status = StatusCode::SERVICE_UNAVAILABLE;
                                }
                                serde_json::json!({
                                    "rate_limit": {"primary_window": {
                                        "used_percent": if already_consumed { 0 } else { 100 },
                                        "reset_at": 4102444800_i64,
                                        "limit_window_seconds": 604800
                                    }},
                                    "rate_limit_reset_credits": {"available_count": available_count}
                                })
                            }
                            "/backend-api/wham/rate-limit-reset-credits" => {
                                assert_eq!(method, Method::GET);
                                if outcome == "details_error" {
                                    status = StatusCode::SERVICE_UNAVAILABLE;
                                }
                                serde_json::json!({
                                    "available_count": available_count,
                                    "credits": [{
                                        "id": "credit-one",
                                        "reset_type": "codex_rate_limits",
                                        "status": if already_consumed { "consumed" } else { "available" },
                                        "granted_at": "2026-10-01T01:02:03.456Z",
                                        "expires_at": expiry,
                                        "title": "Full reset"
                                    }]
                                })
                            }
                            _ => panic!("unexpected fixture request: {path}"),
                        };
                        Ok::<_, Infallible>(
                            Response::builder()
                                .status(status)
                                .body(Full::new(Bytes::from(response.to_string())))
                                .unwrap(),
                        )
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), handler)
                    .await;
            });
        }
    });
    ResetCreditFixture {
        app,
        router,
        calls,
        server,
        home,
    }
}

#[tokio::test]
async fn reset_credits_poll_is_read_only_and_keeps_precise_expiry() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = reset_credit_fixture(dir.path(), "reset", "2100-01-01T01:02:03.456+02:00").await;
    assert!(
        fixture
            .app
            .refresh_managed_usage_at(chrono::Utc::now().timestamp() as u64)
            .await
    );
    let state = fixture.router.routing_snapshot().await;
    let credits = state.account_states["a"].reset_credits.as_ref().unwrap();
    assert_eq!(credits.available_count, 1);
    assert_eq!(
        credits.credits.as_ref().unwrap()[0].expires_at.as_deref(),
        Some("2100-01-01T01:02:03.456+02:00")
    );
    assert!(
        fixture
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|(method, _, _)| *method == Method::GET)
    );
    // A newly signed-in identity must never inherit another identity's credits.
    let auth_path = fixture.home.join("auth.json");
    let mut auth: serde_json::Value =
        serde_json::from_slice(&fs::read(&auth_path).unwrap()).unwrap();
    auth["tokens"]["account_id"] = "replacement-workspace".into();
    auth["tokens"]["access_token"] = reset_credit_token("replacement-workspace").into();
    fs::write(auth_path, serde_json::to_vec(&auth).unwrap()).unwrap();
    assert!(
        fixture.router.routing_snapshot().await.account_states["a"]
            .reset_credits
            .is_none()
    );
}

#[tokio::test]
async fn reset_credit_detail_failure_preserves_usage_and_reports_unknown_details() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = reset_credit_fixture(dir.path(), "details_error", "2100-01-01T00:00:00Z").await;
    let credits = fixture.app.read_reset_credits("a").await.unwrap();
    assert_eq!(credits.available_count, 1);
    assert!(credits.credits.is_none());
    assert!(credits.error.unwrap().contains("503"));
    assert_eq!(
        fixture.router.routing_snapshot().await.account_states["a"].usage_percent,
        Some(100)
    );
}

#[tokio::test]
async fn reset_credit_expired_or_wrong_account_never_posts() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = reset_credit_fixture(dir.path(), "reset", "2000-01-01T00:00:00Z").await;
    for account in ["a", "b", "missing"] {
        assert!(
            fixture
                .app
                .use_reset_credit(account, "credit-one", "request-one")
                .await
                .is_err()
        );
    }
    assert!(
        fixture
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|(method, _, _)| *method == Method::GET)
    );
}

#[tokio::test]
async fn reset_credit_control_requires_confirmation_and_refreshes_after_targeted_post() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = reset_credit_fixture(dir.path(), "reset", "2100-01-01T00:00:00Z").await;
    let mut exhausted = hyper::HeaderMap::new();
    exhausted.insert("x-codex-primary-used-percent", "100".parse().unwrap());
    exhausted.insert("x-codex-primary-reset-at", "4102444800".parse().unwrap());
    fixture.router.quota_failure("a", &exhausted).await;
    assert!(!fixture.router.routing_snapshot().await.account_states["a"].available);
    let state = dir.path().join("state");
    fs::create_dir_all(&state).unwrap();
    let server = crate::control::ControlServer::bind(
        &state,
        dir.path().join("config.toml"),
        fixture.app.config.clone(),
        fixture.router.clone(),
        fixture.app.stats.clone(),
    )
    .unwrap()
    .with_app(fixture.app.clone());
    let worker = tokio::spawn(server.run());
    let mut stream = tokio::net::UnixStream::connect(crate::control::socket_path(&state))
        .await
        .unwrap();
    stream.write_all(b"{\"command\":\"ui_use_reset_credit\",\"account\":\"a\",\"credit_id\":\"credit-one\",\"request_id\":\"request-one\",\"confirm\":false}\n").await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&response).unwrap()["ok"],
        false
    );
    assert!(fixture.calls.lock().unwrap().is_empty());
    let result = tokio::task::spawn_blocking(move || {
        crate::control::use_reset_credit(&state, "a", "credit-one", "request-one")
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.code, crate::reset_credits::ResetOutcome::Reset);
    assert!(result.refresh_error.is_none());
    let snapshot = fixture.router.routing_snapshot().await;
    assert_eq!(snapshot.account_states["a"].usage_percent, Some(0));
    assert!(snapshot.account_states["a"].available);
    assert_eq!(
        snapshot.account_states["a"]
            .reset_credits
            .as_ref()
            .unwrap()
            .available_count,
        0
    );
    let calls = fixture.calls.lock().unwrap();
    let posts: Vec<_> = calls
        .iter()
        .filter(|(method, _, _)| *method == Method::POST)
        .collect();
    assert_eq!(posts.len(), 1);
    assert_eq!(
        posts[0].2,
        serde_json::json!({"credit_id": "credit-one", "redeem_request_id": "request-one"})
    );
    worker.abort();
}

#[tokio::test]
async fn reset_credit_outcomes_are_distinct_and_failed_posts_are_not_retried() {
    for outcome in [
        "nothing_to_reset",
        "no_credit",
        "already_redeemed",
        "http_error",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let fixture = reset_credit_fixture(dir.path(), outcome, "2100-01-01T00:00:00Z").await;
        let result = fixture
            .app
            .use_reset_credit("a", "credit-one", "stable-request")
            .await;
        if outcome == "http_error" {
            assert!(result.is_err());
        } else {
            assert_eq!(
                serde_json::to_value(result.unwrap()).unwrap()["code"],
                outcome
            );
        }
        assert_eq!(
            fixture
                .calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(method, _, _)| *method == Method::POST)
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn reset_credit_success_survives_followup_refresh_failure() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = reset_credit_fixture(dir.path(), "refresh_error", "2100-01-01T00:00:00Z").await;
    let result = fixture
        .app
        .use_reset_credit("a", "credit-one", "request-one")
        .await
        .unwrap();
    assert_eq!(result.code, crate::reset_credits::ResetOutcome::Reset);
    assert!(result.refresh_error.unwrap().contains("503"));
    assert_eq!(
        fixture
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, _, _)| *method == Method::POST)
            .count(),
        1
    );
    assert!(
        fixture.router.routing_snapshot().await.account_states["a"]
            .reset_credits
            .is_none()
    );
}

#[tokio::test]
async fn reset_credit_cannot_race_an_in_progress_usage_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = reset_credit_fixture(dir.path(), "reset", "2100-01-01T00:00:00Z").await;
    let _refresh = fixture.app.usage_locks["a"].lock().await;
    assert!(
        fixture
            .app
            .use_reset_credit("a", "credit-one", "request-one")
            .await
            .unwrap_err()
            .to_string()
            .contains("in progress")
    );
    assert!(fixture.calls.lock().unwrap().is_empty());
}
