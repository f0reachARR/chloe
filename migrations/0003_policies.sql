-- Organization と team の設定 (段階 8c)。config は PolicyLayer の JSON。
-- ゲートウェイの設定 (全体)、Organization、team の順に重ねてワークスペースの方針を作る。
CREATE TABLE policies (
    owner TEXT PRIMARY KEY,
    config TEXT NOT NULL,
    updated_by TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
