pub mod session_log;
pub mod store;

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use dashmap::DashMap;
use uuid::Uuid;

use crate::types::{
    MemoryHit, RunActionEvent, RunActionType, RunRecord, SessionEvent, SessionEventType,
    SessionSummary, WebhookDeliveryRecord, WebhookEndpoint,
};

pub use self::session_log::SessionLogger;
pub use self::store::SqliteStore;

#[derive(Debug, Clone)]
struct ShortTermItem {
    content: String,
    importance: f64,
    inserted_at: Instant,
    expires_at: Instant,
}

#[derive(Debug, Clone)]
pub struct MemoryManager {
    logger: Arc<SessionLogger>,
    store: Arc<SqliteStore>,
    short_term: Arc<DashMap<Uuid, Vec<ShortTermItem>>>,
}

impl MemoryManager {
    pub async fn new(session_root: std::path::PathBuf, database_url: &str) -> anyhow::Result<Self> {
        let logger = Arc::new(SessionLogger::new(session_root));
        let store = Arc::new(SqliteStore::connect(database_url).await?);

        Ok(Self {
            logger,
            store,
            short_term: Arc::new(DashMap::new()),
        })
    }

    pub fn store(&self) -> Arc<SqliteStore> {
        self.store.clone()
    }

    pub async fn create_session(&self, session_id: Uuid) -> anyhow::Result<()> {
        self.store.create_session(session_id).await
    }

    pub async fn append_event(&self, event: SessionEvent) -> anyhow::Result<()> {
        let session_id = event.session_id;
        self.logger.append_event(session_id, &event).await?;

        if matches!(event.event_type, SessionEventType::UserMessage) {
            if let Some(text) = event.payload.get("text").and_then(|v| v.as_str()) {
                self.store.record_message(session_id, "user", text).await?;
            }
        }

        Ok(())
    }

    pub async fn replay(&self, session_id: Uuid) -> anyhow::Result<Vec<crate::types::ReplayEvent>> {
        self.logger.replay(session_id).await
    }

    pub async fn upsert_run(&self, run: &RunRecord) -> anyhow::Result<()> {
        self.store.upsert_run(run).await
    }

    pub async fn get_run(&self, run_id: Uuid) -> anyhow::Result<Option<RunRecord>> {
        self.store.get_run(run_id).await
    }

    pub async fn list_recent_runs(&self, limit: usize) -> anyhow::Result<Vec<RunRecord>> {
        self.store.list_recent_runs(limit).await
    }

    pub async fn append_run_action_event(
        &self,
        run_id: Uuid,
        session_id: Uuid,
        action: RunActionType,
        actor_type: Option<&str>,
        actor_id: Option<&str>,
        cause_event_id: Option<&str>,
        payload: serde_json::Value,
    ) -> anyhow::Result<RunActionEvent> {
        self.store
            .append_run_action_event(
                run_id,
                session_id,
                action,
                actor_type,
                actor_id,
                cause_event_id,
                payload,
            )
            .await
    }

    pub async fn list_run_action_events(
        &self,
        run_id: Uuid,
        limit: usize,
    ) -> anyhow::Result<Vec<RunActionEvent>> {
        self.store.list_run_action_events(run_id, limit).await
    }

    pub async fn list_run_action_events_since(
        &self,
        run_id: Uuid,
        after_seq: i64,
        limit: usize,
    ) -> anyhow::Result<Vec<RunActionEvent>> {
        self.store
            .list_run_action_events_since(run_id, after_seq, limit)
            .await
    }

    pub async fn list_session_runs(
        &self,
        session_id: Uuid,
        limit: usize,
    ) -> anyhow::Result<Vec<RunRecord>> {
        self.store.list_session_runs(session_id, limit).await
    }

    pub async fn list_sessions(&self, limit: usize) -> anyhow::Result<Vec<SessionSummary>> {
        self.store.list_sessions(limit).await
    }

    pub async fn get_session(&self, session_id: Uuid) -> anyhow::Result<Option<SessionSummary>> {
        self.store.get_session(session_id).await
    }

    pub async fn list_session_messages(
        &self,
        session_id: Uuid,
        limit: usize,
    ) -> anyhow::Result<Vec<(i64, String, String, String)>> {
        self.store.list_session_messages(session_id, limit).await
    }

    pub async fn delete_session(&self, session_id: Uuid) -> anyhow::Result<()> {
        self.store.delete_session(session_id).await?;
        self.short_term.remove(&session_id);
        self.logger.delete(session_id).await?;
        Ok(())
    }

    pub async fn remember_short(
        &self,
        session_id: Uuid,
        content: impl Into<String>,
        importance: f64,
        ttl: Duration,
    ) {
        let now = Instant::now();
        let item = ShortTermItem {
            content: content.into(),
            importance,
            inserted_at: now,
            expires_at: now + ttl,
        };

        let mut entry = self
            .short_term
            .entry(session_id)
            .or_insert_with(Vec::<ShortTermItem>::new);
        entry.push(item);
    }

    pub async fn remember_long(
        &self,
        session_id: Uuid,
        scope: &str,
        content: &str,
        importance: f64,
        source_ref: Option<&str>,
    ) -> anyhow::Result<String> {
        let id = self
            .store
            .insert_memory_item(session_id, scope, content, importance)
            .await?;

        if let Some(source_ref) = source_ref {
            self.store.link_memory(id.as_str(), source_ref).await?;
        }

        let event = SessionEvent {
            session_id,
            run_id: None,
            event_type: SessionEventType::MemoryWrite,
            timestamp: Utc::now(),
            payload: serde_json::json!({
                "memory_id": id,
                "scope": scope,
                "importance": importance,
            }),
        };
        self.append_event(event).await?;

        Ok(id)
    }

