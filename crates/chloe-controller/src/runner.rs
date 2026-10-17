//! ランナー (ワークスペースの VM とプロキシ) の起動と停止。
//!
//! ランナーには本物の値を渡さない。ランナーのプロキシは差し替えのたびに問い合わせ用の
//! ソケットへ尋ね、controller が答える ([`crate::broker`]、[`crate::Controller::answer_secret`])。

use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use chloe_policy::{InjectRuleConfig, PolicyConfig};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::Error;
use crate::catalog;
use crate::workspace::{RootMode, Workspace};

/// guest-agent への接続。vsock か、それを中継した接続。
pub trait AgentIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AgentIo for T {}
pub type AgentStream = Box<dyn AgentIo>;

/// ランナーを起動するのに必要なもの。
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// ランナーを起動する。起動する前に、controller は [`RunnerBackend::local_broker_socket`]
    /// があればそこで問い合わせの待ち受けを始めている。
    fn start(&self, spec: RunnerSpec) -> impl Future<Output = Result<(), Error>> + Send;
    /// ランナーを止める。VM の中のファイルシステムは、先に VM を止めて閉じておく。
    fn stop(&self, key: &str) -> impl Future<Output = Result<(), Error>> + Send;
    fn is_running(&self, key: &str) -> impl Future<Output = Result<bool, Error>> + Send;
    /// 動いているランナーのプロキシの方針を入れ替える。VM の環境変数 (プレースホルダー) は
    /// controller が guest-agent に送る。
    fn update(&self, spec: RunnerSpec) -> impl Future<Output = Result<(), Error>> + Send;
    /// 動いているランナーの guest-agent につなぐ。VM が起動しきるまで `wait` だけ待つ。
    fn connect_agent(
        &self,
        key: &str,
        wait: Duration,
    ) -> impl Future<Output = Result<AgentStream, Error>> + Send;
    /// ランナーのプロキシが本物の値を問い合わせる Unix ソケットを、controller が同じホストで
    /// 待ち受けるときの置き場所。ほかのホストのランナーでは `None` (そのホストの `chloe-node`
    /// が待ち受けて、controller に中継する)。
    fn local_broker_socket(&self, key: &str) -> Option<PathBuf>;
    /// 実際に動いているランナーの識別子の一覧。reconcile で記録と突き合わせる。
    fn list(&self) -> impl Future<Output = Result<Vec<String>, Error>> + Send;
    /// ランナーが動くホストの名前。起動したランナーの記録に使う。
    fn node_of(&self, _key: &str) -> impl Future<Output = Result<String, Error>> + Send {
        async { Ok("local".to_string()) }
    }
}

/// すべてのワークスペースに共通の許可リスト。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BasePolicy {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub passthrough: Vec<String>,
    /// 宛先、メソッド、パスの規則。上から順に評価する。
    #[serde(default)]
    pub rules: Vec<chloe_policy::PathRuleConfig>,
}

impl BasePolicy {
    /// 全体の設定に、Organization と team の設定を (上の層から順に) 重ねる。
    ///
    /// 許可リストは足し合わせる。パスの規則は上の層のものを先に並べるので、最初に一致した
    /// 規則で決まる評価では、上の層 (サーバーの運用者、Organization) の規則を下の層が覆せない。
    /// 中継のみの宛先は復号せず規則が効かないので、全体の設定でしか決められない。
    pub fn with_layers(&self, layers: &[PolicyLayer]) -> BasePolicy {
        let mut merged = self.clone();
        for layer in layers {
            for host in &layer.allow {
                if !merged.allow.contains(host) {
                    merged.allow.push(host.clone());
                }
            }
            merged.rules.extend(layer.rules.iter().cloned());
        }
        merged
    }
}

/// Organization か team の設定 (データベースの `policies`)。
///
/// 中継のみの宛先 (`passthrough`) は置けない。復号しないのでパスの規則が効かず、置けると
/// 上の層の規則を素通りする宛先を作れてしまうためである。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyLayer {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<chloe_policy::PathRuleConfig>,
    /// 新しく作るワークスペースのルートの扱いの既定。下の層が優先する。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<RootMode>,
    /// 共有ワークスペースに渡す GitHub のインストールトークンの範囲。下の層が優先する。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<GitHubScope>,
}

/// インストールトークンの範囲。書かなかった項目は、インストールのすべてになる。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitHubScope {
    /// リポジトリの名前 (Organization の名前を含まない)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repositories: Option<Vec<String>>,
    /// 権限の名前と水準 (`contents = "read"` など)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<std::collections::BTreeMap<String, String>>,
}

/// 層を重ねたときに、もっとも下の層で決めた値。
pub fn most_specific<T: Clone>(
    layers: &[PolicyLayer],
    pick: impl Fn(&PolicyLayer) -> Option<&T>,
) -> Option<T> {
    layers.iter().rev().find_map(|layer| pick(layer).cloned())
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
        sign: vec![],
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
    fn layers_add_hosts_and_keep_upper_rules_first() {
        let rule = |name: &str, action| chloe_policy::PathRuleConfig {
            name: Some(name.into()),
            hosts: vec!["api.github.com".into()],
            methods: vec![],
            paths: vec![],
            action,
        };
        let base = BasePolicy {
            allow: vec!["a.example".into()],
            passthrough: vec![],
            rules: vec![rule("global", chloe_policy::Action::Deny)],
        };
        let org = PolicyLayer {
            allow: vec!["a.example".into(), "b.example".into()],
            rules: vec![rule("org", chloe_policy::Action::Allow)],
            root: Some(RootMode::Persistent),
            ..Default::default()
        };
        let team = PolicyLayer {
            allow: vec!["c.example".into()],
            rules: vec![rule("team", chloe_policy::Action::Allow)],
            root: Some(RootMode::Ephemeral),
            ..Default::default()
        };
        let layers = [org, team];
        let merged = base.with_layers(&layers);
        assert_eq!(merged.allow, ["a.example", "b.example", "c.example"]);
        assert!(merged.passthrough.is_empty());
        // 中継のみの宛先は層には書けない。
        assert!(serde_json::from_str::<PolicyLayer>(r#"{"passthrough": ["x.example"]}"#).is_err());
        let names: Vec<_> = merged
            .rules
            .iter()
            .map(|r| r.name.clone().unwrap())
            .collect();
        assert_eq!(names, ["global", "org", "team"]);
        assert_eq!(
            most_specific(&layers, |l| l.root.as_ref()),
            Some(RootMode::Ephemeral)
        );
        assert_eq!(most_specific(&layers, |l| l.github.as_ref()), None);
    }

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
