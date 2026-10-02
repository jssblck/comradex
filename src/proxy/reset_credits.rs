use super::*;
use crate::reset_credits::{ResetCreditsResponse, ResetCreditsSnapshot, ResetOutcome, ResetResult};

/// Held under the account's usage lock. An uncertain POST may only be retried
/// with its original identity and target; successful replies are replayed locally.
pub(super) struct ResetAttempt {
    owner: auth::QuotaOwner,
    bearer_fingerprint: blake3::Hash,
    credit_id: String,
    request_id: String,
    quota_generation: u64,
    result: Option<ResetResult>,
}

impl ResetAttempt {
    fn matches_owner(&self, credentials: &Credentials) -> bool {
        self.owner == credentials.quota_owner()
            && (self.owner.is_known()
                || self.bearer_fingerprint == blake3::hash(credentials.authorization.as_bytes()))
    }
}

impl App {
    // Derive both endpoints from the usage origin, including in tests. Tests cannot
    // accidentally fall through to a production reset endpoint.
    fn reset_credits_url(&self, consume: bool) -> Result<Uri> {
        let mut parts = self.usage_url.clone().into_parts();
        let path = self
            .usage_url
            .path()
            .strip_suffix("/usage")
            .context("invalid usage path")?;
        parts.path_and_query = Some(
            format!(
                "{path}/rate-limit-reset-credits{}",
                if consume { "/consume" } else { "" }
            )
            .parse()?,
        );
        Ok(Uri::from_parts(parts)?)
    }

    async fn fetch_reset_credit_details(
        &self,
        credentials: &Credentials,
    ) -> Result<ResetCreditsResponse> {
        let (status, bytes) = tokio::time::timeout(
            USAGE_FETCH_TIMEOUT,
            self.fetch_account_endpoint(
                credentials,
                Method::GET,
                self.reset_credits_url(false)?,
                empty_body(),
            ),
        )
        .await
        .context("reset credit listing timed out")??;
        if !status.is_success() {
            bail!("reset credit listing returned HTTP {status}");
        }
        serde_json::from_slice(&bytes).context("parse reset credit listing")
    }

    pub(super) async fn refresh_reset_credits(
        &self,
        account: &str,
        credentials: &Credentials,
        available: Option<u64>,
    ) {
        let snapshot = match available {
            None => None,
            Some(available_count) => {
                let mut snapshot = ResetCreditsSnapshot {
                    available_count,
                    observed_at_unix: chrono::Utc::now().timestamp(),
                    credits: None,
                    error: None,
                };
                match self.fetch_reset_credit_details(credentials).await {
                    Ok(details) => {
                        snapshot.available_count = details.available_count;
                        snapshot.credits = Some(details.credits);
                    }
                    Err(error) => snapshot.error = Some(format!("{error:#}")),
                }
                Some(snapshot)
            }
        };
        self.router
            .observe_reset_credits_for_owner(account, snapshot, &credentials.quota_owner())
            .await;
    }

    pub async fn read_reset_credits(&self, account_name: &str) -> Result<ResetCreditsSnapshot> {
        let account = self.managed_reset_account(account_name)?;
        let now = chrono::Utc::now().timestamp().max(0) as u64;
        tokio::time::timeout(
            USAGE_FETCH_ACCOUNT_TIMEOUT,
            self.fetch_managed_usage_account(account_name, account, now),
        )
        .await
        .context("reset credit refresh timed out")??;
        self.router
            .routing_snapshot()
            .await
            .account_states
            .get(account_name)
            .and_then(|state| state.reset_credits.clone())
            .context("reset credits are not reported for this account")
    }

    fn managed_reset_account(&self, name: &str) -> Result<&crate::config::AccountConfig> {
        let account = self.config.accounts.get(name).context("unknown account")?;
        if !matches!(account, crate::config::AccountConfig::CodexHome { .. }) {
            bail!("connect this account to a Codex login before using reset credits");
        }
        Ok(account)
    }

