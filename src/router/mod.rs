use std::collections::HashSet;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use dashmap::DashSet;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use uuid::Uuid;

use crate::types::{CliModelBackendKind, CliModelConfig, TaskProfile};

pub type TokenCallback = Arc<dyn Fn(&str) + Send + Sync>;

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
    ClaudeCode,
    Codex,
    Mock,
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ProviderKind::OpenAi => "openai",
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::Gemini => "gemini",
            ProviderKind::Vllm => "vllm",
            ProviderKind::ClaudeCode => "claude_code",
            ProviderKind::Codex => "codex",
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
    cli_model: Arc<Mutex<Option<CliModelConfig>>>,
    cache: Arc<DashMap<u64, CacheEntry>>,
}

impl ModelRouter {
    pub fn new(
        vllm_base_url: impl Into<String>,
        openai_api_key: Option<String>,
        anthropic_api_key: Option<String>,
        gemini_api_key: Option<String>,
        cli_model: Option<CliModelConfig>,
    ) -> Self {
        let router = Self {
            catalog: Arc::new(default_catalog(cli_model.as_ref())),
            client: ProviderClient::new(
                vllm_base_url.into(),
                openai_api_key,
                anthropic_api_key,
                gemini_api_key,
                cli_model.clone(),
            ),
            preferred_model: Arc::new(Mutex::new(None)),
            cli_model: Arc::new(Mutex::new(cli_model)),
            cache: Arc::new(DashMap::new()),
        };
        if let Some(config) = router.cli_model_config() {
            router.apply_cli_model_bootstrap(&config);
        }
        router
    }

    pub fn with_catalog(
        vllm_base_url: impl Into<String>,
        openai_api_key: Option<String>,
        anthropic_api_key: Option<String>,
        gemini_api_key: Option<String>,
        cli_model: Option<CliModelConfig>,
        catalog: Vec<ModelSpec>,
    ) -> Self {
        let router = Self {
            catalog: Arc::new(catalog),
            client: ProviderClient::new(
                vllm_base_url.into(),
                openai_api_key,
                anthropic_api_key,
                gemini_api_key,
                cli_model.clone(),
            ),
            preferred_model: Arc::new(Mutex::new(None)),
            cli_model: Arc::new(Mutex::new(cli_model)),
            cache: Arc::new(DashMap::new()),
        };
        if let Some(config) = router.cli_model_config() {
            router.apply_cli_model_bootstrap(&config);
        }
        router
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
        self.client.disabled.iter().map(|v| *v.key()).collect()
    }

    pub fn set_preferred_model(&self, model_id: Option<String>) {
        *self.preferred_model.lock().unwrap() = model_id;
    }

    pub fn preferred_model(&self) -> Option<String> {
        self.preferred_model.lock().unwrap().clone()
    }

    pub fn set_cli_model_config(&self, cli_model: Option<CliModelConfig>) {
        *self.cli_model.lock().unwrap() = cli_model.clone();
        self.client.set_cli_model_config(cli_model);
    }

    pub fn cli_model_config(&self) -> Option<CliModelConfig> {
        self.cli_model.lock().unwrap().clone()
    }

    pub fn apply_cli_model_bootstrap(&self, cli_model: &CliModelConfig) {
        let target_provider = provider_for_cli_backend(cli_model.backend);

        self.set_preferred_model(Some(cli_model.backend.default_model_id().to_string()));

        if cli_model.cli_only {
            for provider in [
                ProviderKind::OpenAi,
                ProviderKind::Anthropic,
                ProviderKind::Gemini,
                ProviderKind::Vllm,
                ProviderKind::ClaudeCode,
                ProviderKind::Codex,
                ProviderKind::Mock,
            ] {
                self.set_provider_disabled(provider, provider != target_provider);
            }
        } else {
            self.set_provider_disabled(target_provider, false);
        }
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
            if self.client.disabled.contains(&model.provider) {
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
            TaskProfile::Planning => 300,   // 5 min
            TaskProfile::Extraction => 900, // 15 min
            TaskProfile::Coding => 600,     // 10 min
            TaskProfile::General => 600,    // 10 min
        }
    }

    fn prompt_hash(profile: TaskProfile, prompt: &str, working_dir: Option<&Path>) -> u64 {
        let mut hasher = DefaultHasher::new();
        std::mem::discriminant(&profile).hash(&mut hasher);
        prompt.hash(&mut hasher);
        if let Some(dir) = working_dir {
            dir.as_os_str().hash(&mut hasher);
        }
        hasher.finish()
    }

