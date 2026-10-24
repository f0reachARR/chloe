//! ゲートウェイの設定。

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use chloe_controller::local::RunnerConfig;
use chloe_controller::runner::BasePolicy;
use chloe_controller::workspace::{DiskConfig, MachineConfig};
use serde::Deserialize;

use crate::ban::BanConfig;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub listen: SocketAddr,
    /// ホスト鍵 (OpenSSH 形式)。なければ生成する。
    pub host_key: PathBuf,
    /// 公開鍵を取得する GitHub の URL。`{github_url}/{login}.keys` を読む。
    #[serde(default = "default_github_url")]
    pub github_url: String,
    /// GitHub のログイン名の代わりに使える SSH のユーザー名 (`git@github.com` の `git` にあたる)。
    /// このユーザー名と、GitHub に存在しないユーザー名では、利用者を公開鍵か Device Flow で決める。
    #[serde(default = "default_ssh_user")]
    pub ssh_user: String,
    /// SQLite のデータベース。なければ作る。
    pub database: PathBuf,
    /// 監査ログ (JSON Lines) を追記するファイル。ログイン、セッション、ポート転送、管理コマンド、
    /// ランナーの起動と停止、シークレットの問い合わせを記録する。値は書かない。
    #[serde(default)]
    pub audit_log: Option<PathBuf>,
    /// シークレットのマスター鍵。なければ生成する。
    pub master_key: PathBuf,
    /// 最後の接続が閉じてからランナーを止めるまでの秒数。ワークスペースごとに
    /// `chloe ws idle` で変えられる。
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
    /// GitHub のトークン (`github_token`) を VM に渡す定義。省くと github.com 向けの既定
    /// (`GH_TOKEN` と `GITHUB_TOKEN`、github.com と api.github.com など)。書き方はシークレットの
    /// 定義から値を除いたもの。GitHub Enterprise Server に向けるときに変える。
    #[serde(default)]
    pub github_token: Option<chloe_controller::catalog::DefinitionInput>,
    /// すべてのワークスペースに共通の、外への通信の規則。
    #[serde(default)]
    pub policy: BasePolicy,
    /// ワークスペースのディスクの大きさの既定と上限。
    #[serde(default)]
    pub disks: DiskConfig,
    /// ワークスペースの VM の vCPU の数とメモリの既定。
    #[serde(default)]
    pub machine: MachineConfig,
    /// 同じホスト (Docker か systemd) にランナーを置くときの設定。`cluster` とどちらか一方を書く。
    #[serde(default)]
    pub runner: Option<RunnerConfig>,
    /// 複数のホスト (chloe-node) にランナーを置くときの設定。
    #[serde(default)]
    pub cluster: Option<ClusterConfig>,
    /// HTTP(S) の管理 API。なければ待ち受けない。
    #[serde(default)]
    pub api: Option<ApiConfig>,
    /// 認証を通らずに終わった SSH の接続が多い接続元を、しばらく受け付けない。
    #[serde(default)]
    pub ban: BanConfig,
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
    /// Web の画面を開く URL (`https://chloe.example.com:8080` など。リバースプロキシを置くなら
    /// その URL)。あれば Web の画面を出す。GitHub のログインのコールバックと、cookie で来た
    /// 要求の `Origin` の確認に使う。`https://` なら cookie に `Secure` を付ける。
    #[serde(default)]
    pub public_url: Option<String>,
    /// Web の画面のビルドしたファイル (`index.html` のあるディレクトリ)。deb では
    /// `/usr/share/chloe/web`。なければ画面のファイルは出さない (開発で Vite から出すとき)。
    #[serde(default)]
    pub web_root: Option<PathBuf>,
    /// VM の中で公開したポートを開くプロキシのホストの元 (`https://preview.example.com` など)。
    /// 公開したポートごとに `<ポート>-ws-<ワークスペースの ID>.<ドメイン>` を割り当てるので、
    /// ワイルドカードの DNS と証明書が要る。ログインに Web の画面を使うので、`public_url` も要る。
    #[serde(default)]
    pub proxy_url: Option<String>,
    /// Web の画面のセッションの有効期間 (秒)。
    #[serde(default = "default_session_ttl")]
    pub session_ttl_secs: i64,
}

fn default_session_ttl() -> i64 {
    14 * 24 * 60 * 60
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

fn default_ssh_user() -> String {
    "chloe".into()
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
            allow_personal_rules = true

            [[policy.rules]]
            hosts = ["httpbin.org"]
            action = "allow"

            [disks]
            max_workspace_gib = 500

            [machine]
            memory_mib = 8192

            [runner]
            guest_dir = "build/guest"
            egress_bin = "target/debug/chloe-egress"
            state_root = "build/dev/workspaces"
            "#,
        )
        .unwrap();
        assert_eq!(config.disks.workspace_gib, 20);
        assert_eq!(config.disks.max_workspace_gib, 500);
        assert_eq!(config.machine.vcpus, 4);
        assert_eq!(config.machine.memory_mib, 8192);
        assert_eq!(config.github_url, "https://github.com");
        assert_eq!(config.idle_timeout_secs, 1800);
        assert!(config.is_allowed("alice"));
        assert!(!config.is_allowed("bob"));
        assert!(matches!(config.runner, Some(RunnerConfig::Docker(_))));
        assert_eq!(config.ban.max_failures, 10);
        assert!(config.policy.allow_personal_rules);
        assert_eq!(config.policy.rules.len(), 1);
    }

    #[test]
    fn parses_ban() {
        let config: Config = toml::from_str(
            r#"
            listen = "127.0.0.1:2222"
            host_key = "host_key"
            database = "chloe.db"
            master_key = "master.key"

            [ban]
            max_failures = 3
            ignore = ["10.0.0.0/8", "192.0.2.1"]
            "#,
        )
        .unwrap();
        assert_eq!(config.ban.max_failures, 3);
        assert_eq!(config.ban.window_secs, 600);
        assert!(config.ban.ignore[1].contains("192.0.2.1".parse().unwrap()));
        assert!(
            toml::from_str::<Config>(
                r#"
                listen = "127.0.0.1:2222"
                host_key = "host_key"
                database = "chloe.db"
                master_key = "master.key"
                [ban]
                ignore = ["10.0.0.0/40"]
                "#,
            )
            .is_err()
        );
    }

    #[test]
    fn parses_github_token_definition() {
        let config: Config = toml::from_str(
            r#"
            listen = "127.0.0.1:2222"
            host_key = "host_key"
            database = "chloe.db"
            master_key = "master.key"

            [github_token]
            env = ["GH_TOKEN"]
            placeholder = "ghu_{id}"
            [[github_token.inject]]
            hosts = ["ghe.example.com"]
            header = "authorization"
            format = "basic-password"
            "#,
        )
        .unwrap();
        let spec = config.github_token.unwrap().into_spec().unwrap();
        assert_eq!(spec.inject[0].hosts, ["ghe.example.com"]);
        assert!(spec.is_single());
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
            config.policy.check().unwrap();
            config.disks.check().unwrap();
            config.machine.check().unwrap();
        }
    }
}
