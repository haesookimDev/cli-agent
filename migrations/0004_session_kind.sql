-- Phase A — split general/team chats and persist agent messages live.
-- sessions.kind lets the API filter by chat surface (general vs team).
-- messages gets run_id/node_id/persona_name so an agent reply that streamed
-- during a run can be looked up later, paired with its run/node, and shown
-- under the right persona without rebuilding from RunRecord.outputs.
ALTER TABLE sessions ADD COLUMN kind TEXT NOT NULL DEFAULT 'general';
CREATE INDEX IF NOT EXISTS idx_sessions_kind_created
    ON sessions (kind, created_at DESC);

ALTER TABLE messages ADD COLUMN run_id TEXT;
ALTER TABLE messages ADD COLUMN node_id TEXT;
ALTER TABLE messages ADD COLUMN persona_name TEXT;
-- Partial UNIQUE: dedup only on rows that carry (run_id, node_id). Legacy
-- user rows have run_id NULL and are unaffected.
CREATE UNIQUE INDEX IF NOT EXISTS idx_messages_run_node_dedup
    ON messages (session_id, run_id, node_id)
    WHERE run_id IS NOT NULL;
