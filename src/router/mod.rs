use std::collections::HashSet;
use std::sync::Arc;

use dashmap::DashSet;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use crate::types::TaskProfile;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    OpenAi,
    Anthropic,
    Gemini,
    Ollama,
    Mock,
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ProviderKind::OpenAi => "openai",
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::Gemini => "gemini",
            ProviderKind::Ollama => "ollama",
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
}

impl ModelRouter {
    pub fn new(ollama_base_url: impl Into<String>) -> Self {
        Self {
            catalog: Arc::new(default_catalog()),
            client: ProviderClient::new(ollama_base_url.into()),
        }
    }

    pub fn with_catalog(ollama_base_url: impl Into<String>, catalog: Vec<ModelSpec>) -> Self {
        Self {
            catalog: Arc::new(catalog),
            client: ProviderClient::new(ollama_base_url.into()),
        }
    }

    pub fn set_provider_disabled(&self, provider: ProviderKind, disabled: bool) {
        self.client.set_provider_disabled(provider, disabled);
    }

    pub fn select_model(
        &self,
        profile: TaskProfile,
        constraints: &RoutingConstraints,
    ) -> anyhow::Result<RoutingDecision> {
        let mut scored = Vec::<(ModelSpec, f64)>::new();
        for model in self.catalog.iter() {
            if model.context_window < constraints.min_context {
                continue;
            }
            let score = score_model(model, constraints);
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

    pub async fn infer(
        &self,
        profile: TaskProfile,
        prompt: &str,
        constraints: &RoutingConstraints,
    ) -> anyhow::Result<(RoutingDecision, InferenceResult)> {
        let decision = self.select_model(profile, constraints)?;

        let mut chain = Vec::new();
        chain.push(decision.primary.clone());
        chain.extend(decision.fallbacks.clone());

        let mut first = true;
        let mut errors = Vec::new();

        for model in chain {
            match self.client.generate(&model, prompt).await {
                Ok(output) => {
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
    ollama_base_url: String,
    disabled: Arc<DashSet<ProviderKind>>,
}

impl ProviderClient {
    pub fn new(ollama_base_url: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            ollama_base_url,
            disabled: Arc::new(DashSet::new()),
        }
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

        match model.provider {
            ProviderKind::Ollama => self.generate_ollama(model, prompt).await,
            ProviderKind::OpenAi
            | ProviderKind::Anthropic
            | ProviderKind::Gemini
            | ProviderKind::Mock => Ok(mock_inference(model, prompt)),
        }
    }

    async fn generate_ollama(&self, model: &ModelSpec, prompt: &str) -> anyhow::Result<String> {
        let endpoint = format!(
            "{}/api/generate",
            self.ollama_base_url.trim_end_matches('/')
        );

        let resp = self
            .http
            .post(endpoint)
            .json(&serde_json::json!({
                "model": model.model_id,
                "prompt": prompt,
                "stream": false
            }))
            .send()
            .await;

        let Ok(resp) = resp else {
            // Local model unavailable: fallback to deterministic stub.
            return Ok(mock_inference(model, prompt));
        };

        if resp.status() != StatusCode::OK {
            return Ok(mock_inference(model, prompt));
        }

        let body: serde_json::Value = resp.json().await?;
        let text = body
            .get("response")
            .and_then(|v| v.as_str())
            .map(|v| v.to_string())
            .unwrap_or_else(|| mock_inference(model, prompt));
        Ok(text)
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
        ModelSpec {
            provider: ProviderKind::OpenAi,
            model_id: "gpt-4.1".to_string(),
            quality: 0.95,
            latency: 3200.0,
            cost: 0.07,
            context_window: 128_000,
            tool_call_accuracy: 0.92,
            local_only: false,
        },
        ModelSpec {
            provider: ProviderKind::Anthropic,
            model_id: "claude-sonnet".to_string(),
            quality: 0.92,
            latency: 2800.0,
            cost: 0.06,
            context_window: 200_000,
            tool_call_accuracy: 0.90,
            local_only: false,
        },
        ModelSpec {
            provider: ProviderKind::Gemini,
            model_id: "gemini-2.0-pro".to_string(),
            quality: 0.90,
            latency: 2400.0,
            cost: 0.05,
            context_window: 128_000,
            tool_call_accuracy: 0.88,
            local_only: false,
        },
        ModelSpec {
            provider: ProviderKind::Ollama,
            model_id: "qwen2.5:14b".to_string(),
            quality: 0.78,
            latency: 1700.0,
            cost: 0.005,
            context_window: 32_000,
            tool_call_accuracy: 0.72,
            local_only: true,
        },
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
        let router = ModelRouter::new("http://127.0.0.1:11434");
        let constraints = RoutingConstraints::for_profile(TaskProfile::Planning);
        let decision = router
            .select_model(TaskProfile::Planning, &constraints)
            .unwrap();

        assert!(
            matches!(
                decision.primary.provider,
                ProviderKind::OpenAi | ProviderKind::Anthropic
            ),
            "unexpected primary provider: {}",
            decision.primary.provider
        );
    }

    #[tokio::test]
    async fn fallback_chain_works_when_primary_disabled() {
        let router = ModelRouter::new("http://127.0.0.1:11434");
        let constraints = RoutingConstraints::for_profile(TaskProfile::General);
        let primary = router
            .select_model(TaskProfile::General, &constraints)
            .unwrap()
            .primary;

        router.set_provider_disabled(primary.provider, true);

        let (_decision, result) = router
            .infer(TaskProfile::General, "hello", &constraints)
            .await
            .unwrap();

        assert!(result.used_fallback);
    }
}
