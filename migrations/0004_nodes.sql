-- 複数のランナーのホスト (chloe-node) (段階 9)。

-- 参加したホスト。
CREATE TABLE nodes (
    name TEXT PRIMARY KEY,
    -- コントロールプレーンが API を呼ぶ URL (https://host:port)。
    url TEXT NOT NULL,
    joined_at INTEGER
);

-- 一度だけ使える参加用のトークン。値は持たず、SHA-256 のハッシュだけを置く。
CREATE TABLE node_joins (
    token_hash TEXT PRIMARY KEY,
    name TEXT NOT NULL REFERENCES nodes (name) ON DELETE CASCADE,
    expires_at INTEGER NOT NULL
);

-- ワークスペースを置いたホスト。ディスクがそのホストにあるので、最初に起動したホストに結びつける。
ALTER TABLE workspaces ADD COLUMN node TEXT;
