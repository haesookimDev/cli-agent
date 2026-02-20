use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use tracing::{error, info, warn};

use crate::gateway::{
    parse_gateway_text, poll_and_deliver, GatewayAdapter, GatewayManager, GatewayResponse,
    GatewayResponsePayload, MessageOrigin, Platform,
};

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone)]
pub struct SlackConfig {
    pub bot_token: String,
    pub signing_secret: String,
}

impl SlackConfig {
    pub fn from_env() -> Option<Self> {
        let bot_token = std::env::var("SLACK_BOT_TOKEN").ok()?;
        let signing_secret = std::env::var("SLACK_SIGNING_SECRET").ok()?;
        Some(Self {
            bot_token,
            signing_secret,
        })
    }
}

pub struct SlackAdapter {
    config: SlackConfig,
    client: reqwest::Client,
}

impl SlackAdapter {
    pub fn new(config: SlackConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }

    fn verify_slack_signature(&self, headers: &HeaderMap, body: &[u8]) -> anyhow::Result<()> {
        let timestamp = headers
            .get("X-Slack-Request-Timestamp")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| anyhow::anyhow!("missing X-Slack-Request-Timestamp"))?;

        let signature = headers
            .get("X-Slack-Signature")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| anyhow::anyhow!("missing X-Slack-Signature"))?;

        let now = chrono::Utc::now().timestamp();
        let ts: i64 = timestamp
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid timestamp"))?;
        if (now - ts).abs() > 300 {
            return Err(anyhow::anyhow!("timestamp too old"));
        }

        let sig_basestring = format!("v0:{}:{}", timestamp, String::from_utf8_lossy(body));
        let mut mac = HmacSha256::new_from_slice(self.config.signing_secret.as_bytes())?;
        mac.update(sig_basestring.as_bytes());
        let result = mac.finalize().into_bytes();
        let expected = format!(
            "v0={}",
            result
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect::<String>()
        );

        if !constant_time_eq(expected.as_bytes(), signature.as_bytes()) {
            return Err(anyhow::anyhow!("signature mismatch"));
        }

        Ok(())
    }

    async fn post_message(
        &self,
        channel: &str,
        thread_ts: Option<&str>,
        text: &str,
    ) -> anyhow::Result<()> {
        let mut body = serde_json::json!({
            "channel": channel,
            "text": text,
        });
        if let Some(ts) = thread_ts {
            body["thread_ts"] = serde_json::Value::String(ts.to_string());
        }
        let resp = self
            .client
            .post("https://slack.com/api/chat.postMessage")
            .header(
                "Authorization",
                format!("Bearer {}", self.config.bot_token),
            )
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            warn!("slack post_message failed: {}", resp.status());
        }
        Ok(())
    }

    async fn respond_to_url(&self, response_url: &str, text: &str) -> anyhow::Result<()> {
        let body = serde_json::json!({
            "response_type": "in_channel",
            "text": text,
        });
        self.client.post(response_url).json(&body).send().await?;
        Ok(())
    }
}

#[derive(Clone)]
struct SlackState {
    manager: Arc<GatewayManager>,
    adapter: Arc<SlackAdapter>,
}

#[async_trait]
impl GatewayAdapter for SlackAdapter {
    fn platform(&self) -> Platform {
        Platform::Slack
    }

    fn routes(&self, manager: Arc<GatewayManager>) -> Router {
        let adapter = Arc::new(SlackAdapter::new(self.config.clone()));
        let state = SlackState {
            manager,
            adapter: adapter.clone(),
        };
        Router::new()
            .route("/gateway/slack/commands", post(slash_command_handler))
            .route("/gateway/slack/events", post(events_handler))
            .with_state(state)
    }

    async fn start_background(&self, _manager: Arc<GatewayManager>) -> anyhow::Result<()> {
        info!("Slack adapter ready (webhook-based, no background task)");
        Ok(())
    }

    async fn deliver_response(&self, response: GatewayResponse) -> anyhow::Result<()> {
        let text = format_slack_response(&response.payload);
        self.post_message(
            &response.origin.channel_id,
            response.origin.thread_id.as_deref(),
            &text,
        )
        .await
    }
}

