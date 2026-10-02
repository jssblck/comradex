//! Backend-owned reset credits. Dates stay in RFC3339 form without losing precision.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetCredit {
    pub id: String,
    pub reset_type: String,
    pub status: String,
    pub granted_at: String,
    pub expires_at: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
}

impl ResetCredit {
    pub fn is_available_at(&self, now: DateTime<Utc>) -> bool {
        self.status == "available"
            && self.expires_at.as_ref().is_none_or(|expiry| {
                DateTime::parse_from_rfc3339(expiry).is_ok_and(|expiry| expiry > now)
            })
    }

    pub fn can_redeem_at(&self, now: DateTime<Utc>) -> bool {
        self.reset_type == "codex_rate_limits" && self.is_available_at(now)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetCreditsSnapshot {
    pub available_count: u64,
    pub observed_at_unix: i64,
    /// None means details could not be fetched, not an empty credit balance.
    pub credits: Option<Vec<ResetCredit>>,
    pub error: Option<String>,
}

impl ResetCreditsSnapshot {
    pub fn available_count_at(&self, now: DateTime<Utc>) -> u64 {
        self.credits
            .as_ref()
            .map_or(self.available_count, |credits| {
                credits
                    .iter()
                    .filter(|credit| credit.is_available_at(now))
                    .count() as u64
            })
    }
}

#[derive(Debug, Deserialize)]
pub struct ResetCreditsResponse {
    pub available_count: u64,
    pub credits: Vec<ResetCredit>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResetOutcome {
    Reset,
    NothingToReset,
    NoCredit,
    AlreadyRedeemed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResetResult {
    pub code: ResetOutcome,
    /// Redemption and the subsequent read are distinct; a failed read must not invite a new reset.
    #[serde(default)]
    pub refresh_error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_expiry_and_unknown_types_are_not_redeemable() {
        let mut credit: ResetCredit = serde_json::from_value(serde_json::json!({
            "id": "one", "reset_type": "codex_rate_limits", "status": "available",
            "granted_at": "2026-10-01T00:00:00Z", "expires_at": "2026-10-02T12:34:56.789+02:00"
        }))
        .unwrap();
        let expiry = DateTime::parse_from_rfc3339(credit.expires_at.as_ref().unwrap())
            .unwrap()
            .with_timezone(&Utc);
        assert!(credit.can_redeem_at(expiry - chrono::Duration::milliseconds(1)));
        assert!(!credit.can_redeem_at(expiry));
        credit.expires_at = Some("invalid".into());
        assert!(!credit.can_redeem_at(expiry));
        credit.expires_at = None;
        assert!(credit.can_redeem_at(expiry));
        credit.reset_type = "future_type".into();
        assert!(!credit.can_redeem_at(expiry));
    }
}
