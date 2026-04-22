use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::crypto::{DiscordEd25519Verifier, SignatureVerifier};
use crate::gateway::{
    GatewayAction, GatewayAdapter, GatewayCommand, GatewayManager, GatewayResponse,
    GatewayResponsePayload, MessageOrigin, Platform, parse_profile,
};
use crate::types::{
    RunRecord, RunStatus, RunSubmission, SessionSummary, TaskProfile, WorkflowTemplate,
};

// ---------------------------------------------------------------------------
// Status colors & emoji
// ---------------------------------------------------------------------------

fn status_color(status: &RunStatus) -> u32 {
    match status {
        RunStatus::Queued => 0x95a5a6,
        RunStatus::Running => 0x3498db,
        RunStatus::Cancelling => 0xe67e22,
        RunStatus::Succeeded => 0x2ecc71,
        RunStatus::Failed => 0xe74c3c,
        RunStatus::Cancelled => 0xe67e22,
        RunStatus::Paused => 0xf1c40f,
    }
}

fn status_emoji(status: &RunStatus) -> &'static str {
    match status {
        RunStatus::Queued => "\u{23f3}",
        RunStatus::Running => "\u{26a1}",
        RunStatus::Cancelling => "\u{1f6d1}",
        RunStatus::Succeeded => "\u{2705}",
        RunStatus::Failed => "\u{274c}",
        RunStatus::Cancelled => "\u{1f6ab}",
        RunStatus::Paused => "\u{23f8}\u{fe0f}",
    }
}

fn short_id(id: Uuid) -> String {
    id.to_string()[..8].to_string()
}

// ---------------------------------------------------------------------------
// Custom ID encoding for button components
// ---------------------------------------------------------------------------

fn encode_custom_id(action: &str, run_id: Uuid) -> String {
    format!("{action}:{run_id}")
}

fn decode_custom_id(custom_id: &str) -> Option<(String, Uuid)> {
    let (action, id_str) = custom_id.split_once(':')?;
    let run_id = Uuid::parse_str(id_str).ok()?;
    Some((action.to_string(), run_id))
}

// ---------------------------------------------------------------------------
// Action buttons based on run status
// ---------------------------------------------------------------------------

fn build_action_buttons(run_id: Uuid, status: &RunStatus) -> serde_json::Value {
    let mut buttons = vec![serde_json::json!({
        "type": 2,
        "style": 2,
        "label": "Refresh",
        "custom_id": encode_custom_id("refresh", run_id),
        "emoji": {"name": "\u{1f504}"}
    })];

    match status {
        RunStatus::Queued | RunStatus::Running => {
            buttons.push(serde_json::json!({
                "type": 2, "style": 4, "label": "Cancel",
                "custom_id": encode_custom_id("cancel", run_id),
            }));
            buttons.push(serde_json::json!({
                "type": 2, "style": 1, "label": "Pause",
                "custom_id": encode_custom_id("pause", run_id),
            }));
        }
        RunStatus::Paused => {
            buttons.push(serde_json::json!({
                "type": 2, "style": 3, "label": "Resume",
                "custom_id": encode_custom_id("resume", run_id),
            }));
            buttons.push(serde_json::json!({
                "type": 2, "style": 4, "label": "Cancel",
                "custom_id": encode_custom_id("cancel", run_id),
            }));
        }
        RunStatus::Failed | RunStatus::Cancelled => {
            buttons.push(serde_json::json!({
                "type": 2, "style": 1, "label": "Retry",
                "custom_id": encode_custom_id("retry", run_id),
            }));
        }
        RunStatus::Succeeded => {
            buttons.push(serde_json::json!({
                "type": 2, "style": 1, "label": "Results",
                "custom_id": encode_custom_id("results", run_id),
            }));
        }
        RunStatus::Cancelling => {}
    }

    serde_json::json!([{ "type": 1, "components": buttons }])
}

// ---------------------------------------------------------------------------
// Embed builders
// ---------------------------------------------------------------------------

