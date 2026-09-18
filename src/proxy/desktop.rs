//! Opt-in compatibility for Desktop's native backend calls. The upstream authority is fixed;
//! Desktop credentials never follow a client-supplied authority or an upstream redirect.
use super::*;
use hyper::header::{ACCEPT_ENCODING, CACHE_CONTROL, CONTENT_ENCODING};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const BACKEND: &str = "https://chatgpt.com";
const USAGE_PATH: &str = "/backend-api/wham/usage";
const IDLE: Duration = Duration::from_secs(120);
const MAX_USAGE_PROJECTION_FRAMES: usize = 4096;

impl App {
    pub async fn run_desktop_listener(self: Arc<Self>, listener: ListenerConfig) -> Result<()> {
        let tcp = TcpListener::bind(listener.address)
            .await
            .context("bind Desktop listener")?;
        self.serve_desktop_tcp(listener, tcp, BACKEND.to_owned())
            .await
    }

    pub(super) async fn serve_desktop_tcp(
        self: Arc<Self>,
        listener: ListenerConfig,
        tcp: TcpListener,
        backend: String,
    ) -> Result<()> {
        let connections = Arc::new(Semaphore::new(
            self.config
                .proxy
                .max_inflight
                .saturating_add(self.config.proxy.max_upgrades),
        ));
        loop {
            let (stream, peer) = tcp.accept().await?;
            if !peer.ip().is_loopback() {
                continue;
            }
            let Ok(connection) = connections.clone().try_acquire_owned() else {
                continue;
            };
            let app = self.clone();
            let listener = listener.clone();
            let backend = backend.clone();
            self.spawn_tracked(async move {
                let _connection = connection;
                let service = service_fn(move |request| {
                    let app = app.clone();
                    let listener = listener.clone();
                    let backend = backend.clone();
                    async move {
                        Ok::<_, Infallible>(
                            app.desktop_request(request, listener, &backend)
                                .await
                                .unwrap_or_else(|_| {
                                    error_response(
                                        StatusCode::BAD_GATEWAY,
                                        "desktop_backend_error",
                                        "Desktop backend request failed",
                                    )
                                }),
                        )
                    }
                });
                let mut builder = Builder::new(TokioExecutor::new());
                builder
                    .http1()
                    .timer(hyper_util::rt::TokioTimer::new())
                    .header_read_timeout(Duration::from_secs(15));
                let _ = builder
                    .serve_connection_with_upgrades(TokioIo::new(stream), service)
                    .await;
            })
            .await;
        }
    }

