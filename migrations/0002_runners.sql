-- 動いているランナー。controller が起動したら記録し、止めたら消す。
-- 起動時と定期的に、実際に動いているランナーと突き合わせる (reconcile)。
-- node は段階 9 で、ランナーが動いているホストを記録するのに使う。
CREATE TABLE runners (
    workspace_id INTEGER PRIMARY KEY REFERENCES workspaces (id) ON DELETE CASCADE,
    node TEXT NOT NULL,
    started_at INTEGER NOT NULL
);
