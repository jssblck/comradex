// Every reset-credit operation in this module targets a loopback mock with synthetic credentials.
type SeenCreditRequest = (Method, String, hyper::HeaderMap, Value);

struct CreditUpstream {
    url: String,
    seen: Arc<AsyncMutex<Vec<SeenCreditRequest>>>,
    response: Arc<AsyncMutex<(StatusCode, Value)>>,
    list_status: Arc<AtomicUsize>,
    spends: Arc<AtomicUsize>,
    hold: Arc<AtomicBool>,
    started: Arc<Notify>,
    release: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for CreditUpstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl CreditUpstream {
    async fn new() -> Self {
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", tcp.local_addr().unwrap());
        let seen = Arc::new(AsyncMutex::new(Vec::new()));
        let response = Arc::new(AsyncMutex::new((
            StatusCode::OK,
            json!({ "code": "reset" }),
        )));
        let list_status = Arc::new(AtomicUsize::new(200));
        let spends = Arc::new(AtomicUsize::new(0));
        let hold = Arc::new(AtomicBool::new(false));
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let redeemed = Arc::new(AsyncMutex::new(std::collections::HashSet::new()));
        let (s, r, l, n, h, st, rel) = (
            seen.clone(),
            response.clone(),
            list_status.clone(),
            spends.clone(),
            hold.clone(),
            started.clone(),
            release.clone(),
        );
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                let (stream, _) = tcp.accept().await.unwrap();
                let (s, r, l, n, h, st, rel, redeemed) = (
                    s.clone(),
                    r.clone(),
                    l.clone(),
                    n.clone(),
                    h.clone(),
                    st.clone(),
                    rel.clone(),
                    redeemed.clone(),
                );
                connections.spawn(async move {
                    let service = service_fn(move |req: Request<Incoming>| {
                        let (s, r, l, n, h, st, rel, redeemed) =
                            (s.clone(), r.clone(), l.clone(), n.clone(), h.clone(), st.clone(), rel.clone(), redeemed.clone());
                        async move {
                            let (parts, body) = req.into_parts();
                            let bytes = body.collect().await.unwrap().to_bytes();
                            let body = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
                            let path = parts.uri.path().to_owned();
                            s.lock().await.push((parts.method.clone(), path.clone(), parts.headers, body.clone()));
                            let (status, body) = match (parts.method, path.as_str()) {
                                (Method::GET, "/rate-limit-reset-credits") => (
                                    StatusCode::from_u16(l.load(Ordering::Relaxed) as u16).unwrap(),
                                    json!({ "available_count": 1, "credits": [
                                        { "id": "credit-ada", "status": "available", "reset_type": "codex_rate_limits", "granted_at": "2026-01-01T00:00:00Z", "expires_at": "2100-01-01T00:00:00Z" },
                                        { "id": "expired", "status": "available", "reset_type": "codex_rate_limits", "granted_at": "2026-01-01T00:00:00Z", "expires_at": "2000-01-01T00:00:00Z" },
                                    ] }),
                                ),
                                (Method::GET, "/usage") => (StatusCode::OK, json!({
                                    "rate_limit": { "primary_window": {
                                        "used_percent": 2, "limit_window_seconds": 604800, "reset_at": 4102444800_i64,
                                    } },
                                })),
                                (Method::POST, "/rate-limit-reset-credits/consume") => {
                                    st.notify_one();
                                    if h.load(Ordering::Relaxed) { rel.notified().await; }
                                    let (status, mut result) = r.lock().await.clone();
                                    if status.is_success() && result["code"] == "reset" {
                                        if redeemed.lock().await.insert(body["redeem_request_id"].as_str().unwrap().to_owned()) {
                                            n.fetch_add(1, Ordering::Relaxed);
                                        } else { result = json!({ "code": "already_redeemed" }); }
                                    }
                                    (status, result)
                                }
                                _ => panic!("unexpected mock request"),
                            };
                            Ok::<_, Infallible>(Response::builder().status(status)
                                .header(CONTENT_TYPE, "application/json").body(json_body(body)).unwrap())
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service).await;
                });
            }
        });
        Self {
            url,
            seen,
            response,
            list_status,
            spends,
            hold,
            started,
            release,
            task,
        }
    }
}

const REDEEM_ID: &str = "11111111-1111-5111-8111-111111111111";

fn credit_call_body(account: &str) -> Value {
    json!({
        "auth_index": account, "method": "POST",
        "url": format!("{}/consume", reset_credits::CODEX_CREDITS_URL),
        "header": { "Authorization": "Bearer attacker", "Chatgpt-Account-Id": "wrong-account" },
        "data": json!({ "credit_id": "credit-ada", "redeem_request_id": REDEEM_ID }).to_string(),
    })
}

