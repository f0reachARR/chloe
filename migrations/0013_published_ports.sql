-- VM の中で公開した、ブラウザから開ける HTTP(S) のサーバーのポート (段階 23)。
-- scheme は VM の中のサーバーとの話し方で、"http" か "https"。取り消すまで残す。
CREATE TABLE published_ports (
    workspace_id INTEGER NOT NULL REFERENCES workspaces (id) ON DELETE CASCADE,
    port INTEGER NOT NULL,
    scheme TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, port)
);

-- プロキシのホストの cookie のセッション。Web の画面のセッションに結び付け、ログアウトや
-- `chloe web revoke` で一緒に消す。値は持たず、SHA-256 のハッシュだけを置く。
CREATE TABLE proxy_sessions (
    id INTEGER PRIMARY KEY,
    web_session_id INTEGER NOT NULL REFERENCES web_sessions (id) ON DELETE CASCADE,
    workspace_id INTEGER NOT NULL REFERENCES workspaces (id) ON DELETE CASCADE,
    port INTEGER NOT NULL,
    session_hash TEXT NOT NULL UNIQUE,
    expires_at INTEGER NOT NULL
);

-- Web の画面からプロキシのホストへ戻すときの、一度だけ使えるコード。使うと消す。
CREATE TABLE proxy_codes (
    code_hash TEXT PRIMARY KEY,
    web_session_id INTEGER NOT NULL REFERENCES web_sessions (id) ON DELETE CASCADE,
    workspace_id INTEGER NOT NULL REFERENCES workspaces (id) ON DELETE CASCADE,
    port INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);
