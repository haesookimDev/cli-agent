use std::str::FromStr;

use chrono::{DateTime, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::types::{MemoryHit, RunRecord, SessionSummary, WebhookEndpoint};

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
                last_run_at: last_run_at_raw
                    .as_deref()
                    .map(parse_rfc3339)
                    .transpose()?,
                last_task: row.get("last_task"),
            });
        }

        Ok(sessions)
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
        let pattern = format!("%{}%", query_text.to_lowercase());

        let rows = sqlx::query(
            r#"
            SELECT id, content, importance, created_at
            FROM memory_items
            WHERE session_id = ?1 AND LOWER(content) LIKE ?2
            ORDER BY importance DESC, created_at DESC
            LIMIT ?3
            "#,
        )
        .bind(session_id.to_string())
        .bind(pattern)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let created_at_raw: String = row.get("created_at");
            let created_at = DateTime::parse_from_rfc3339(&created_at_raw)?.with_timezone(&Utc);

            out.push(MemoryHit {
                id: row.get("id"),
                content: row.get("content"),
                importance: row.get("importance"),
                created_at,
                score: 0.0,
            });
        }

        Ok(out)
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
}

fn parse_rfc3339(value: &str) -> anyhow::Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc))
}
