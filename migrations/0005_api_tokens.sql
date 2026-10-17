-- 管理 API のトークン (段階 9e)。値は持たず、SHA-256 のハッシュだけを置く。
-- トークンの権限は、発行した利用者 (login) の権限と同じにする。
CREATE TABLE api_tokens (
    id INTEGER PRIMARY KEY,
    login TEXT NOT NULL,
    name TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    expires_at INTEGER,
    last_used_at INTEGER,
    revoked_at INTEGER
);