fn build_run_submitted_embed(sub: &RunSubmission) -> serde_json::Value {
    serde_json::json!({
        "embeds": [{
            "title": format!("{} Run Submitted", status_emoji(&sub.status)),
            "color": status_color(&sub.status),
            "fields": [
                {"name": "Run ID", "value": format!("`{}`", short_id(sub.run_id)), "inline": true},
                {"name": "Session", "value": format!("`{}`", short_id(sub.session_id)), "inline": true},
                {"name": "Status", "value": format!("{} {}", status_emoji(&sub.status), sub.status), "inline": true}
            ],
            "footer": {"text": format!("{}", sub.run_id)}
        }],
        "components": build_action_buttons(sub.run_id, &sub.status)
    })
}

fn build_run_details_embed(run: &RunRecord) -> serde_json::Value {
    let task_preview: String = run.task.chars().take(200).collect();
    let mut fields = vec![
        serde_json::json!({"name": "Status", "value": format!("{} {}", status_emoji(&run.status), run.status), "inline": true}),
        serde_json::json!({"name": "Profile", "value": format!("`{}`", run.profile), "inline": true}),
        serde_json::json!({"name": "Session", "value": format!("`{}`", short_id(run.session_id)), "inline": true}),
    ];

    if let Some(ref err) = run.error {
        let err_preview: String = err.chars().take(300).collect();
        fields.push(
            serde_json::json!({"name": "Error", "value": format!("```\n{err_preview}\n```")}),
        );
    }

    if !run.outputs.is_empty() {
        let outputs: String = run
            .outputs
            .iter()
            .take(8)
            .map(|o| {
                let mark = if o.succeeded { "\u{2705}" } else { "\u{274c}" };
                format!(
                    "{mark} `{nid}` **{role}** {dur}ms",
                    nid = o.node_id,
                    role = o.role,
                    dur = o.duration_ms,
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        fields.push(serde_json::json!({"name": "Agent Outputs", "value": outputs}));
    }

    if let (Some(start), Some(end)) = (run.started_at, run.finished_at) {
        let dur = (end - start).num_milliseconds();
        fields.push(
            serde_json::json!({"name": "Duration", "value": format!("{dur}ms"), "inline": true}),
        );
    }

    serde_json::json!({
        "embeds": [{
            "title": format!("{} Run {}", status_emoji(&run.status), short_id(run.run_id)),
            "description": task_preview,
            "color": status_color(&run.status),
            "fields": fields,
            "footer": {"text": format!("{}", run.run_id)},
            "timestamp": run.created_at.to_rfc3339()
        }],
        "components": build_action_buttons(run.run_id, &run.status)
    })
}

fn build_run_action_embed(run_id: Uuid, action: &str, success: bool) -> serde_json::Value {
    let (color, mark) = if success {
        (0x2ecc71, "\u{2705}")
    } else {
        (0xe74c3c, "\u{274c}")
    };
    serde_json::json!({
        "embeds": [{
            "title": format!("{mark} Run Action"),
            "description": format!("`{action}` on run `{}`", short_id(run_id)),
            "color": color,
            "fields": [
                {"name": "Result", "value": if success { "Success" } else { "Failed" }, "inline": true}
            ],
            "footer": {"text": format!("{run_id}")}
        }]
    })
}

fn build_run_list_embed(runs: &[RunRecord]) -> serde_json::Value {
    let desc = if runs.is_empty() {
        "No runs found.".to_string()
    } else {
        runs.iter()
            .take(15)
            .map(|r| {
                let task_preview: String = r.task.chars().take(40).collect();
                format!(
                    "{} `{}` **{}** {}",
                    status_emoji(&r.status),
                    short_id(r.run_id),
                    r.status,
                    task_preview,
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    serde_json::json!({
        "embeds": [{
            "title": format!("Recent Runs ({})", runs.len()),
            "description": desc,
            "color": 0x3498db,
        }]
    })
}

fn build_session_list_embed(sessions: &[SessionSummary]) -> serde_json::Value {
    let desc = if sessions.is_empty() {
        "No sessions found.".to_string()
    } else {
        sessions
            .iter()
            .take(15)
            .map(|s| {
                let task = s
                    .last_task
                    .as_deref()
                    .map(|t| t.chars().take(40).collect::<String>())
                    .unwrap_or_default();
                format!(
                    "`{}` \u{2022} **{}** runs \u{2022} {}",
                    short_id(s.session_id),
                    s.run_count,
                    task,
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    serde_json::json!({
        "embeds": [{
            "title": format!("Sessions ({})", sessions.len()),
            "description": desc,
            "color": 0x3498db,
        }]
    })
}

fn build_help_embed(text: &str) -> serde_json::Value {
    serde_json::json!({
        "embeds": [{
            "title": "Agent Orchestrator \u{2014} Help",
            "description": format!("```\n{text}\n```"),
            "color": 0x3498db,
            "footer": {"text": "Use slash commands or buttons to interact"}
        }]
    })
}

fn build_error_embed(err: &str) -> serde_json::Value {
    serde_json::json!({
        "embeds": [{
            "title": "\u{274c} Error",
            "description": err,
            "color": 0xe74c3c,
        }]
    })
}

fn build_workflow_list_embed(workflows: &[WorkflowTemplate]) -> serde_json::Value {
    let desc = if workflows.is_empty() {
        "No workflows found.".to_string()
    } else {
        workflows
            .iter()
            .take(15)
            .map(|w| {
                let nodes = w.graph_template.nodes.len();
                let params = w.parameters.len();
                format!(
                    "**{}** \u{2014} {}\n> {} nodes \u{2022} {} params",
                    w.name, w.description, nodes, params,
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    };

    serde_json::json!({
        "embeds": [{
            "title": format!("Workflows ({})", workflows.len()),
            "description": desc,
            "color": 0x9b59b6,
            "footer": {"text": "Use /agent-workflow-run <id> to execute"}
        }]
    })
}

fn build_run_results_embed(run: &RunRecord) -> serde_json::Value {
    let mut fields: Vec<serde_json::Value> = Vec::new();

    for output in run.outputs.iter().take(5) {
        let mark = if output.succeeded {
            "\u{2705}"
        } else {
            "\u{274c}"
        };
        let name = format!("{mark} {} [{}]", output.node_id, output.role);
        let mut value = output.output.chars().take(500).collect::<String>();
        if output.output.len() > 500 {
            value.push_str("...");
        }
        if value.is_empty() {
            value = "(no output)".to_string();
        }
        // Wrap in code block, respecting 1024-char field limit
        let formatted = if value.len() + 8 > 1024 {
            value.truncate(1014);
            format!("```\n{value}...\n```")
        } else {
            format!("```\n{value}\n```")
        };
        fields.push(serde_json::json!({"name": name, "value": formatted}));
    }

    if fields.is_empty() {
        fields.push(serde_json::json!({"name": "Outputs", "value": "No agent outputs yet."}));
    }

    serde_json::json!({
        "embeds": [{
            "title": format!("Results: Run {}", short_id(run.run_id)),
            "color": status_color(&run.status),
            "fields": fields,
            "footer": {"text": format!("Showing up to 5 agent outputs \u{2022} {}", run.run_id)}
        }],
        "components": build_action_buttons(run.run_id, &run.status)
    })
}

fn build_progress_embed(run: &RunRecord) -> serde_json::Value {
    let total = run.outputs.len()
        + if run.status == RunStatus::Running {
            1
        } else {
            0
        };
    let completed = run.outputs.iter().filter(|o| o.succeeded).count();
    let failed = run.outputs.iter().filter(|o| !o.succeeded).count();

    let mut fields = vec![
        serde_json::json!({"name": "Status", "value": format!("{} {}", status_emoji(&run.status), run.status), "inline": true}),
        serde_json::json!({"name": "Progress", "value": format!("{completed}\u{2705} {failed}\u{274c} / {total} nodes"), "inline": true}),
        serde_json::json!({"name": "Profile", "value": format!("`{}`", run.profile), "inline": true}),
    ];

    if !run.outputs.is_empty() {
        let recent: String = run
            .outputs
            .iter()
            .rev()
            .take(5)
            .map(|o| {
                let mark = if o.succeeded { "\u{2705}" } else { "\u{274c}" };
                format!(
                    "{mark} `{}` **{}** {:.0}ms",
                    o.node_id, o.role, o.duration_ms
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        fields.push(serde_json::json!({"name": "Recent Activity", "value": recent}));
    }

    let task_preview: String = run.task.chars().take(100).collect();

    serde_json::json!({
        "embeds": [{
            "title": format!("\u{26a1} Run {} \u{2014} In Progress", short_id(run.run_id)),
            "description": task_preview,
            "color": 0x3498db,
            "fields": fields,
            "footer": {"text": format!("Auto-updating... \u{2022} {}", run.run_id)}
        }],
        "components": build_action_buttons(run.run_id, &run.status)
    })
}

// ---------------------------------------------------------------------------
// Main dispatch: GatewayResponsePayload -> Discord embed JSON
// ---------------------------------------------------------------------------

fn format_discord_embed(payload: &GatewayResponsePayload) -> serde_json::Value {
    match payload {
        GatewayResponsePayload::RunSubmitted(sub) => build_run_submitted_embed(sub),
        GatewayResponsePayload::RunDetails(run) => build_run_details_embed(run),
        GatewayResponsePayload::RunAction {
            run_id,
            action,
            success,
        } => build_run_action_embed(*run_id, action, *success),
        GatewayResponsePayload::RunList(runs) => build_run_list_embed(runs),
        GatewayResponsePayload::SessionList(sessions) => build_session_list_embed(sessions),
        GatewayResponsePayload::WorkflowList(workflows) => build_workflow_list_embed(workflows),
        GatewayResponsePayload::HelpText(text) => build_help_embed(text),
        GatewayResponsePayload::Error(err) => build_error_embed(err),
    }
}

// ---------------------------------------------------------------------------
// Discord-specific live polling with embed updates
// ---------------------------------------------------------------------------

async fn discord_poll_and_update(
    manager: Arc<GatewayManager>,
    adapter: Arc<DiscordAdapter>,
    run_id: Uuid,
    origin: MessageOrigin,
    interaction_token: String,
) {
    let mut last_output_count = 0usize;
    let mut tick = 0u32;
    let mut token_expired = false;

    loop {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        tick += 1;

        match manager.orchestrator.get_run(run_id).await {
            Ok(Some(run)) => {
                let is_terminal = run.status.is_terminal();
                let output_count = run.outputs.len();

                if is_terminal || tick % 3 == 0 || output_count != last_output_count {
                    let embed = if is_terminal {
                        build_run_details_embed(&run)
                    } else {
                        build_progress_embed(&run)
                    };

                    if !token_expired {
                        match adapter
                            .edit_original_response(&interaction_token, &embed)
                            .await
                        {
                            Ok(()) => {}
                            Err(err) => {
                                warn!("discord progress edit failed for {run_id}: {err}");
                                token_expired = true;
                            }
                        }
                    }

                    // Fallback: send as channel message if token expired and terminal
                    if token_expired && is_terminal {
                        let embed = build_run_details_embed(&run);
                        if let Err(err) = adapter
                            .send_channel_message(&origin.channel_id, &embed)
                            .await
                        {
                            error!("discord fallback channel message failed for {run_id}: {err}");
                        }
                    }

                    last_output_count = output_count;
                }

                if is_terminal {
                    break;
                }
            }
            Ok(None) => {
                error!("run {run_id} not found during discord poll");
                break;
            }
            Err(err) => {
                error!("error polling run {run_id}: {err}");
                break;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Config & adapter
// ---------------------------------------------------------------------------

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
        DiscordEd25519Verifier::new(self.config.public_key.clone()).verify(headers, body)
    }

    async fn edit_original_response(
        &self,
        interaction_token: &str,
        payload: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let url = format!(
            "https://discord.com/api/v10/webhooks/{}/{}/messages/@original",
            self.config.application_id, interaction_token
        );
        let resp = self
            .client
            .patch(&url)
            .header("Authorization", format!("Bot {}", self.config.bot_token))
            .json(payload)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("edit_original_response failed: {status} {body}");
        }
        Ok(())
    }

    async fn send_channel_message(
        &self,
        channel_id: &str,
        payload: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let url = format!(
            "https://discord.com/api/v10/channels/{}/messages",
            channel_id
        );
        self.client
            .post(&url)
            .header("Authorization", format!("Bot {}", self.config.bot_token))
            .json(payload)
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
                "name": "agent-workflows",
                "description": "List saved workflows",
                "options": [
                    {"type": 4, "name": "limit", "description": "Number of workflows to show"}
                ]
            },
            {
                "name": "agent-workflow-run",
                "description": "Execute a workflow by name",
                "options": [
                    {"type": 3, "name": "workflow_id", "description": "Workflow ID to execute", "required": true},
                    {"type": 3, "name": "params", "description": "JSON parameters (e.g. {\"key\":\"value\"})"}
                ]
            },
            {
                "name": "agent-results",
                "description": "Show detailed agent outputs for a run",
                "options": [
                    {"type": 3, "name": "run_id", "description": "Run ID", "required": true}
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

// ---------------------------------------------------------------------------
// GatewayAdapter impl
// ---------------------------------------------------------------------------

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
        let payload = format_discord_embed(&response.payload);
        self.send_channel_message(&response.origin.channel_id, &payload)
            .await
    }
}

// ---------------------------------------------------------------------------
// Interaction handler
// ---------------------------------------------------------------------------

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

    let interaction: serde_json::Value = match serde_json::from_slice(body.as_ref()) {
        Ok(v) => v,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": err.to_string()})),
            );
        }
    };

    let interaction_type = interaction
        .get("type")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    // Type 1: PING
    if interaction_type == 1 {
        return (StatusCode::OK, Json(serde_json::json!({"type": 1})));
    }

    // Type 2: APPLICATION_COMMAND
    if interaction_type == 2 {
        return handle_application_command(&state, &interaction);
    }

    // Type 3: MESSAGE_COMPONENT (button clicks)
    if interaction_type == 3 {
        return handle_message_component(&state, &interaction);
    }

    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({"error": "unknown interaction type"})),
    )
}

// ---------------------------------------------------------------------------
// Type 2: slash command handler
// ---------------------------------------------------------------------------

fn handle_application_command(
    state: &DiscordState,
    interaction: &serde_json::Value,
) -> (StatusCode, Json<serde_json::Value>) {
    let data = interaction.get("data");
    let command_name = data
        .and_then(|d| d.get("name"))
        .and_then(|n| n.as_str())
        .unwrap_or("")
        .to_string();

    let channel_id = interaction
        .get("channel_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let user_id = interaction
        .get("member")
        .and_then(|m| m.get("user"))
        .or_else(|| interaction.get("user"))
        .and_then(|u| u.get("id"))
        .and_then(|i| i.as_str())
        .unwrap_or("unknown")
        .to_string();

    let interaction_token = interaction
        .get("token")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let origin = MessageOrigin {
        platform: Platform::Discord,
        channel_id,
        thread_id: None,
        user_id,
    };

    let action = parse_discord_command(&command_name, data.unwrap_or(&serde_json::Value::Null));
    let cmd = GatewayCommand {
        action: action.clone(),
        origin: origin.clone(),
        session_id: None,
    };

    let is_run = matches!(
        action,
        GatewayAction::SubmitRun { .. } | GatewayAction::ExecuteWorkflow { .. }
    );
    let wants_results = command_name == "agent-results";

    let manager = state.manager.clone();
    let adapter = state.adapter.clone();
    let token_for_poll = interaction_token.clone();

    tokio::spawn(async move {
        let response = manager.handle_command(cmd).await;

        let embed = if wants_results {
            if let GatewayResponsePayload::RunDetails(ref run) = response.payload {
                build_run_results_embed(run)
            } else {
                format_discord_embed(&response.payload)
            }
        } else {
            format_discord_embed(&response.payload)
        };

        if let Err(err) = adapter
            .edit_original_response(&interaction_token, &embed)
            .await
        {
            error!("discord edit_original_response failed: {err}");
        }

        if is_run {
            if let GatewayResponsePayload::RunSubmitted(ref sub) = response.payload {
                discord_poll_and_update(manager, adapter, sub.run_id, origin, token_for_poll).await;
            }
        }
    });

    // Type 5: DEFERRED_CHANNEL_MESSAGE_WITH_SOURCE
    (StatusCode::OK, Json(serde_json::json!({"type": 5})))
}

// ---------------------------------------------------------------------------
// Type 3: button interaction handler
// ---------------------------------------------------------------------------

fn handle_message_component(
    state: &DiscordState,
    interaction: &serde_json::Value,
) -> (StatusCode, Json<serde_json::Value>) {
    let custom_id = interaction
        .get("data")
        .and_then(|d| d.get("custom_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let interaction_token = interaction
        .get("token")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let channel_id = interaction
        .get("channel_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let user_id = interaction
        .get("member")
        .and_then(|m| m.get("user"))
        .or_else(|| interaction.get("user"))
        .and_then(|u| u.get("id"))
        .and_then(|i| i.as_str())
        .unwrap_or("unknown")
        .to_string();

    let Some((action_str, run_id)) = decode_custom_id(custom_id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "invalid custom_id"})),
        );
    };

    let origin = MessageOrigin {
        platform: Platform::Discord,
        channel_id,
        thread_id: None,
        user_id,
    };

    let manager = state.manager.clone();
    let adapter = state.adapter.clone();

    tokio::spawn(async move {
        // Execute action if it's not just a refresh
        let gateway_action = match action_str.as_str() {
            "cancel" => Some(GatewayAction::CancelRun { run_id }),
            "pause" => Some(GatewayAction::PauseRun { run_id }),
            "resume" => Some(GatewayAction::ResumeRun { run_id }),
            "retry" => Some(GatewayAction::RetryRun { run_id }),
            _ => None, // refresh, results -> just re-fetch
        };

        let mut new_run_id: Option<Uuid> = None;

        if let Some(action) = gateway_action {
            let cmd = GatewayCommand {
                action,
                origin: origin.clone(),
                session_id: None,
            };
            let response = manager.handle_command(cmd).await;

            // Capture new run_id from retry
            if action_str == "retry" {
                if let GatewayResponsePayload::RunSubmitted(ref sub) = response.payload {
                    new_run_id = Some(sub.run_id);
                }
            }
        }

        // Build updated embed
        let embed = if action_str == "results" {
            match manager.orchestrator.get_run(run_id).await {
                Ok(Some(run)) => build_run_results_embed(&run),
                _ => build_error_embed("Run not found"),
            }
        } else if let Some(new_id) = new_run_id {
            // Retry: show the new run's submitted embed
            match manager.orchestrator.get_run(new_id).await {
                Ok(Some(run)) => build_run_details_embed(&run),
                _ => build_run_action_embed(run_id, "retry", true),
            }
        } else {
            match manager.orchestrator.get_run(run_id).await {
                Ok(Some(run)) => build_run_details_embed(&run),
                _ => build_error_embed("Run not found"),
            }
        };

        if let Err(err) = adapter
            .edit_original_response(&interaction_token, &embed)
            .await
        {
            error!("discord component edit failed: {err}");
        }

        // If retry created a new run, start polling for it
        if let Some(new_id) = new_run_id {
            discord_poll_and_update(manager, adapter, new_id, origin, interaction_token).await;
        }
    });

    // Type 6: DEFERRED_UPDATE_MESSAGE
    (StatusCode::OK, Json(serde_json::json!({"type": 6})))
}

// ---------------------------------------------------------------------------
// Slash command parser
// ---------------------------------------------------------------------------

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
        "agent-workflows" => {
            let limit = get_int("limit").unwrap_or(20).clamp(1, 50);
            GatewayAction::ListWorkflows { limit }
        }
        "agent-workflow-run" => {
            let workflow_id = get_str("workflow_id").unwrap_or_default();
            let params =
                get_str("params").and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
            GatewayAction::ExecuteWorkflow {
                workflow_id,
                params,
            }
        }
        "agent-results" => {
            let run_id = get_str("run_id")
                .and_then(|s| Uuid::parse_str(&s).ok())
                .unwrap_or_default();
            GatewayAction::GetRun { run_id }
        }
        "agent-help" | _ => GatewayAction::Help,
    }
}