async fn post_credit(fixture: &Fixture, body: Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{}/v0/management/api-call", fixture.urls[0]))
        .bearer_auth(KEY)
        .header(CONTENT_TYPE, "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap()
}

async fn acknowledge_reset(fixture: &Fixture, account: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{}/v0/management/reset-quota", fixture.urls[0]))
        .bearer_auth(KEY)
        .header(CONTENT_TYPE, "application/json")
        .body(json!({ "auth_index": account }).to_string())
        .send()
        .await
        .unwrap()
}

async fn block_codex(fixture: &Fixture) {
    let headers = hyper::HeaderMap::from_iter([
        ("retry-after".parse().unwrap(), "3600".parse().unwrap()),
        (
            "x-codex-primary-used-percent".parse().unwrap(),
            "100".parse().unwrap(),
        ),
        (
            "x-codex-primary-reset-at".parse().unwrap(),
            "4102444800".parse().unwrap(),
        ),
    ]);
    fixture.app.router.quota_failure("ada", &headers).await;
}

#[tokio::test]
async fn reset_credits_reads_only_the_selected_account_and_preserves_provider_data() {
    let upstream = CreditUpstream::new().await;
    let fixture = Fixture::with_credit_upstream(Some(&upstream.url)).await;
    let home = fixture.app.config.accounts["ada"].home().unwrap();
    fs::write(
        home.join("auth.json"),
        json!({
            "tokens": { "access_token": "test-codex-secret", "account_id": "ada-account" },
        })
        .to_string(),
    )
    .unwrap();
    let before = fixture.app.router.routing_snapshot().await;
    let response = json_response(
        fixture
            .call("ada", "GET", reset_credits::CODEX_CREDITS_URL)
            .await,
    )
    .await;
    assert_eq!(response["status_code"], 200);
    let body: Value = serde_json::from_str(response["body"].as_str().unwrap()).unwrap();
    assert_eq!(body["credits"].as_array().unwrap().len(), 2);
    assert_eq!(body["credits"][0]["id"], "credit-ada");
    let seen = upstream.seen.lock().await;
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, Method::GET);
    assert_eq!(seen[0].2[AUTHORIZATION], "Bearer test-codex-secret");
    assert_eq!(seen[0].2["chatgpt-account-id"], "ada-account");
    assert_eq!(upstream.spends.load(Ordering::Relaxed), 0);
    assert_eq!(before, fixture.app.router.routing_snapshot().await);
    fixture.app.shutdown_connections().await;
}

#[tokio::test]
async fn reset_credit_confirmation_clears_only_quota_and_retries_keep_the_same_id() {
    let upstream = CreditUpstream::new().await;
    let fixture = Fixture::with_credit_upstream(Some(&upstream.url)).await;
    fixture
        .app
        .router
        .set_preferred("codex", Some("ada".into()))
        .await;
    fixture
        .app
        .router
        .set_preserved("claude", Some("anna".into()))
        .await;
    fixture
        .observe(
            "ada",
            &[("primary", Some(100), Some(604800), Some(4102444800))],
        )
        .await;
    block_codex(&fixture).await;
    assert_eq!(
        acknowledge_reset(&fixture, "ada").await.status(),
        StatusCode::CONFLICT
    );
    assert_eq!(upstream.spends.load(Ordering::Relaxed), 0);
    let result = json_response(post_credit(&fixture, credit_call_body("ada")).await).await;
    assert_eq!(result["status_code"], 200);
    assert_eq!(
        serde_json::from_str::<Value>(result["body"].as_str().unwrap()).unwrap()["code"],
        "reset"
    );
    assert_eq!(
        acknowledge_reset(&fixture, "ada").await.status(),
        StatusCode::OK
    );
    let state = fixture.app.router.routing_snapshot().await;
    assert!(state.account_states["ada"].available);
    assert_eq!(
        state.account_states["ada"].usage_windows["primary"].used_percent,
        Some(2)
    );
    assert_eq!(state.preferred_accounts["codex"], "ada");
    assert_eq!(state.preserved_accounts["claude"], "anna");

    // An old receipt cannot clear a subsequent block.
    block_codex(&fixture).await;
    let result = json_response(post_credit(&fixture, credit_call_body("ada")).await).await;
    assert_eq!(
        serde_json::from_str::<Value>(result["body"].as_str().unwrap()).unwrap()["code"],
        "reset"
    );
    assert_eq!(
        acknowledge_reset(&fixture, "ada").await.status(),
        StatusCode::CONFLICT
    );
    assert_eq!(upstream.spends.load(Ordering::Relaxed), 1);
    for (_, _, headers, body) in upstream
        .seen
        .lock()
        .await
        .iter()
        .filter(|r| r.0 == Method::POST)
    {
        assert_eq!(body["redeem_request_id"], REDEEM_ID);
        assert_eq!(headers[AUTHORIZATION], "Bearer test-codex-secret");
        assert!(!headers.contains_key("chatgpt-account-id"));
    }
    fixture.app.shutdown_connections().await;
}