    pub async fn infer(
        &self,
        profile: TaskProfile,
        prompt: &str,
        constraints: &RoutingConstraints,
    ) -> anyhow::Result<(RoutingDecision, InferenceResult)> {
        self.infer_in_dir(profile, prompt, constraints, None).await
    }

    pub async fn infer_in_dir(
        &self,
        profile: TaskProfile,
        prompt: &str,
        constraints: &RoutingConstraints,
        working_dir: Option<&Path>,
    ) -> anyhow::Result<(RoutingDecision, InferenceResult)> {
        // Check cache
        let hash = Self::prompt_hash(profile, prompt, working_dir);
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
        let fallback_start = Instant::now();

        for model in chain {
            // Abort fallback if too little time remains for another attempt
            if fallback_start.elapsed().as_secs() > 100 {
                errors.push("fallback chain deadline exceeded (25s)".to_string());
                break;
            }

            match self.client.generate(&model, prompt, working_dir).await {
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

    /// Streaming inference: calls on_token for each token chunk.
    pub async fn infer_stream(
        &self,
        profile: TaskProfile,
        prompt: &str,
        constraints: &RoutingConstraints,
        on_token: TokenCallback,
    ) -> anyhow::Result<(RoutingDecision, InferenceResult)> {
        self.infer_stream_in_dir(profile, prompt, constraints, None, on_token)
            .await
    }

    pub async fn infer_stream_in_dir(
        &self,
        profile: TaskProfile,
        prompt: &str,
        constraints: &RoutingConstraints,
        working_dir: Option<&Path>,
        on_token: TokenCallback,
    ) -> anyhow::Result<(RoutingDecision, InferenceResult)> {
        let decision = self.select_model(profile, constraints)?;

        let mut chain = Vec::new();
        chain.push(decision.primary.clone());
        chain.extend(decision.fallbacks.clone());

        let mut first = true;
        let mut errors = Vec::new();
        let fallback_start = Instant::now();

        for model in chain {
            if fallback_start.elapsed().as_secs() > 100 {
                errors.push("fallback chain deadline exceeded (100s)".to_string());
                break;
            }

            match self
                .client
                .generate_stream(&model, prompt, working_dir, &on_token)
                .await
            {
                Ok(output) => {
                    // Store in cache
                    let hash = Self::prompt_hash(profile, prompt, working_dir);
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
struct CliProviderConfig {
    command: String,
    args: Vec<String>,
    timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct ProviderClient {
    http: reqwest::Client,
    vllm_base_url: String,
    openai_api_key: Option<String>,
    anthropic_api_key: Option<String>,
    gemini_api_key: Option<String>,
    cli_providers: Arc<DashMap<ProviderKind, CliProviderConfig>>,
    disabled: Arc<DashSet<ProviderKind>>,
    disabled_models: Arc<DashSet<String>>,
}

impl ProviderClient {
    pub fn new(
        vllm_base_url: String,
        openai_api_key: Option<String>,
        anthropic_api_key: Option<String>,
        gemini_api_key: Option<String>,
        cli_model: Option<CliModelConfig>,
    ) -> Self {
        let client = Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            vllm_base_url,
            openai_api_key,
            anthropic_api_key,
            gemini_api_key,
            cli_providers: Arc::new(DashMap::new()),
            disabled: Arc::new(DashSet::new()),
            disabled_models: Arc::new(DashSet::new()),
        };
        client.set_cli_model_config(cli_model);
        client
    }

    pub fn set_cli_model_config(&self, cli_model: Option<CliModelConfig>) {
        self.cli_providers.remove(&ProviderKind::ClaudeCode);
        self.cli_providers.remove(&ProviderKind::Codex);

        match cli_model {
            Some(cli_model) => {
                let provider = provider_for_cli_backend(cli_model.backend);
                self.cli_providers.insert(
                    provider,
                    CliProviderConfig {
                        command: cli_model.command,
                        args: cli_model.args,
                        timeout: Duration::from_millis(cli_model.timeout_ms),
                    },
                );
                self.set_provider_disabled(provider, false);
                let other = match provider {
                    ProviderKind::ClaudeCode => Some(ProviderKind::Codex),
                    ProviderKind::Codex => Some(ProviderKind::ClaudeCode),
                    _ => None,
                };
                if let Some(other) = other {
                    self.set_provider_disabled(other, true);
                }
            }
            None => {
                self.set_provider_disabled(ProviderKind::ClaudeCode, true);
                self.set_provider_disabled(ProviderKind::Codex, true);
            }
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

    pub async fn generate(
        &self,
        model: &ModelSpec,
        prompt: &str,
        working_dir: Option<&Path>,
    ) -> anyhow::Result<String> {
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
            ProviderKind::ClaudeCode => self.generate_claude_code(prompt, working_dir).await,
            ProviderKind::Codex => self.generate_codex(prompt, working_dir).await,
            ProviderKind::Mock => Ok(mock_inference(model, prompt)),
        }
    }

    async fn generate_openai(&self, model: &ModelSpec, prompt: &str) -> anyhow::Result<String> {
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

    async fn generate_anthropic(&self, model: &ModelSpec, prompt: &str) -> anyhow::Result<String> {
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

    async fn generate_gemini(&self, model: &ModelSpec, prompt: &str) -> anyhow::Result<String> {
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

    async fn generate_vllm(&self, model: &ModelSpec, prompt: &str) -> anyhow::Result<String> {
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
            .map_err(|_| anyhow::anyhow!("vLLM server unavailable at {}", self.vllm_base_url))?;

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

    fn cli_provider(&self, provider: ProviderKind) -> anyhow::Result<CliProviderConfig> {
        self.cli_providers
            .get(&provider)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| anyhow::anyhow!("CLI provider {} is not configured", provider))
    }

    fn cli_working_dir(&self, working_dir: Option<&Path>) -> PathBuf {
        working_dir
            .map(|path| path.to_path_buf())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }

    async fn run_cli_command(
        &self,
        mut cmd: Command,
        timeout: Duration,
        label: &str,
        stdin_payload: Option<&str>,
    ) -> anyhow::Result<std::process::Output> {
        if stdin_payload.is_some() {
            cmd.stdin(std::process::Stdio::piped());
        }

        let mut child = cmd
            .spawn()
            .map_err(|err| anyhow::anyhow!("{label} failed to spawn: {err}"))?;

        if let Some(payload) = stdin_payload {
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(payload.as_bytes()).await?;
            }
        }

        tokio::time::timeout(timeout, child.wait_with_output())
            .await
            .map_err(|_| anyhow::anyhow!("{label} timed out after {:?}", timeout))?
            .map_err(|err| anyhow::anyhow!("{label} failed while waiting: {err}"))
    }

    fn decode_cli_text(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).trim().to_string()
    }

    fn ensure_cli_success(label: &str, output: &std::process::Output) -> anyhow::Result<()> {
        if output.status.success() {
            return Ok(());
        }

        let stderr = Self::decode_cli_text(&output.stderr);
        let stdout = Self::decode_cli_text(&output.stdout);
        let detail = if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            "no output".to_string()
        };

        Err(anyhow::anyhow!(
            "{} exited with code {}: {}",
            label,
            output.status.code().unwrap_or(-1),
            detail
        ))
    }

    async fn generate_claude_code(
        &self,
        prompt: &str,
        working_dir: Option<&Path>,
    ) -> anyhow::Result<String> {
        let config = self.cli_provider(ProviderKind::ClaudeCode)?;
        let resolved_command = crate::command_resolver::resolve_command_path(&config.command);
        let mut cmd = Command::new(&resolved_command);
        cmd.arg("-p")
            .arg(prompt)
            .args(&config.args)
            .current_dir(self.cli_working_dir(working_dir));

        let output = self
            .run_cli_command(cmd, config.timeout, "Claude Code CLI", None)
            .await?;
        Self::ensure_cli_success("Claude Code CLI", &output)?;

        Ok(Self::decode_cli_text(&output.stdout))
    }

    async fn generate_codex(
        &self,
        prompt: &str,
        working_dir: Option<&Path>,
    ) -> anyhow::Result<String> {
        let config = self.cli_provider(ProviderKind::Codex)?;
        let output_path =
            std::env::temp_dir().join(format!("codex-last-message-{}.txt", Uuid::new_v4()));
        let resolved_command = crate::command_resolver::resolve_command_path(&config.command);
        let mut cmd = Command::new(&resolved_command);
        cmd.arg("exec")
            .arg("--skip-git-repo-check")
            .arg("--full-auto")
            .arg("--ephemeral")
            .arg("--output-last-message")
            .arg(&output_path)
            .args(&config.args)
            .arg("-")
            .current_dir(self.cli_working_dir(working_dir));

        let output = self
            .run_cli_command(cmd, config.timeout, "Codex CLI", Some(prompt))
            .await?;
        let file_output = tokio::fs::read_to_string(&output_path).await.ok();
        let _ = tokio::fs::remove_file(&output_path).await;

        Self::ensure_cli_success("Codex CLI", &output)?;

        if let Some(text) = file_output {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return Ok(trimmed.to_string());
            }
        }

        let stdout = Self::decode_cli_text(&output.stdout);
        if !stdout.is_empty() {
            return Ok(stdout);
        }

        let stderr = Self::decode_cli_text(&output.stderr);
        if !stderr.is_empty() {
            return Ok(stderr);
        }

        Ok(String::new())
    }

    /// Streaming generate: sends token chunks via callback, returns full output.
    pub async fn generate_stream(
        &self,
        model: &ModelSpec,
        prompt: &str,
        working_dir: Option<&Path>,
        on_token: &TokenCallback,
    ) -> anyhow::Result<String> {
        if self.disabled.contains(&model.provider) {
            return Err(anyhow::anyhow!("provider {} is disabled", model.provider));
        }
        if self.disabled_models.contains(&model.model_id) {
            return Err(anyhow::anyhow!("model {} is disabled", model.model_id));
        }

        match model.provider {
            ProviderKind::OpenAi | ProviderKind::Vllm => {
                self.stream_openai_compat(model, prompt, on_token).await
            }
            ProviderKind::Anthropic => self.stream_anthropic(model, prompt, on_token).await,
            ProviderKind::Gemini => self.stream_gemini(model, prompt, on_token).await,
            ProviderKind::ClaudeCode | ProviderKind::Codex => {
                let output = self.generate(model, prompt, working_dir).await?;
                if !output.is_empty() {
                    on_token(&output);
                }
                Ok(output)
            }
            ProviderKind::Mock => {
                let output = mock_inference(model, prompt);
                on_token(&output);
                Ok(output)
            }
        }
    }

    /// OpenAI-compatible streaming (works for OpenAI + vLLM)
    async fn stream_openai_compat(
        &self,
        model: &ModelSpec,
        prompt: &str,
        on_token: &TokenCallback,
    ) -> anyhow::Result<String> {
        let base_url = if model.provider == ProviderKind::Vllm {
            format!(
                "{}/v1/chat/completions",
                self.vllm_base_url.trim_end_matches('/')
            )
        } else {
            "https://api.openai.com/v1/chat/completions".to_string()
        };

        let mut req = self.http.post(&base_url).json(&serde_json::json!({
            "model": model.model_id,
            "messages": [{"role": "user", "content": prompt}],
            "stream": true,
        }));

        if model.provider == ProviderKind::OpenAi {
            let api_key = self
                .openai_api_key
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("OPENAI_API_KEY not set"))?;
            req = req.bearer_auth(api_key);
        }

        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("API error {}: {}", status, body));
        }

        let mut full_output = String::new();
        let mut stream = resp.bytes_stream();
        let mut buf = Vec::<u8>::new();
        let mut done = false;

        while let Some(chunk) = stream.next().await {
            let bytes = chunk?;
            buf.extend_from_slice(&bytes);

            for raw_line in drain_sse_lines(&mut buf) {
                let line = raw_line.trim();
                if line.is_empty() || !line.starts_with("data: ") {
                    continue;
                }
                let data = &line[6..];
                if data == "[DONE]" {
                    done = true;
                    break;
                }
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(data) {
                    if let Some(delta) = val["choices"][0]["delta"]["content"].as_str() {
                        if !delta.is_empty() {
                            on_token(delta);
                            full_output.push_str(delta);
                        }
                    }
                }
            }
            if done {
                break;
            }
        }

        Ok(full_output)
    }

    /// Anthropic streaming
    async fn stream_anthropic(
        &self,
        model: &ModelSpec,
        prompt: &str,
        on_token: &TokenCallback,
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
                "stream": true,
            }))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("Anthropic API error {}: {}", status, body));
        }

        let mut full_output = String::new();
        let mut stream = resp.bytes_stream();
        let mut buf = Vec::<u8>::new();

        while let Some(chunk) = stream.next().await {
            let bytes = chunk?;
            buf.extend_from_slice(&bytes);

            for raw_line in drain_sse_lines(&mut buf) {
                let line = raw_line.trim();
                if line.is_empty() || !line.starts_with("data: ") {
                    continue;
                }
                let data = &line[6..];
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(data) {
                    if val["type"] == "content_block_delta" {
                        if let Some(text) = val["delta"]["text"].as_str() {
                            if !text.is_empty() {
                                on_token(text);
                                full_output.push_str(text);
                            }
                        }
                    }
                }
            }
        }

        Ok(full_output)
    }

