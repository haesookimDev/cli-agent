use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use dashmap::DashMap;
use dashmap::DashSet;
use serde::{Deserialize, Serialize};

use crate::types::TaskProfile;

#[derive(Debug, Clone)]
struct CacheEntry {
    output: String,
    model_id: String,
    provider: ProviderKind,
    created_at: Instant,
    ttl_secs: u64,
}

impl CacheEntry {
    fn is_expired(&self) -> bool {
        self.created_at.elapsed().as_secs() > self.ttl_secs
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    OpenAi,
    Anthropic,
    Gemini,
    Vllm,
    Mock,
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ProviderKind::OpenAi => "openai",
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::Gemini => "gemini",
            ProviderKind::Vllm => "vllm",
            ProviderKind::Mock => "mock",
        };
        write!(f, "{s}")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSpec {
    pub provider: ProviderKind,
    pub model_id: String,
    pub quality: f64,
    pub latency: f64,
    pub cost: f64,
    pub context_window: usize,
    pub tool_call_accuracy: f64,
    pub local_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingConstraints {
    pub quality_weight: f64,
    pub latency_budget_ms: u64,
    pub cost_budget: f64,
    pub min_context: usize,
    pub tool_call_weight: f64,
}

impl RoutingConstraints {
    pub fn for_profile(profile: TaskProfile) -> Self {
        match profile {
            TaskProfile::Planning => Self {
                quality_weight: 0.45,
                latency_budget_ms: 8_000,
                cost_budget: 0.08,
                min_context: 24_000,
                tool_call_weight: 0.10,
            },
            TaskProfile::Extraction => Self {
                quality_weight: 0.20,
                latency_budget_ms: 2_000,
                cost_budget: 0.02,
                min_context: 12_000,
                tool_call_weight: 0.05,
            },
            TaskProfile::Coding => Self {
                quality_weight: 0.35,
                latency_budget_ms: 5_000,
                cost_budget: 0.05,
                min_context: 20_000,
                tool_call_weight: 0.15,
            },
            TaskProfile::General => Self {
                quality_weight: 0.30,
                latency_budget_ms: 4_000,
                cost_budget: 0.04,
                min_context: 16_000,
                tool_call_weight: 0.10,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingDecision {
    pub profile: TaskProfile,
    pub primary: ModelSpec,
    pub fallbacks: Vec<ModelSpec>,
    pub scored: Vec<(ModelSpec, f64)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceResult {
    pub provider: ProviderKind,
    pub model_id: String,
    pub output: String,
    pub used_fallback: bool,
}

#[derive(Debug, Clone)]
pub struct ModelRouter {
    catalog: Arc<Vec<ModelSpec>>,
    client: ProviderClient,
    preferred_model: Arc<Mutex<Option<String>>>,
    cache: Arc<DashMap<u64, CacheEntry>>,
}

impl ModelRouter {
    pub fn new(
        vllm_base_url: impl Into<String>,
        openai_api_key: Option<String>,
        anthropic_api_key: Option<String>,
        gemini_api_key: Option<String>,
    ) -> Self {
        Self {
            catalog: Arc::new(default_catalog()),
            client: ProviderClient::new(
                vllm_base_url.into(),
                openai_api_key,
                anthropic_api_key,
                gemini_api_key,
            ),
            preferred_model: Arc::new(Mutex::new(None)),
            cache: Arc::new(DashMap::new()),
        }
    }

    pub fn with_catalog(
        vllm_base_url: impl Into<String>,
        openai_api_key: Option<String>,
        anthropic_api_key: Option<String>,
        gemini_api_key: Option<String>,
        catalog: Vec<ModelSpec>,
    ) -> Self {
        Self {
            catalog: Arc::new(catalog),
            client: ProviderClient::new(
                vllm_base_url.into(),
                openai_api_key,
                anthropic_api_key,
                gemini_api_key,
            ),
            preferred_model: Arc::new(Mutex::new(None)),
            cache: Arc::new(DashMap::new()),
        }
    }

    pub fn catalog(&self) -> &[ModelSpec] {
        &self.catalog
    }

    pub fn set_provider_disabled(&self, provider: ProviderKind, disabled: bool) {
        self.client.set_provider_disabled(provider, disabled);
    }

    pub fn set_model_disabled(&self, model_id: &str, disabled: bool) {
        self.client.set_model_disabled(model_id, disabled);
    }

    pub fn is_model_disabled(&self, model_id: &str) -> bool {
        self.client.is_model_disabled(model_id)
    }

    pub fn disabled_models(&self) -> Vec<String> {
        self.client
            .disabled_models
            .iter()
            .map(|v| v.key().clone())
            .collect()
    }

    pub fn disabled_providers(&self) -> Vec<ProviderKind> {
        self.client
            .disabled
            .iter()
            .map(|v| *v.key())
            .collect()
    }

    pub fn set_preferred_model(&self, model_id: Option<String>) {
        *self.preferred_model.lock().unwrap() = model_id;
    }

    pub fn preferred_model(&self) -> Option<String> {
        self.preferred_model.lock().unwrap().clone()
    }

    pub fn select_model(
        &self,
        profile: TaskProfile,
        constraints: &RoutingConstraints,
    ) -> anyhow::Result<RoutingDecision> {
        let preferred = self.preferred_model();
        let mut scored = Vec::<(ModelSpec, f64)>::new();
        for model in self.catalog.iter() {
            if model.context_window < constraints.min_context {
                continue;
            }
            if self.client.is_model_disabled(&model.model_id) {
                continue;
            }
            let mut score = score_model(model, constraints);
            // Preferred model gets a bonus
            if preferred.as_deref() == Some(model.model_id.as_str()) {
                score += 0.15;
            }
            scored.push((model.clone(), score));
        }

        if scored.is_empty() {
            return Err(anyhow::anyhow!("no model satisfies routing constraints"));
        }

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let primary = scored.first().map(|v| v.0.clone()).expect("non-empty");
        let fallbacks = scored
            .iter()
            .skip(1)
            .map(|v| v.0.clone())
            .collect::<Vec<_>>();

        Ok(RoutingDecision {
            profile,
            primary,
            fallbacks,
            scored,
        })
    }

    fn cache_ttl_secs(profile: TaskProfile) -> u64 {
        match profile {
            TaskProfile::Planning => 300,    // 5 min
            TaskProfile::Extraction => 900,  // 15 min
            TaskProfile::Coding => 600,      // 10 min
            TaskProfile::General => 600,     // 10 min
        }
    }

    fn prompt_hash(profile: TaskProfile, prompt: &str) -> u64 {
        let mut hasher = DefaultHasher::new();
        std::mem::discriminant(&profile).hash(&mut hasher);
        prompt.hash(&mut hasher);
        hasher.finish()
    }

    pub async fn infer(
        &self,
        profile: TaskProfile,
        prompt: &str,
        constraints: &RoutingConstraints,
    ) -> anyhow::Result<(RoutingDecision, InferenceResult)> {
        // Check cache
        let hash = Self::prompt_hash(profile, prompt);
        if let Some(entry) = self.cache.get(&hash) {
            if !entry.is_expired() {
                let decision = self.select_model(profile, constraints)?;
                return Ok((
                    decision,
                    InferenceResult {
                        provider: entry.provider,
                        model_id: entry.model_id.clone(),
                        output: entry.output.clone(),
                        used_fallback: false,
                    },
                ));
            } else {
                drop(entry);
                self.cache.remove(&hash);
            }
        }

        // Evict expired entries periodically (when cache exceeds 500)
        if self.cache.len() > 500 {
            self.cache.retain(|_, v| !v.is_expired());
        }

        let decision = self.select_model(profile, constraints)?;

        let mut chain = Vec::new();
        chain.push(decision.primary.clone());
        chain.extend(decision.fallbacks.clone());

        let mut first = true;
        let mut errors = Vec::new();

        for model in chain {
            match self.client.generate(&model, prompt).await {
                Ok(output) => {
                    // Store in cache
                    self.cache.insert(
                        hash,
                        CacheEntry {
                            output: output.clone(),
                            model_id: model.model_id.clone(),
                            provider: model.provider,
                            created_at: Instant::now(),
                            ttl_secs: Self::cache_ttl_secs(profile),
                        },
                    );

                    return Ok((
                        decision,
                        InferenceResult {
                            provider: model.provider,
                            model_id: model.model_id,
                            output,
                            used_fallback: !first,
                        },
                    ));
                }
                Err(err) => {
                    errors.push(format!("{}:{} => {}", model.provider, model.model_id, err));
                }
            }
            first = false;
        }

        Err(anyhow::anyhow!(
            "all model candidates failed: {}",
            errors.join(" | ")
        ))
    }
}

#[derive(Debug, Clone)]
pub struct ProviderClient {
    http: reqwest::Client,
    vllm_base_url: String,
    openai_api_key: Option<String>,
    anthropic_api_key: Option<String>,
    gemini_api_key: Option<String>,
    disabled: Arc<DashSet<ProviderKind>>,
    disabled_models: Arc<DashSet<String>>,
}

impl ProviderClient {
    pub fn new(
        vllm_base_url: String,
        openai_api_key: Option<String>,
        anthropic_api_key: Option<String>,
        gemini_api_key: Option<String>,
    ) -> Self {
        Self {
            http: reqwest::Client::new(),
            vllm_base_url,
            openai_api_key,
            anthropic_api_key,
            gemini_api_key,
            disabled: Arc::new(DashSet::new()),
            disabled_models: Arc::new(DashSet::new()),
        }
    }

    pub fn set_model_disabled(&self, model_id: &str, disabled: bool) {
        if disabled {
            self.disabled_models.insert(model_id.to_string());
        } else {
            self.disabled_models.remove(model_id);
        }
    }

    pub fn is_model_disabled(&self, model_id: &str) -> bool {
        self.disabled_models.contains(model_id)
    }

    pub fn set_provider_disabled(&self, provider: ProviderKind, disabled: bool) {
        if disabled {
            self.disabled.insert(provider);
        } else {
            self.disabled.remove(&provider);
        }
    }

    pub async fn generate(&self, model: &ModelSpec, prompt: &str) -> anyhow::Result<String> {
        if self.disabled.contains(&model.provider) {
            return Err(anyhow::anyhow!("provider {} is disabled", model.provider));
        }
        if self.disabled_models.contains(&model.model_id) {
            return Err(anyhow::anyhow!("model {} is disabled", model.model_id));
        }

        match model.provider {
            ProviderKind::OpenAi => self.generate_openai(model, prompt).await,
            ProviderKind::Anthropic => self.generate_anthropic(model, prompt).await,
            ProviderKind::Gemini => self.generate_gemini(model, prompt).await,
            ProviderKind::Vllm => self.generate_vllm(model, prompt).await,
            ProviderKind::Mock => Ok(mock_inference(model, prompt)),
        }
    }

    async fn generate_openai(
        &self,
        model: &ModelSpec,
        prompt: &str,
    ) -> anyhow::Result<String> {
        let api_key = self
            .openai_api_key
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("OPENAI_API_KEY not set"))?;

        let resp = self
            .http
            .post("https://api.openai.com/v1/chat/completions")
            .bearer_auth(api_key)
            .json(&serde_json::json!({
                "model": model.model_id,
                "messages": [{"role": "user", "content": prompt}],
            }))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("OpenAI API error {}: {}", status, body));
        }

        let body: serde_json::Value = resp.json().await?;
        body["choices"][0]["message"]["content"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("unexpected OpenAI response format"))
    }

    async fn generate_anthropic(
        &self,
        model: &ModelSpec,
        prompt: &str,
    ) -> anyhow::Result<String> {
        let api_key = self
            .anthropic_api_key
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("ANTHROPIC_API_KEY not set"))?;

        let resp = self
            .http
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&serde_json::json!({
                "model": model.model_id,
                "max_tokens": 4096,
                "messages": [{"role": "user", "content": prompt}],
            }))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("Anthropic API error {}: {}", status, body));
        }

        let body: serde_json::Value = resp.json().await?;
        body["content"][0]["text"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("unexpected Anthropic response format"))
    }

