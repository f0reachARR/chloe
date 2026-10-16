//! ランナー (ワークスペースの VM とプロキシ) の起動と停止。

use std::future::Future;
use std::path::PathBuf;

use chloe_policy::{InjectRuleConfig, PolicyConfig};
use chloe_secrets::SecretString;
use serde::Deserialize;

use crate::Error;
use crate::catalog;
use crate::workspace::{RootMode, Workspace};

/// ランナーを起動するのに必要なもの。
pub struct RunnerSpec {
    /// ランナーの識別子 ([`Workspace::key`])。
    pub key: String,
    pub root_mode: RootMode,
    /// プロキシの方針。
    pub policy: PolicyConfig,
    /// プロキシに渡す本物の値 (環境変数の名前と値)。
    pub secrets: Vec<(String, SecretString)>,
    /// VM の環境変数に入れるプレースホルダー。
    pub guest_env: Vec<(String, String)>,
}

pub trait RunnerBackend: Send + Sync + 'static {
    /// ランナーを起動し、guest-agent につながる vsock の Unix ソケットを返す。
    fn start(&self, spec: RunnerSpec) -> impl Future<Output = Result<PathBuf, Error>> + Send;
    /// ランナーを止める。VM の中のファイルシステムは、先に VM を止めて閉じておく。
    fn stop(&self, key: &str) -> impl Future<Output = Result<(), Error>> + Send;
    fn is_running(&self, key: &str) -> impl Future<Output = Result<bool, Error>> + Send;
}

/// すべてのワークスペースに共通の許可リスト。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BasePolicy {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub passthrough: Vec<String>,
    /// 宛先、メソッド、パスの規則。上から順に評価する。
    #[serde(default)]
    pub rules: Vec<chloe_policy::PathRuleConfig>,
}

/// ワークスペースのランナーの起動に必要なものを組み立てる。
///
/// 差し替えの規則とプレースホルダーの環境変数は、持ち主がシークレットを登録している種類だけに作る。
/// 登録していない種類のプレースホルダーを VM に入れると、ツールがそれを使おうとして
/// サブスクリプションのログインなどほかの手段を試さなくなる。
pub fn build_spec(
    workspace: &Workspace,
    base: &BasePolicy,
    placeholders: &std::collections::BTreeMap<String, String>,
    secrets: Vec<(String, SecretString)>,
) -> RunnerSpec {
    let mut policy = PolicyConfig {
        allow: base.allow.clone(),
        passthrough: base.passthrough.clone(),
        inject: vec![],
        rules: base.rules.clone(),
    };
    let mut guest_env = Vec::new();
    let mut runner_secrets = Vec::new();
    for (name, value) in secrets {
        let (Some(kind), Some(placeholder)) = (catalog::kind(&name), placeholders.get(&name))
        else {
            continue;
        };
        let secret_env = catalog::secret_env(kind.name);
        for (index, rule) in kind.rules.iter().enumerate() {
            policy.inject.push(InjectRuleConfig {
                name: format!("{}-{index}", kind.name),
                placeholder: placeholder.clone(),
                hosts: rule.hosts.iter().map(|h| h.to_string()).collect(),
                header: rule.header.to_string(),
                format: rule.format,
                secret_env: secret_env.clone(),
            });
            for host in rule.hosts {
                if !policy.allow.iter().any(|a| a == host) {
                    policy.allow.push(host.to_string());
                }
            }
        }
        for env in kind.env {
            guest_env.push((env.to_string(), placeholder.clone()));
        }
        runner_secrets.push((secret_env, value));
    }
    RunnerSpec {
        key: workspace.key(),
        root_mode: workspace.root_mode,
        policy,
        secrets: runner_secrets,
        guest_env,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chloe_secrets::{ExposeSecret, Owner};

    #[test]
    fn builds_rules_only_for_registered_secrets() {
        let ws = Workspace {
            id: 7,
            owner: Owner::User("alice".into()),
            name: "api".into(),
            root_mode: RootMode::Ephemeral,
        };
        let placeholders = [
            ("anthropic_api_key", "sk-ant-api03-chloeA"),
            ("openai_api_key", "sk-proj-chloeO"),
            ("github_token", "ghu_chloeG"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let base = BasePolicy {
            allow: vec!["api.anthropic.com".into()],
            passthrough: vec!["registry-1.docker.io".into()],
            rules: vec![],
        };
        let spec = build_spec(
            &ws,
            &base,
            &placeholders,
            vec![
                ("anthropic_api_key".into(), "sk-real-a".to_string().into()),
                ("github_token".into(), "ghu_real".to_string().into()),
            ],
        );
        assert_eq!(spec.key, "ws-7");
        assert_eq!(spec.policy.inject.len(), 3);
        assert!(spec.policy.allow.iter().any(|a| a == "github.com"));
        assert_eq!(
            spec.policy
                .allow
                .iter()
                .filter(|a| *a == "api.anthropic.com")
                .count(),
            1
        );
        assert!(
            spec.guest_env
                .contains(&("ANTHROPIC_API_KEY".into(), "sk-ant-api03-chloeA".into()))
        );
        assert!(
            spec.guest_env
                .contains(&("GH_TOKEN".into(), "ghu_chloeG".into()))
        );
        assert!(!spec.guest_env.iter().any(|(k, _)| k == "OPENAI_API_KEY"));
        let (env, value) = &spec.secrets[0];
        assert_eq!(env, "CHLOE_SECRET_ANTHROPIC_API_KEY");
        assert_eq!(value.expose_secret(), "sk-real-a");
    }
}
