use super::*;
use crate::config::AccountConfig;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub(super) const CODEX_CREDITS_URL: &str =
    "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits";

pub(super) struct ResetCredits {
    pub codex_url: Uri,
    pub claude: AsyncMutex<HashMap<String, ClaudeCredits>>,
}

pub(super) struct ClaudeCredits {
    pub owner: auth::QuotaOwner,
    pub value: Value,
}

impl Default for ResetCredits {
    fn default() -> Self {
        Self {
            codex_url: CODEX_CREDITS_URL.parse().expect("static credit URL"),
            claude: AsyncMutex::new(HashMap::new()),
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ConsumeCredit {
    credit_id: String,
    redeem_request_id: String,
}

impl ConsumeCredit {
    pub fn parse(data: Option<&str>) -> Option<Self> {
        let value: Self = serde_json::from_str(data?).ok()?;
        if value.credit_id.is_empty()
            || !value
                .credit_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || !crate::claude::wire::uuid(&value.redeem_request_id)
        {
            return None;
        }
        Some(value)
    }
}

impl App {
    pub(super) async fn codex_reset_credits(
        &self,
        account_id: &str,
        consume: Option<ConsumeCredit>,
    ) -> Response<ProxyBody> {
        let operation = tokio::time::timeout(
            USAGE_FETCH_ACCOUNT_TIMEOUT,
            self.codex_reset_credits_inner(account_id, consume),
        )
        .await;
        match operation {
            Ok(Ok((status, body))) => super::usage_management::api_response(status, body),
            _ => super::usage_management::api_response(
                StatusCode::BAD_GATEWAY,
                json!({ "error": "reset-credit request could not be completed" }),
            ),
        }
    }

    async fn codex_reset_credits_inner(
        &self,
        account_id: &str,
        consume: Option<ConsumeCredit>,
    ) -> Result<(StatusCode, Value)> {
        let account = &self.config.accounts[account_id];
        anyhow::ensure!(matches!(account, AccountConfig::CodexHome { .. }));
        let mut credentials = self.auth.resolve(account, &hyper::HeaderMap::new()).await?;
        let owner = credentials.quota_owner();
        let marker = self
            .router
            .reset_credit_marker(account_id, &owner)
            .await
            .context("account identity changed")?;
        let mut response = self
            .send_reset_credit_request(&credentials, consume.as_ref())
            .await?;
        // A rejected read may refresh once. A redemption is never replayed automatically.
        if consume.is_none() && response.0 == StatusCode::UNAUTHORIZED {
            credentials = self
                .auth
                .force_refresh(account, &credentials)
                .await?
                .context("credentials cannot refresh")?;
            response = self.send_reset_credit_request(&credentials, None).await?;
        }
        if !response.0.is_success() {
            return Ok((
                response.0,
                json!({ "error": "provider rejected reset-credit request" }),
            ));
        }
        if consume.is_some() {
            let code = response.1["code"]
                .as_str()
                .context("missing reset outcome")?;
            anyhow::ensure!(matches!(
                code,
                "reset" | "nothing_to_reset" | "no_credit" | "already_redeemed"
            ));
            // A replayed receipt does not prove that a newer quota block was reset.
            if code == "reset" {
                self.router
                    .confirm_credit_reset(account_id, &owner, marker)
                    .await;
            }
        } else {
            anyhow::ensure!(response.1["credits"].is_array(), "missing reset credits");
        }
        Ok(response)
    }

    async fn send_reset_credit_request(
        &self,
        credentials: &Credentials,
        consume: Option<&ConsumeCredit>,
    ) -> Result<(StatusCode, Value)> {
        let url = if consume.is_some() {
            format!("{}/consume", self.reset_credits.codex_url).parse()?
        } else {
            self.reset_credits.codex_url.clone()
        };
        let mut request = Request::builder()
            .method(if consume.is_some() {
                Method::POST
            } else {
                Method::GET
            })
            .uri(url)
            .header(AUTHORIZATION, &credentials.authorization)
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .header("openai-beta", "codex-1")
            .header("originator", "Codex Desktop");
        if let Some(id) = &credentials.account_id {
            request = request.header("chatgpt-account-id", id);
        }
        let body = match consume {
            Some(value) => bytes_body(bytes::Bytes::from(serde_json::to_vec(value)?)),
            None => empty_body(),
        };
        tokio::time::timeout(USAGE_FETCH_TIMEOUT, async {
            let response = self.client.request(request.body(body)?).await?;
            let status = response.status();
            let bytes =
                http_body_util::Limited::new(response.into_body(), MAX_USAGE_RESPONSE_BYTES)
                    .collect()
                    .await
                    .map_err(|_| anyhow::anyhow!("invalid reset-credit response body"))?
                    .to_bytes();
            let value = if status.is_success() {
                serde_json::from_slice(&bytes)?
            } else {
                Value::Null
            };
            Ok((status, value))
        })
        .await
        .context("reset-credit request timed out")?
    }

    pub(super) async fn acknowledge_credit_reset(&self, account: &str) -> Response<ProxyBody> {
        if !matches!(
            self.config.accounts.get(account),
            Some(AccountConfig::CodexHome { .. })
        ) {
            return error_response(StatusCode::NOT_FOUND, "not_found", "unknown Codex account");
        }
        let _ = tokio::time::timeout(
            USAGE_FETCH_TIMEOUT,
            self.fetch_managed_usage_account(
                account,
                &self.config.accounts[account],
                chrono::Utc::now().timestamp().max(0) as u64,
            ),
        )
        .await;
        // Redemption clears its own cooldown. This compatibility call cannot clear
        // a block on its own, including a newer rejection after the reset completed.
        let snapshot = self.router.routing_snapshot().await;
        if snapshot
            .account_states
            .get(account)
            .is_some_and(|state| state.unavailable_reason.as_deref() == Some("quota"))
        {
            return error_response(
                StatusCode::CONFLICT,
                "quota_still_active",
                "account still has an active quota cooldown",
            );
        }
        super::usage_management::management_json(json!({ "status": "ok" }))
    }

    pub(super) async fn observe_claude_reset_credits(
        &self,
        account: &str,
        owner: auth::QuotaOwner,
        bytes: &[u8],
    ) {
        let value = serde_json::from_slice::<Value>(bytes)
            .ok()
            .and_then(|mut body| body.as_object_mut()?.remove("cedar_ember"));
        let mut observations = self.reset_credits.claude.lock().await;
        if let Some(value) = value {
            observations.insert(account.into(), ClaudeCredits { owner, value });
        } else {
            observations.remove(account);
        }
    }

    pub(super) async fn claude_reset_credits(&self, account: &str) -> Option<Value> {
        let AccountConfig::ClaudeHome { path } = &self.config.accounts[account] else {
            return None;
        };
        let current = crate::claude::auth::read(path).ok()?;
        self.reset_credits
            .claude
            .lock()
            .await
            .get(account)
            .filter(|observation| observation.owner == current.owner())
            .map(|observation| observation.value.clone())
    }
}