    async fn generate_gemini(
        &self,
        model: &ModelSpec,
        prompt: &str,
    ) -> anyhow::Result<String> {
        let api_key = self
            .gemini_api_key
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("GEMINI_API_KEY not set"))?;

        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
            model.model_id, api_key
        );

        let resp = self
            .http
            .post(&url)
            .json(&serde_json::json!({
                "contents": [{"parts": [{"text": prompt}]}],
            }))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("Gemini API error {}: {}", status, body));
        }

        let body: serde_json::Value = resp.json().await?;
        body["candidates"][0]["content"]["parts"][0]["text"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("unexpected Gemini response format"))
    }

    async fn generate_vllm(
        &self,
        model: &ModelSpec,
        prompt: &str,
    ) -> anyhow::Result<String> {
        let endpoint = format!(
            "{}/v1/chat/completions",
            self.vllm_base_url.trim_end_matches('/')
        );

        let resp = self
            .http
            .post(&endpoint)
            .json(&serde_json::json!({
                "model": model.model_id,
                "messages": [{"role": "user", "content": prompt}],
            }))
            .send()
            .await
            .map_err(|_| {
                anyhow::anyhow!("vLLM server unavailable at {}", self.vllm_base_url)
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("vLLM API error {}: {}", status, body));
        }

        let body: serde_json::Value = resp.json().await?;
        body["choices"][0]["message"]["content"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("unexpected vLLM response format"))
    }
}

