//! AppSettings read/write and runtime application logic.
//!
//! Settings flow: `current_settings` merges the persisted row from
//! memory/store with whatever the ModelRouter already knows. `update_settings`
//! applies a `SettingsPatch`, normalizes CLI-model fields, pushes the result
//! through `apply_settings_to_runtime` (which talks to the router), and
//! persists the merged struct back to disk.

use tracing::{error, info};

use crate::router::ProviderKind;
use crate::types::{AppSettings, CliModelConfig, SettingsPatch, TaskProfile};

use super::Orchestrator;

impl Orchestrator {
    pub fn get_settings(&self) -> AppSettings {
        let cli_model = self.router.cli_model_config();
        let (
            cli_model_enabled,
            cli_model_backend,
            cli_model_command,
            cli_model_args,
            cli_model_timeout_ms,
            cli_model_only,
        ) = match cli_model {
            Some(config) => (
                true,
                Some(config.backend),
                config.command,
                config.args,
                config.timeout_ms,
                config.cli_only,
            ),
            None => (false, None, String::new(), Vec::new(), 300_000, false),
        };

        AppSettings {
            default_profile: TaskProfile::General,
            preferred_model: self.router.preferred_model(),
            disabled_models: self.router.disabled_models(),
            disabled_providers: self
                .router
                .disabled_providers()
                .iter()
                .map(|p| p.to_string())
                .collect(),
            cli_model_enabled,
            cli_model_backend,
            cli_model_command,
            cli_model_args,
            cli_model_timeout_ms,
            cli_model_only,
            terminal_command: crate::types::default_terminal_command(),
            terminal_args: crate::types::default_terminal_args(),
            terminal_auto_spawn: false,
            vllm_base_url: crate::types::default_vllm_base_url(),
            vllm_custom_model: None,
        }
    }

    pub async fn current_settings(&self) -> AppSettings {
        let mut settings = self.get_settings();
        if let Ok(Some(persisted)) = self.memory.store().load_settings().await {
            settings.default_profile = persisted.default_profile;
            settings.terminal_command = persisted.terminal_command;
            settings.terminal_args = persisted.terminal_args;
            settings.terminal_auto_spawn = persisted.terminal_auto_spawn;

            if !settings.cli_model_enabled && persisted.cli_model_enabled {
                settings.cli_model_enabled = true;
                settings.cli_model_backend = persisted.cli_model_backend;
                settings.cli_model_command = persisted.cli_model_command;
                settings.cli_model_args = persisted.cli_model_args;
                settings.cli_model_timeout_ms = persisted.cli_model_timeout_ms;
                settings.cli_model_only = persisted.cli_model_only;
            }
        }
        settings
    }

    pub async fn persist_current_settings(&self) {
        let settings = self.current_settings().await;
        if let Err(e) = self.memory.store().save_settings(&settings).await {
            error!("failed to persist settings: {e}");
        }
    }