    /// Gemini streaming
    async fn stream_gemini(
        &self,
        model: &ModelSpec,
        prompt: &str,
        on_token: &TokenCallback,
    ) -> anyhow::Result<String> {
        let api_key = self
            .gemini_api_key
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("GEMINI_API_KEY not set"))?;

        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:streamGenerateContent?alt=sse&key={}",
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

        let mut full_output = String::new();
        let mut stream = resp.bytes_stream();
        let mut buf = Vec::<u8>::new();

        while let Some(chunk) = stream.next().await {
            let bytes = chunk?;
            buf.extend_from_slice(&bytes);

            for raw_line in drain_sse_lines(&mut buf) {
                let line = raw_line.trim();
                if line.is_empty() || !line.starts_with("data: ") {
                    continue;
                }
                let data = &line[6..];
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(data) {
                    if let Some(text) = val["candidates"][0]["content"]["parts"][0]["text"].as_str()
                    {
                        if !text.is_empty() {
                            on_token(text);
                            full_output.push_str(text);
                        }
                    }
                }
            }
        }

        Ok(full_output)
    }
}

fn drain_sse_lines(buf: &mut Vec<u8>) -> Vec<String> {
    let mut lines = Vec::new();
    while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
        let mut line = buf.drain(..=pos).collect::<Vec<u8>>();
        if line.last().copied() == Some(b'\n') {
            line.pop();
        }
        if line.last().copied() == Some(b'\r') {
            line.pop();
        }
        lines.push(String::from_utf8_lossy(line.as_slice()).into_owned());
    }
    lines
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