#[tokio::test]
async fn reset_credit_failures_and_malformed_outcomes_never_clear_quota_or_retry() {
    let upstream = CreditUpstream::new().await;
    for (status, body, expected_status) in [
        (StatusCode::OK, json!({ "code": "nothing_to_reset" }), 200),
        (StatusCode::OK, json!({ "code": "no_credit" }), 200),
        (StatusCode::OK, json!({ "code": "already_redeemed" }), 200),
        (StatusCode::OK, json!({ "code": "unknown" }), 502),
        (StatusCode::UNAUTHORIZED, json!({}), 401),
        (StatusCode::TOO_MANY_REQUESTS, json!({}), 429),
        (StatusCode::INTERNAL_SERVER_ERROR, json!({}), 500),
    ] {
        let fixture = Fixture::with_credit_upstream(Some(&upstream.url)).await;
        *upstream.response.lock().await = (status, body);
        block_codex(&fixture).await;
        let before = upstream.seen.lock().await.len();
        let response = json_response(post_credit(&fixture, credit_call_body("ada")).await).await;
        assert_eq!(response["status_code"], expected_status);
        assert_eq!(upstream.seen.lock().await.iter().skip(before).filter(|r| r.0 == Method::POST).count(), 1);
        assert_eq!(
            fixture.app.router.routing_snapshot().await.account_states["ada"]
                .unavailable_reason
                .as_deref(),
            Some("quota")
        );
    }
    let fixture = Fixture::with_credit_upstream(Some(&upstream.url)).await;
    upstream.list_status.store(429, Ordering::Relaxed);
    let response = json_response(
        fixture
            .call("ada", "GET", reset_credits::CODEX_CREDITS_URL)
            .await,
    )
    .await;
    assert_eq!(response["status_code"], 429);
    assert_eq!(upstream.spends.load(Ordering::Relaxed), 0);
    fixture.app.shutdown_connections().await;
}

#[tokio::test]
async fn management_and_native_reset_calls_share_uncertain_attempts_and_confirmed_results() {
    let upstream = CreditUpstream::new().await;
    let fixture = Fixture::with_credit_upstream(Some(&upstream.url)).await;
    *upstream.response.lock().await = (StatusCode::OK, json!({ "code": "unknown" }));
    let response = json_response(post_credit(&fixture, credit_call_body("ada")).await).await;
    assert_eq!(response["status_code"], 502);
    assert!(fixture.app.use_reset_credit("ada", "credit-ada", "different-request").await
        .unwrap_err().to_string().contains("previous reset outcome is unknown"));
    assert_eq!(upstream.seen.lock().await.iter().filter(|r| r.0 == Method::POST).count(), 1);

    *upstream.response.lock().await = (StatusCode::OK, json!({ "code": "reset" }));
    let result = fixture.app.use_reset_credit("ada", "credit-ada", REDEEM_ID).await.unwrap();
    assert_eq!(result.code, crate::reset_credits::ResetOutcome::Reset);
    block_codex(&fixture).await;
    let response = json_response(post_credit(&fixture, credit_call_body("ada")).await).await;
    let body: Value = serde_json::from_str(response["body"].as_str().unwrap()).unwrap();
    assert_eq!(body["code"], "reset");
    assert_eq!(upstream.seen.lock().await.iter().filter(|r| r.0 == Method::POST).count(), 2);
    assert_eq!(upstream.spends.load(Ordering::Relaxed), 1);
    assert_eq!(fixture.app.router.routing_snapshot().await.account_states["ada"]
        .unavailable_reason.as_deref(), Some("quota"));
    fixture.app.shutdown_connections().await;
}

#[tokio::test]
async fn reset_credit_validation_rejects_wrong_providers_and_bad_inputs_before_dispatch() {
    let upstream = CreditUpstream::new().await;
    let fixture = Fixture::with_credit_upstream(Some(&upstream.url)).await;
    for account in ["unknown", "inbound", "grace"] {
        assert!(
            post_credit(&fixture, credit_call_body(account))
                .await
                .status()
                .is_client_error()
        );
    }
    for data in [
        None, Some("{}".into()), Some("not-json".into()),
        Some(json!({ "credit_id": "", "redeem_request_id": REDEEM_ID }).to_string()),
        Some(json!({ "credit_id": "credit-ada", "redeem_request_id": "invalid" }).to_string()),
        Some(json!({ "credit_id": "credit-ada", "redeem_request_id": REDEEM_ID, "account_id": "other" }).to_string()),
    ] {
        let mut body = credit_call_body("ada");
        body["data"] = json!(data);
        assert_eq!(post_credit(&fixture, body).await.status(), StatusCode::BAD_REQUEST);
    }
    let mut body = credit_call_body("ada");
    body["url"] = json!(format!(
        "{}/consume?unexpected=1",
        reset_credits::CODEX_CREDITS_URL
    ));
    assert_eq!(
        post_credit(&fixture, body.clone()).await.status(),
        StatusCode::BAD_REQUEST
    );
    body["url"] = json!(format!("{}/consume", reset_credits::CODEX_CREDITS_URL));
    body["method"] = json!("GET");
    assert_eq!(
        post_credit(&fixture, body).await.status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        acknowledge_reset(&fixture, "grace").await.status(),
        StatusCode::NOT_FOUND
    );
    assert!(upstream.seen.lock().await.is_empty());
    fixture.app.shutdown_connections().await;
}

