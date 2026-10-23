-- VM の中で公開した、ブラウザから開ける HTTP(S) のサーバーのポート (段階 23)。
-- scheme は VM の中のサーバーとの話し方で、"http" か "https"。取り消すまで残す。
-- name はプロキシのホスト名 (<name>.<ドメイン>) に使うランダムな名前で、公開し直しても変えない。
CREATE TABLE published_ports (
    workspace_id INTEGER NOT NULL REFERENCES workspaces (id) ON DELETE CASCADE,
    port INTEGER NOT NULL,
    scheme TEXT NOT NULL,
    name TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, port)
);

-- プロキシのホストの cookie のセッション。Web の画面のセッションに結び付け、ログアウトや
-- `chloe web revoke` で一緒に消す。公開の名前 (ホスト) に結び付け、公開を取り消すと消す。
-- 値は持たず、SHA-256 のハッシュだけを置く。
CREATE TABLE proxy_sessions (
    id INTEGER PRIMARY KEY,
    web_session_id INTEGER NOT NULL REFERENCES web_sessions (id) ON DELETE CASCADE,
    name TEXT NOT NULL REFERENCES published_ports (name) ON DELETE CASCADE,
    session_hash TEXT NOT NULL UNIQUE,
    expires_at INTEGER NOT NULL
);

-- Web の画面からプロキシのホストへ戻すときの、一度だけ使えるコード。使うと消す。
CREATE TABLE proxy_codes (
    code_hash TEXT PRIMARY KEY,
    web_session_id INTEGER NOT NULL REFERENCES web_sessions (id) ON DELETE CASCADE,
    name TEXT NOT NULL REFERENCES published_ports (name) ON DELETE CASCADE,
    expires_at INTEGER NOT NULL
);
