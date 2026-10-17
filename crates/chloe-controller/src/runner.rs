//! ランナー (ワークスペースの VM とプロキシ) の起動と停止。
//!
//! ランナーには本物の値を渡さない。ランナーのプロキシは差し替えのたびに問い合わせ用の
//! ソケットへ尋ね、controller が答える ([`crate::broker`]、[`crate::Controller::answer_secret`])。

use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use chloe_policy::{
    Action, Credential, InjectRuleConfig, PathRuleConfig, PolicyConfig, SignRuleConfig, SignScheme,
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::Error;
use crate::catalog::{self, SecretSpec, SignKind};
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
    /// 止めたランナーの状態 (ワークスペースと Docker のディスク、CA、ログ) を消す。
    fn remove_state(&self, key: &str) -> impl Future<Output = Result<(), Error>> + Send;
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

/// すべてのワークスペースに共通の、外への通信の規則。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BasePolicy {
    /// 宛先、メソッド、パスの規則。上から順に評価し、どれにも一致しなければ遮断する。
    #[serde(default)]
    pub rules: Vec<PathRuleConfig>,
}

impl BasePolicy {
    /// 全体の設定に、Organization と team の設定を (上の層から順に) 重ねる。
    ///
    /// 規則は上の層のものを先に並べるので、最初に一致した規則で決まる評価では、上の層
    /// (サーバーの運用者、Organization) の規則を下の層が覆せない。
    pub fn with_layers(&self, layers: &[PolicyLayer]) -> BasePolicy {
        let mut merged = self.clone();
        for layer in layers {
            merged.rules.extend(layer.rules.iter().cloned());
        }
        merged
    }

    /// 規則が成り立っているかを確かめる ([`chloe_policy::check_rules`])。
    pub fn check(&self) -> Result<(), String> {
        chloe_policy::check_rules(&self.rules)
    }
}

/// Organization か team の設定 (データベースの `policies`)。
///
/// 中継のみ (`passthrough`) の規則は置けない。復号しないのでほかの規則が効かず、置けると
/// 上の層の規則を素通りする宛先を作れてしまうためである。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyLayer {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<PathRuleConfig>,
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

