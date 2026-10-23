-- Web の画面のセッション (段階 20)。値は持たず、SHA-256 のハッシュだけを置く。
-- セッションの権限は、ログインした利用者 (login) の権限と同じにする。
CREATE TABLE web_sessions (
    id INTEGER PRIMARY KEY,
    login TEXT NOT NULL,
    session_hash TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    last_used_at INTEGER
);

-- `chloe web login` が出す、一度だけ使えるログインの URL のコード。使うと消す。
CREATE TABLE web_login_links (
    code_hash TEXT PRIMARY KEY,
    login TEXT NOT NULL,
    expires_at INTEGER NOT NULL
);
