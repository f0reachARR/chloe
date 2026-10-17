-- Device Flow で GitHub のログイン名を確かめたときに使っていた SSH の公開鍵。
-- 予約したユーザー名 (既定は chloe) で接続したとき、鍵から利用者を決める。
-- 一つの鍵は一人の利用者にだけ結び付ける。
CREATE TABLE ssh_keys (
    fingerprint TEXT PRIMARY KEY,
    login TEXT NOT NULL,
    public_key TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    last_used_at INTEGER
);

CREATE INDEX ssh_keys_login ON ssh_keys (login);
