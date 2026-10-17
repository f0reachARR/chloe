-- シークレットの定義 (VM の環境変数、プレースホルダーの形、差し替えと署名の規則) を JSON で置く。
-- 定義のない行は、組み込みの種類の定義を使う。定義は値の追加認証データに入れる。
ALTER TABLE secrets ADD COLUMN spec TEXT;

-- プレースホルダーをシークレットの欄ごとにし、作ったときの形 (`{id}` を乱数に置き換える) を残す。
-- 形が変わったら作り直す。組み込みの種類は今のプレースホルダーをそのまま使い続ける。
CREATE TABLE placeholders_v2 (
    workspace_id INTEGER NOT NULL REFERENCES workspaces (id) ON DELETE CASCADE,
    secret_name TEXT NOT NULL,
    field TEXT NOT NULL,
    template TEXT NOT NULL,
    placeholder TEXT NOT NULL,
    PRIMARY KEY (workspace_id, secret_name, field)
);

INSERT INTO placeholders_v2 (workspace_id, secret_name, field, template, placeholder)
SELECT
    workspace_id,
    secret_name,
    'value',
    CASE secret_name
        WHEN 'anthropic_api_key' THEN 'sk-ant-api03-{id}'
        WHEN 'claude_oauth_token' THEN 'sk-ant-oat01-{id}'
        WHEN 'openai_api_key' THEN 'sk-proj-{id}'
        WHEN 'github_token' THEN 'ghu_{id}'
        ELSE ''
    END,
    placeholder
FROM placeholders;

DROP TABLE placeholders;

ALTER TABLE placeholders_v2 RENAME TO placeholders;