fn provider_for_cli_backend(backend: CliModelBackendKind) -> ProviderKind {
    match backend {
        CliModelBackendKind::ClaudeCode => ProviderKind::ClaudeCode,
        CliModelBackendKind::Codex => ProviderKind::Codex,
    }
}

fn cli_model_spec(backend: CliModelBackendKind) -> ModelSpec {
    let (quality, latency, tool_call_accuracy) = match backend {
        CliModelBackendKind::ClaudeCode => (0.94, 2600.0, 0.90),
        CliModelBackendKind::Codex => (0.93, 2400.0, 0.88),
    };

    ModelSpec {
        provider: provider_for_cli_backend(backend),
        model_id: backend.default_model_id().to_string(),
        quality,
        latency,
        cost: 0.001,
        context_window: 200_000,
        tool_call_accuracy,
        local_only: true,
    }
}

fn default_catalog(_cli_model: Option<&CliModelConfig>) -> Vec<ModelSpec> {
    let mut catalog = vec![
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
            model_id: "claude-sonnet-4-6".to_string(),
            quality: 0.94,
            latency: 2800.0,
            cost: 0.06,
            context_window: 200_000,
            tool_call_accuracy: 0.92,
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
            model_id: "gemini-3.1-pro-preview".to_string(),
            quality: 0.93,
            latency: 2400.0,
            cost: 0.05,
            context_window: 1_000_000,
            tool_call_accuracy: 0.90,
            local_only: false,
        },
        ModelSpec {
            provider: ProviderKind::Gemini,
            model_id: "gemini-3-flash-preview".to_string(),
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
    ];

    catalog.push(cli_model_spec(CliModelBackendKind::ClaudeCode));
    catalog.push(cli_model_spec(CliModelBackendKind::Codex));

    catalog
}

