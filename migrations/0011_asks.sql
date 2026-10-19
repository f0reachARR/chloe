-- VM の中のエージェントからの依頼 (通信の許可と質問)。
-- kind は "network" か "question"、body は種類ごとの中身の JSON。
-- status は "pending"、"allowed"、"denied"、"answered"、"cancelled"。期限 (expires_at) を
-- 過ぎた "pending" は、読むときに "expired" として扱う。
CREATE TABLE asks (
    id INTEGER PRIMARY KEY,
    workspace_id INTEGER NOT NULL REFERENCES workspaces (id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    body TEXT NOT NULL,
    reason TEXT,
    status TEXT NOT NULL DEFAULT 'pending',
    answer TEXT,
    answered_by TEXT,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    answered_at INTEGER
);

CREATE INDEX asks_by_workspace ON asks (workspace_id, status);