    /// Only explicit control requests reach this method. Never called by a scheduler.
    pub async fn use_reset_credit(
        &self,
        account_name: &str,
        credit_id: &str,
        request_id: &str,
    ) -> Result<ResetResult> {
        if credit_id.is_empty()
            || credit_id.len() > 256
            || request_id.is_empty()
            || request_id.len() > 128
        {
            bail!("a credit ID and a stable request ID are required");
        }
        let account = self.managed_reset_account(account_name)?;
        let mut action = self
            .usage_locks
            .get(account_name)
            .context("unknown account")?
            .try_lock()
            .context("usage refresh or reset already in progress; try again shortly")?;
        let credentials = self.auth.resolve(account, &hyper::HeaderMap::new()).await?;
        let is_retry = match action.as_ref() {
            Some(previous) if previous.request_id == request_id => {
                if previous.credit_id != credit_id || !previous.matches_owner(&credentials) {
                    bail!("reset retry must use the original account identity and credit");
                }
                if let Some(result) = &previous.result {
                    return Ok(result.clone());
                }
                true
            }
            Some(previous) if previous.result.is_none() && previous.matches_owner(&credentials) => {
                bail!(
                    "previous reset outcome is unknown; retry its original credit and request ID"
                );
            }
            _ => false,
        };
        if !is_retry {
            let details = self.fetch_reset_credit_details(&credentials).await?;
            let credit = details
                .credits
                .iter()
                .find(|credit| credit.id == credit_id)
                .context("selected reset credit no longer exists; refresh its status")?;
            if !credit.can_redeem_at(chrono::Utc::now()) {
                bail!(
                    "selected reset credit is expired, unavailable, or unsupported; refresh its status"
                );
            }
        }
        let current = self.auth.resolve(account, &hyper::HeaderMap::new()).await?;
        let owner = credentials.quota_owner();
        if current.quota_owner() != owner
            || (!owner.is_known() && current.authorization != credentials.authorization)
        {
            bail!("account identity changed; refresh before using a reset");
        }
        let request_generation = self
            .router
            .reset_generation_for_owner(account_name, &owner)
            .await
            .context("account identity changed; refresh before using a reset")?;
        if !is_retry {
            *action = Some(ResetAttempt {
                owner: owner.clone(),
                bearer_fingerprint: blake3::hash(credentials.authorization.as_bytes()),
                credit_id: credit_id.into(),
                request_id: request_id.into(),
                quota_generation: request_generation,
                result: None,
            });
        }
        let attempt = action.as_mut().expect("reset attempt was reserved");
        self.router
            .observe_reset_credits_for_owner(account_name, None, &credentials.quota_owner())
            .await;
        let body = json_body(
            serde_json::json!({ "credit_id": credit_id, "redeem_request_id": request_id }),
        );
        // Do not retry a POST or switch identities on failure. The caller retains
        // request_id, and every attempt targets this same credit.
        let (status, bytes) = tokio::time::timeout(
            USAGE_FETCH_TIMEOUT,
            self.fetch_account_endpoint(
                &credentials,
                Method::POST,
                self.reset_credits_url(true)?,
                body,
            ),
        )
        .await
        .context(
            "reset outcome unknown; refresh credits before retrying with the same request ID",
        )??;
        if !status.is_success() {
            bail!(
                "reset endpoint returned HTTP {status}; refresh credits before retrying with the same request ID"
            );
        }
        let mut result: ResetResult = serde_json::from_slice(&bytes).context(
            "reset outcome unknown; refresh credits before retrying with the same request ID",
        )?;
        if result.code == ResetOutcome::Reset
            || (is_retry && result.code == ResetOutcome::AlreadyRedeemed)
        {
            // A fresh reset happened during this POST. An idempotent replay only
            // confirms the original attempt and cannot erase intervening failures.
            let generation = if result.code == ResetOutcome::Reset {
                request_generation
            } else {
                attempt.quota_generation
            };
            self.router
                .confirm_reset_for_owner(account_name, &owner, generation)
                .await;
        }
        // Retain the confirmed outcome even if the following refresh is interrupted
        // or the local client loses this response. A replay must not clear later quota.
        attempt.result = Some(result.clone());
        let now = chrono::Utc::now().timestamp().max(0) as u64;
        let refreshed = tokio::time::timeout(
            USAGE_FETCH_ACCOUNT_TIMEOUT,
            self.fetch_managed_usage_account_locked(account_name, account, now),
        )
        .await
        .context("usage refresh timed out")
        .and_then(|result| result);
        if let Err(error) = refreshed {
            result.refresh_error = Some(format!("{error:#}"));
        }
        attempt.result = Some(result.clone());
        Ok(result)
    }
}