    async fn desktop_request(
        self: Arc<Self>,
        mut request: Request<Incoming>,
        listener: ListenerConfig,
        backend: &str,
    ) -> Result<Response<ProxyBody>> {
        // Readiness uses the service's random nonce, independently of Desktop's OAuth token.
        if request.method() == Method::GET && self.service_health_path(request.uri()) {
            return Ok(self.health_response());
        }
        let prefix = format!("/{}", self.config.proxy.installation_secret);
        let backend_path = request
            .uri()
            .path_and_query()
            .and_then(|path| path.as_str().strip_prefix(&prefix))
            .filter(|path| path.starts_with("/backend-api/"))
            .map(str::to_owned);
        // Origin-form backend paths only. In particular, never become a general forward proxy.
        if request.uri().scheme().is_some()
            || request.uri().authority().is_some()
            || backend_path.is_none()
            || request.method() == Method::CONNECT
        {
            return Ok(error_response(
                StatusCode::NOT_FOUND,
                "not_found",
                "unknown Desktop backend path",
            ));
        }
        if !self
            .auth
            .credentials_usable(&crate::config::AccountConfig::Inbound, request.headers())
        {
            return Ok(error_response(
                StatusCode::UNAUTHORIZED,
                "authentication_required",
                "Desktop authorization is required",
            ));
        }
        // Route native inference through the same authentication, model, and continuity logic.
        let backend_path = backend_path.unwrap();
        if backend_path.starts_with("/backend-api/codex/") {
            return Ok(self.handle(request, listener).await.unwrap());
        }
        *request.uri_mut() = backend_path.parse()?;
        let Ok(permit) = self.http_slots.clone().try_acquire_owned() else {
            return Ok(error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "desktop_busy",
                "Desktop backend capacity is busy",
            ));
        };
        let usage = request.method() == Method::GET && request.uri().path() == USAGE_PATH;
        let allowed = if usage {
            self.desktop_pool_available(&listener.pool, request.headers())
                .await
        } else {
            false
        };
        let upgrade = is_upgrade(&request);
        let upgrade_permit = if upgrade {
            match self.upgrade_slots.clone().try_acquire_owned() {
                Ok(permit) => Some(permit),
                Err(_) => {
                    return Ok(error_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "desktop_busy",
                        "Desktop upgrade capacity is busy",
                    ));
                }
            }
        } else {
            None
        };
        let downstream = upgrade.then(|| hyper::upgrade::on(&mut request));
        let upgrade_header = request.headers().get(UPGRADE).cloned();
        let (mut parts, body) = request.into_parts();
        parts.uri = format!("{backend}{}", parts.uri.path_and_query().unwrap()).parse()?;
        headers::strip_hop_by_hop(&mut parts.headers);
        if usage {
            parts.headers.insert(ACCEPT_ENCODING, "identity".parse()?);
        }
        if upgrade {
            parts.headers.insert(CONNECTION, "upgrade".parse()?);
            if let Some(value) = upgrade_header {
                parts.headers.insert(UPGRADE, value);
            }
        }
        if parts
            .headers
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .is_some_and(|length| length > self.config.proxy.max_request_bytes as u64)
        {
            return Ok(error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request_too_large",
                "Desktop request exceeds configured limit",
            ));
        }
        let body = DesktopBody::new(
            incoming_body(body),
            Some(self.config.proxy.max_request_bytes),
            None,
            Duration::from_secs(30),
        )
        .boxed();
        let (body, progress) = progress_body(body);
        let outgoing = Request::from_parts(parts, body);
        let mut response = await_upstream_headers(
            async {
                if upgrade {
                    self.upgrade_client
                        .request(outgoing)
                        .await
                        .map_err(anyhow::Error::from)
                } else {
                    self.client
                        .request(outgoing)
                        .await
                        .map_err(anyhow::Error::from)
                }
            },
            progress,
            Duration::from_secs(30),
            Duration::from_secs(30),
        )
        .await?;
        if response.status() == StatusCode::SWITCHING_PROTOCOLS {
            let Some(downstream) = downstream else {
                bail!("unsolicited backend upgrade");
            };
            let upstream = hyper::upgrade::on(&mut response);
            self.spawn_tracked(async move {
                let _permit = permit;
                let _upgrade_permit = upgrade_permit;
                if let Ok(Ok((downstream, upstream))) =
                    tokio::time::timeout(Duration::from_secs(15), async {
                        Ok::<_, hyper::Error>((downstream.await?, upstream.await?))
                    })
                    .await
                {
                    let _ = tunnel(TokioIo::new(downstream), TokioIo::new(upstream)).await;
                }
            })
            .await;
            let (parts, _) = response.into_parts();
            return Ok(Response::from_parts(parts, empty_body()));
        }
        let (mut parts, body) = response.into_parts();
        headers::strip_hop_by_hop(&mut parts.headers);
        let mut body = DesktopBody::new(incoming_body(body), None, Some(permit), IDLE).boxed();
        if usage
            && parts.status == StatusCode::OK
            && parts
                .headers
                .get(CONTENT_ENCODING)
                .is_none_or(|value| value == "identity")
        {
            let (projected_body, projected) = project_usage_body(
                body,
                allowed,
                MAX_USAGE_RESPONSE_BYTES,
                Duration::from_secs(10),
            )
            .await;
            body = projected_body;
            if projected {
                parts.headers.remove(CONTENT_LENGTH);
                parts.headers.remove(CONTENT_ENCODING);
                parts.headers.remove("etag");
                parts.headers.insert(CACHE_CONTROL, "no-store".parse()?);
            }
        }
        Ok(Response::from_parts(parts, body))
    }

    pub(super) async fn desktop_pool_available(
        &self,
        pool_name: &str,
        inbound: &hyper::HeaderMap,
    ) -> bool {
        let Some(pool) = self.config.pools.get(pool_name) else {
            return false;
        };
        let snapshot = self.router.routing_snapshot().await;
        let now = chrono::Utc::now().timestamp();
        for name in &pool.members {
            if snapshot.account_states.get(name).is_some_and(|account| {
                let reported_exhausted: Vec<_> = account
                    .usage_windows
                    .values()
                    .filter(|window| window.used_percent.is_some_and(|used| used >= 100))
                    .collect();
                let known_reset = !reported_exhausted.is_empty()
                    && reported_exhausted
                        .iter()
                        .all(|window| window.reset_at_unix.is_some_and(|reset| reset <= now));
                account.available
                    && (account.usage_percent.is_none_or(|usage| usage < 100) || known_reset)
            }) && self
                .config
                .accounts
                .get(name)
                .is_some_and(|account| self.auth.credentials_usable(account, inbound))
            {
                return true;
            }
        }
        false
    }
}