fn score_model(model: &ModelSpec, constraints: &RoutingConstraints) -> f64 {
    let quality = model.quality;
    let latency_fit =
        (constraints.latency_budget_ms as f64 / model.latency.max(1.0)).min(1.5) / 1.5;
    let cost_fit = (constraints.cost_budget / model.cost.max(0.0001)).min(1.5) / 1.5;
    let context_fit =
        (model.context_window as f64 / constraints.min_context.max(1) as f64).min(1.5) / 1.5;
    let tool = model.tool_call_accuracy;

    let quality_w = constraints.quality_weight;
    let latency_w = (0.30 - quality_w / 4.0).max(0.10);
    let cost_w = 0.20;
    let context_w = 0.20;
    let tool_w = constraints.tool_call_weight;

    (quality_w * quality)
        + (latency_w * latency_fit)
        + (cost_w * cost_fit)
        + (context_w * context_fit)
        + (tool_w * tool)
}

fn mock_inference(model: &ModelSpec, prompt: &str) -> String {
    let mut summary = prompt.replace('\n', " ");
    if summary.len() > 200 {
        summary = summary.chars().take(200).collect();
    }
    format!(
        "[provider={} model={}] {}",
        model.provider, model.model_id, summary
    )
}

fn default_catalog() -> Vec<ModelSpec> {
    vec![
        // --- OpenAI ---
        ModelSpec {
            provider: ProviderKind::OpenAi,
            model_id: "gpt-5.2".to_string(),
            quality: 0.95,
            latency: 3200.0,
            cost: 0.07,
            context_window: 128_000,
            tool_call_accuracy: 0.92,
            local_only: false,
        },
        ModelSpec {
            provider: ProviderKind::OpenAi,
            model_id: "gpt-5.1".to_string(),
            quality: 0.82,
            latency: 1400.0,
            cost: 0.015,
            context_window: 128_000,
            tool_call_accuracy: 0.85,
            local_only: false,
        },
        ModelSpec {
            provider: ProviderKind::OpenAi,
            model_id: "gpt-5-mini".to_string(),
            quality: 0.88,
            latency: 4500.0,
            cost: 0.04,
            context_window: 200_000,
            tool_call_accuracy: 0.80,
            local_only: false,
        },
        // --- Anthropic ---
        ModelSpec {
            provider: ProviderKind::Anthropic,
            model_id: "claude-sonnet-4-5-20250929".to_string(),
            quality: 0.94,
            latency: 2800.0,
            cost: 0.06,
            context_window: 200_000,
            tool_call_accuracy: 0.92,
            local_only: false,
        },
        ModelSpec {
            provider: ProviderKind::Anthropic,
            model_id: "claude-opus-4-5-20251101".to_string(),
            quality: 0.80,
            latency: 1200.0,
            cost: 0.005,
            context_window: 200_000,
            tool_call_accuracy: 0.82,
            local_only: false,
        },
        ModelSpec {
            provider: ProviderKind::Anthropic,
            model_id: "claude-opus-4-6".to_string(),
            quality: 0.97,
            latency: 5000.0,
            cost: 0.12,
            context_window: 200_000,
            tool_call_accuracy: 0.95,
            local_only: false,
        },
        // --- Gemini ---
        ModelSpec {
            provider: ProviderKind::Gemini,
            model_id: "gemini-2.5-pro".to_string(),
            quality: 0.93,
            latency: 2400.0,
            cost: 0.05,
            context_window: 1_000_000,
            tool_call_accuracy: 0.90,
            local_only: false,
        },
        ModelSpec {
            provider: ProviderKind::Gemini,
            model_id: "gemini-2.5-flash".to_string(),
            quality: 0.82,
            latency: 800.0,
            cost: 0.008,
            context_window: 1_000_000,
            tool_call_accuracy: 0.78,
            local_only: false,
        },
        // --- vLLM (local) ---
        ModelSpec {
            provider: ProviderKind::Vllm,
            model_id: "qwen2.5:14b".to_string(),
            quality: 0.78,
            latency: 1700.0,
            cost: 0.005,
            context_window: 32_000,
            tool_call_accuracy: 0.72,
            local_only: true,
        },
        ModelSpec {
            provider: ProviderKind::Vllm,
            model_id: "qwen2.5:7b".to_string(),
            quality: 0.68,
            latency: 900.0,
            cost: 0.002,
            context_window: 32_000,
            tool_call_accuracy: 0.60,
            local_only: true,
        },
        // --- Mock ---
        ModelSpec {
            provider: ProviderKind::Mock,
            model_id: "mock-general".to_string(),
            quality: 0.40,
            latency: 50.0,
            cost: 0.0001,
            context_window: 16_000,
            tool_call_accuracy: 0.40,
            local_only: false,
        },
    ]
}