impl PolicyLayer {
    /// `base` に重ねられるかを確かめる。中継のみの規則は `base` にしかないので、ほかの層は
    /// 見なくてよい。
    pub fn check(&self, base: &BasePolicy) -> Result<(), String> {
        if self.rules.iter().any(|r| r.action == Action::Passthrough) {
            return Err("passthrough rules can only be set in the gateway's [policy]".to_string());
        }
        base.with_layers(std::slice::from_ref(self)).check()
    }
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
/// 差し替えと署名の規則とプレースホルダーの環境変数は、`specs` (そのワークスペースで使える
/// シークレット) だけに作る。使えない種類のプレースホルダーを VM に入れると、
/// ツールがそれを使おうとして、サブスクリプションのログインなどほかの手段を試さなくなる。
/// 差し替えと署名の宛先は、許す規則として一番後ろに足す。ただし、すでにどれかの規則に
/// 書いた宛先には足さない (書いた規則で、許すかどうかと場所を決める)。
pub fn build_spec(
    workspace: &Workspace,
    base: &BasePolicy,
    placeholders: &std::collections::BTreeMap<(String, String), String>,
    specs: &[(String, SecretSpec)],
) -> RunnerSpec {
    let mut policy = PolicyConfig {
        inject: vec![],
        sign: vec![],
        rules: base.rules.clone(),
    };
    // ワークスペースの名前。ゲストはホスト名に使い、プロンプトに出る。シークレットが同じ
    // 環境変数を使おうとしても、こちらを先に置くので上書きされない。
    let mut guest_env: Vec<(String, String)> = vec![
        ("CHLOE_WORKSPACE".into(), workspace.display_name()),
        ("CHLOE_HOSTNAME".into(), workspace.hostname()),
    ];
    fn allow(name: &str, hosts: &[String], policy: &mut PolicyConfig) {
        let hosts: Vec<String> = hosts
            .iter()
            .filter(|host| {
                !policy.rules.iter().any(|rule| {
                    rule.hosts
                        .iter()
                        .any(|other| chloe_policy::hosts_overlap(host, other))
                })
            })
            .cloned()
            .collect();
        if !hosts.is_empty() {
            policy.rules.push(PathRuleConfig {
                name: Some(name.to_string()),
                hosts,
                methods: vec![],
                paths: vec![],
                action: Action::Allow,
            });
        }
    }
    for (name, spec) in specs {
        let placeholder = |field: &str| placeholders.get(&(name.clone(), field.to_string()));
        if spec.fields.keys().any(|field| placeholder(field).is_none()) {
            continue;
        }
        let credential = |field: &str| Credential {
            placeholder: placeholder(field).cloned().unwrap_or_default(),
            secret_env: catalog::secret_ref(name, field),
        };
        for (index, rule) in spec.inject.iter().enumerate() {
            let rule_name = format!("{name}-{index}");
            allow(&rule_name, &rule.hosts, &mut policy);
            policy.inject.push(InjectRuleConfig {
                name: rule_name,
                placeholder: credential(&rule.field).placeholder,
                hosts: rule.hosts.clone(),
                header: rule.header.clone(),
                format: rule.format,
                secret_env: catalog::secret_ref(name, &rule.field),
            });
        }
        for (index, rule) in spec.sign.iter().enumerate() {
            let scheme = match rule.scheme {
                SignKind::AwsSigV4 => SignScheme::AwsSigV4 {
                    access_key_id: credential("access_key_id"),
                    secret_access_key: credential("secret_access_key"),
                    session_token: spec
                        .fields
                        .contains_key("session_token")
                        .then(|| credential("session_token")),
                },
            };
            let rule_name = format!("{name}-sign-{index}");
            allow(&rule_name, &rule.hosts, &mut policy);
            policy.sign.push(SignRuleConfig {
                name: rule_name,
                hosts: rule.hosts.clone(),
                scheme,
            });
        }
        for (field, field_spec) in &spec.fields {
            for env in &field_spec.env {
                // 同じ環境変数を二つのシークレットが使うなら、名前の順で先のほうを渡す。
                if guest_env.iter().any(|(e, _)| e == env) {
                    tracing::warn!(workspace = %workspace.display_name(), secret = %name, env = %env, "environment variable is already used by another secret");
                    continue;
                }
                guest_env.push((env.clone(), credential(field).placeholder));
            }
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
    fn layers_keep_upper_rules_first() {
        let base = BasePolicy {
            rules: vec![rule("global", "api.github.com", Action::Deny)],
        };
        let org = PolicyLayer {
            rules: vec![rule("org", "api.github.com", Action::Allow)],
            root: Some(RootMode::Persistent),
            ..Default::default()
        };
        let team = PolicyLayer {
            rules: vec![rule("team", "api.github.com", Action::Allow)],
            root: Some(RootMode::Ephemeral),
            ..Default::default()
        };
        let layers = [org, team];
        let merged = base.with_layers(&layers);
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

    fn rule(name: &str, host: &str, action: Action) -> PathRuleConfig {
        PathRuleConfig {
            name: Some(name.into()),
            hosts: vec![host.into()],
            methods: vec![],
            paths: vec![],
            action,
        }
    }

    #[test]
    fn layers_cannot_pass_through_or_overlap_passthrough() {
        let base = BasePolicy {
            rules: vec![rule("docker", "*.docker.com", Action::Passthrough)],
        };
        base.check().unwrap();
        let passthrough = PolicyLayer {
            rules: vec![rule("org", "x.example", Action::Passthrough)],
            ..Default::default()
        };
        assert!(passthrough.check(&base).is_err());
        let overlapping = PolicyLayer {
            rules: vec![rule("team", "hub.docker.com", Action::Deny)],
            ..Default::default()
        };
        assert!(overlapping.check(&base).is_err());
        let fine = PolicyLayer {
            rules: vec![rule("team", "docker.com", Action::Allow)],
            ..Default::default()
        };
        fine.check(&base).unwrap();
        // 古い書き方 (allow、passthrough) は受け付けない。
        assert!(serde_json::from_str::<PolicyLayer>(r#"{"allow": ["x.example"]}"#).is_err());
        assert!(toml::from_str::<BasePolicy>(r#"allow = ["x.example"]"#).is_err());
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
            ("anthropic_api_key", "value", "sk-ant-api03-chloeA"),
            ("openai_api_key", "value", "sk-proj-chloeO"),
            ("github_token", "value", "ghu_chloeG"),
            ("aws", "access_key_id", "AKIAchloeK"),
            ("aws", "secret_access_key", "chloeS"),
        ]
        .into_iter()
        .map(|(k, f, v)| ((k.to_string(), f.to_string()), v.to_string()))
        .collect();
        let base = BasePolicy {
            rules: vec![
                rule("anthropic", "api.anthropic.com", Action::Allow),
                rule("docker", "registry-1.docker.io", Action::Passthrough),
                PathRuleConfig {
                    paths: vec!["/repos/acme/**".into()],
                    ..rule("acme-only", "api.github.com", Action::Allow)
                },
            ],
        };
        let aws: catalog::SecretInput = toml::from_str(
            r#"
            [fields.access_key_id]
            env = ["AWS_ACCESS_KEY_ID"]
            placeholder = "AKIA{id}"
            value = "x"
            [fields.secret_access_key]
            env = ["AWS_SECRET_ACCESS_KEY"]
            value = "y"
            [[sign]]
            scheme = "aws-sigv4"
            hosts = ["*.amazonaws.com"]
            "#,
        )
        .unwrap();
        let preset = |name: &str| (name.to_string(), catalog::spec_for_test(name));
        let spec = build_spec(
            &ws,
            &base,
            &placeholders,
            &[
                preset("anthropic_api_key"),
                preset("github_token"),
                ("aws".into(), aws.into_parts().unwrap().0.unwrap()),
                // プレースホルダーのないものは使わない。
                preset("claude_oauth_token"),
            ],
        );
        assert_eq!(spec.key, "ws-7");
        assert_eq!(
            spec.guest_env[..2],
            [
                ("CHLOE_WORKSPACE".into(), "api".into()),
                ("CHLOE_HOSTNAME".into(), "api".into()),
            ]
        );
        assert_eq!(spec.policy.inject.len(), 3);
        assert_eq!(
            spec.policy.sign,
            [SignRuleConfig {
                name: "aws-sign-0".into(),
                hosts: vec!["*.amazonaws.com".into()],
                scheme: SignScheme::AwsSigV4 {
                    access_key_id: Credential {
                        placeholder: "AKIAchloeK".into(),
                        secret_env: "CHLOE_SECRET_AWS__ACCESS_KEY_ID".into(),
                    },
                    secret_access_key: Credential {
                        placeholder: "chloeS".into(),
                        secret_env: "CHLOE_SECRET_AWS__SECRET_ACCESS_KEY".into(),
                    },
                    session_token: None,
                },
            }]
        );
        let reachable = chloe_policy::reachable_hosts(&spec.policy.rules);
        assert!(reachable.iter().any(|a| a == "*.amazonaws.com"));
        assert!(
            spec.guest_env
                .contains(&("AWS_SECRET_ACCESS_KEY".into(), "chloeS".into()))
        );
        assert!(
            !spec
                .guest_env
                .iter()
                .any(|(k, _)| k == "CLAUDE_CODE_OAUTH_TOKEN")
        );
        assert_eq!(
            spec.policy.inject[0].secret_env,
            "CHLOE_SECRET_ANTHROPIC_API_KEY"
        );
        assert!(reachable.iter().any(|a| a == "github.com"));
        // すでに規則に書いた宛先には足さない (書いた規則で絞った場所が広がらない)。
        let mentions = |host: &str| {
            spec.policy
                .rules
                .iter()
                .filter(|r| r.hosts.iter().any(|h| h == host))
                .count()
        };
        assert_eq!(mentions("api.anthropic.com"), 1);
        assert_eq!(mentions("api.github.com"), 1);
        assert_eq!(
            spec.policy.rules.last().unwrap().name.as_deref(),
            Some("aws-sign-0")
        );
        chloe_policy::check_rules(&spec.policy.rules).unwrap();
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
