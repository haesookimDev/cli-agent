use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::gateway::{
    GatewayAction, GatewayAdapter, GatewayCommand, GatewayManager, GatewayResponse,
    GatewayResponsePayload, MessageOrigin, Platform, parse_profile, poll_and_deliver,
};
use crate::types::TaskProfile;

#[derive(Debug, Clone)]
pub struct DiscordConfig {
    pub bot_token: String,
    pub application_id: String,
    pub public_key: String,
}

impl DiscordConfig {
    pub fn from_env() -> Option<Self> {
        let bot_token = std::env::var("DISCORD_BOT_TOKEN").ok()?;
        let application_id = std::env::var("DISCORD_APPLICATION_ID").ok()?;
        let public_key = std::env::var("DISCORD_PUBLIC_KEY").ok()?;
        Some(Self {
            bot_token,
            application_id,
            public_key,
        })
    }
}

pub struct DiscordAdapter {
    config: DiscordConfig,
    client: reqwest::Client,
}

impl DiscordAdapter {
    pub fn new(config: DiscordConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }

    fn verify_discord_signature(&self, headers: &HeaderMap, body: &[u8]) -> anyhow::Result<()> {
        let signature_hex = headers
            .get("X-Signature-Ed25519")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| anyhow::anyhow!("missing X-Signature-Ed25519"))?;

        let timestamp = headers
            .get("X-Signature-Timestamp")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| anyhow::anyhow!("missing X-Signature-Timestamp"))?;

        let public_key_bytes = hex::decode(&self.config.public_key)
            .map_err(|_| anyhow::anyhow!("invalid public key hex"))?;

        let verifying_key = VerifyingKey::from_bytes(
            public_key_bytes
                .as_slice()
                .try_into()
                .map_err(|_| anyhow::anyhow!("public key must be 32 bytes"))?,
        )
        .map_err(|_| anyhow::anyhow!("invalid Ed25519 public key"))?;

        let signature_bytes =
            hex::decode(signature_hex).map_err(|_| anyhow::anyhow!("invalid signature hex"))?;

        let signature = Signature::from_bytes(
            signature_bytes
                .as_slice()
                .try_into()
                .map_err(|_| anyhow::anyhow!("signature must be 64 bytes"))?,
        );

        let mut message = Vec::with_capacity(timestamp.len() + body.len());
        message.extend_from_slice(timestamp.as_bytes());
        message.extend_from_slice(body);

        verifying_key
            .verify(&message, &signature)
            .map_err(|_| anyhow::anyhow!("Ed25519 signature verification failed"))
    }

    async fn edit_original_response(
        &self,
        interaction_token: &str,
        content: &str,
    ) -> anyhow::Result<()> {
        let url = format!(
            "https://discord.com/api/v10/webhooks/{}/{}/messages/@original",
            self.config.application_id, interaction_token
        );
        let resp = self
            .client
            .patch(&url)
            .header("Authorization", format!("Bot {}", self.config.bot_token))
            .json(&serde_json::json!({ "content": content }))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            warn!("discord edit_original_response failed: {status} {body}");
        }
        Ok(())
    }

    async fn send_channel_message(&self, channel_id: &str, content: &str) -> anyhow::Result<()> {
        let url = format!(
            "https://discord.com/api/v10/channels/{}/messages",
            channel_id
        );
        self.client
            .post(&url)
            .header("Authorization", format!("Bot {}", self.config.bot_token))
            .json(&serde_json::json!({ "content": content }))
            .send()
            .await?;
        Ok(())
    }

    async fn register_global_commands(&self) -> anyhow::Result<()> {
        let url = format!(
            "https://discord.com/api/v10/applications/{}/commands",
            self.config.application_id
        );

        let commands = serde_json::json!([
            {
                "name": "agent-run",
                "description": "Submit a task to the agent orchestrator",
                "options": [
                    {"type": 3, "name": "task", "description": "The task to execute", "required": true},
                    {"type": 3, "name": "profile", "description": "Task profile",
                     "choices": [
                       {"name": "General", "value": "general"},
                       {"name": "Planning", "value": "planning"},
                       {"name": "Extraction", "value": "extraction"},
                       {"name": "Coding", "value": "coding"}
                     ]}
                ]
            },
            {
                "name": "agent-status",
                "description": "Get the status of a run",
                "options": [
                    {"type": 3, "name": "run_id", "description": "Run ID", "required": true}
                ]
            },
            {
                "name": "agent-cancel",
                "description": "Cancel a run",
                "options": [
                    {"type": 3, "name": "run_id", "description": "Run ID", "required": true}
                ]
            },
            {
                "name": "agent-pause",
                "description": "Pause a run",
                "options": [
                    {"type": 3, "name": "run_id", "description": "Run ID", "required": true}
                ]
            },
            {
                "name": "agent-resume",
                "description": "Resume a paused run",
                "options": [
                    {"type": 3, "name": "run_id", "description": "Run ID", "required": true}
                ]
            },
            {
                "name": "agent-retry",
                "description": "Retry a run",
                "options": [
                    {"type": 3, "name": "run_id", "description": "Run ID", "required": true}
                ]
            },
            {
                "name": "agent-runs",
                "description": "List recent runs",
                "options": [
                    {"type": 4, "name": "limit", "description": "Number of runs to show"}
                ]
            },
            {
                "name": "agent-sessions",
                "description": "List sessions",
                "options": [
                    {"type": 4, "name": "limit", "description": "Number of sessions to show"}
                ]
            },
            {
                "name": "agent-help",
                "description": "Show help for agent commands"
            }
        ]);

        let resp = self
            .client
            .put(&url)
            .header("Authorization", format!("Bot {}", self.config.bot_token))
            .json(&commands)
            .send()
            .await?;

        if resp.status().is_success() {
            info!("Discord global commands registered successfully");
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            warn!("Discord command registration failed: {status} {body}");
        }
        Ok(())
    }
}