/// Projection is optional: retain every consumed frame until a complete, bounded JSON object
/// can be edited. A limit, timeout, trailer, transport error, or parse failure replays the exact
/// prefix followed by the remaining stream under the original response headers.
pub(super) async fn project_usage_body(
    mut body: ProxyBody,
    allowed: bool,
    limit: usize,
    timeout: Duration,
) -> (ProxyBody, bool) {
    let mut frames = VecDeque::new();
    let mut bytes = Vec::new();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match tokio::time::timeout_at(deadline, body.frame()).await {
            Ok(None) => {
                if let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&bytes)
                    && value
                        .get("rate_limit")
                        .is_some_and(serde_json::Value::is_object)
                {
                    project_usage(&mut value, allowed);
                    return (json_body(value), true);
                }
                break;
            }
            Ok(Some(frame)) => {
                let data = frame.as_ref().ok().and_then(|frame| frame.data_ref());
                let can_collect = data
                    .is_some_and(|data| data.len() <= limit.saturating_sub(bytes.len()))
                    && frames.len() < MAX_USAGE_PROJECTION_FRAMES;
                if can_collect {
                    bytes.extend_from_slice(data.unwrap());
                }
                frames.push_back(frame);
                if !can_collect {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    (
        ReplayedIncoming {
            frames,
            inner: body,
        }
        .boxed(),
        false,
    )
}

pub(super) fn project_usage(value: &mut serde_json::Value, allowed: bool) {
    fn project_limit(value: &mut serde_json::Value, allowed: bool) {
        let Some(limit) = value.as_object_mut() else {
            return;
        };
        limit.insert("allowed".into(), allowed.into());
        limit.insert("limit_reached".into(), (!allowed).into());
        for window in ["primary_window", "secondary_window"] {
            if let Some(window) = limit
                .get_mut(window)
                .and_then(serde_json::Value::as_object_mut)
            {
                window.insert("used_percent".into(), if allowed { 0 } else { 100 }.into());
            }
        }
    }
    if let Some(limit) = value.get_mut("rate_limit") {
        project_limit(limit, allowed);
    }
    if allowed {
        if let Some(value) = value.get_mut("rate_limit_reached_type") {
            *value = serde_json::Value::Null;
        }
        if let Some(value) = value.get_mut("rate_limit_upsell") {
            *value = serde_json::Value::Null;
        }
    }
    // Do not invent credit balances, plans, reset timestamps, or unlimited capacity.
}

struct DesktopBody {
    inner: ProxyBody,
    remaining: Option<usize>,
    _permit: Option<tokio::sync::OwnedSemaphorePermit>,
    idle: Pin<Box<tokio::time::Sleep>>,
    duration: Duration,
}

impl DesktopBody {
    fn new(
        inner: ProxyBody,
        remaining: Option<usize>,
        permit: Option<tokio::sync::OwnedSemaphorePermit>,
        duration: Duration,
    ) -> Self {
        Self {
            inner,
            remaining,
            _permit: permit,
            idle: Box::pin(tokio::time::sleep(duration)),
            duration,
        }
    }
}

impl Body for DesktopBody {
    type Data = bytes::Bytes;
    type Error = std::io::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<Option<std::io::Result<Frame<Self::Data>>>> {
        if self.idle.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Some(Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Desktop body idle timeout",
            ))));
        }
        let frame = Pin::new(&mut self.inner).poll_frame(cx);
        if let Poll::Ready(Some(Ok(frame))) = &frame {
            if let (Some(remaining), Some(data)) = (&mut self.remaining, frame.data_ref()) {
                if data.len() > *remaining {
                    return Poll::Ready(Some(Err(std::io::Error::other(
                        "Desktop request exceeds configured limit",
                    ))));
                }
                *remaining -= data.len();
            }
            let deadline = tokio::time::Instant::now() + self.duration;
            self.idle.as_mut().reset(deadline);
        }
        frame
    }
    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

async fn tunnel(
    mut downstream: TokioIo<hyper::upgrade::Upgraded>,
    mut upstream: TokioIo<hyper::upgrade::Upgraded>,
) -> Result<()> {
    let mut from_client = [0; 16 * 1024];
    let mut from_backend = [0; 16 * 1024];
    loop {
        tokio::select! {
            count = downstream.read(&mut from_client) => {
                let count = count?;
                if count == 0 { return Ok(()); }
                tokio::time::timeout(IDLE, upstream.write_all(&from_client[..count])).await??;
            }
            count = upstream.read(&mut from_backend) => {
                let count = count?;
                if count == 0 { return Ok(()); }
                tokio::time::timeout(IDLE, downstream.write_all(&from_backend[..count])).await??;
            }
            _ = tokio::time::sleep(IDLE) => bail!("Desktop upgrade idle timeout"),
        }
    }
}
