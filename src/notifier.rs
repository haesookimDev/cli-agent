use tracing::warn;

/// Configuration for outgoing release notifications.
/// Set `SLACK_NOTIFY_WEBHOOK_URL` and/or `DISCORD_NOTIFY_WEBHOOK_URL` in the environment.
pub struct NotifyConfig {
    pub slack_webhook_url: Option<String>,
    pub discord_webhook_url: Option<String>,
}

impl NotifyConfig {
    pub fn from_env() -> Self {
        Self {
            slack_webhook_url: std::env::var("SLACK_NOTIFY_WEBHOOK_URL").ok(),
            discord_webhook_url: std::env::var("DISCORD_NOTIFY_WEBHOOK_URL").ok(),
        }
    }

    pub fn any_configured(&self) -> bool {
        self.slack_webhook_url.is_some() || self.discord_webhook_url.is_some()
    }
}

/// Send a release notification to all configured webhook endpoints.
///
/// Slack incoming webhook expects `{"text": "..."}`.
/// Discord webhook expects `{"content": "..."}`.
pub async fn notify_release(
    config: &NotifyConfig,
    version: &str,
    notes: &str,
) -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    let message = format!("*Release {}*\n{}", version, notes);

    if let Some(url) = &config.slack_webhook_url {
        let body = serde_json::json!({"text": message});
        match client.post(url).json(&body).send().await {
            Ok(resp) if !resp.status().is_success() => {
                warn!("slack notify returned {}", resp.status());
            }
            Err(e) => warn!("slack notify failed: {e}"),
            _ => {}
        }
    }

    if let Some(url) = &config.discord_webhook_url {
        let body = serde_json::json!({"content": message});
        match client.post(url).json(&body).send().await {
            Ok(resp) if !resp.status().is_success() => {
                warn!("discord notify returned {}", resp.status());
            }
            Err(e) => warn!("discord notify failed: {e}"),
            _ => {}
        }
    }

    Ok(())
}

/// Extract the release notes for a given version tag from RELEASE.md contents.
///
/// Finds the `## <version>` heading and collects lines until the next `##` section.
pub fn parse_release_notes(content: &str, version: &str) -> Option<String> {
    let mut in_section = false;
    let mut lines: Vec<&str> = Vec::new();

    for line in content.lines() {
        if line.starts_with("## ") {
            if in_section {
                break;
            }
            if line.contains(version) {
                in_section = true;
            }
        } else if in_section {
            lines.push(line);
        }
    }

    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n").trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_release_notes_extracts_correct_section() {
        let md = "## v0.2.0 - 2026-03-20\n- Feature A\n- Feature B\n\n## v0.1.0 - 2026-03-01\n- Initial release\n";
        let notes = parse_release_notes(md, "v0.2.0").unwrap();
        assert!(notes.contains("Feature A"));
        assert!(notes.contains("Feature B"));
        assert!(!notes.contains("Initial release"));
    }

    #[test]
    fn parse_release_notes_returns_none_for_missing_version() {
        let md = "## v0.1.0\n- Initial release\n";
        assert!(parse_release_notes(md, "v9.9.9").is_none());
    }
}