async fn slash_command_handler(
    State(state): State<SlackState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(err) = state.adapter.verify_slack_signature(&headers, body.as_ref()) {
        warn!("slack signature verification failed: {err}");
        return (StatusCode::UNAUTHORIZED, "invalid signature".to_string());
    }

    let form: Vec<(String, String)> =
        serde_urlencoded::from_bytes(body.as_ref()).unwrap_or_default();
    let get_field = |name: &str| -> String {
        form.iter()
            .find(|(k, _): &&(String, String)| k == name)
            .map(|(_, v): &(String, String)| v.clone())
            .unwrap_or_default()
    };

    let text = get_field("text");
    let channel_id = get_field("channel_id");
    let user_id = get_field("user_id");
    let response_url = get_field("response_url");

    let origin = MessageOrigin {
        platform: Platform::Slack,
        channel_id: channel_id.clone(),
        thread_id: None,
        user_id,
    };

    let cmd = parse_gateway_text(&text, origin.clone());
    let is_run = matches!(cmd.action, crate::gateway::GatewayAction::SubmitRun { .. });

    let manager = state.manager.clone();
    let adapter = state.adapter.clone();

    tokio::spawn(async move {
        let response = manager.handle_command(cmd).await;
        let formatted = format_slack_response(&response.payload);

        if !response_url.is_empty() {
            if let Err(err) = adapter.respond_to_url(&response_url, &formatted).await {
                error!("slack respond_to_url failed: {err}");
            }
        } else {
            if let Err(err) = adapter
                .post_message(&channel_id, None, &formatted)
                .await
            {
                error!("slack post_message failed: {err}");
            }
        }

        if is_run {
            if let GatewayResponsePayload::RunSubmitted(ref sub) = response.payload {
                poll_and_deliver(manager, adapter as Arc<dyn GatewayAdapter>, sub.run_id, origin)
                    .await;
            }
        }
    });

    (StatusCode::OK, String::new())
}

async fn events_handler(
    State(state): State<SlackState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(err) = state.adapter.verify_slack_signature(&headers, body.as_ref()) {
        warn!("slack events signature verification failed: {err}");
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
            )
        }
    };

    let event_type = payload
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if event_type == "url_verification" {
        let challenge = payload
            .get("challenge")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        return (
            StatusCode::OK,
            Json(serde_json::json!({"challenge": challenge})),
        );
    }

    if event_type == "event_callback" {
        if let Some(event) = payload.get("event") {
            let sub_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if sub_type == "app_mention" || sub_type == "message" {
                let text = event.get("text").and_then(|v| v.as_str()).unwrap_or("");
                let channel = event
                    .get("channel")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let user = event.get("user").and_then(|v| v.as_str()).unwrap_or("");
                let thread_ts = event
                    .get("thread_ts")
                    .or_else(|| event.get("ts"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let cleaned_text = text
                    .split_whitespace()
                    .skip_while(|w| w.starts_with('<') && w.ends_with('>'))
                    .collect::<Vec<_>>()
                    .join(" ");

                let origin = MessageOrigin {
                    platform: Platform::Slack,
                    channel_id: channel.to_string(),
                    thread_id: thread_ts,
                    user_id: user.to_string(),
                };

                let cmd = parse_gateway_text(&cleaned_text, origin.clone());
                let is_run =
                    matches!(cmd.action, crate::gateway::GatewayAction::SubmitRun { .. });
                let manager = state.manager.clone();
                let adapter = state.adapter.clone();

                tokio::spawn(async move {
                    let response = manager.handle_command(cmd).await;
                    if let Err(err) = adapter.deliver_response(response.clone()).await {
                        error!("slack deliver_response failed: {err}");
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
            }
        }
    }

    (StatusCode::OK, Json(serde_json::json!({"ok": true})))
}

fn format_slack_response(payload: &GatewayResponsePayload) -> String {
    match payload {
        GatewayResponsePayload::RunSubmitted(sub) => {
            format!(
                ":rocket: *Run submitted*\n`run_id: {}`\n`session: {}`\n`status: {}`",
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
                        "  - `{}` [{}] model=`{}` {}ms {}",
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
                ":mag: *Run {}*\ntask: {}\nstatus: `{}`\nprofile: `{}`\n{}",
                run.run_id,
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
            let emoji = if *success {
                ":white_check_mark:"
            } else {
                ":x:"
            };
            format!("{} `{}` on run `{}`", emoji, action, run_id)
        }
        GatewayResponsePayload::RunList(runs) => {
            let lines: Vec<String> = runs
                .iter()
                .take(10)
                .map(|r| {
                    format!(
                        "  - `{}` [{}] {}",
                        &r.run_id.to_string()[..8],
                        r.status,
                        r.task.chars().take(50).collect::<String>()
                    )
                })
                .collect();
            format!(
                ":clipboard: *Recent Runs* ({})\n{}",
                runs.len(),
                lines.join("\n")
            )
        }
        GatewayResponsePayload::SessionList(sessions) => {
            let lines: Vec<String> = sessions
                .iter()
                .take(10)
                .map(|s| {
                    format!(
                        "  - `{}` runs={}",
                        &s.session_id.to_string()[..8],
                        s.run_count
                    )
                })
                .collect();
            format!(
                ":file_folder: *Sessions* ({})\n{}",
                sessions.len(),
                lines.join("\n")
            )
        }
        GatewayResponsePayload::HelpText(text) => format!("```\n{}\n```", text),
        GatewayResponsePayload::Error(err) => format!(":warning: Error: {}", err),
    }
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
