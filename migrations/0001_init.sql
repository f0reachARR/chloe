-- 持ち主は "user:<login>"、"team:<org>/<team>"、"org:<org>" の形の文字列で表す。

-- シークレット。値はシークレットごとのデータ鍵で暗号化し、データ鍵はマスター鍵で暗号化する。
-- どちらも先頭 12 バイトがノンスで、追加認証データに持ち主と名前を入れる。
CREATE TABLE secrets (
    owner TEXT NOT NULL,
    name TEXT NOT NULL,
    master_key_id TEXT NOT NULL,
    wrapped_key BLOB NOT NULL,
    ciphertext BLOB NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (owner, name)
);

CREATE TABLE workspaces (
    id INTEGER PRIMARY KEY,
    owner TEXT NOT NULL,
    name TEXT NOT NULL,
    -- "ephemeral" (起動ごとに捨てる) か "persistent" (ワークスペースのディスクに残す)
    root_mode TEXT NOT NULL DEFAULT 'ephemeral',
    created_by TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE (owner, name)
);

-- ワークスペースごとに固定するプレースホルダー。
CREATE TABLE placeholders (
    workspace_id INTEGER NOT NULL REFERENCES workspaces (id) ON DELETE CASCADE,
    secret_name TEXT NOT NULL,
    placeholder TEXT NOT NULL,
    PRIMARY KEY (workspace_id, secret_name)
);
