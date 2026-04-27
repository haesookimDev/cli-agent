//! Per-API-key in-process token-bucket rate limiter.
//!
//! Each unique API key gets its own bucket of `capacity` tokens that refills
//! at `refill_per_sec`. A request consumes one token; if the bucket is empty
//! the request is rejected with HTTP 429.
//!
//! This sits in front of the auth check so an attacker that gets the key
//! still can't exhaust DB or downstream LLM resources at full speed.

use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use dashmap::DashMap;

#[derive(Clone, Copy, Debug)]
pub struct RateLimitConfig {
    pub capacity: f64,
    pub refill_per_sec: f64,
}

impl RateLimitConfig {
    pub const fn new(capacity: f64, refill_per_sec: f64) -> Self {
        Self {
            capacity,
            refill_per_sec,
        }
    }

    /// Read overrides from env. Falls back to a permissive default
    /// (120 req per key per minute, burst 120) suitable for local development.
    pub fn from_env() -> Self {
        let capacity = std::env::var("CLI_AGENT_RATE_LIMIT_CAPACITY")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(120.0);
        let refill_per_sec = std::env::var("CLI_AGENT_RATE_LIMIT_PER_SEC")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(2.0);
        Self::new(capacity, refill_per_sec)
    }
}

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

#[derive(Debug, Clone)]
pub struct RateLimiter {
    config: RateLimitConfig,
    buckets: Arc<DashMap<String, Bucket>>,
}

impl RateLimiter {
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            config,
            buckets: Arc::new(DashMap::new()),
        }
    }

    pub fn config(&self) -> RateLimitConfig {
        self.config
    }

    /// Try to consume one token for `key`. Returns `true` on success, `false`
    /// if the bucket is empty.
    pub fn try_acquire(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut entry = self.buckets.entry(key.to_string()).or_insert_with(|| Bucket {
            tokens: self.config.capacity,
            last_refill: now,
        });

        let elapsed = now.duration_since(entry.last_refill).as_secs_f64();
        if elapsed > 0.0 {
            entry.tokens =
                (entry.tokens + elapsed * self.config.refill_per_sec).min(self.config.capacity);
            entry.last_refill = now;
        }

        if entry.tokens >= 1.0 {
            entry.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

fn rate_limit_key(headers: &HeaderMap) -> String {
    // Prefer the API key header, fall back to forwarded-for / real-ip, then
    // a constant — anonymous requests share one bucket so a flood still slows.
    if let Some(key) = headers.get("X-API-Key").and_then(|v| v.to_str().ok()) {
        return format!("key:{key}");
    }
    if let Some(ip) = headers
        .get("X-Forwarded-For")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(str::trim)
    {
        return format!("ip:{ip}");
    }
    if let Some(ip) = headers.get("X-Real-IP").and_then(|v| v.to_str().ok()) {
        return format!("ip:{ip}");
    }
    "anonymous".to_string()
}

pub async fn middleware(
    State(limiter): State<RateLimiter>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let key = rate_limit_key(request.headers());
    if !limiter.try_acquire(&key) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({"error": "rate limit exceeded"})),
        )
            .into_response();
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_bucket_rejects_then_refills() {
        let limiter = RateLimiter::new(RateLimitConfig::new(2.0, 1000.0));
        assert!(limiter.try_acquire("k"));
        assert!(limiter.try_acquire("k"));
        // Capacity exhausted — instant retry should fail until refill.
        assert!(!limiter.try_acquire("k"));

        // Refill rate is 1000 tokens/sec so a 5ms sleep restores plenty.
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(limiter.try_acquire("k"));
    }

    #[test]
    fn buckets_are_per_key() {
        let limiter = RateLimiter::new(RateLimitConfig::new(1.0, 0.0));
        assert!(limiter.try_acquire("a"));
        assert!(limiter.try_acquire("b"));
        assert!(!limiter.try_acquire("a"));
        assert!(!limiter.try_acquire("b"));
    }

    #[test]
    fn rate_limit_key_prefers_api_key() {
        let mut h = HeaderMap::new();
        h.insert("X-API-Key", "abc".parse().unwrap());
        h.insert("X-Forwarded-For", "1.2.3.4".parse().unwrap());
        assert_eq!(rate_limit_key(&h), "key:abc");
    }

    #[test]
    fn rate_limit_key_falls_back_to_forwarded_for() {
        let mut h = HeaderMap::new();
        h.insert("X-Forwarded-For", "1.2.3.4, 5.6.7.8".parse().unwrap());
        assert_eq!(rate_limit_key(&h), "ip:1.2.3.4");
    }

    #[test]
    fn rate_limit_key_falls_back_to_anonymous() {
        let h = HeaderMap::new();
        assert_eq!(rate_limit_key(&h), "anonymous");
    }
}
