use std::collections::HashSet;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::types::{
    CronSchedule, KnowledgeItem, Meeting, MeetingMessage, MeetingSpeakerKind, MeetingStatus,
    MemoryHit, RunActionEvent, RunActionType, RunRecord, SessionMemoryItem, SessionSummary,
    WebhookDeliveryRecord, WebhookEndpoint, Workspace, WorkspaceFile, WorkspaceFileCreatedBy,
    WorkspaceKind,
};

#[derive(Debug, Clone)]
pub struct SqliteStore {
    pool: SqlitePool,
}

/// One row of `messages`. Carries optional run/node/persona linkage so
/// chat UIs can show agent replies under the right persona heading and
/// pair them with their owning run.
#[derive(Debug, Clone)]
pub struct StoredMessage {
    pub id: i64,
    pub role: String,
    pub content: String,
    pub created_at: String,
    pub run_id: Option<String>,
    pub node_id: Option<String>,
    pub persona_name: Option<String>,
}

/// Row payload for `batch_insert_run_action_events`. Identical fields to
/// `append_run_action_event`'s arguments, just bundled so a Vec can be
/// queued and flushed together.
#[derive(Debug, Clone)]
pub struct RunActionEventInput {
    pub run_id: Uuid,
    pub session_id: Uuid,
    pub action: RunActionType,
    pub actor_type: Option<String>,
    pub actor_id: Option<String>,
    pub cause_event_id: Option<String>,
    pub payload: serde_json::Value,
}

impl SqliteStore {
    pub async fn connect(database_url: &str) -> anyhow::Result<Self> {
        let options = SqliteConnectOptions::from_str(database_url)?.create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(12)
            .connect_with(options)
            .await?;

        let store = Self { pool };
        store.init_schema().await?;
        Ok(store)
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Apply embedded `migrations/*.sql` via `sqlx::migrate!()`. The macro
    /// tracks applied versions in `_sqlx_migrations` so re-running is safe.
    /// Pre-existing databases (created before migrations were introduced)
    /// are upgraded transparently because each migration uses
    /// `CREATE TABLE IF NOT EXISTS`.
    pub async fn init_schema(&self) -> anyhow::Result<()> {
        sqlx::migrate!("./migrations").run(&self.pool).await?;
        // Defensive: an early version of init_schema added the embedding
        // column via a silent ALTER on every boot. Some DBs may have predated
        // both the initial CREATE-with-embedding and the migrations system,
        // so attempt the ALTER once more and ignore "duplicate column" errors.
        let _ = sqlx::query("ALTER TABLE memory_items ADD COLUMN embedding BLOB")
            .execute(&self.pool)
            .await;
        Ok(())
    }

    pub async fn create_session(&self, session_id: Uuid, kind: &str) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT OR IGNORE INTO sessions (id, created_at, kind)
            VALUES (?1, ?2, ?3)
            "#,
        )
        .bind(session_id.to_string())
        .bind(Utc::now().to_rfc3339())
        .bind(kind)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn record_message(
        &self,
        session_id: Uuid,
        role: &str,
        content: &str,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO messages (session_id, role, content, created_at)
            VALUES (?1, ?2, ?3, ?4)
            "#,
        )
        .bind(session_id.to_string())
        .bind(role)
        .bind(content)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Persist an agent reply tied to a specific run + node so the chat UI
    /// can show it as soon as the node settles, instead of waiting for the
    /// whole run to finish and surface it through `RunRecord.outputs`.
    /// Idempotent on (session_id, run_id, node_id) — re-runs of the same
    /// node update content rather than duplicating the row.
    pub async fn record_agent_message(
        &self,
        session_id: Uuid,
        run_id: Uuid,
        node_id: &str,
        persona_name: Option<&str>,
        content: &str,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO messages (session_id, role, content, run_id, node_id, persona_name, created_at)
            VALUES (?1, 'agent', ?2, ?3, ?4, ?5, ?6)
            ON CONFLICT(session_id, run_id, node_id) WHERE run_id IS NOT NULL
            DO UPDATE SET
                content = excluded.content,
                persona_name = excluded.persona_name,
                created_at = excluded.created_at
            "#,
        )
        .bind(session_id.to_string())
        .bind(content)
        .bind(run_id.to_string())
        .bind(node_id)
        .bind(persona_name)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_session_messages(
        &self,
        session_id: Uuid,
        limit: usize,
    ) -> anyhow::Result<Vec<StoredMessage>> {
        let rows = sqlx::query(
            r#"
            SELECT id, role, content, created_at, run_id, node_id, persona_name
            FROM messages
            WHERE session_id = ?1
            ORDER BY id DESC
            LIMIT ?2
            "#,
        )
        .bind(session_id.to_string())
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        let mut msgs = Vec::with_capacity(rows.len());
        for row in rows {
            msgs.push(StoredMessage {
                id: row.get("id"),
                role: row.get("role"),
                content: row.get("content"),
                created_at: row.get("created_at"),
                run_id: row.try_get("run_id").ok(),
                node_id: row.try_get("node_id").ok(),
                persona_name: row.try_get("persona_name").ok(),
            });
        }
        msgs.reverse();
        Ok(msgs)
    }

