//! ゲートウェイの設定。

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;

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
    /// シークレットのマスター鍵。なければ生成する。
    pub master_key: PathBuf,
    /// 最後の接続が閉じてからランナーを止めるまでの秒数。
    #[serde(default = "default_idle_timeout")]
    pub idle_timeout_secs: u64,
    /// ログインを許す GitHub のログイン名。GitHub App による Organization の所属の確認を
    /// 入れるまでは、ここで絞る。
    pub allowed_users: Vec<String>,
    /// team と Organization のワークスペースに入れる利用者 (持ち主 → ログイン名)。
    /// GitHub App による所属の確認を入れるまでの仮のもの。
    #[serde(default)]
    pub memberships: BTreeMap<String, Vec<String>>,
    /// すべてのワークスペースに共通の許可リスト。
    #[serde(default)]
    pub policy: BasePolicy,
    pub runner: DockerRunner,
}

/// 開発用のランナー (chloe-dev-runner のコンテナ) の設定。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DockerRunner {
    #[serde(default = "default_runner_image")]
    pub image: String,
    pub guest_dir: PathBuf,
    pub egress_bin: PathBuf,
    pub state_root: PathBuf,
    #[serde(default = "default_name_prefix")]
    pub name_prefix: String,
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

fn default_runner_image() -> String {
    "chloe-dev-runner".into()
}

fn default_name_prefix() -> String {
    "chloe-".into()
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
        assert_eq!(config.runner.image, "chloe-dev-runner");
    }
}
