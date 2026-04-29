//! Per-million-token USD pricing layer (TODO 8-4 가격표).
//!
//! `Pricing` is loaded once at startup from `config/pricing.yaml` (path
//! overridable via `CLI_AGENT_PRICING_PATH`) and then used to convert any
//! `TokenUsage` + model identifier into a USD estimate via
//! `Pricing::cost_for`.
//!
//! Estimates are best-effort: provider rates change, free-tier credits and
//! discounts aren't modeled, and CLI backends report no usage at all. Every
//! API surface that exposes a cost number must also surface
//! `cost_is_estimate: true` so consumers don't treat it as billing-grade.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::types::TokenUsage;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
pub struct ModelPrice {
    /// USD per 1,000,000 input tokens.
    #[serde(default)]
    pub input_per_1m: f64,
    /// USD per 1,000,000 output tokens.
    #[serde(default)]
    pub output_per_1m: f64,
}

impl ModelPrice {
    pub const ZERO: ModelPrice = ModelPrice {
        input_per_1m: 0.0,
        output_per_1m: 0.0,
    };

    pub fn cost(&self, usage: &TokenUsage) -> f64 {
        let input = (usage.input_tokens as f64) * self.input_per_1m / 1_000_000.0;
        let output = (usage.output_tokens as f64) * self.output_per_1m / 1_000_000.0;
        input + output
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Pricing {
    #[serde(default = "default_global")]
    pub global_default: ModelPrice,
    #[serde(default)]
    pub provider_defaults: HashMap<String, ModelPrice>,
    #[serde(default)]
    pub models: HashMap<String, ModelPrice>,
}

fn default_global() -> ModelPrice {
    ModelPrice {
        input_per_1m: 1.0,
        output_per_1m: 3.0,
    }
}

impl Pricing {
    /// Load pricing config. Resolution order:
    /// 1. `CLI_AGENT_PRICING_PATH` env var
    /// 2. `./config/pricing.yaml` relative to current working dir
    /// 3. built-in fallback (`Pricing::builtin_default`)
    pub fn load() -> Self {
        let candidates = Self::candidate_paths();
        for path in &candidates {
            if path.is_file() {
                match std::fs::read_to_string(path) {
                    Ok(raw) => match serde_yaml::from_str::<Pricing>(&raw) {
                        Ok(loaded) => {
                            tracing::info!(
                                path = %path.display(),
                                models = loaded.models.len(),
                                "loaded model pricing"
                            );
                            return loaded;
                        }
                        Err(e) => {
                            tracing::warn!(
                                path = %path.display(),
                                error = %e,
                                "pricing.yaml parse failed; trying next candidate"
                            );
                        }
                    },
                    Err(e) => {
                        tracing::warn!(
                            path = %path.display(),
                            error = %e,
                            "pricing.yaml read failed; trying next candidate"
                        );
                    }
                }
            }
        }
        tracing::info!("falling back to built-in default pricing table");
        Self::builtin_default()
    }

    fn candidate_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if let Ok(p) = std::env::var("CLI_AGENT_PRICING_PATH") {
            paths.push(PathBuf::from(p));
        }
        paths.push(PathBuf::from("config/pricing.yaml"));
        paths
    }

    /// Built-in pricing used when no config file is found. Mirrors the values
    /// in `config/pricing.yaml` so behavior matches with or without the file
    /// on disk.
    pub fn builtin_default() -> Self {
        let mut provider_defaults = HashMap::new();
        provider_defaults.insert(
            "anthropic".to_string(),
            ModelPrice {
                input_per_1m: 3.0,
                output_per_1m: 15.0,
            },
        );
        provider_defaults.insert(
            "openai".to_string(),
            ModelPrice {
                input_per_1m: 2.5,
                output_per_1m: 10.0,
            },
        );
        provider_defaults.insert(
            "gemini".to_string(),
            ModelPrice {
                input_per_1m: 1.25,
                output_per_1m: 5.0,
            },
        );
        provider_defaults.insert("vllm".to_string(), ModelPrice::ZERO);

        Self {
            global_default: default_global(),
            provider_defaults,
            models: HashMap::new(),
        }
    }

    /// Resolve the price for a model id. Match order:
    ///   1. exact `{provider}:{model_id}`
    ///   2. exact bare `model_id`
    ///   3. provider-default (when key contains `:`)
    ///   4. global default
    pub fn price_for(&self, model: &str) -> ModelPrice {
        if let Some(price) = self.models.get(model) {
            return *price;
        }
        if let Some((_, bare)) = model.split_once(':') {
            if let Some(price) = self.models.get(bare) {
                return *price;
            }
            if let Some(price) = self.provider_defaults.get(model.split(':').next().unwrap_or("")) {
                return *price;
            }
        }
        self.global_default
    }

    pub fn cost_for(&self, model: &str, usage: &TokenUsage) -> f64 {
        self.price_for(model).cost(usage)
    }
}

/// Convenience helper: load once at startup, share via `Arc`.
pub fn shared_default() -> std::sync::Arc<Pricing> {
    std::sync::Arc::new(Pricing::load())
}

/// Path to the bundled default pricing.yaml relative to the project root.
/// Exposed for tests and tooling.
pub fn default_config_path() -> &'static Path {
    Path::new("config/pricing.yaml")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: u32, output: u32) -> TokenUsage {
        TokenUsage {
            input_tokens: input,
            output_tokens: output,
        }
    }

    #[test]
    fn cost_uses_per_million_math() {
        let p = ModelPrice {
            input_per_1m: 3.0,
            output_per_1m: 15.0,
        };
        let c = p.cost(&usage(1_000_000, 1_000_000));
        assert!((c - 18.0).abs() < 1e-9);
    }

    #[test]
    fn lookup_prefers_exact_then_bare_then_provider_then_global() {
        let mut models = HashMap::new();
        models.insert(
            "anthropic:claude-opus".to_string(),
            ModelPrice {
                input_per_1m: 99.0,
                output_per_1m: 99.0,
            },
        );
        models.insert(
            "claude-sonnet".to_string(),
            ModelPrice {
                input_per_1m: 5.0,
                output_per_1m: 10.0,
            },
        );
        let mut provider_defaults = HashMap::new();
        provider_defaults.insert(
            "anthropic".to_string(),
            ModelPrice {
                input_per_1m: 1.0,
                output_per_1m: 2.0,
            },
        );
        let pricing = Pricing {
            global_default: ModelPrice {
                input_per_1m: 0.5,
                output_per_1m: 0.5,
            },
            provider_defaults,
            models,
        };

        // exact provider:model
        assert_eq!(pricing.price_for("anthropic:claude-opus").input_per_1m, 99.0);
        // bare model id
        assert_eq!(pricing.price_for("anthropic:claude-sonnet").input_per_1m, 5.0);
        // provider default
        assert_eq!(
            pricing.price_for("anthropic:claude-haiku").input_per_1m,
            1.0
        );
        // global default
        assert_eq!(pricing.price_for("openai:unknown").input_per_1m, 0.5);
    }

    #[test]
    fn cost_for_unknown_model_uses_global_default() {
        let p = Pricing {
            global_default: ModelPrice {
                input_per_1m: 1.0,
                output_per_1m: 4.0,
            },
            ..Default::default()
        };
        let c = p.cost_for("openai:never-heard-of-it", &usage(2_000_000, 500_000));
        // 2.0 + 2.0 = 4.0
        assert!((c - 4.0).abs() < 1e-9);
    }

    #[test]
    fn cli_backend_returns_zero_when_no_usage() {
        let p = Pricing::builtin_default();
        let c = p.cost_for("claude_code:cli", &usage(0, 0));
        assert_eq!(c, 0.0);
    }

    #[test]
    fn loads_yaml_from_disk_when_present() {
        let dir = std::env::temp_dir().join(format!(
            "pricing-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pricing.yaml");
        std::fs::write(
            &path,
            r#"
global_default:
  input_per_1m: 0.5
  output_per_1m: 1.5
provider_defaults:
  anthropic:
    input_per_1m: 4.0
    output_per_1m: 16.0
models:
  "anthropic:claude-opus-4-7":
    input_per_1m: 12.0
    output_per_1m: 60.0
"#,
        )
        .unwrap();

        // Use env override since cwd-relative path may differ in test runner.
        // SAFETY: env::set_var requires unsafe in 2024 edition; tests are
        // single-threaded by default for these few seconds.
        unsafe {
            std::env::set_var("CLI_AGENT_PRICING_PATH", &path);
        }
        let pricing = Pricing::load();
        unsafe {
            std::env::remove_var("CLI_AGENT_PRICING_PATH");
        }
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(
            pricing.price_for("anthropic:claude-opus-4-7").input_per_1m,
            12.0
        );
        assert_eq!(
            pricing.price_for("anthropic:claude-haiku").input_per_1m,
            4.0
        );
        assert_eq!(pricing.global_default.input_per_1m, 0.5);
    }
}