#[tokio::test]
async fn reset_credit_response_cannot_clear_a_newer_rejection_or_replacement_identity() {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    for change_identity in [false, true] {
        let upstream = CreditUpstream::new().await;
        upstream.hold.store(true, Ordering::Relaxed);
        let fixture = Fixture::with_credit_upstream(Some(&upstream.url)).await;
        let home = fixture.app.config.accounts["ada"].home().unwrap();
        let write_identity = |name: &str| {
            let claims = json!({
                "exp": 4102444800_i64,
                "https://api.openai.com/auth": { "chatgpt_account_id": name, "chatgpt_user_id": "ada-user" },
            });
            fs::write(home.join("auth.json"), json!({
                "tokens": { "access_token": format!("e30.{}.sig", URL_SAFE_NO_PAD.encode(claims.to_string())), "account_id": name },
            }).to_string()).unwrap();
        };
        write_identity("original-account");
        block_codex(&fixture).await;
        let pending = post_credit(&fixture, credit_call_body("ada"));
        let change = async {
            upstream.started.notified().await;
            if change_identity {
                write_identity("replacement-account");
            }
            block_codex(&fixture).await;
            upstream.release.notify_one();
        };
        let (response, ()) = tokio::join!(pending, change);
        assert_eq!(json_response(response).await["status_code"], 200);
        assert_eq!(
            fixture.app.router.routing_snapshot().await.account_states["ada"]
                .unavailable_reason
                .as_deref(),
            Some("quota")
        );
        assert_eq!(upstream.spends.load(Ordering::Relaxed), 1);
        fixture.app.shutdown_connections().await;
    }
}

#[tokio::test]
async fn claude_reset_data_is_cached_account_scoped_and_returned_without_redemption() {
    let fixture = Fixture::new().await;
    let home = fixture.app.config.accounts["grace"].home().unwrap();
    let credentials = crate::claude::auth::read(home).unwrap();
    let block = json!({
        "eligible": true, "next_grant_id": "grant_grace",
        "grants": [{ "id": "grant_grace", "resets_left": 2, "ends_at": "2100-01-01T00:00:00Z", "paused": false, "usable_now": true }],
    });
    fixture
        .observe("grace", &[("5h", Some(15), Some(18000), Some(4102444800))])
        .await;
    fixture
        .observe("anna", &[("5h", Some(25), Some(18000), Some(4102444800))])
        .await;
    fixture
        .app
        .observe_claude_reset_credits(
            "grace",
            credentials.owner(),
            json!({ "cedar_ember": block }).to_string().as_bytes(),
        )
        .await;
    for url in [
        CLAUDE_URL.to_owned(),
        format!("{CLAUDE_URL}?cedar_ember=1&skip_spend=1"),
    ] {
        let body = usage_body(fixture.call("grace", "GET", &url).await).await;
        assert_eq!(body["cedar_ember"], block);
        assert_eq!(body["five_hour"]["utilization"], 15);
    }
    let other = usage_body(fixture.call("anna", "GET", CLAUDE_URL).await).await;
    assert!(other.get("cedar_ember").is_none());
    fixture
        .app
        .observe_claude_reset_credits("grace", credentials.owner(), b"{}")
        .await;
    assert!(
        usage_body(fixture.call("grace", "GET", CLAUDE_URL).await)
            .await
            .get("cedar_ember")
            .is_none()
    );
    fixture
        .app
        .observe_claude_reset_credits(
            "grace",
            credentials.owner(),
            json!({ "cedar_ember": block }).to_string().as_bytes(),
        )
        .await;
    let mut replaced = credentials;
    replaced.account_uuid = "33333333-3333-4333-8333-333333333333".into();
    fs::write(
        home.join("claude-auth.json"),
        serde_json::to_vec(&replaced).unwrap(),
    )
    .unwrap();
    assert!(fixture.app.claude_reset_credits("grace").await.is_none());
    assert_eq!(
        fixture
            .app
            .stats
            .usage_fetch_accounts_checked
            .load(Ordering::Relaxed),
        0
    );
    fixture.app.shutdown_connections().await;
}
