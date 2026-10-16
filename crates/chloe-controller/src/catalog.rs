//! chloe が扱うシークレットの種類。
//!
//! シークレットの名前が種類の名前になる。種類ごとに、VM に渡す環境変数、
//! プレースホルダーの接頭辞 (ツールが手元で形式を調べることがあるので本物に似せる)、
//! プロキシで差し替える宛先と場所を決めておく。

use chloe_policy::InjectFormat;

pub struct SecretKind {
    pub name: &'static str,
    /// プレースホルダーを入れる VM の環境変数。
    pub env: &'static [&'static str],
    pub placeholder_prefix: &'static str,
    pub rules: &'static [Rule],
}

pub struct Rule {
    pub hosts: &'static [&'static str],
    pub header: &'static str,
    pub format: InjectFormat,
}

pub const KINDS: &[SecretKind] = &[
    SecretKind {
        name: "anthropic_api_key",
        env: &["ANTHROPIC_API_KEY"],
        placeholder_prefix: "sk-ant-api03-",
        rules: &[Rule {
            hosts: &["api.anthropic.com"],
            header: "x-api-key",
            format: InjectFormat::Replace,
        }],
    },
    SecretKind {
        name: "claude_oauth_token",
        env: &["CLAUDE_CODE_OAUTH_TOKEN"],
        placeholder_prefix: "sk-ant-oat01-",
        rules: &[Rule {
            hosts: &["api.anthropic.com"],
            header: "authorization",
            format: InjectFormat::Replace,
        }],
    },
    SecretKind {
        name: "openai_api_key",
        // codex exec は CODEX_API_KEY を読む。
        env: &["OPENAI_API_KEY", "CODEX_API_KEY"],
        placeholder_prefix: "sk-proj-",
        rules: &[Rule {
            hosts: &["api.openai.com"],
            header: "authorization",
            format: InjectFormat::Replace,
        }],
    },
    SecretKind {
        name: "github_token",
        env: &["GH_TOKEN", "GITHUB_TOKEN"],
        // GitHub App のユーザートークンの接頭辞。
        placeholder_prefix: "ghu_",
        rules: &[
            Rule {
                hosts: &["github.com"],
                header: "authorization",
                format: InjectFormat::BasicPassword,
            },
            Rule {
                hosts: &["api.github.com", "uploads.github.com"],
                header: "authorization",
                format: InjectFormat::Replace,
            },
        ],
    },
];

pub fn kind(name: &str) -> Option<&'static SecretKind> {
    KINDS.iter().find(|k| k.name == name)
}

/// ランナーの環境変数でプロキシに本物の値を渡すときの変数名。
pub fn secret_env(name: &str) -> String {
    format!("CHLOE_SECRET_{}", name.to_ascii_uppercase())
}