    pub async fn upsert_run(&self, run: &RunRecord) -> anyhow::Result<()> {
        let total_input = run
            .total_token_usage
            .map(|u| u.input_tokens as i64)
            .unwrap_or(0);
        let total_output = run
            .total_token_usage
            .map(|u| u.output_tokens as i64)
            .unwrap_or(0);
        let total_cost = run.total_cost_estimate_usd.unwrap_or(0.0);
        sqlx::query(
            r#"
            INSERT INTO agent_runs (
                run_id,
                session_id,
                status,
                profile,
                task,
                run_json,
                error,
                created_at,
                updated_at,
                total_input_tokens,
                total_output_tokens,
                total_cost_usd
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            ON CONFLICT(run_id)
            DO UPDATE SET
                status = excluded.status,
                run_json = excluded.run_json,
                error = excluded.error,
                updated_at = excluded.updated_at,
                total_input_tokens = excluded.total_input_tokens,
                total_output_tokens = excluded.total_output_tokens,
                total_cost_usd = excluded.total_cost_usd
            "#,
        )
        .bind(run.run_id.to_string())
        .bind(run.session_id.to_string())
        .bind(run.status.to_string())
        .bind(run.profile.to_string())
        .bind(run.task.clone())
        .bind(serde_json::to_string(run)?)
        .bind(run.error.clone())
        .bind(run.created_at.to_rfc3339())
        .bind(Utc::now().to_rfc3339())
        .bind(total_input)
        .bind(total_output)
        .bind(total_cost)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_run(&self, run_id: Uuid) -> anyhow::Result<Option<RunRecord>> {
        let row = sqlx::query(
            r#"
            SELECT run_json FROM agent_runs WHERE run_id = ?1
            "#,
        )
        .bind(run_id.to_string())
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };
        let run_json: String = row.get("run_json");
        let run: RunRecord = serde_json::from_str(&run_json)?;
        Ok(Some(run))
    }

    pub async fn list_recent_runs(&self, limit: usize) -> anyhow::Result<Vec<RunRecord>> {
        let rows = sqlx::query(
            r#"
            SELECT run_json
            FROM agent_runs
            ORDER BY created_at DESC
            LIMIT ?1
            "#,
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        let mut runs = Vec::with_capacity(rows.len());
        for row in rows {
            let run_json: String = row.get("run_json");
            runs.push(serde_json::from_str(&run_json)?);
        }
        Ok(runs)
    }

    /// Bulk-insert `events` in a single transaction. Used by the high-volume
    /// token-chunk path to amortize SQLite roundtrips across many rows.
    pub async fn batch_insert_run_action_events(
        &self,
        events: &[RunActionEventInput],
    ) -> anyhow::Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        for ev in events {
            let event_id = Uuid::new_v4().to_string();
            let timestamp = Utc::now().to_rfc3339();
            let payload_raw = serde_json::to_string(&ev.payload)?;
            sqlx::query(
                r#"
                INSERT INTO run_action_events (
                    event_id,
                    run_id,
                    session_id,
                    timestamp,
                    action,
                    actor_type,
                    actor_id,
                    cause_event_id,
                    payload
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                "#,
            )
            .bind(event_id)
            .bind(ev.run_id.to_string())
            .bind(ev.session_id.to_string())
            .bind(timestamp)
            .bind(ev.action.to_string())
            .bind(ev.actor_type.as_deref())
            .bind(ev.actor_id.as_deref())
            .bind(ev.cause_event_id.as_deref())
            .bind(payload_raw)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
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
        let event_id = Uuid::new_v4().to_string();
        let timestamp = Utc::now();
        let payload_raw = serde_json::to_string(&payload)?;

        let result = sqlx::query(
            r#"
            INSERT INTO run_action_events (
                event_id,
                run_id,
                session_id,
                timestamp,
                action,
                actor_type,
                actor_id,
                cause_event_id,
                payload
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            "#,
        )
        .bind(event_id.as_str())
        .bind(run_id.to_string())
        .bind(session_id.to_string())
        .bind(timestamp.to_rfc3339())
        .bind(action.to_string())
        .bind(actor_type)
        .bind(actor_id)
        .bind(cause_event_id)
        .bind(payload_raw)
        .execute(&self.pool)
        .await?;

        Ok(RunActionEvent {
            seq: result.last_insert_rowid(),
            event_id,
            run_id,
            session_id,
            timestamp,
            action,
            actor_type: actor_type.map(ToString::to_string),
            actor_id: actor_id.map(ToString::to_string),
            cause_event_id: cause_event_id.map(ToString::to_string),
            payload,
        })
    }

    pub async fn list_run_action_events(
        &self,
        run_id: Uuid,
        limit: usize,
    ) -> anyhow::Result<Vec<RunActionEvent>> {
        let rows = sqlx::query(
            r#"
            SELECT
                seq,
                event_id,
                run_id,
                session_id,
                timestamp,
                action,
                actor_type,
                actor_id,
                cause_event_id,
                payload
            FROM run_action_events
            WHERE run_id = ?1
            ORDER BY seq ASC
            LIMIT ?2
            "#,
        )
        .bind(run_id.to_string())
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        let mut events = Vec::with_capacity(rows.len());
        for row in rows {
            let run_raw: String = row.get("run_id");
            let session_raw: String = row.get("session_id");
            let ts_raw: String = row.get("timestamp");
            let action_raw: String = row.get("action");
            let payload_raw: String = row.get("payload");

            events.push(RunActionEvent {
                seq: row.get("seq"),
                event_id: row.get("event_id"),
                run_id: Uuid::parse_str(run_raw.as_str())?,
                session_id: Uuid::parse_str(session_raw.as_str())?,
                timestamp: parse_rfc3339(ts_raw.as_str())?,
                action: parse_run_action(action_raw.as_str())?,
                actor_type: row.get("actor_type"),
                actor_id: row.get("actor_id"),
                cause_event_id: row.get("cause_event_id"),
                payload: serde_json::from_str(payload_raw.as_str())?,
            });
        }

        Ok(events)
    }

    pub async fn list_run_action_events_since(
        &self,
        run_id: Uuid,
        after_seq: i64,
        limit: usize,
    ) -> anyhow::Result<Vec<RunActionEvent>> {
        let rows = sqlx::query(
            r#"
            SELECT
                seq,
                event_id,
                run_id,
                session_id,
                timestamp,
                action,
                actor_type,
                actor_id,
                cause_event_id,
                payload
            FROM run_action_events
            WHERE run_id = ?1 AND seq > ?2
            ORDER BY seq ASC
            LIMIT ?3
            "#,
        )
        .bind(run_id.to_string())
        .bind(after_seq)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        let mut events = Vec::with_capacity(rows.len());
        for row in rows {
            let run_raw: String = row.get("run_id");
            let session_raw: String = row.get("session_id");
            let ts_raw: String = row.get("timestamp");
            let action_raw: String = row.get("action");
            let payload_raw: String = row.get("payload");

            events.push(RunActionEvent {
                seq: row.get("seq"),
                event_id: row.get("event_id"),
                run_id: Uuid::parse_str(run_raw.as_str())?,
                session_id: Uuid::parse_str(session_raw.as_str())?,
                timestamp: parse_rfc3339(ts_raw.as_str())?,
                action: parse_run_action(action_raw.as_str())?,
                actor_type: row.get("actor_type"),
                actor_id: row.get("actor_id"),
                cause_event_id: row.get("cause_event_id"),
                payload: serde_json::from_str(payload_raw.as_str())?,
            });
        }

        Ok(events)
    }

    pub async fn list_session_runs(
        &self,
        session_id: Uuid,
        limit: usize,
    ) -> anyhow::Result<Vec<RunRecord>> {
        let rows = sqlx::query(
            r#"
            SELECT run_json
            FROM agent_runs
            WHERE session_id = ?1
            ORDER BY created_at DESC
            LIMIT ?2
            "#,
        )
        .bind(session_id.to_string())
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        let mut runs = Vec::with_capacity(rows.len());
        for row in rows {
            let run_json: String = row.get("run_json");
            runs.push(serde_json::from_str(&run_json)?);
        }
        Ok(runs)
    }

    pub async fn list_sessions(
        &self,
        limit: usize,
        kind: Option<&str>,
    ) -> anyhow::Result<Vec<SessionSummary>> {
        let rows = sqlx::query(
            r#"
            SELECT
                s.id AS session_id,
                s.created_at AS created_at,
                COUNT(ar.run_id) AS run_count,
                MAX(ar.created_at) AS last_run_at,
                (
                    SELECT ar2.task
                    FROM agent_runs ar2
                    WHERE ar2.session_id = s.id
                    ORDER BY ar2.created_at DESC
                    LIMIT 1
                ) AS last_task,
                COALESCE(SUM(ar.total_input_tokens), 0)  AS total_input,
                COALESCE(SUM(ar.total_output_tokens), 0) AS total_output,
                COALESCE(SUM(ar.total_cost_usd), 0.0)    AS total_cost
            FROM sessions s
            LEFT JOIN agent_runs ar ON ar.session_id = s.id
            WHERE (?2 IS NULL OR s.kind = ?2)
            GROUP BY s.id
            ORDER BY COALESCE(MAX(ar.created_at), s.created_at) DESC
            LIMIT ?1
            "#,
        )
        .bind(limit as i64)
        .bind(kind)
        .fetch_all(&self.pool)
        .await?;

        let mut sessions = Vec::with_capacity(rows.len());
        for row in rows {
            let session_raw: String = row.get("session_id");
            let created_at_raw: String = row.get("created_at");
            let last_run_at_raw: Option<String> = row.get("last_run_at");
            let total_input: i64 = row.try_get("total_input").unwrap_or(0);
            let total_output: i64 = row.try_get("total_output").unwrap_or(0);
            let total_cost: f64 = row.try_get("total_cost").unwrap_or(0.0);
            let (token_usage, cost) = build_session_totals(total_input, total_output, total_cost);

            sessions.push(SessionSummary {
                session_id: Uuid::parse_str(session_raw.as_str())?,
                created_at: parse_rfc3339(created_at_raw.as_str())?,
                run_count: row.get::<i64, _>("run_count").max(0) as usize,
                last_run_at: last_run_at_raw.as_deref().map(parse_rfc3339).transpose()?,
                last_task: row.get("last_task"),
                total_token_usage: token_usage,
                total_cost_estimate_usd: cost,
                cost_is_estimate: cost.is_some(),
            });
        }

        Ok(sessions)
    }

    pub async fn get_session(&self, session_id: Uuid) -> anyhow::Result<Option<SessionSummary>> {
        let row = sqlx::query(
            r#"
            SELECT
                s.id AS session_id,
                s.created_at AS created_at,
                COUNT(ar.run_id) AS run_count,
                MAX(ar.created_at) AS last_run_at,
                (
                    SELECT ar2.task
                    FROM agent_runs ar2
                    WHERE ar2.session_id = s.id
                    ORDER BY ar2.created_at DESC
                    LIMIT 1
                ) AS last_task,
                COALESCE(SUM(ar.total_input_tokens), 0)  AS total_input,
                COALESCE(SUM(ar.total_output_tokens), 0) AS total_output,
                COALESCE(SUM(ar.total_cost_usd), 0.0)    AS total_cost
            FROM sessions s
            LEFT JOIN agent_runs ar ON ar.session_id = s.id
            WHERE s.id = ?1
            GROUP BY s.id
            "#,
        )
        .bind(session_id.to_string())
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };

        let session_raw: String = row.get("session_id");
        let created_at_raw: String = row.get("created_at");
        let last_run_at_raw: Option<String> = row.get("last_run_at");
        let total_input: i64 = row.try_get("total_input").unwrap_or(0);
        let total_output: i64 = row.try_get("total_output").unwrap_or(0);
        let total_cost: f64 = row.try_get("total_cost").unwrap_or(0.0);
        let (token_usage, cost) = build_session_totals(total_input, total_output, total_cost);

        Ok(Some(SessionSummary {
            session_id: Uuid::parse_str(session_raw.as_str())?,
            created_at: parse_rfc3339(created_at_raw.as_str())?,
            run_count: row.get::<i64, _>("run_count").max(0) as usize,
            last_run_at: last_run_at_raw.as_deref().map(parse_rfc3339).transpose()?,
            last_task: row.get("last_task"),
            total_token_usage: token_usage,
            total_cost_estimate_usd: cost,
            cost_is_estimate: cost.is_some(),
        }))
    }

    pub async fn delete_session(&self, session_id: Uuid) -> anyhow::Result<()> {
        let session = session_id.to_string();
        let mut tx = self.pool.begin().await?;

        sqlx::query(
            r#"
            DELETE FROM memory_links
            WHERE memory_item_id IN (
                SELECT id FROM memory_items WHERE session_id = ?1
            )
            "#,
        )
        .bind(session.as_str())
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            DELETE FROM memory_items
            WHERE session_id = ?1
            "#,
        )
        .bind(session.as_str())
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            DELETE FROM messages
            WHERE session_id = ?1
            "#,
        )
        .bind(session.as_str())
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            DELETE FROM agent_runs
            WHERE session_id = ?1
            "#,
        )
        .bind(session.as_str())
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            DELETE FROM run_action_events
            WHERE session_id = ?1
            "#,
        )
        .bind(session.as_str())
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            DELETE FROM sessions
            WHERE id = ?1
            "#,
        )
        .bind(session.as_str())
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }

    pub async fn insert_memory_item(
        &self,
        session_id: Uuid,
        scope: &str,
        content: &str,
        importance: f64,
    ) -> anyhow::Result<String> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        sqlx::query(
            r#"
            INSERT INTO memory_items (id, session_id, content, importance, scope, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
        )
        .bind(id.clone())
        .bind(session_id.to_string())
        .bind(content)
        .bind(importance)
        .bind(scope)
        .bind(now.clone())
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(id)
    }

    /// Store a precomputed embedding vector for an existing memory item.
    pub async fn update_memory_embedding(
        &self,
        id: &str,
        embedding_bytes: &[u8],
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE memory_items SET embedding = ?1 WHERE id = ?2",
        )
        .bind(embedding_bytes)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn link_memory(&self, memory_item_id: &str, source_ref: &str) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO memory_links (memory_item_id, source_ref, created_at)
            VALUES (?1, ?2, ?3)
            "#,
        )
        .bind(memory_item_id)
        .bind(source_ref)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn search_memory(
        &self,
        session_id: Uuid,
        query_text: &str,
        query_embedding: Option<&[f32]>,
        limit: usize,
    ) -> anyhow::Result<Vec<MemoryHit>> {
        use crate::memory::embedding::{cosine_similarity, decode_embedding};

        let normalized_query = query_text.trim().to_lowercase();
        if normalized_query.is_empty() {
            return Ok(Vec::new());
        }

        let rows = sqlx::query(
            r#"
            SELECT id, content, importance, created_at, embedding
            FROM memory_items
            WHERE session_id = ?1
            ORDER BY updated_at DESC
            LIMIT ?2
            "#,
        )
        .bind(session_id.to_string())
        .bind((limit.max(10) * 20) as i64)
        .fetch_all(&self.pool)
        .await?;

        let query_tokens = tokenize_lexical(normalized_query.as_str());
        let mut scored = Vec::<(f64, MemoryHit)>::new();
        for row in rows {
            let created_at_raw: String = row.get("created_at");
            let created_at = DateTime::parse_from_rfc3339(&created_at_raw)?.with_timezone(&Utc);
            let content: String = row.get("content");
            let importance: f64 = row.get("importance");
            let row_embedding_bytes: Option<Vec<u8>> = row.get("embedding");

            let age_secs = (Utc::now() - created_at).num_seconds().max(0) as f64;
            let recency = (1.0 / (1.0 + age_secs / 7200.0)).clamp(0.0, 1.0);

            // Use vector similarity when both query and row embeddings are available.
            let rank = if let (Some(qe), Some(bytes)) = (query_embedding, &row_embedding_bytes) {
                let row_vec = decode_embedding(bytes);
                let cosine = cosine_similarity(qe, &row_vec);
                // Threshold: skip items with very low semantic relevance.
                if cosine < 0.20 {
                    continue;
                }
                (0.70 * cosine + 0.20 * importance + 0.10 * recency).clamp(0.0, 1.0)
            } else {
                // Fallback: keyword-based scoring (existing behaviour).
                let content_lower = content.to_lowercase();
                let full_match = content_lower.contains(normalized_query.as_str());
                let token_score = token_overlap_score(&query_tokens, &content_lower);
                let lexical = if full_match { 1.0 } else { token_score };
                if lexical <= 0.0 {
                    continue;
                }
                (0.70 * lexical + 0.20 * importance + 0.10 * recency).clamp(0.0, 1.0)
            };

            scored.push((
                rank,
                MemoryHit {
                    id: row.get("id"),
                    content,
                    importance,
                    created_at,
                    score: rank,
                },
            ));
        }

        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        let mut out = scored.into_iter().map(|(_, hit)| hit).collect::<Vec<_>>();
        out.truncate(limit);
        Ok(out)
    }

    pub async fn list_session_memory_items(
        &self,
        session_id: Uuid,
        query_text: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<SessionMemoryItem>> {
        let query_norm = query_text.unwrap_or("").trim().to_lowercase();
        let pattern = format!("%{}%", query_norm);

        let rows = sqlx::query(
            r#"
            SELECT
                m.id,
                m.session_id,
                m.scope,
                m.content,
                m.importance,
                m.created_at,
                m.updated_at,
                COALESCE(GROUP_CONCAT(l.source_ref, char(31)), '') AS source_refs
            FROM memory_items m
            LEFT JOIN memory_links l ON l.memory_item_id = m.id
            WHERE m.session_id = ?1
              AND (?2 = '' OR LOWER(m.content) LIKE ?3 OR LOWER(m.scope) LIKE ?3)
            GROUP BY m.id, m.session_id, m.scope, m.content, m.importance, m.created_at, m.updated_at
            ORDER BY m.updated_at DESC
            LIMIT ?4
            "#,
        )
        .bind(session_id.to_string())
        .bind(query_norm)
        .bind(pattern)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let session_raw: String = row.get("session_id");
            let created_at_raw: String = row.get("created_at");
            let updated_at_raw: String = row.get("updated_at");
            let source_refs_raw: String = row.get("source_refs");
            let source_refs = if source_refs_raw.is_empty() {
                Vec::new()
            } else {
                source_refs_raw
                    .split('\u{1f}')
                    .map(ToString::to_string)
                    .collect()
            };

            out.push(SessionMemoryItem {
                id: row.get("id"),
                session_id: Uuid::parse_str(session_raw.as_str())?,
                scope: row.get("scope"),
                content: row.get("content"),
                importance: row.get("importance"),
                source_refs,
                created_at: parse_rfc3339(created_at_raw.as_str())?,
                updated_at: parse_rfc3339(updated_at_raw.as_str())?,
            });
        }

        Ok(out)
    }

    pub async fn update_memory_item(
        &self,
        memory_id: &str,
        content: Option<&str>,
        importance: Option<f64>,
        scope: Option<&str>,
    ) -> anyhow::Result<bool> {
        if content.is_none() && importance.is_none() && scope.is_none() {
            return Ok(false);
        }

        let result = sqlx::query(
            r#"
            UPDATE memory_items
            SET
                content = COALESCE(?2, content),
                importance = COALESCE(?3, importance),
                scope = COALESCE(?4, scope),
                updated_at = ?5
            WHERE id = ?1
            "#,
        )
        .bind(memory_id)
        .bind(content)
        .bind(importance)
        .bind(scope)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn delete_memory_item(&self, memory_id: &str) -> anyhow::Result<bool> {
        let mut tx = self.pool.begin().await?;

        sqlx::query(
            r#"
            DELETE FROM memory_links
            WHERE memory_item_id = ?1
            "#,
        )
        .bind(memory_id)
        .execute(&mut *tx)
        .await?;

        let result = sqlx::query(
            r#"
            DELETE FROM memory_items
            WHERE id = ?1
            "#,
        )
        .bind(memory_id)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn register_webhook(
        &self,
        url: &str,
        events: &[String],
        secret: &str,
    ) -> anyhow::Result<WebhookEndpoint> {
        let endpoint = WebhookEndpoint {
            id: Uuid::new_v4().to_string(),
            url: url.to_string(),
            events: events.to_vec(),
            // Encrypt at rest. The plaintext is preserved on the returned
            // record so callers (e.g. the API response) get the value the
            // operator actually configured.
            secret: secret.to_string(),
            enabled: true,
            created_at: Utc::now(),
        };

        let stored_secret = crate::crypto::encrypt_secret(secret, &webhook_secret_passphrase())?;

        sqlx::query(
            r#"
            INSERT INTO webhook_endpoints (id, url, events, secret, enabled, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
        )
        .bind(endpoint.id.clone())
        .bind(endpoint.url.clone())
        .bind(serde_json::to_string(&endpoint.events)?)
        .bind(stored_secret)
        .bind(if endpoint.enabled { 1_i64 } else { 0_i64 })
        .bind(endpoint.created_at.to_rfc3339())
        .execute(&self.pool)
        .await?;

        Ok(endpoint)
    }

    pub async fn list_webhooks(&self) -> anyhow::Result<Vec<WebhookEndpoint>> {
        let rows = sqlx::query(
            r#"
            SELECT id, url, events, secret, enabled, created_at
            FROM webhook_endpoints
            WHERE enabled = 1
            ORDER BY created_at ASC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        let passphrase = webhook_secret_passphrase();
        let mut endpoints = Vec::with_capacity(rows.len());
        for row in rows {
            let events_raw: String = row.get("events");
            let created_at_raw: String = row.get("created_at");
            let created_at = DateTime::parse_from_rfc3339(&created_at_raw)?.with_timezone(&Utc);
            let stored_secret: String = row.get("secret");
            let secret = crate::crypto::decrypt_secret(&stored_secret, &passphrase)
                .unwrap_or(stored_secret);

            endpoints.push(WebhookEndpoint {
                id: row.get("id"),
                url: row.get("url"),
                events: serde_json::from_str(&events_raw)?,
                secret,
                enabled: row.get::<i64, _>("enabled") != 0,
                created_at,
            });
        }

        Ok(endpoints)
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
        let created_at = Utc::now();
        let payload_raw = serde_json::to_string(payload)?;
        let result = sqlx::query(
            r#"
            INSERT INTO webhook_deliveries (
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
                created_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            "#,
        )
        .bind(endpoint_id)
        .bind(event)
        .bind(event_id)
        .bind(url)
        .bind(attempts as i64)
        .bind(if delivered { 1_i64 } else { 0_i64 })
        .bind(if dead_letter { 1_i64 } else { 0_i64 })
        .bind(status_code.map(|v| v as i64))
        .bind(error)
        .bind(payload_raw)
        .bind(created_at.to_rfc3339())
        .execute(&self.pool)
        .await?;

        Ok(WebhookDeliveryRecord {
            id: result.last_insert_rowid(),
            endpoint_id: endpoint_id.to_string(),
            event: event.to_string(),
            event_id: event_id.to_string(),
            url: url.to_string(),
            attempts,
            delivered,
            dead_letter,
            status_code,
            error: error.map(ToString::to_string),
            payload: payload.clone(),
            created_at,
        })
    }

    pub async fn list_webhook_deliveries(
        &self,
        dead_letter_only: bool,
        limit: usize,
    ) -> anyhow::Result<Vec<WebhookDeliveryRecord>> {
        let rows = if dead_letter_only {
            sqlx::query(
                r#"
                SELECT id, endpoint_id, event, event_id, url, attempts, delivered, dead_letter, status_code, error, payload, created_at
                FROM webhook_deliveries
                WHERE dead_letter = 1
                ORDER BY created_at DESC
                LIMIT ?1
                "#,
            )
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                r#"
                SELECT id, endpoint_id, event, event_id, url, attempts, delivered, dead_letter, status_code, error, payload, created_at
                FROM webhook_deliveries
                ORDER BY created_at DESC
                LIMIT ?1
                "#,
            )
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?
        };

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let created_at_raw: String = row.get("created_at");
            let payload_raw: String = row.get("payload");
            let status_code_raw: Option<i64> = row.get("status_code");

            out.push(WebhookDeliveryRecord {
                id: row.get("id"),
                endpoint_id: row.get("endpoint_id"),
                event: row.get("event"),
                event_id: row.get("event_id"),
                url: row.get("url"),
                attempts: row.get::<i64, _>("attempts").max(0) as u32,
                delivered: row.get::<i64, _>("delivered") != 0,
                dead_letter: row.get::<i64, _>("dead_letter") != 0,
                status_code: status_code_raw.map(|v| v as u16),
                error: row.get("error"),
                payload: serde_json::from_str(payload_raw.as_str())?,
                created_at: parse_rfc3339(created_at_raw.as_str())?,
            });
        }
        Ok(out)
    }

    pub async fn get_webhook_delivery(
        &self,
        delivery_id: i64,
    ) -> anyhow::Result<Option<WebhookDeliveryRecord>> {
        let row = sqlx::query(
            r#"
            SELECT id, endpoint_id, event, event_id, url, attempts, delivered, dead_letter, status_code, error, payload, created_at
            FROM webhook_deliveries
            WHERE id = ?1
            "#,
        )
        .bind(delivery_id)
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };
        let created_at_raw: String = row.get("created_at");
        let payload_raw: String = row.get("payload");
        let status_code_raw: Option<i64> = row.get("status_code");

        Ok(Some(WebhookDeliveryRecord {
            id: row.get("id"),
            endpoint_id: row.get("endpoint_id"),
            event: row.get("event"),
            event_id: row.get("event_id"),
            url: row.get("url"),
            attempts: row.get::<i64, _>("attempts").max(0) as u32,
            delivered: row.get::<i64, _>("delivered") != 0,
            dead_letter: row.get::<i64, _>("dead_letter") != 0,
            status_code: status_code_raw.map(|v| v as u16),
            error: row.get("error"),
            payload: serde_json::from_str(payload_raw.as_str())?,
            created_at: parse_rfc3339(created_at_raw.as_str())?,
        }))
    }

    pub async fn compact_session(&self, session_id: Uuid) -> anyhow::Result<()> {
        let rows = sqlx::query(
            r#"
            SELECT content
            FROM messages
            WHERE session_id = ?1
            ORDER BY id DESC
            LIMIT 25
            "#,
        )
        .bind(session_id.to_string())
        .fetch_all(&self.pool)
        .await?;

        if rows.is_empty() {
            return Ok(());
        }

        let summary = rows
            .iter()
            .map(|r| r.get::<String, _>("content"))
            .collect::<Vec<_>>()
            .join("\n")
            .chars()
            .take(1500)
            .collect::<String>();

        self.insert_memory_item(session_id, "session_summary", summary.as_str(), 0.8)
            .await?;
        Ok(())
    }

    pub async fn vacuum(&self) -> anyhow::Result<()> {
        sqlx::query("VACUUM").execute(&self.pool).await?;
        Ok(())
    }

    // --- Knowledge Base ---

    pub async fn insert_knowledge(
        &self,
        topic: &str,
        content: &str,
        importance: f64,
    ) -> anyhow::Result<String> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            r#"
            INSERT INTO knowledge_base (id, topic, content, importance, access_count, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6)
            "#,
        )
        .bind(&id)
        .bind(topic)
        .bind(content)
        .bind(importance)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    pub async fn search_knowledge(
        &self,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<(String, String, String, f64)>> {
        let pattern = format!("%{}%", query.to_lowercase());
        let rows = sqlx::query(
            r#"
            SELECT id, topic, content, importance
            FROM knowledge_base
            WHERE LOWER(content) LIKE ?1 OR LOWER(topic) LIKE ?1
            ORDER BY importance DESC, access_count DESC
            LIMIT ?2
            "#,
        )
        .bind(&pattern)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push((
                row.get("id"),
                row.get("topic"),
                row.get("content"),
                row.get("importance"),
            ));

            // Increment access count
            let id: String = row.get("id");
            let _ = sqlx::query(
                "UPDATE knowledge_base SET access_count = access_count + 1, updated_at = ?2 WHERE id = ?1",
            )
            .bind(&id)
            .bind(Utc::now().to_rfc3339())
            .execute(&self.pool)
            .await;
        }
        Ok(out)
    }

    pub async fn list_knowledge_items(
        &self,
        query_text: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeItem>> {
        let query_norm = query_text.unwrap_or("").trim().to_lowercase();
        let pattern = format!("%{}%", query_norm);

        let rows = sqlx::query(
            r#"
            SELECT id, topic, content, importance, access_count, created_at, updated_at
            FROM knowledge_base
            WHERE (?1 = '' OR LOWER(topic) LIKE ?2 OR LOWER(content) LIKE ?2)
            ORDER BY importance DESC, updated_at DESC
            LIMIT ?3
            "#,
        )
        .bind(query_norm)
        .bind(pattern)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let created_at_raw: String = row.get("created_at");
            let updated_at_raw: String = row.get("updated_at");
            let access_count_raw: i64 = row.get("access_count");
            out.push(KnowledgeItem {
                id: row.get("id"),
                topic: row.get("topic"),
                content: row.get("content"),
                importance: row.get("importance"),
                access_count: access_count_raw.max(0) as u64,
                created_at: parse_rfc3339(created_at_raw.as_str())?,
                updated_at: parse_rfc3339(updated_at_raw.as_str())?,
            });
        }

        Ok(out)
    }

    pub async fn update_knowledge_item(
        &self,
        knowledge_id: &str,
        topic: Option<&str>,
        content: Option<&str>,
        importance: Option<f64>,
    ) -> anyhow::Result<bool> {
        if topic.is_none() && content.is_none() && importance.is_none() {
            return Ok(false);
        }

        let result = sqlx::query(
            r#"
            UPDATE knowledge_base
            SET
                topic = COALESCE(?2, topic),
                content = COALESCE(?3, content),
                importance = COALESCE(?4, importance),
                updated_at = ?5
            WHERE id = ?1
            "#,
        )
        .bind(knowledge_id)
        .bind(topic)
        .bind(content)
        .bind(importance)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    // --- Cron Schedule CRUD ---

    pub async fn create_schedule(&self, schedule: &CronSchedule) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO cron_schedules (id, workflow_id, cron_expr, enabled, parameters, last_run_at, next_run_at, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
        )
        .bind(schedule.id.to_string())
        .bind(&schedule.workflow_id)
        .bind(&schedule.cron_expr)
        .bind(if schedule.enabled { 1_i64 } else { 0_i64 })
        .bind(schedule.parameters.as_ref().map(|p| serde_json::to_string(p).unwrap_or_default()))
        .bind(schedule.last_run_at.map(|t| t.to_rfc3339()))
        .bind(schedule.next_run_at.map(|t| t.to_rfc3339()))
        .bind(schedule.created_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_schedules(&self, limit: usize) -> anyhow::Result<Vec<CronSchedule>> {
        let rows = sqlx::query(
            r#"
            SELECT id, workflow_id, cron_expr, enabled, parameters, last_run_at, next_run_at, created_at
            FROM cron_schedules
            ORDER BY created_at DESC
            LIMIT ?1
            "#,
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(parse_schedule_row(&row)?);
        }
        Ok(out)
    }

    pub async fn get_schedule(&self, id: Uuid) -> anyhow::Result<Option<CronSchedule>> {
        let row = sqlx::query(
            r#"
            SELECT id, workflow_id, cron_expr, enabled, parameters, last_run_at, next_run_at, created_at
            FROM cron_schedules WHERE id = ?1
            "#,
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(r) => Ok(Some(parse_schedule_row(&r)?)),
            None => Ok(None),
        }
    }

    pub async fn update_schedule(
        &self,
        id: Uuid,
        cron_expr: Option<&str>,
        enabled: Option<bool>,
        parameters: Option<Option<&serde_json::Value>>,
        next_run_at: Option<Option<DateTime<Utc>>>,
    ) -> anyhow::Result<()> {
        // Build dynamic update
        let mut sets = Vec::new();
        let mut binds: Vec<String> = Vec::new();

        if let Some(expr) = cron_expr {
            sets.push(format!("cron_expr = ?{}", binds.len() + 2));
            binds.push(expr.to_string());
        }
        if let Some(en) = enabled {
            sets.push(format!("enabled = ?{}", binds.len() + 2));
            binds.push(if en { "1".to_string() } else { "0".to_string() });
        }
        if let Some(params) = &parameters {
            sets.push(format!("parameters = ?{}", binds.len() + 2));
            binds.push(
                params
                    .map(|p| serde_json::to_string(p).unwrap_or_default())
                    .unwrap_or_default(),
            );
        }
        if let Some(next) = &next_run_at {
            sets.push(format!("next_run_at = ?{}", binds.len() + 2));
            binds.push(next.map(|t| t.to_rfc3339()).unwrap_or_default());
        }

        if sets.is_empty() {
            return Ok(());
        }

        let sql = format!(
            "UPDATE cron_schedules SET {} WHERE id = ?1",
            sets.join(", ")
        );

        let mut query = sqlx::query(&sql).bind(id.to_string());
        for b in &binds {
            query = query.bind(b.as_str());
        }
        query.execute(&self.pool).await?;
        Ok(())
    }

    pub async fn update_schedule_last_run(
        &self,
        id: Uuid,
        last_run_at: DateTime<Utc>,
        next_run_at: Option<DateTime<Utc>>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            UPDATE cron_schedules
            SET last_run_at = ?2, next_run_at = ?3
            WHERE id = ?1
            "#,
        )
        .bind(id.to_string())
        .bind(last_run_at.to_rfc3339())
        .bind(next_run_at.map(|t| t.to_rfc3339()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_schedule(&self, id: Uuid) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM cron_schedules WHERE id = ?1")
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn list_due_schedules(
        &self,
        now: DateTime<Utc>,
    ) -> anyhow::Result<Vec<CronSchedule>> {
        let rows = sqlx::query(
            r#"
            SELECT id, workflow_id, cron_expr, enabled, parameters, last_run_at, next_run_at, created_at
            FROM cron_schedules
            WHERE enabled = 1 AND next_run_at IS NOT NULL AND next_run_at <= ?1
            ORDER BY next_run_at ASC
            "#,
        )
        .bind(now.to_rfc3339())
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(parse_schedule_row(&row)?);
        }
        Ok(out)
    }

    // --- Workflow CRUD ---

    pub async fn save_workflow(
        &self,
        template: &crate::types::WorkflowTemplate,
    ) -> anyhow::Result<()> {
        let graph_json = serde_json::to_string(&template.graph_template)?;
        let params_json = serde_json::to_string(&template.parameters)?;
        sqlx::query(
            r#"
            INSERT OR REPLACE INTO workflow_templates
            (id, name, description, source_run_id, graph_json, parameters_json, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
        )
        .bind(&template.id)
        .bind(&template.name)
        .bind(&template.description)
        .bind(template.source_run_id.map(|u| u.to_string()))
        .bind(&graph_json)
        .bind(&params_json)
        .bind(template.created_at.to_rfc3339())
        .bind(template.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_workflow(
        &self,
        id: &str,
    ) -> anyhow::Result<Option<crate::types::WorkflowTemplate>> {
        let row = sqlx::query(
            r#"SELECT id, name, description, source_run_id, graph_json, parameters_json, created_at, updated_at
               FROM workflow_templates WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(r) => Ok(Some(parse_workflow_row(&r)?)),
            None => Ok(None),
        }
    }

    pub async fn list_workflows(
        &self,
        limit: usize,
    ) -> anyhow::Result<Vec<crate::types::WorkflowTemplate>> {
        let rows = sqlx::query(
            r#"SELECT id, name, description, source_run_id, graph_json, parameters_json, created_at, updated_at
               FROM workflow_templates ORDER BY updated_at DESC LIMIT ?1"#,
        )
        .bind(limit.min(500) as i64)
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            out.push(parse_workflow_row(r)?);
        }
        Ok(out)
    }

    pub async fn delete_workflow(&self, id: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM workflow_templates WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn load_settings(&self) -> anyhow::Result<Option<crate::types::AppSettings>> {
        let row = sqlx::query("SELECT data FROM app_settings WHERE id = 1")
            .fetch_optional(&self.pool)
            .await?;
        match row {
            Some(r) => {
                let json: String = r.get("data");
                Ok(Some(serde_json::from_str(&json)?))
            }
            None => Ok(None),
        }
    }

    // --- Workspaces ---

    pub async fn insert_workspace(&self, ws: &Workspace) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO workspaces
                (id, slug, name, kind, root_path, description, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
        )
        .bind(&ws.id)
        .bind(&ws.slug)
        .bind(&ws.name)
        .bind(ws.kind.to_string())
        .bind(&ws.root_path)
        .bind(ws.description.as_deref())
        .bind(ws.created_at.to_rfc3339())
        .bind(ws.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_workspace(&self, id: &str) -> anyhow::Result<Option<Workspace>> {
        let row = sqlx::query(
            r#"SELECT id, slug, name, kind, root_path, description, created_at, updated_at
               FROM workspaces WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|r| workspace_from_row(&r).ok()))
    }

    pub async fn get_workspace_by_slug(&self, slug: &str) -> anyhow::Result<Option<Workspace>> {
        let row = sqlx::query(
            r#"SELECT id, slug, name, kind, root_path, description, created_at, updated_at
               FROM workspaces WHERE slug = ?1"#,
        )
        .bind(slug)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|r| workspace_from_row(&r).ok()))
    }

    pub async fn list_workspaces(
        &self,
        kind: Option<&str>,
    ) -> anyhow::Result<Vec<Workspace>> {
        let rows = sqlx::query(
            r#"SELECT id, slug, name, kind, root_path, description, created_at, updated_at
               FROM workspaces
               WHERE (?1 IS NULL OR kind = ?1)
               ORDER BY created_at DESC"#,
        )
        .bind(kind)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            if let Ok(ws) = workspace_from_row(&row) {
                out.push(ws);
            }
        }
        Ok(out)
    }

    pub async fn update_workspace(
        &self,
        id: &str,
        name: Option<&str>,
        description: Option<Option<&str>>,
    ) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        if let Some(name) = name {
            sqlx::query("UPDATE workspaces SET name = ?1, updated_at = ?2 WHERE id = ?3")
                .bind(name)
                .bind(&now)
                .bind(id)
                .execute(&self.pool)
                .await?;
        }
        if let Some(desc) = description {
            sqlx::query("UPDATE workspaces SET description = ?1, updated_at = ?2 WHERE id = ?3")
                .bind(desc)
                .bind(&now)
                .bind(id)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    pub async fn delete_workspace(&self, id: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM workspaces WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Set or clear the workspace association for a session. Used after
    /// `POST /v1/sessions { workspace_id }` and during workspace soft
    /// reattachment.
    pub async fn set_session_workspace(
        &self,
        session_id: Uuid,
        workspace_id: Option<&str>,
    ) -> anyhow::Result<()> {
        sqlx::query("UPDATE sessions SET workspace_id = ?1 WHERE id = ?2")
            .bind(workspace_id)
            .bind(session_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn list_workspace_sessions(
        &self,
        workspace_id: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<Uuid>> {
        let rows = sqlx::query(
            r#"SELECT id FROM sessions
               WHERE workspace_id = ?1
               ORDER BY created_at DESC
               LIMIT ?2"#,
        )
        .bind(workspace_id)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let raw: String = row.get("id");
            if let Ok(uuid) = Uuid::parse_str(&raw) {
                out.push(uuid);
            }
        }
        Ok(out)
    }

    // --- Workspace Files ---

    pub async fn upsert_workspace_file(&self, file: &WorkspaceFile) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO workspace_files
                (id, workspace_id, session_id, relative_path, size_bytes, mime, sha256,
                 created_by, created_by_persona, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            ON CONFLICT(workspace_id, relative_path) DO UPDATE SET
                size_bytes = excluded.size_bytes,
                mime = excluded.mime,
                sha256 = excluded.sha256,
                created_by = excluded.created_by,
                created_by_persona = excluded.created_by_persona,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(&file.id)
        .bind(&file.workspace_id)
        .bind(file.session_id.map(|u| u.to_string()))
        .bind(&file.relative_path)
        .bind(file.size_bytes as i64)
        .bind(file.mime.as_deref())
        .bind(file.sha256.as_deref())
        .bind(file.created_by.to_string())
        .bind(file.created_by_persona.as_deref())
        .bind(file.created_at.to_rfc3339())
        .bind(file.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_workspace_file(
        &self,
        workspace_id: &str,
        relative_path: &str,
    ) -> anyhow::Result<Option<WorkspaceFile>> {
        let row = sqlx::query(
            r#"SELECT id, workspace_id, session_id, relative_path, size_bytes, mime, sha256,
                      created_by, created_by_persona, created_at, updated_at
               FROM workspace_files
               WHERE workspace_id = ?1 AND relative_path = ?2"#,
        )
        .bind(workspace_id)
        .bind(relative_path)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|r| workspace_file_from_row(&r).ok()))
    }

    pub async fn list_workspace_files(
        &self,
        workspace_id: &str,
        session_id: Option<Uuid>,
        prefix: Option<&str>,
    ) -> anyhow::Result<Vec<WorkspaceFile>> {
        let rows = sqlx::query(
            r#"SELECT id, workspace_id, session_id, relative_path, size_bytes, mime, sha256,
                      created_by, created_by_persona, created_at, updated_at
               FROM workspace_files
               WHERE workspace_id = ?1
                 AND (?2 IS NULL OR session_id = ?2)
                 AND (?3 IS NULL OR relative_path LIKE ?3 || '%')
               ORDER BY relative_path ASC"#,
        )
        .bind(workspace_id)
        .bind(session_id.map(|u| u.to_string()))
        .bind(prefix)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            if let Ok(f) = workspace_file_from_row(&row) {
                out.push(f);
            }
        }
        Ok(out)
    }

    pub async fn delete_workspace_file(
        &self,
        workspace_id: &str,
        relative_path: &str,
    ) -> anyhow::Result<bool> {
        let result =
            sqlx::query("DELETE FROM workspace_files WHERE workspace_id = ?1 AND relative_path = ?2")
                .bind(workspace_id)
                .bind(relative_path)
                .execute(&self.pool)
                .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Cross-session memory lookup by `scope` prefix. Persona-private and
    /// team-shared memories use namespaced scopes (`persona:<name>:...`,
    /// `team:<workspace_id>:...`); this lets the context builder pull
    /// every entry under a namespace without joining anything.
    pub async fn list_memory_by_scope_prefix(
        &self,
        prefix: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<SessionMemoryItem>> {
        let pattern = format!("{prefix}%");
        let rows = sqlx::query(
            r#"SELECT id, session_id, content, importance, scope, created_at, updated_at
               FROM memory_items
               WHERE scope LIKE ?1
               ORDER BY updated_at DESC
               LIMIT ?2"#,
        )
        .bind(pattern)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let session_raw: String = row.get("session_id");
            let created_at_raw: String = row.get("created_at");
            let updated_at_raw: String = row.get("updated_at");
            out.push(SessionMemoryItem {
                id: row.get("id"),
                session_id: Uuid::parse_str(&session_raw)
                    .unwrap_or_else(|_| Uuid::nil()),
                scope: row.get("scope"),
                content: row.get("content"),
                importance: row.get("importance"),
                source_refs: Vec::new(),
                created_at: parse_rfc3339(&created_at_raw)?,
                updated_at: parse_rfc3339(&updated_at_raw)?,
            });
        }
        Ok(out)
    }

    // --- Meetings ---

    pub async fn insert_meeting(&self, m: &Meeting) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO meetings
                (id, workspace_id, session_id, topic, participants_json, status,
                 created_by, created_at, closed_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            "#,
        )
        .bind(&m.id)
        .bind(&m.workspace_id)
        .bind(m.session_id.map(|u| u.to_string()))
        .bind(&m.topic)
        .bind(serde_json::to_string(&m.participants)?)
        .bind(m.status.to_string())
        .bind(&m.created_by)
        .bind(m.created_at.to_rfc3339())
        .bind(m.closed_at.map(|d| d.to_rfc3339()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn close_meeting(&self, id: &str) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE meetings SET status = 'closed', closed_at = ?1 WHERE id = ?2",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_meeting(&self, id: &str) -> anyhow::Result<Option<Meeting>> {
        let row = sqlx::query(
            r#"SELECT id, workspace_id, session_id, topic, participants_json, status,
                      created_by, created_at, closed_at
               FROM meetings WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|r| meeting_from_row(&r).ok()))
    }

    pub async fn list_workspace_meetings(
        &self,
        workspace_id: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<Meeting>> {
        let rows = sqlx::query(
            r#"SELECT id, workspace_id, session_id, topic, participants_json, status,
                      created_by, created_at, closed_at
               FROM meetings
               WHERE workspace_id = ?1
               ORDER BY created_at DESC
               LIMIT ?2"#,
        )
        .bind(workspace_id)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            if let Ok(m) = meeting_from_row(&row) {
                out.push(m);
            }
        }
        Ok(out)
    }

    pub async fn append_meeting_message(
        &self,
        meeting_id: &str,
        speaker_kind: MeetingSpeakerKind,
        speaker_name: &str,
        content: &str,
    ) -> anyhow::Result<i64> {
        let result = sqlx::query(
            r#"INSERT INTO meeting_messages
                (meeting_id, speaker_kind, speaker_name, content, created_at)
               VALUES (?1, ?2, ?3, ?4, ?5)"#,
        )
        .bind(meeting_id)
        .bind(speaker_kind.to_string())
        .bind(speaker_name)
        .bind(content)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(result.last_insert_rowid())
    }

    pub async fn list_meeting_messages(
        &self,
        meeting_id: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<MeetingMessage>> {
        let rows = sqlx::query(
            r#"SELECT id, meeting_id, speaker_kind, speaker_name, content, created_at
               FROM meeting_messages
               WHERE meeting_id = ?1
               ORDER BY id ASC
               LIMIT ?2"#,
        )
        .bind(meeting_id)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            if let Ok(m) = meeting_message_from_row(&row) {
                out.push(m);
            }
        }
        Ok(out)
    }

    pub async fn save_settings(&self, settings: &crate::types::AppSettings) -> anyhow::Result<()> {
        let json = serde_json::to_string(settings)?;
        sqlx::query(
            "INSERT OR REPLACE INTO app_settings (id, data, updated_at) VALUES (1, ?1, ?2)",
        )
        .bind(&json)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // --- Coder Sessions ---

    pub async fn insert_coder_session(
        &self,
        id: &str,
        run_id: Uuid,
        node_id: &str,
        backend: &str,
        terminal_session_id: Option<&str>,
        working_dir: Option<&str>,
        status: &str,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO coder_sessions (id, run_id, node_id, backend, terminal_session_id, working_dir, status, started_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
        )
        .bind(id)
        .bind(run_id.to_string())
        .bind(node_id)
        .bind(backend)
        .bind(terminal_session_id)
        .bind(working_dir)
        .bind(status)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_coder_session_completed(
        &self,
        id: &str,
        status: &str,
        exit_code: Option<i32>,
        files_changed_json: Option<&str>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            UPDATE coder_sessions
            SET status = ?1, exit_code = ?2, files_changed_json = ?3, ended_at = ?4
            WHERE id = ?5
            "#,
        )
        .bind(status)
        .bind(exit_code)
        .bind(files_changed_json)
        .bind(Utc::now().to_rfc3339())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_coder_sessions_for_run(
        &self,
        run_id: Uuid,
    ) -> anyhow::Result<Vec<serde_json::Value>> {
        let rows =
            sqlx::query(r#"SELECT * FROM coder_sessions WHERE run_id = ?1 ORDER BY started_at"#)
                .bind(run_id.to_string())
                .fetch_all(&self.pool)
                .await?;

        let mut result = Vec::new();
        for r in rows {
            let id: String = r.get("id");
            let node_id: String = r.get("node_id");
            let backend: String = r.get("backend");
            let terminal_sid: Option<String> = r.get("terminal_session_id");
            let working_dir: Option<String> = r.get("working_dir");
            let worktree_branch: Option<String> = r.get("worktree_branch");
            let status: String = r.get("status");
            let exit_code: Option<i32> = r.get("exit_code");
            let files_json: Option<String> = r.get("files_changed_json");
            let started_at: String = r.get("started_at");
            let ended_at: Option<String> = r.get("ended_at");
            result.push(serde_json::json!({
                "id": id,
                "run_id": run_id.to_string(),
                "node_id": node_id,
                "backend": backend,
                "terminal_session_id": terminal_sid,
                "working_dir": working_dir,
                "worktree_branch": worktree_branch,
                "status": status,
                "exit_code": exit_code,
                "files_changed": files_json.and_then(|j| serde_json::from_str::<serde_json::Value>(&j).ok()),
                "started_at": started_at,
                "ended_at": ended_at,
            }));
        }
        Ok(result)
    }
}

fn parse_schedule_row(r: &sqlx::sqlite::SqliteRow) -> anyhow::Result<CronSchedule> {
    let id_raw: String = r.get("id");
    let params_raw: Option<String> = r.get("parameters");
    let last_run_raw: Option<String> = r.get("last_run_at");
    let next_run_raw: Option<String> = r.get("next_run_at");
    let created_at_raw: String = r.get("created_at");

    Ok(CronSchedule {
        id: Uuid::parse_str(&id_raw)?,
        workflow_id: r.get("workflow_id"),
        cron_expr: r.get("cron_expr"),
        enabled: r.get::<i64, _>("enabled") != 0,
        parameters: params_raw
            .filter(|s| !s.is_empty())
            .map(|s| serde_json::from_str(&s))
            .transpose()?,
        last_run_at: last_run_raw.as_deref().map(parse_rfc3339).transpose()?,
        next_run_at: next_run_raw
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(parse_rfc3339)
            .transpose()?,
        created_at: parse_rfc3339(&created_at_raw)?,
    })
}

fn parse_workflow_row(
    r: &sqlx::sqlite::SqliteRow,
) -> anyhow::Result<crate::types::WorkflowTemplate> {
    let graph_json: String = r.get("graph_json");
    let params_json: String = r.get("parameters_json");
    let source_run_str: Option<String> = r.get("source_run_id");
    Ok(crate::types::WorkflowTemplate {
        id: r.get("id"),
        name: r.get("name"),
        description: r.get("description"),
        source_run_id: source_run_str.and_then(|s| Uuid::parse_str(&s).ok()),
        graph_template: serde_json::from_str(&graph_json)?,
        parameters: serde_json::from_str(&params_json)?,
        created_at: parse_rfc3339(&r.get::<String, _>("created_at"))?,
        updated_at: parse_rfc3339(&r.get::<String, _>("updated_at"))?,
        source: Default::default(),
    })
}

/// Convert raw aggregate columns into the `(token_usage, cost)` tuple used by
/// SessionSummary. Returns None for both when nothing was reported (so the
/// summary stays slim for sessions that haven't run a metered model yet).
fn build_session_totals(
    total_input: i64,
    total_output: i64,
    total_cost: f64,
) -> (Option<crate::types::TokenUsage>, Option<f64>) {
    let token_usage = if total_input > 0 || total_output > 0 {
        Some(crate::types::TokenUsage {
            input_tokens: total_input.max(0).min(u32::MAX as i64) as u32,
            output_tokens: total_output.max(0).min(u32::MAX as i64) as u32,
        })
    } else {
        None
    };
    let cost = if total_cost > 0.0 {
        Some(total_cost)
    } else {
        None
    };
    (token_usage, cost)
}

/// Read the master passphrase used to encrypt webhook secrets at rest. Falls
/// back to a fixed dev value so local boot still works; production deploys
/// should set `CLI_AGENT_DB_KEY`.
fn webhook_secret_passphrase() -> String {
    std::env::var("CLI_AGENT_DB_KEY").unwrap_or_else(|_| "cli-agent-dev-key".to_string())
}

fn parse_rfc3339(value: &str) -> anyhow::Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc))
}

fn workspace_from_row(row: &sqlx::sqlite::SqliteRow) -> anyhow::Result<Workspace> {
    let kind_raw: String = row.get("kind");
    let kind = WorkspaceKind::parse(&kind_raw)
        .ok_or_else(|| anyhow::anyhow!("invalid workspace kind: {kind_raw}"))?;
    let created_at_raw: String = row.get("created_at");
    let updated_at_raw: String = row.get("updated_at");
    Ok(Workspace {
        id: row.get("id"),
        slug: row.get("slug"),
        name: row.get("name"),
        kind,
        root_path: row.get("root_path"),
        description: row.try_get("description").ok(),
        created_at: parse_rfc3339(&created_at_raw)?,
        updated_at: parse_rfc3339(&updated_at_raw)?,
    })
}

fn meeting_from_row(row: &sqlx::sqlite::SqliteRow) -> anyhow::Result<Meeting> {
    let session_raw: Option<String> = row.try_get("session_id").ok();
    let session_id = match session_raw {
        Some(s) if !s.is_empty() => Some(Uuid::parse_str(&s)?),
        _ => None,
    };
    let status_raw: String = row.get("status");
    let status = match status_raw.as_str() {
        "open" => MeetingStatus::Open,
        "closed" => MeetingStatus::Closed,
        other => return Err(anyhow::anyhow!("invalid meeting status: {other}")),
    };
    let created_at_raw: String = row.get("created_at");
    let closed_at_raw: Option<String> = row.try_get("closed_at").ok();
    let participants_raw: String = row.get("participants_json");
    let participants: Vec<String> =
        serde_json::from_str(&participants_raw).unwrap_or_default();
    Ok(Meeting {
        id: row.get("id"),
        workspace_id: row.get("workspace_id"),
        session_id,
        topic: row.get("topic"),
        participants,
        status,
        created_by: row.get("created_by"),
        created_at: parse_rfc3339(&created_at_raw)?,
        closed_at: closed_at_raw
            .as_deref()
            .map(parse_rfc3339)
            .transpose()?,
    })
}

fn meeting_message_from_row(row: &sqlx::sqlite::SqliteRow) -> anyhow::Result<MeetingMessage> {
    let kind_raw: String = row.get("speaker_kind");
    let speaker_kind = match kind_raw.as_str() {
        "user" => MeetingSpeakerKind::User,
        "persona" => MeetingSpeakerKind::Persona,
        "system" => MeetingSpeakerKind::System,
        other => return Err(anyhow::anyhow!("invalid speaker_kind: {other}")),
    };
    let created_at_raw: String = row.get("created_at");
    Ok(MeetingMessage {
        id: row.get("id"),
        meeting_id: row.get("meeting_id"),
        speaker_kind,
        speaker_name: row.get("speaker_name"),
        content: row.get("content"),
        created_at: parse_rfc3339(&created_at_raw)?,
    })
}

fn workspace_file_from_row(row: &sqlx::sqlite::SqliteRow) -> anyhow::Result<WorkspaceFile> {
    let session_raw: Option<String> = row.try_get("session_id").ok();
    let session_id = match session_raw {
        Some(s) if !s.is_empty() => Some(Uuid::parse_str(&s)?),
        _ => None,
    };
    let created_by_raw: String = row.get("created_by");
    let created_by = match created_by_raw.as_str() {
        "user" => WorkspaceFileCreatedBy::User,
        "persona" => WorkspaceFileCreatedBy::Persona,
        "system" => WorkspaceFileCreatedBy::System,
        other => return Err(anyhow::anyhow!("invalid created_by: {other}")),
    };
    let size: i64 = row.get("size_bytes");
    let created_at_raw: String = row.get("created_at");
    let updated_at_raw: String = row.get("updated_at");
    Ok(WorkspaceFile {
        id: row.get("id"),
        workspace_id: row.get("workspace_id"),
        session_id,
        relative_path: row.get("relative_path"),
        size_bytes: size.max(0) as u64,
        mime: row.try_get("mime").ok(),
        sha256: row.try_get("sha256").ok(),
        created_by,
        created_by_persona: row.try_get("created_by_persona").ok(),
        created_at: parse_rfc3339(&created_at_raw)?,
        updated_at: parse_rfc3339(&updated_at_raw)?,
    })
}

fn tokenize_lexical(text: &str) -> HashSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn token_overlap_score(query_tokens: &HashSet<String>, content: &str) -> f64 {
    if query_tokens.is_empty() {
        return 0.0;
    }

    let content_tokens = tokenize_lexical(content);
    if content_tokens.is_empty() {
        return 0.0;
    }

    let overlap = query_tokens
        .iter()
        .filter(|token| content_tokens.contains(*token))
        .count() as f64;
    (overlap / query_tokens.len() as f64).clamp(0.0, 1.0)
}

fn parse_run_action(value: &str) -> anyhow::Result<RunActionType> {
    let action = match value {
        "run_queued" => RunActionType::RunQueued,
        "run_started" => RunActionType::RunStarted,
        "run_cancel_requested" => RunActionType::RunCancelRequested,
        "run_pause_requested" => RunActionType::RunPauseRequested,
        "run_resumed" => RunActionType::RunResumed,
        "graph_initialized" => RunActionType::GraphInitialized,
        "node_progress" => RunActionType::NodeProgress,
        "node_started" => RunActionType::NodeStarted,
        "node_completed" => RunActionType::NodeCompleted,
        "node_failed" => RunActionType::NodeFailed,
        "node_skipped" => RunActionType::NodeSkipped,
        "dynamic_node_added" => RunActionType::DynamicNodeAdded,
        "graph_completed" => RunActionType::GraphCompleted,
        "model_selected" => RunActionType::ModelSelected,
        "run_finished" => RunActionType::RunFinished,
        "webhook_dispatched" => RunActionType::WebhookDispatched,
        "mcp_tool_called" => RunActionType::McpToolCalled,
        "node_token_chunk" => RunActionType::NodeTokenChunk,
        "subtask_planned" => RunActionType::SubtaskPlanned,
        "verification_started" => RunActionType::VerificationStarted,
        "verification_complete" => RunActionType::VerificationComplete,
        "replan_triggered" => RunActionType::ReplanTriggered,
        "recovery_phase_started" => RunActionType::RecoveryPhaseStarted,
        "recovery_phase_completed" => RunActionType::RecoveryPhaseCompleted,
        "terminal_suggested" => RunActionType::TerminalSuggested,
        "coder_session_started" => RunActionType::CoderSessionStarted,
        "coder_session_completed" => RunActionType::CoderSessionCompleted,
        "validation_passed" => RunActionType::ValidationPassed,
        "validation_failed" => RunActionType::ValidationFailed,
        "git_commit_created" => RunActionType::GitCommitCreated,
        "git_push_completed" => RunActionType::GitPushCompleted,
        "repo_clone_completed" => RunActionType::RepoCloneCompleted,
        "repo_analysis_completed" => RunActionType::RepoAnalysisCompleted,
        "interactive_step" => RunActionType::InteractiveStep,
        "github_issue_created" => RunActionType::GitHubIssueCreated,
        "github_issue_commented" => RunActionType::GitHubIssueCommented,
        "github_issue_closed" => RunActionType::GitHubIssueClosed,
        "github_pr_created" => RunActionType::GitHubPrCreated,
        "github_pr_reviewed" => RunActionType::GitHubPrReviewed,
        "github_pr_commented" => RunActionType::GitHubPrCommented,
        "github_pr_merged" => RunActionType::GitHubPrMerged,
        "github_branch_created" => RunActionType::GitHubBranchCreated,
        _ => {
            return Err(anyhow::anyhow!("unknown run action event type: {value}"));
        }
    };
    Ok(action)
}

// --- GitHub Activity persistence ---

impl SqliteStore {
    /// Record a GitHub activity performed by an agent persona.
    pub async fn record_github_activity(
        &self,
        id: &str,
        run_id: &str,
        session_id: &str,
        persona_name: &str,
        activity_type: &str,
        github_url: Option<&str>,
        target_number: Option<i64>,
        title: &str,
        body_preview: &str,
        metadata: &serde_json::Value,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO github_activities (id, run_id, session_id, persona_name, activity_type, github_url, target_number, title, body_preview, metadata, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, datetime('now'))
            "#,
        )
        .bind(id)
        .bind(run_id)
        .bind(session_id)
        .bind(persona_name)
        .bind(activity_type)
        .bind(github_url)
        .bind(target_number)
        .bind(title)
        .bind(&body_preview[..body_preview.len().min(200)])
        .bind(serde_json::to_string(metadata).unwrap_or_default())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// List GitHub activities, optionally filtered by persona and/or run_id.
    pub async fn list_github_activities(
        &self,
        persona: Option<&str>,
        run_id: Option<&str>,
        limit: i64,
    ) -> anyhow::Result<Vec<serde_json::Value>> {
        let mut conditions = vec!["1=1".to_string()];
        if let Some(p) = persona {
            conditions.push(format!("persona_name = '{}'", p.replace('\'', "''")));
        }
        if let Some(r) = run_id {
            conditions.push(format!("run_id = '{}'", r.replace('\'', "''")));
        }
        let where_clause = conditions.join(" AND ");

        let query = format!(
            "SELECT id, run_id, session_id, persona_name, activity_type, github_url, target_number, title, body_preview, metadata, created_at \
             FROM github_activities WHERE {} ORDER BY created_at DESC LIMIT {}",
            where_clause, limit
        );

        let rows = sqlx::query(&query).fetch_all(&self.pool).await?;

        let mut result = Vec::with_capacity(rows.len());
        for row in &rows {
            use sqlx::Row;
            result.push(serde_json::json!({
                "id": row.get::<String, _>("id"),
                "run_id": row.get::<String, _>("run_id"),
                "session_id": row.get::<String, _>("session_id"),
                "persona_name": row.get::<String, _>("persona_name"),
                "activity_type": row.get::<String, _>("activity_type"),
                "github_url": row.get::<Option<String>, _>("github_url"),
                "target_number": row.get::<Option<i64>, _>("target_number"),
                "title": row.get::<String, _>("title"),
                "body_preview": row.get::<String, _>("body_preview"),
                "metadata": row.get::<String, _>("metadata"),
                "created_at": row.get::<String, _>("created_at"),
            }));
        }
        Ok(result)
    }

    /// Get GitHub activity statistics per persona.
    pub async fn github_activity_stats(&self) -> anyhow::Result<Vec<serde_json::Value>> {
        let rows = sqlx::query(
            r#"
            SELECT persona_name, activity_type, COUNT(*) as count
            FROM github_activities
            GROUP BY persona_name, activity_type
            ORDER BY persona_name, count DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        let mut result = Vec::new();
        for row in &rows {
            use sqlx::Row;
            result.push(serde_json::json!({
                "persona_name": row.get::<String, _>("persona_name"),
                "activity_type": row.get::<String, _>("activity_type"),
                "count": row.get::<i64, _>("count"),
            }));
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_run_action_accepts_extended_action_set() {
        assert!(matches!(
            parse_run_action("node_progress").unwrap(),
            RunActionType::NodeProgress
        ));
        assert!(matches!(
            parse_run_action("repo_clone_completed").unwrap(),
            RunActionType::RepoCloneCompleted
        ));
        assert!(matches!(
            parse_run_action("repo_analysis_completed").unwrap(),
            RunActionType::RepoAnalysisCompleted
        ));
        assert!(matches!(
            parse_run_action("interactive_step").unwrap(),
            RunActionType::InteractiveStep
        ));
        assert!(matches!(
            parse_run_action("recovery_phase_started").unwrap(),
            RunActionType::RecoveryPhaseStarted
        ));
        assert!(matches!(
            parse_run_action("recovery_phase_completed").unwrap(),
            RunActionType::RecoveryPhaseCompleted
        ));
    }

    #[test]
    fn recovery_phase_action_types_round_trip_to_string() {
        assert_eq!(
            RunActionType::RecoveryPhaseStarted.to_string(),
            "recovery_phase_started"
        );
        assert_eq!(
            RunActionType::RecoveryPhaseCompleted.to_string(),
            "recovery_phase_completed"
        );
    }

    fn temp_db_url() -> String {
        let path =
            std::env::temp_dir().join(format!("cli-agent-memory-test-{}.db", Uuid::new_v4()));
        format!("sqlite://{}", path.display())
    }

    #[tokio::test]
    async fn batch_insert_run_action_events_persists_all_rows() {
        let store = SqliteStore::connect(temp_db_url().as_str())
            .await
            .expect("store");
        let run_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        store
            .create_session(session_id, "general")
            .await
            .expect("create session");

        let inputs: Vec<RunActionEventInput> = (0..7)
            .map(|i| RunActionEventInput {
                run_id,
                session_id,
                action: RunActionType::NodeTokenChunk,
                actor_type: Some("runtime".to_string()),
                actor_id: Some(format!("node-{i}")),
                cause_event_id: None,
                payload: serde_json::json!({"i": i}),
            })
            .collect();

        store
            .batch_insert_run_action_events(&inputs)
            .await
            .expect("batch insert");

        let listed = store
            .list_run_action_events(run_id, 100)
            .await
            .expect("list");
        assert_eq!(listed.len(), 7);
        // Sequence numbers must be monotonic so the SSE replay order matches
        // the order the producer queued them.
        let seqs: Vec<i64> = listed.iter().map(|e| e.seq).collect();
        let mut sorted = seqs.clone();
        sorted.sort();
        assert_eq!(seqs, sorted);
    }

    #[tokio::test]
    async fn batch_insert_empty_is_noop() {
        let store = SqliteStore::connect(temp_db_url().as_str())
            .await
            .expect("store");
        store
            .batch_insert_run_action_events(&[])
            .await
            .expect("empty batch ok");
    }

    #[tokio::test]
    async fn session_memory_crud_roundtrip() {
        let store = SqliteStore::connect(temp_db_url().as_str())
            .await
            .expect("store");
        let session_id = Uuid::new_v4();
        store
            .create_session(session_id, "general")
            .await
            .expect("create session");

        let memory_id = store
            .insert_memory_item(session_id, "agent_output", "tool result alpha", 0.7)
            .await
            .expect("insert memory");
        store
            .link_memory(memory_id.as_str(), "node:tool_caller")
            .await
            .expect("link");

        let list = store
            .list_session_memory_items(session_id, Some("alpha"), 20)
            .await
            .expect("list");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, memory_id);
        assert_eq!(list[0].scope, "agent_output");
        assert!(list[0].source_refs.iter().any(|v| v == "node:tool_caller"));

        let updated = store
            .update_memory_item(
                memory_id.as_str(),
                Some("updated memory body"),
                Some(0.9),
                Some("manual_note"),
            )
            .await
            .expect("update");
        assert!(updated);

        let list_updated = store
            .list_session_memory_items(session_id, Some("updated"), 20)
            .await
            .expect("list updated");
        assert_eq!(list_updated.len(), 1);
        assert_eq!(list_updated[0].scope, "manual_note");
        assert_eq!(list_updated[0].importance, 0.9);
        assert_eq!(list_updated[0].content, "updated memory body");

        let deleted = store
            .delete_memory_item(memory_id.as_str())
            .await
            .expect("delete memory");
        assert!(deleted);

        let list_after_delete = store
            .list_session_memory_items(session_id, None, 20)
            .await
            .expect("list after delete");
        assert!(list_after_delete.is_empty());

        let deleted_missing = store
            .delete_memory_item(memory_id.as_str())
            .await
            .expect("delete missing memory");
        assert!(!deleted_missing);
    }

    #[tokio::test]
    async fn knowledge_memory_crud_roundtrip() {
        let store = SqliteStore::connect(temp_db_url().as_str())
            .await
            .expect("store");

        let knowledge_id = store
            .insert_knowledge("global_topic", "shared memory baseline", 0.8)
            .await
            .expect("insert knowledge");

        let list = store
            .list_knowledge_items(Some("baseline"), 20)
            .await
            .expect("list knowledge");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, knowledge_id);
        assert_eq!(list[0].topic, "global_topic");

        let updated = store
            .update_knowledge_item(
                knowledge_id.as_str(),
                Some("global_topic_updated"),
                Some("shared memory updated"),
                Some(0.95),
            )
            .await
            .expect("update knowledge");
        assert!(updated);

        let list_updated = store
            .list_knowledge_items(Some("updated"), 20)
            .await
            .expect("list updated knowledge");
        assert_eq!(list_updated.len(), 1);
        assert_eq!(list_updated[0].topic, "global_topic_updated");
        assert_eq!(list_updated[0].content, "shared memory updated");
        assert_eq!(list_updated[0].importance, 0.95);

        let searched = store
            .search_knowledge("shared memory", 20)
            .await
            .expect("search knowledge");
        assert_eq!(searched.len(), 1);
        assert_eq!(searched[0].0, knowledge_id);
    }
}
