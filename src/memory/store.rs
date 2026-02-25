use std::collections::HashSet;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::types::{
    CronSchedule, KnowledgeItem, MemoryHit, RunActionEvent, RunActionType, RunRecord,
    SessionMemoryItem, SessionSummary, WebhookDeliveryRecord, WebhookEndpoint,
};

#[derive(Debug, Clone)]
pub struct SqliteStore {
    pool: SqlitePool,
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

    pub async fn init_schema(&self) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                created_at TEXT NOT NULL
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS memory_items (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                content TEXT NOT NULL,
                importance REAL NOT NULL,
                scope TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS memory_links (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                memory_item_id TEXT NOT NULL,
                source_ref TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS agent_runs (
                run_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                status TEXT NOT NULL,
                profile TEXT NOT NULL,
                task TEXT NOT NULL,
                run_json TEXT NOT NULL,
                error TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS run_action_events (
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                event_id TEXT NOT NULL,
                run_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                action TEXT NOT NULL,
                actor_type TEXT,
                actor_id TEXT,
                cause_event_id TEXT,
                payload TEXT NOT NULL
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE INDEX IF NOT EXISTS idx_run_action_events_run_seq
            ON run_action_events (run_id, seq);
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS artifacts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id TEXT NOT NULL,
                name TEXT NOT NULL,
                data TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS webhook_endpoints (
                id TEXT PRIMARY KEY,
                url TEXT NOT NULL,
                events TEXT NOT NULL,
                secret TEXT NOT NULL,
                enabled INTEGER NOT NULL,
                created_at TEXT NOT NULL
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS webhook_deliveries (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                endpoint_id TEXT NOT NULL,
                event TEXT NOT NULL,
                event_id TEXT NOT NULL,
                url TEXT NOT NULL,
                attempts INTEGER NOT NULL,
                delivered INTEGER NOT NULL,
                dead_letter INTEGER NOT NULL,
                status_code INTEGER,
                error TEXT,
                payload TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE INDEX IF NOT EXISTS idx_webhook_deliveries_dead_letter_created
            ON webhook_deliveries (dead_letter, created_at DESC);
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS workflow_templates (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT NOT NULL DEFAULT '',
                source_run_id TEXT,
                graph_json TEXT NOT NULL,
                parameters_json TEXT NOT NULL DEFAULT '[]',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS cron_schedules (
                id TEXT PRIMARY KEY,
                workflow_id TEXT NOT NULL,
                cron_expr TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                parameters TEXT,
                last_run_at TEXT,
                next_run_at TEXT,
                created_at TEXT NOT NULL
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS knowledge_base (
                id TEXT PRIMARY KEY,
                topic TEXT NOT NULL,
                content TEXT NOT NULL,
                importance REAL NOT NULL,
                access_count INTEGER DEFAULT 0,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS app_settings (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                data TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS run_attempts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id TEXT NOT NULL,
                node_id TEXT NOT NULL,
                attempt_no INTEGER NOT NULL,
                status TEXT NOT NULL,
                failure_class TEXT,
                reason TEXT,
                delta_prompt_json TEXT,
                created_at TEXT NOT NULL,
                UNIQUE(run_id, node_id, attempt_no)
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"CREATE INDEX IF NOT EXISTS idx_run_attempts_run ON run_attempts(run_id);"#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS coder_sessions (
                id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL,
                node_id TEXT NOT NULL,
                backend TEXT NOT NULL,
                terminal_session_id TEXT,
                working_dir TEXT,
                worktree_branch TEXT,
                status TEXT NOT NULL,
                exit_code INTEGER,
                files_changed_json TEXT,
                started_at TEXT NOT NULL,
                ended_at TEXT
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"CREATE INDEX IF NOT EXISTS idx_coder_sessions_run ON coder_sessions(run_id);"#,
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn create_session(&self, session_id: Uuid) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT OR IGNORE INTO sessions (id, created_at)
            VALUES (?1, ?2)
            "#,
        )
        .bind(session_id.to_string())
        .bind(Utc::now().to_rfc3339())
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

    pub async fn list_session_messages(
        &self,
        session_id: Uuid,
        limit: usize,
    ) -> anyhow::Result<Vec<(i64, String, String, String)>> {
        let rows = sqlx::query(
            r#"
            SELECT id, role, content, created_at
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
            let id: i64 = row.get("id");
            let role: String = row.get("role");
            let content: String = row.get("content");
            let created_at: String = row.get("created_at");
            msgs.push((id, role, content, created_at));
        }
        msgs.reverse();
        Ok(msgs)
    }

    pub async fn upsert_run(&self, run: &RunRecord) -> anyhow::Result<()> {
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
                updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(run_id)
            DO UPDATE SET
                status = excluded.status,
                run_json = excluded.run_json,
                error = excluded.error,
                updated_at = excluded.updated_at
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

    pub async fn list_sessions(&self, limit: usize) -> anyhow::Result<Vec<SessionSummary>> {
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
                ) AS last_task
            FROM sessions s
            LEFT JOIN agent_runs ar ON ar.session_id = s.id
            GROUP BY s.id
            ORDER BY COALESCE(MAX(ar.created_at), s.created_at) DESC
            LIMIT ?1
            "#,
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        let mut sessions = Vec::with_capacity(rows.len());
        for row in rows {
            let session_raw: String = row.get("session_id");
            let created_at_raw: String = row.get("created_at");
            let last_run_at_raw: Option<String> = row.get("last_run_at");

            sessions.push(SessionSummary {
                session_id: Uuid::parse_str(session_raw.as_str())?,
                created_at: parse_rfc3339(created_at_raw.as_str())?,
                run_count: row.get::<i64, _>("run_count").max(0) as usize,
                last_run_at: last_run_at_raw.as_deref().map(parse_rfc3339).transpose()?,
                last_task: row.get("last_task"),
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
                ) AS last_task
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

        Ok(Some(SessionSummary {
            session_id: Uuid::parse_str(session_raw.as_str())?,
            created_at: parse_rfc3339(created_at_raw.as_str())?,
            run_count: row.get::<i64, _>("run_count").max(0) as usize,
            last_run_at: last_run_at_raw.as_deref().map(parse_rfc3339).transpose()?,
            last_task: row.get("last_task"),
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
        limit: usize,
    ) -> anyhow::Result<Vec<MemoryHit>> {
        let normalized_query = query_text.trim().to_lowercase();
        if normalized_query.is_empty() {
            return Ok(Vec::new());
        }

        let rows = sqlx::query(
            r#"
            SELECT id, content, importance, created_at
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
            let content_lower = content.to_lowercase();

            let full_match = content_lower.contains(normalized_query.as_str());
            let token_score = token_overlap_score(&query_tokens, content_lower.as_str());
            let lexical = if full_match { 1.0 } else { token_score };
            if lexical <= 0.0 {
                continue;
            }

            let age_secs = (Utc::now() - created_at).num_seconds().max(0) as f64;
            let recency = (1.0 / (1.0 + age_secs / 7200.0)).clamp(0.0, 1.0);
            let rank = (0.70 * lexical + 0.20 * importance + 0.10 * recency).clamp(0.0, 1.0);

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
            secret: secret.to_string(),
            enabled: true,
            created_at: Utc::now(),
        };

        sqlx::query(
            r#"
            INSERT INTO webhook_endpoints (id, url, events, secret, enabled, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
        )
        .bind(endpoint.id.clone())
        .bind(endpoint.url.clone())
        .bind(serde_json::to_string(&endpoint.events)?)
        .bind(endpoint.secret.clone())
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

        let mut endpoints = Vec::with_capacity(rows.len());
        for row in rows {
            let events_raw: String = row.get("events");
            let created_at_raw: String = row.get("created_at");
            let created_at = DateTime::parse_from_rfc3339(&created_at_raw)?.with_timezone(&Utc);

            endpoints.push(WebhookEndpoint {
                id: row.get("id"),
                url: row.get("url"),
                events: serde_json::from_str(&events_raw)?,
                secret: row.get("secret"),
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
        let rows = sqlx::query(
            r#"SELECT * FROM coder_sessions WHERE run_id = ?1 ORDER BY started_at"#,
        )
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
    })
}

fn parse_rfc3339(value: &str) -> anyhow::Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc))
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
        "terminal_suggested" => RunActionType::TerminalSuggested,
        "coder_session_started" => RunActionType::CoderSessionStarted,
        "coder_session_completed" => RunActionType::CoderSessionCompleted,
        _ => {
            return Err(anyhow::anyhow!("unknown run action event type: {value}"));
        }
    };
    Ok(action)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db_url() -> String {
        let path =
            std::env::temp_dir().join(format!("cli-agent-memory-test-{}.db", Uuid::new_v4()));
        format!("sqlite://{}", path.display())
    }

    #[tokio::test]
    async fn session_memory_crud_roundtrip() {
        let store = SqliteStore::connect(temp_db_url().as_str())
            .await
            .expect("store");
        let session_id = Uuid::new_v4();
        store
            .create_session(session_id)
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
