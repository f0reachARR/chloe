//! ランナー (ワークスペースの VM とプロキシ) の起動と停止。
//!
//! ランナーには本物の値を渡さない。ランナーのプロキシは差し替えのたびに controller の
//! 問い合わせ用ソケット ([`RunnerBackend::broker_socket`]) へ尋ねる ([`crate::broker`])。

use std::future::Future;
use std::path::PathBuf;

use chloe_policy::{InjectRuleConfig, PolicyConfig};
use serde::Deserialize;

use crate::Error;
use crate::catalog;
use crate::workspace::{RootMode, Workspace};

/// ランナーを起動するのに必要なもの。
pub struct RunnerSpec {
    /// ランナーの識別子 ([`Workspace::key`])。
    pub key: String,
    pub root_mode: RootMode,
    /// プロキシの方針。差し替えの規則は持つが、本物の値は持たない。
    pub policy: PolicyConfig,
    /// VM の環境変数に入れるプレースホルダー。
    pub guest_env: Vec<(String, String)>,
}

pub trait RunnerBackend: Send + Sync + 'static {
    /// ランナーを起動し、guest-agent につながる vsock の Unix ソケットを返す。
    /// 起動する前に、controller は [`RunnerBackend::broker_socket`] で待ち受けを始めている。
    fn start(&self, spec: RunnerSpec) -> impl Future<Output = Result<PathBuf, Error>> + Send;
    /// ランナーを止める。VM の中のファイルシステムは、先に VM を止めて閉じておく。
    fn stop(&self, key: &str) -> impl Future<Output = Result<(), Error>> + Send;
    fn is_running(&self, key: &str) -> impl Future<Output = Result<bool, Error>> + Send;
    /// 動いているランナーのプロキシの方針を入れ替える。VM の環境変数 (プレースホルダー) は
    /// 変えないので、起動時になかった種類のシークレットには再起動が要る。
    fn update(&self, spec: RunnerSpec) -> impl Future<Output = Result<(), Error>> + Send;
    /// ランナーのプロキシが本物の値を問い合わせる Unix ソケットの置き場所。
    fn broker_socket(&self, key: &str) -> PathBuf;
    /// 動いているランナーの guest-agent につながる vsock の Unix ソケット。
    fn vsock_path(&self, key: &str) -> PathBuf;
    /// 実際に動いているランナーの識別子の一覧。reconcile で記録と突き合わせる。
    fn list(&self) -> impl Future<Output = Result<Vec<String>, Error>> + Send;
    /// ランナーが動くホストの名前。段階 9 で、複数のホストを区別するのに使う。
    fn node(&self) -> &str {
        "local"
    }
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
/// 差し替えの規則とプレースホルダーの環境変数は、`kinds` (そのワークスペースで使える
/// シークレットの種類) だけに作る。使えない種類のプレースホルダーを VM に入れると、
/// ツールがそれを使おうとして、サブスクリプションのログインなどほかの手段を試さなくなる。
pub fn build_spec(
    workspace: &Workspace,
    base: &BasePolicy,
    placeholders: &std::collections::BTreeMap<String, String>,
    kinds: &[String],
) -> RunnerSpec {
    let mut policy = PolicyConfig {
        allow: base.allow.clone(),
        passthrough: base.passthrough.clone(),
        inject: vec![],
        rules: base.rules.clone(),
    };
    let mut guest_env = Vec::new();
    for name in kinds {
        let (Some(kind), Some(placeholder)) = (catalog::kind(name), placeholders.get(name)) else {
            continue;
        };
        for (index, rule) in kind.rules.iter().enumerate() {
            policy.inject.push(InjectRuleConfig {
                name: format!("{}-{index}", kind.name),
                placeholder: placeholder.clone(),
                hosts: rule.hosts.iter().map(|h| h.to_string()).collect(),
                header: rule.header.to_string(),
                format: rule.format,
                secret_env: catalog::secret_env(kind.name),
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
    }
    RunnerSpec {
        key: workspace.key(),
        root_mode: workspace.root_mode,
        policy,
        guest_env,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chloe_secrets::Owner;

    #[test]
    fn builds_rules_only_for_available_kinds() {
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
            &[
                "anthropic_api_key".into(),
                "github_token".into(),
                "unknown".into(),
            ],
        );
        assert_eq!(spec.key, "ws-7");
        assert_eq!(spec.policy.inject.len(), 3);
        assert_eq!(
            spec.policy.inject[0].secret_env,
            "CHLOE_SECRET_ANTHROPIC_API_KEY"
        );
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
        // 方針を書き出しても本物の値は入らない (そもそも持っていない)。
        let json = serde_json::to_string(&spec.policy).unwrap();
        assert!(!json.contains("real"));
    }
}
