-- search_memory pulls `limit * 20` rows ordered by updated_at DESC for each
-- session lookup. Without an index SQLite was doing a sort over every row
-- belonging to the session. A composite (session_id, updated_at DESC) index
-- lets the planner skip the sort.
CREATE INDEX IF NOT EXISTS idx_memory_items_session_updated
    ON memory_items (session_id, updated_at DESC);
