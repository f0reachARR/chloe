//! ゲートウェイの設定。

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use chloe_controller::docker::DockerConfig;
use chloe_controller::runner::BasePolicy;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub listen: SocketAddr,
    /// ホスト鍵 (OpenSSH 形式)。なければ生成する。
    pub host_key: PathBuf,
    /// 公開鍵を取得する GitHub の URL。`{github_url}/{login}.keys` を読む。
    #[serde(default = "default_github_url")]
    pub github_url: String,
    /// SQLite のデータベース。なければ作る。
    pub database: PathBuf,
    /// 監査ログ (JSON Lines) を追記するファイル。ログイン、セッション、ポート転送、管理コマンド、
    /// ランナーの起動と停止、シークレットの問い合わせを記録する。値は書かない。
    #[serde(default)]
    pub audit_log: Option<PathBuf>,
    /// シークレットのマスター鍵。なければ生成する。
    pub master_key: PathBuf,
    /// 最後の接続が閉じてからランナーを止めるまでの秒数。
    #[serde(default = "default_idle_timeout")]
    pub idle_timeout_secs: u64,
    /// エージェントが作業中のとき、止める前に作業中かを尋ね直す間隔の秒数。
    #[serde(default = "default_activity_poll")]
    pub activity_poll_secs: u64,
    /// エージェントが作業中でも、接続がなくなってからこれだけ経ったら止める秒数。
    #[serde(default = "default_max_busy_wait")]
    pub max_busy_wait_secs: u64,
    /// 保存しているランナーの記録と実際に動いているランナーを突き合わせる間隔の秒数。
    #[serde(default = "default_reconcile_interval")]
    pub reconcile_interval_secs: u64,
    /// GitHub App の設定 (`github-app.toml`)。あれば、アプリの Organization のメンバーが
    /// ログインでき、Device Flow と所属の確認が使える。
    #[serde(default)]
    pub github_app: Option<PathBuf>,
    /// ログインを許す GitHub のログイン名。GitHub App の Organization のメンバーに加えて許す。
    #[serde(default)]
    pub allowed_users: Vec<String>,
    /// サーバーの運用者の GitHub のログイン名。どの Organization と team の設定と
    /// シークレットも変えられる (GitHub の Organization の管理者に加えて)。
    #[serde(default)]
    pub admins: Vec<String>,
    /// team と Organization のワークスペースに入れる利用者 (持ち主 → ログイン名)。
    /// GitHub 上の所属に加えて許す (開発用)。
    #[serde(default)]
    pub memberships: BTreeMap<String, Vec<String>>,
    /// すべてのワークスペースに共通の許可リスト。
    #[serde(default)]
    pub policy: BasePolicy,
    /// 同じホストの Docker にランナーを置くときの設定。`cluster` とどちらか一方を書く。
    #[serde(default)]
    pub runner: Option<DockerConfig>,
    /// 複数のホスト (chloe-node) にランナーを置くときの設定。
    #[serde(default)]
    pub cluster: Option<ClusterConfig>,
    /// HTTP(S) の管理 API。なければ待ち受けない。
    #[serde(default)]
    pub api: Option<ApiConfig>,
}

/// HTTP(S) の管理 API の設定。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiConfig {
    pub listen: SocketAddr,
    /// TLS の証明書と鍵 (PEM)。どちらもなければ HTTP で待ち受ける (手前に TLS を終端する
    /// リバースプロキシを置くか、ローカルからだけ使うとき)。
    #[serde(default)]
    pub tls_cert: Option<PathBuf>,
    #[serde(default)]
    pub tls_key: Option<PathBuf>,
}

/// 複数のホストにランナーを置くときの、コントロールプレーンの設定。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClusterConfig {
    /// chloe-node から受ける内部 API (相互 TLS) の待ち受け。
    pub listen: SocketAddr,
    /// 内部 API の証明書に入れる名前 (chloe-node がつなぐホスト名か IP アドレス)。
    pub server_names: Vec<String>,
    /// CA の証明書と鍵を置くディレクトリ。なければ作る。
    pub pki_dir: PathBuf,
    /// `node add` で作る参加用のトークンの有効期間 (秒)。
    #[serde(default = "default_join_ttl")]
    pub join_ttl_secs: i64,
}

fn default_join_ttl() -> i64 {
    60 * 60
}

impl Config {
    pub fn is_allowed(&self, login: &str) -> bool {
        self.allowed_users
            .iter()
            .any(|u| u.eq_ignore_ascii_case(login))
    }
}

fn default_github_url() -> String {
    "https://github.com".into()
}

fn default_idle_timeout() -> u64 {
    30 * 60
}

fn default_activity_poll() -> u64 {
    60
}

fn default_max_busy_wait() -> u64 {
    12 * 60 * 60
}

fn default_reconcile_interval() -> u64 {
    60
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_example() {
        let config: Config = toml::from_str(
            r#"
            listen = "127.0.0.1:2222"
            host_key = "host_key"
            database = "chloe.db"
            master_key = "master.key"
            allowed_users = ["Alice"]

            [memberships]
            "org:fortefibre" = ["alice"]

            [policy]
            allow = ["httpbin.org"]

            [runner]
            guest_dir = "build/guest"
            egress_bin = "target/debug/chloe-egress"
            state_root = "build/dev/workspaces"
            "#,
        )
        .unwrap();
        assert_eq!(config.github_url, "https://github.com");
        assert_eq!(config.idle_timeout_secs, 1800);
        assert!(config.is_allowed("alice"));
        assert!(!config.is_allowed("bob"));
        assert_eq!(config.runner.unwrap().image, "chloe-dev-runner");
    }

    /// リポジトリに置いた設定の例が読めること。
    #[test]
    fn parses_shipped_examples() {
        for example in [
            include_str!("../../../deploy/gateway.toml"),
            include_str!("../../../images/guest/dev/gateway.example.toml"),
        ] {
            let config: Config = toml::from_str(example).unwrap();
            assert!(config.runner.is_some());
        }
    }
}