    pub async fn update_settings(&self, patch: SettingsPatch) {
        let cli_patch_applied = patch.cli_model_enabled.is_some()
            || patch.cli_model_backend.is_some()
            || patch.cli_model_command.is_some()
            || patch.cli_model_args.is_some()
            || patch.cli_model_timeout_ms.is_some()
            || patch.cli_model_only.is_some();

        let mut settings = self.current_settings().await;

        if let Some(profile) = patch.default_profile {
            settings.default_profile = profile;
        }
        if let Some(preferred) = patch.preferred_model {
            settings.preferred_model = preferred;
        }
        if let Some(disabled_models) = patch.disabled_models {
            settings.disabled_models = disabled_models;
        }
        if let Some(disabled_providers) = patch.disabled_providers {
            settings.disabled_providers = disabled_providers;
        }
        if let Some(enabled) = patch.cli_model_enabled {
            settings.cli_model_enabled = enabled;
            if !enabled {
                settings.cli_model_backend = None;
            }
        }
        if let Some(backend) = patch.cli_model_backend {
            settings.cli_model_backend = backend;
        }
        if let Some(command) = patch.cli_model_command {
            settings.cli_model_command = command;
        }
        if let Some(args) = patch.cli_model_args {
            settings.cli_model_args = args;
        }
        if let Some(timeout_ms) = patch.cli_model_timeout_ms {
            settings.cli_model_timeout_ms = timeout_ms;
        }
        if let Some(cli_only) = patch.cli_model_only {
            settings.cli_model_only = cli_only;
        }
        if let Some(cmd) = patch.terminal_command {
            settings.terminal_command = cmd;
        }
        if let Some(args) = patch.terminal_args {
            settings.terminal_args = args;
        }
        if let Some(auto) = patch.terminal_auto_spawn {
            settings.terminal_auto_spawn = auto;
        }
        if let Some(url) = patch.vllm_base_url {
            settings.vllm_base_url = url;
        }
        if let Some(model) = patch.vllm_custom_model {
            settings.vllm_custom_model = model;
        }

        if cli_patch_applied {
            match Self::normalize_cli_model_config(&settings) {
                Some(cli_model) => {
                    settings.preferred_model =
                        Some(cli_model.backend.default_model_id().to_string());
                }
                None => {
                    if settings
                        .preferred_model
                        .as_deref()
                        .is_some_and(Self::is_cli_model_id)
                    {
                        settings.preferred_model = None;
                    }
                }
            }
        }

        self.apply_settings_to_runtime(&settings);

        if let Err(e) = self.memory.store().save_settings(&settings).await {
            error!("failed to persist settings: {e}");
        }
    }

    pub async fn load_persisted_settings(&self) {
        match self.memory.store().load_settings().await {
            Ok(Some(settings)) => {
                self.apply_settings_to_runtime(&settings);
                info!("restored persisted settings");
            }
            Ok(None) => {}
            Err(e) => {
                error!("failed to load persisted settings: {e}");
            }
        }
    }

    pub(super) fn apply_settings_to_runtime(&self, settings: &AppSettings) {
        self.router
            .set_preferred_model(settings.preferred_model.clone());

        for spec in self.router.catalog() {
            self.router.set_model_disabled(&spec.model_id, false);
        }
        for provider in Self::all_providers() {
            self.router.set_provider_disabled(provider, false);
        }

        self.router.set_vllm_config(
            settings.vllm_base_url.clone(),
            settings.vllm_custom_model.clone(),
        );

        let cli_model = Self::normalize_cli_model_config(settings);
        self.router.set_cli_model_config(cli_model.clone());
        if let Some(cli_model) = cli_model.as_ref() {
            self.router.apply_cli_model_bootstrap(cli_model);
            self.router.set_preferred_model(
                settings
                    .preferred_model
                    .clone()
                    .or_else(|| Some(cli_model.backend.default_model_id().to_string())),
            );
        }

        for model_id in &settings.disabled_models {
            self.router.set_model_disabled(model_id, true);
        }

        if !(settings.cli_model_enabled && settings.cli_model_only) {
            for name in &settings.disabled_providers {
                if let Ok(pk) =
                    serde_json::from_value::<ProviderKind>(serde_json::Value::String(name.clone()))
                {
                    self.router.set_provider_disabled(pk, true);
                }
            }
        }
    }

    fn normalize_cli_model_config(settings: &AppSettings) -> Option<CliModelConfig> {
        if !settings.cli_model_enabled {
            return None;
        }

        let backend = settings.cli_model_backend?;
        let command = if settings.cli_model_command.trim().is_empty() {
            backend.default_command().to_string()
        } else {
            settings.cli_model_command.clone()
        };

        Some(CliModelConfig {
            backend,
            command,
            args: settings.cli_model_args.clone(),
            timeout_ms: settings.cli_model_timeout_ms.max(1_000),
            cli_only: settings.cli_model_only,
        })
    }

    fn all_providers() -> [ProviderKind; 7] {
        [
            ProviderKind::OpenAi,
            ProviderKind::Anthropic,
            ProviderKind::Gemini,
            ProviderKind::Vllm,
            ProviderKind::ClaudeCode,
            ProviderKind::Codex,
            ProviderKind::Mock,
        ]
    }

    fn is_cli_model_id(model_id: &str) -> bool {
        matches!(model_id, "claude-code-cli" | "codex-cli")
    }
}
