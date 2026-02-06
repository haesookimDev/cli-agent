use std::sync::Arc;
use std::time::Duration;

use axum::http::HeaderMap;
use chrono::Utc;
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use reqwest::StatusCode;
use sha2::Sha256;
use tracing::{info, warn};
use uuid::Uuid;

use crate::memory::MemoryManager;

pub type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone)]
pub struct AuthManager {
    api_key: String,
    api_secret: String,
    max_drift_secs: i64,
    nonce_cache: Arc<DashMap<String, i64>>,
}

impl AuthManager {
    pub fn new(
        api_key: impl Into<String>,
        api_secret: impl Into<String>,
        max_drift_secs: i64,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            api_secret: api_secret.into(),
            max_drift_secs,
            nonce_cache: Arc::new(DashMap::new()),
        }
    }

    pub fn api_key(&self) -> &str {
        self.api_key.as_str()
    }

    pub fn verify_headers(&self, headers: &HeaderMap, body: &[u8]) -> anyhow::Result<()> {
        let key = header_value(headers, "X-API-Key")?;
        if key != self.api_key {
            return Err(anyhow::anyhow!("invalid api key"));
        }

        let signature = header_value(headers, "X-Signature")?;
        let timestamp_raw = header_value(headers, "X-Timestamp")?;
        let nonce = header_value(headers, "X-Nonce")?;

        let timestamp = timestamp_raw
            .parse::<i64>()
            .map_err(|_| anyhow::anyhow!("invalid timestamp header"))?;

        let now = Utc::now().timestamp();
        if (now - timestamp).abs() > self.max_drift_secs {
            return Err(anyhow::anyhow!("timestamp drift exceeded"));
        }

        self.cleanup_old_nonces(now);
        if self.nonce_cache.contains_key(&nonce) {
            return Err(anyhow::anyhow!("nonce reuse detected"));
        }

        let expected = sign_payload(
            self.api_secret.as_str(),
            timestamp_raw.as_str(),
            nonce.as_str(),
            body,
        )?;
        if !constant_time_eq(expected.as_bytes(), signature.as_bytes()) {
            return Err(anyhow::anyhow!("signature mismatch"));
        }

        self.nonce_cache.insert(nonce, now);
        Ok(())
    }

    pub fn sign(&self, timestamp: &str, nonce: &str, body: &[u8]) -> anyhow::Result<String> {
        sign_payload(self.api_secret.as_str(), timestamp, nonce, body)
    }

    fn cleanup_old_nonces(&self, now: i64) {
        let min = now - (self.max_drift_secs * 2);
        self.nonce_cache.retain(|_, ts| *ts >= min);
    }
}

#[derive(Debug, Clone)]
pub struct WebhookDispatcher {
    memory: Arc<MemoryManager>,
    auth: Arc<AuthManager>,
    client: reqwest::Client,
    timeout: Duration,
}

impl WebhookDispatcher {
    pub fn new(memory: Arc<MemoryManager>, auth: Arc<AuthManager>, timeout: Duration) -> Self {
        Self {
            memory,
            auth,
            client: reqwest::Client::new(),
            timeout,
        }
    }

