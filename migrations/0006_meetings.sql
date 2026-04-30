-- Phase D — meetings as a workspace-level collaboration thread.
-- Each meeting groups a topic, a participant list, and a transcript of
-- messages from both users and personas. Lives outside the run/node
-- timeline so a team can hold async discussions independent of any
-- specific run.
CREATE TABLE IF NOT EXISTS meetings (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    session_id TEXT REFERENCES sessions(id) ON DELETE SET NULL,
    topic TEXT NOT NULL,
    participants_json TEXT NOT NULL DEFAULT '[]',
    status TEXT NOT NULL CHECK (status IN ('open','closed')),
    created_by TEXT NOT NULL,
    created_at TEXT NOT NULL,
    closed_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_meetings_workspace
    ON meetings (workspace_id, created_at DESC);

CREATE TABLE IF NOT EXISTS meeting_messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    speaker_kind TEXT NOT NULL CHECK (speaker_kind IN ('user','persona','system')),
    speaker_name TEXT NOT NULL,
    content TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_meeting_messages_meeting
    ON meeting_messages (meeting_id, id);