#[derive(Clone)]
struct DiscordState {
    manager: Arc<GatewayManager>,
    adapter: Arc<DiscordAdapter>,
}

#[async_trait]
impl GatewayAdapter for DiscordAdapter {
    fn platform(&self) -> Platform {
        Platform::Discord
    }

    fn routes(&self, manager: Arc<GatewayManager>) -> Router {
        let adapter = Arc::new(DiscordAdapter::new(self.config.clone()));
        let state = DiscordState {
            manager,
            adapter: adapter.clone(),
        };
        Router::new()
            .route("/gateway/discord/interactions", post(interactions_handler))
            .with_state(state)
    }

    async fn start_background(&self, _manager: Arc<GatewayManager>) -> anyhow::Result<()> {
        info!("Discord adapter: registering global slash commands");
        self.register_global_commands().await?;
        info!("Discord adapter ready");
        Ok(())
    }

    async fn deliver_response(&self, response: GatewayResponse) -> anyhow::Result<()> {
        let text = format_discord_response(&response.payload);
        self.send_channel_message(&response.origin.channel_id, &text)
            .await
    }
}

async fn interactions_handler(
    State(state): State<DiscordState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(err) = state
        .adapter
        .verify_discord_signature(&headers, body.as_ref())
    {
        warn!("discord signature verification failed: {err}");
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "invalid signature"})),
        );
    }

    let payload: serde_json::Value = match serde_json::from_slice(body.as_ref()) {
        Ok(v) => v,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": err.to_string()})),
            );
        }
    };

    let interaction_type = payload.get("type").and_then(|v| v.as_u64()).unwrap_or(0);

    // Type 1: PING
    if interaction_type == 1 {
        return (StatusCode::OK, Json(serde_json::json!({"type": 1})));
    }

    // Type 2: APPLICATION_COMMAND
    if interaction_type == 2 {
        let data = payload.get("data");
        let command_name = data
            .and_then(|d| d.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("");

        let channel_id = payload
            .get("channel_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let user_id = payload
            .get("member")
            .and_then(|m| m.get("user"))
            .or_else(|| payload.get("user"))
            .and_then(|u| u.get("id"))
            .and_then(|i| i.as_str())
            .unwrap_or("unknown")
            .to_string();

        let interaction_token = payload
            .get("token")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let origin = MessageOrigin {
            platform: Platform::Discord,
            channel_id: channel_id.clone(),
            thread_id: None,
            user_id,
        };

        let action = parse_discord_command(command_name, data.unwrap_or(&serde_json::Value::Null));
        let cmd = GatewayCommand {
            action: action.clone(),
            origin: origin.clone(),
            session_id: None,
        };
        let is_run = matches!(action, GatewayAction::SubmitRun { .. });

        let manager = state.manager.clone();
        let adapter = state.adapter.clone();

        tokio::spawn(async move {
            let response = manager.handle_command(cmd).await;
            let formatted = format_discord_response(&response.payload);

            if let Err(err) = adapter
                .edit_original_response(&interaction_token, &formatted)
                .await
            {
                error!("discord edit_original_response failed: {err}");
            }

            if is_run {
                if let GatewayResponsePayload::RunSubmitted(ref sub) = response.payload {
                    poll_and_deliver(
                        manager,
                        adapter as Arc<dyn GatewayAdapter>,
                        sub.run_id,
                        origin,
                    )
                    .await;
                }
            }
        });

        // Type 5: DEFERRED_CHANNEL_MESSAGE_WITH_SOURCE
        return (StatusCode::OK, Json(serde_json::json!({"type": 5})));
    }

    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({"error": "unknown interaction type"})),
    )
}