    pub async fn retrieve(
        &self,
        session_id: Uuid,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<MemoryHit>> {
        let mut long_hits = self
            .store
            .search_memory(session_id, query, limit * 2)
            .await?;

        let now = Instant::now();
        let mut short_hits = Vec::<MemoryHit>::new();

        if let Some(mut entries) = self.short_term.get_mut(&session_id) {
            entries.retain(|e| e.expires_at > now);
            for item in entries.iter() {
                let recency = recency_score(item.inserted_at, now);
                let similarity = similarity_score(&item.content, query);
                let score = 0.35 * recency + 0.35 * item.importance + 0.30 * similarity;

                if similarity > 0.0 {
                    short_hits.push(MemoryHit {
                        id: format!("short:{}", Uuid::new_v4()),
                        content: item.content.clone(),
                        importance: item.importance,
                        created_at: Utc::now(),
                        score,
                    });
                }
            }
        }

        for hit in &mut long_hits {
            let age_secs = (Utc::now() - hit.created_at).num_seconds().max(0) as f64;
            let recency = (1.0 / (1.0 + age_secs / 3600.0)).clamp(0.0, 1.0);
            let similarity = similarity_score(hit.content.as_str(), query);
            hit.score = 0.35 * recency + 0.35 * hit.importance + 0.30 * similarity;
        }

        let mut all_hits = Vec::new();
        all_hits.extend(short_hits);
        all_hits.extend(long_hits);

        all_hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        all_hits.truncate(limit);

        Ok(all_hits)
    }

    pub async fn register_webhook(
        &self,
        url: &str,
        events: &[String],
        secret: &str,
    ) -> anyhow::Result<WebhookEndpoint> {
        self.store.register_webhook(url, events, secret).await
    }

    pub async fn list_webhooks(&self) -> anyhow::Result<Vec<WebhookEndpoint>> {
        self.store.list_webhooks().await
    }

    pub async fn insert_webhook_delivery(
        &self,
        endpoint_id: &str,
        event: &str,
        event_id: &str,
        url: &str,
        attempts: u32,
        delivered: bool,
        dead_letter: bool,
        status_code: Option<u16>,
        error: Option<&str>,
        payload: &serde_json::Value,
    ) -> anyhow::Result<WebhookDeliveryRecord> {
        self.store
            .insert_webhook_delivery(
                endpoint_id,
                event,
                event_id,
                url,
                attempts,
                delivered,
                dead_letter,
                status_code,
                error,
                payload,
            )
            .await
    }

    pub async fn list_webhook_deliveries(
        &self,
        dead_letter_only: bool,
        limit: usize,
    ) -> anyhow::Result<Vec<WebhookDeliveryRecord>> {
        self.store
            .list_webhook_deliveries(dead_letter_only, limit)
            .await
    }

    pub async fn get_webhook_delivery(
        &self,
        delivery_id: i64,
    ) -> anyhow::Result<Option<WebhookDeliveryRecord>> {
        self.store.get_webhook_delivery(delivery_id).await
    }

    pub async fn compact_session(&self, session_id: Uuid) -> anyhow::Result<()> {
        self.store.compact_session(session_id).await
    }

    pub async fn vacuum(&self) -> anyhow::Result<()> {
        self.store.vacuum().await
    }

    pub async fn save_workflow(
        &self,
        template: &crate::types::WorkflowTemplate,
    ) -> anyhow::Result<()> {
        self.store.save_workflow(template).await
    }

    pub async fn get_workflow(
        &self,
        id: &str,
    ) -> anyhow::Result<Option<crate::types::WorkflowTemplate>> {
        self.store.get_workflow(id).await
    }

    pub async fn list_workflows(
        &self,
        limit: usize,
    ) -> anyhow::Result<Vec<crate::types::WorkflowTemplate>> {
        self.store.list_workflows(limit).await
    }

    pub async fn delete_workflow(&self, id: &str) -> anyhow::Result<()> {
        self.store.delete_workflow(id).await
    }
}

fn recency_score(inserted_at: Instant, now: Instant) -> f64 {
    let age = now.duration_since(inserted_at).as_secs_f64();
    (1.0 / (1.0 + age / 1800.0)).clamp(0.0, 1.0)
}

fn similarity_score(text: &str, query: &str) -> f64 {
    let text_tokens = tokenize(text);
    let query_tokens = tokenize(query);
    if text_tokens.is_empty() || query_tokens.is_empty() {
        return 0.0;
    }

    let intersection = text_tokens.intersection(&query_tokens).count() as f64;
    let union = text_tokens.union(&query_tokens).count() as f64;

    (intersection / union).clamp(0.0, 1.0)
}

fn tokenize(s: &str) -> std::collections::HashSet<String> {
    s.to_lowercase()
        .split_whitespace()
        .map(|tok| {
            tok.chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
        })
        .filter(|tok| !tok.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similarity_score_behaves() {
        let score_high = similarity_score("rust async dag execution", "rust dag");
        let score_low = similarity_score("cat dog bird", "webhook hmac");
        assert!(score_high > score_low);
    }
}