pub fn providers(catalog: &[ModelSpec]) -> HashSet<ProviderKind> {
    catalog.iter().map(|m| m.provider).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[tokio::test]
    async fn planning_prefers_quality_models() {
        let router = ModelRouter::new("http://127.0.0.1:8000", None, None, None, None);
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
        let router = ModelRouter::new("http://127.0.0.1:8000", None, None, None, None);
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

        assert_eq!(result.provider, ProviderKind::Mock);
        assert!(!result.used_fallback);
    }

    #[test]
    fn cli_model_is_added_to_catalog_when_configured() {
        let config = CliModelConfig {
            backend: CliModelBackendKind::Codex,
            command: "codex".to_string(),
            args: vec!["--full-auto".to_string()],
            timeout_ms: 60_000,
            cli_only: true,
        };

        let catalog = default_catalog(Some(&config));

        assert!(catalog.iter().any(|spec| {
            spec.provider == ProviderKind::Codex && spec.model_id == "codex-cli" && spec.local_only
        }));
    }

    #[test]
    fn prompt_hash_scopes_cache_by_working_dir() {
        let left = ModelRouter::prompt_hash(
            TaskProfile::General,
            "hello",
            Some(Path::new("/tmp/session-a")),
        );
        let right = ModelRouter::prompt_hash(
            TaskProfile::General,
            "hello",
            Some(Path::new("/tmp/session-b")),
        );

        assert_ne!(left, right);
    }
}