fn parse_discord_command(name: &str, data: &serde_json::Value) -> GatewayAction {
    let get_str = |opt_name: &str| -> Option<String> {
        data.get("options")
            .and_then(|v| v.as_array())
            .and_then(|arr| {
                arr.iter()
                    .find(|o| o.get("name").and_then(|n| n.as_str()) == Some(opt_name))
                    .and_then(|o| o.get("value").and_then(|v| v.as_str()))
                    .map(|s| s.to_string())
            })
    };

    let get_int = |opt_name: &str| -> Option<usize> {
        data.get("options")
            .and_then(|v| v.as_array())
            .and_then(|arr| {
                arr.iter()
                    .find(|o| o.get("name").and_then(|n| n.as_str()) == Some(opt_name))
                    .and_then(|o| o.get("value").and_then(|v| v.as_u64()))
                    .map(|v| v as usize)
            })
    };

    match name {
        "agent-run" => {
            let task = get_str("task").unwrap_or_default();
            let profile = get_str("profile")
                .map(|p| parse_profile(&p))
                .unwrap_or(TaskProfile::General);
            GatewayAction::SubmitRun { task, profile }
        }
        "agent-status" => {
            let run_id = get_str("run_id")
                .and_then(|s| Uuid::parse_str(&s).ok())
                .unwrap_or_default();
            GatewayAction::GetRun { run_id }
        }
        "agent-cancel" => {
            let run_id = get_str("run_id")
                .and_then(|s| Uuid::parse_str(&s).ok())
                .unwrap_or_default();
            GatewayAction::CancelRun { run_id }
        }
        "agent-pause" => {
            let run_id = get_str("run_id")
                .and_then(|s| Uuid::parse_str(&s).ok())
                .unwrap_or_default();
            GatewayAction::PauseRun { run_id }
        }
        "agent-resume" => {
            let run_id = get_str("run_id")
                .and_then(|s| Uuid::parse_str(&s).ok())
                .unwrap_or_default();
            GatewayAction::ResumeRun { run_id }
        }
        "agent-retry" => {
            let run_id = get_str("run_id")
                .and_then(|s| Uuid::parse_str(&s).ok())
                .unwrap_or_default();
            GatewayAction::RetryRun { run_id }
        }
        "agent-runs" => {
            let limit = get_int("limit").unwrap_or(20).clamp(1, 100);
            GatewayAction::ListRuns {
                session_id: None,
                limit,
            }
        }
        "agent-sessions" => {
            let limit = get_int("limit").unwrap_or(20).clamp(1, 100);
            GatewayAction::ListSessions { limit }
        }
        "agent-help" | _ => GatewayAction::Help,
    }
}

fn format_discord_response(payload: &GatewayResponsePayload) -> String {
    match payload {
        GatewayResponsePayload::RunSubmitted(sub) => {
            format!(
                "**Run submitted**\n`run_id: {}`\n`session: {}`\n`status: {}`",
                sub.run_id, sub.session_id, sub.status
            )
        }
        GatewayResponsePayload::RunDetails(run) => {
            let outputs = run
                .outputs
                .iter()
                .take(5)
                .map(|o| {
                    format!(
                        "- `{}` [{}] model=`{}` {}ms {}",
                        o.node_id,
                        o.role,
                        o.model,
                        o.duration_ms,
                        if o.succeeded { "ok" } else { "FAILED" }
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "**Run {}**\ntask: {}\nstatus: `{}`\nprofile: `{}`\n{}",
                &run.run_id.to_string()[..8],
                run.task.chars().take(100).collect::<String>(),
                run.status,
                run.profile,
                if outputs.is_empty() {
                    "no outputs yet".to_string()
                } else {
                    format!("outputs:\n{}", outputs)
                }
            )
        }
        GatewayResponsePayload::RunAction {
            run_id,
            action,
            success,
        } => {
            let mark = if *success { "+" } else { "x" };
            format!("[{}] `{}` on run `{}`", mark, action, run_id)
        }
        GatewayResponsePayload::RunList(runs) => {
            let lines: Vec<String> = runs
                .iter()
                .take(10)
                .map(|r| {
                    format!(
                        "- `{}` [{}] {}",
                        &r.run_id.to_string()[..8],
                        r.status,
                        r.task.chars().take(50).collect::<String>()
                    )
                })
                .collect();
            format!("**Recent Runs** ({})\n{}", runs.len(), lines.join("\n"))
        }
        GatewayResponsePayload::SessionList(sessions) => {
            let lines: Vec<String> = sessions
                .iter()
                .take(10)
                .map(|s| {
                    format!(
                        "- `{}` runs={}",
                        &s.session_id.to_string()[..8],
                        s.run_count
                    )
                })
                .collect();
            format!("**Sessions** ({})\n{}", sessions.len(), lines.join("\n"))
        }
        GatewayResponsePayload::HelpText(text) => format!("```\n{}\n```", text),
        GatewayResponsePayload::Error(err) => format!("Error: {}", err),
    }
}