pub fn providers(catalog: &[ModelSpec]) -> HashSet<ProviderKind> {
    catalog.iter().map(|m| m.provider).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn planning_prefers_quality_models() {
        let router = ModelRouter::new("http://127.0.0.1:8000", None, None, None);
        let constraints = RoutingConstraints::for_profile(TaskProfile::Planning);
        let decision = router
            .select_model(TaskProfile::Planning, &constraints)
            .unwrap();

        assert!(
            matches!(
                decision.primary.provider,
                ProviderKind::OpenAi | ProviderKind::Anthropic | ProviderKind::Gemini
            ),
            "unexpected primary provider: {}",
            decision.primary.provider
        );
    }

    #[tokio::test]
    async fn fallback_chain_works_when_primary_disabled() {
        let router = ModelRouter::new("http://127.0.0.1:8000", None, None, None);
        let constraints = RoutingConstraints::for_profile(TaskProfile::General);
        let primary = router
            .select_model(TaskProfile::General, &constraints)
            .unwrap()
            .primary;

        router.set_provider_disabled(primary.provider, true);

        // Use Mock provider to avoid needing real API keys
        router.set_provider_disabled(ProviderKind::OpenAi, true);
        router.set_provider_disabled(ProviderKind::Anthropic, true);
        router.set_provider_disabled(ProviderKind::Gemini, true);
        router.set_provider_disabled(ProviderKind::Vllm, true);
        // Re-enable mock so fallback chain can succeed
        router.set_provider_disabled(ProviderKind::Mock, false);

        let (_decision, result) = router
            .infer(TaskProfile::General, "hello", &constraints)
            .await
            .unwrap();

        assert!(result.used_fallback);
    }
}
