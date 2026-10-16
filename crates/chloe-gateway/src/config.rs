//! ゲートウェイの設定。
//!
//! 段階 6 でワークスペースの管理 (chloe-controller) ができるまでは、
//! ワークスペースとその VM の vsock をこの設定に直接書く。

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;

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
    /// ワークスペースを指定しなかったときに入るワークスペース。
    #[serde(default = "default_workspace")]
    pub default_workspace: String,
    pub workspaces: BTreeMap<String, Workspace>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    /// VM の guest-agent につながる Firecracker の vsock の Unix ソケット。
    pub vsock: PathBuf,
    /// このワークスペースに入れる GitHub のログイン名。
    pub members: Vec<String>,
}

impl Workspace {
    pub fn is_member(&self, login: &str) -> bool {
        self.members.iter().any(|m| m.eq_ignore_ascii_case(login))
    }
}

fn default_github_url() -> String {
    "https://github.com".into()
}

fn default_workspace() -> String {
    "default".into()
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

            [workspaces.default]
            vsock = "build/dev/vsock.sock"
            members = ["Alice"]
            "#,
        )
        .unwrap();
        assert_eq!(config.github_url, "https://github.com");
        assert!(config.workspaces["default"].is_member("alice"));
        assert!(!config.workspaces["default"].is_member("bob"));
    }
}
