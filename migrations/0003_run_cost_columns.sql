-- Phase 8 (TODO 8-4) — denormalized cost columns for cheap per-session
-- aggregation. The full numbers still live inside run_json; these columns
-- exist so SUM(...) by session_id works without parsing the blob.
ALTER TABLE agent_runs ADD COLUMN total_input_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE agent_runs ADD COLUMN total_output_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE agent_runs ADD COLUMN total_cost_usd REAL NOT NULL DEFAULT 0.0;

CREATE INDEX IF NOT EXISTS idx_agent_runs_session_cost
    ON agent_runs (session_id);
