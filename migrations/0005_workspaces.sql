-- Phase B — workspace as a project entity that owns N sessions and a
-- shared file area. sessions get an optional workspace_id FK so legacy
-- rows stay valid (NULL = "no workspace, default ad-hoc dir"). Files
-- live in workspace_files keyed by (workspace_id, relative_path) so
-- uploads are uniquely addressable per workspace.
CREATE TABLE IF NOT EXISTS workspaces (
    id TEXT PRIMARY KEY,
    slug TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('general','team')),
    root_path TEXT NOT NULL,
    description TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_workspaces_kind
    ON workspaces (kind, created_at DESC);

ALTER TABLE sessions ADD COLUMN workspace_id TEXT
    REFERENCES workspaces(id) ON DELETE SET NULL;
CREATE INDEX IF NOT EXISTS idx_sessions_workspace
    ON sessions (workspace_id, created_at DESC);

CREATE TABLE IF NOT EXISTS workspace_files (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    session_id TEXT REFERENCES sessions(id) ON DELETE SET NULL,
    relative_path TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    mime TEXT,
    sha256 TEXT,
    created_by TEXT NOT NULL CHECK (created_by IN ('user','persona','system')),
    created_by_persona TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (workspace_id, relative_path)
);
CREATE INDEX IF NOT EXISTS idx_workspace_files_session
    ON workspace_files (workspace_id, session_id);
