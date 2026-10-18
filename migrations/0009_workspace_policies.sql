-- ワークスペースごとの規則。config は WorkspacePolicy の JSON。
-- 持ち主の層 (個人か、Organization と team) のあとに重ねる。
CREATE TABLE workspace_policies (
    workspace_id INTEGER PRIMARY KEY REFERENCES workspaces (id) ON DELETE CASCADE,
    config TEXT NOT NULL,
    updated_by TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