    pub async fn dispatch(&self, event: &str, payload: serde_json::Value) -> anyhow::Result<()> {
        let endpoints = self.memory.list_webhooks().await?;
        if endpoints.is_empty() {
            return Ok(());
        }

        let matched = endpoints
            .into_iter()
            .filter(|ep| ep.enabled && ep.events.iter().any(|e| e == event))
            .collect::<Vec<_>>();

        for endpoint in matched {
            let event_id = Uuid::new_v4().to_string();
            let body = serde_json::json!({
                "event": event,
                "event_id": event_id,
                "timestamp": Utc::now().to_rfc3339(),
                "data": payload,
            });
            let body_bytes = serde_json::to_vec(&body)?;

            let mut delivered = false;
            for attempt in 1..=3 {
                let timestamp = Utc::now().timestamp().to_string();
                let nonce = Uuid::new_v4().to_string();
                let signature =
                    sign_payload(endpoint.secret.as_str(), &timestamp, &nonce, &body_bytes)?;

                let send = self
                    .client
                    .post(endpoint.url.as_str())
                    .header("Content-Type", "application/json")
                    .header("X-API-Key", self.auth.api_key())
                    .header("X-Timestamp", timestamp)
                    .header("X-Nonce", nonce)
                    .header("X-Signature", signature)
                    .header("X-Idempotency-Key", event_id.as_str())
                    .timeout(self.timeout)
                    .body(body_bytes.clone())
                    .send()
                    .await;

                match send {
                    Ok(resp) if resp.status().is_success() => {
                        delivered = true;
                        info!("webhook delivered to {}", endpoint.url);
                        break;
                    }
                    Ok(resp) if resp.status() == StatusCode::CONFLICT => {
                        delivered = true;
                        info!("webhook idempotent conflict treated as delivered");
                        break;
                    }
                    Ok(resp) => {
                        warn!(
                            "webhook delivery failed [{}] attempt {} status {}",
                            endpoint.url,
                            attempt,
                            resp.status()
                        );
                    }
                    Err(err) => {
                        warn!(
                            "webhook delivery error [{}] attempt {}: {}",
                            endpoint.url, attempt, err
                        );
                    }
                }

                tokio::time::sleep(Duration::from_millis(150 * attempt as u64)).await;
            }

            if !delivered {
                warn!("webhook delivery permanently failed for {}", endpoint.url);
            }
        }

        Ok(())
    }
}

fn header_value(headers: &HeaderMap, key: &str) -> anyhow::Result<String> {
    let value = headers
        .get(key)
        .ok_or_else(|| anyhow::anyhow!("missing header: {key}"))?
        .to_str()
        .map_err(|_| anyhow::anyhow!("invalid header encoding: {key}"))?
        .to_string();
    Ok(value)
}

pub fn sign_payload(
    secret: &str,
    timestamp: &str,
    nonce: &str,
    body: &[u8],
) -> anyhow::Result<String> {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())?;
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(nonce.as_bytes());
    mac.update(b".");
    mac.update(body);
    let result = mac.finalize().into_bytes();
    Ok(to_hex(&result))
}

fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(format!("{:02x}", byte).as_str());
    }
    out
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue};

    use super::*;

    fn signed_headers(auth: &AuthManager, body: &[u8], nonce: &str, ts: i64) -> HeaderMap {
        let mut headers = HeaderMap::new();
        let timestamp = ts.to_string();
        let signature = auth.sign(&timestamp, nonce, body).unwrap();

        headers.insert("X-API-Key", HeaderValue::from_str(auth.api_key()).unwrap());
        headers.insert("X-Timestamp", HeaderValue::from_str(&timestamp).unwrap());
        headers.insert("X-Nonce", HeaderValue::from_str(nonce).unwrap());
        headers.insert("X-Signature", HeaderValue::from_str(&signature).unwrap());
        headers
    }

    #[test]
    fn rejects_nonce_replay_and_bad_signature() {
        let auth = AuthManager::new("k", "s", 300);
        let body = b"{\"x\":1}";
        let ts = Utc::now().timestamp();

        let headers = signed_headers(&auth, body, "nonce1", ts);
        auth.verify_headers(&headers, body).unwrap();

        let replay = signed_headers(&auth, body, "nonce1", ts);
        assert!(auth.verify_headers(&replay, body).is_err());

        let mut bad = signed_headers(&auth, body, "nonce2", ts);
        bad.insert("X-Signature", HeaderValue::from_static("deadbeef"));
        assert!(auth.verify_headers(&bad, body).is_err());
    }

    #[test]
    fn rejects_timestamp_drift() {
        let auth = AuthManager::new("k", "s", 300);
        let body = b"{}";
        let ts = Utc::now().timestamp() - 1200;

        let headers = signed_headers(&auth, body, "nonce3", ts);
        assert!(auth.verify_headers(&headers, body).is_err());
    }
}
