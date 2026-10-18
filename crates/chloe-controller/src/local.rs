//! 同じホストでランナーを動かす実装 (Docker と systemd) の選択と、両方に共通の処理。
//!
//! どちらの実装も、ワークスペースの状態を `{state_root}/{key}/` に置く (ワークスペースと Docker の
//! ディスク、vsock、CA、ログ、本物の値の問い合わせ用ソケット)。本物の値はランナーに渡さない。

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde::de::Error as _;
use tracing::info;

use crate::Error;
use crate::docker::{DockerBackend, DockerConfig};
use crate::runner::{AgentStream, RunnerBackend, RunnerSpec};
use crate::systemd::{SystemdBackend, SystemdConfig};

/// 設定の `[runner]`。`kind` で実装を選ぶ。省くと Docker (今までの設定をそのまま読む)。
#[derive(Debug, Clone)]
pub enum RunnerConfig {
    Docker(DockerConfig),
    Systemd(SystemdConfig),
}

impl<'de> Deserialize<'de> for RunnerConfig {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut table = serde_json::Map::deserialize(deserializer)?;
        let kind = table.remove("kind");
        let rest = serde_json::Value::Object(table);
        match kind.as_ref().map(|k| k.as_str()) {
            None | Some(Some("docker")) => DockerConfig::deserialize(rest)
                .map(Self::Docker)
                .map_err(D::Error::custom),
            Some(Some("systemd")) => SystemdConfig::deserialize(rest)
                .map(Self::Systemd)
                .map_err(D::Error::custom),
            _ => Err(D::Error::custom(
                "unknown runner kind (expected \"docker\" or \"systemd\")",
            )),
        }
    }
}

impl RunnerConfig {
    pub fn backend(&self) -> std::io::Result<LocalBackend> {
        Ok(match self {
            Self::Docker(config) => LocalBackend::Docker(config.backend()?),
            Self::Systemd(config) => LocalBackend::Systemd(config.backend()?),
        })
    }
}

pub enum LocalBackend {
    Docker(DockerBackend),
    Systemd(SystemdBackend),
}

impl LocalBackend {
    fn state_root(&self) -> &Path {
        match self {
            Self::Docker(b) => &b.state_root,
            Self::Systemd(b) => &b.state_root,
        }
    }

    pub fn state_dir(&self, key: &str) -> PathBuf {
        self.state_root().join(key)
    }

    /// ランナーのプロキシが本物の値を問い合わせる Unix ソケット。
    pub fn broker_socket(&self, key: &str) -> PathBuf {
        broker_socket(&self.state_dir(key))
    }
}

impl RunnerBackend for LocalBackend {
    async fn start(&self, spec: RunnerSpec) -> Result<(), Error> {
        match self {
            Self::Docker(b) => b.start(spec).await,
            Self::Systemd(b) => b.start(spec).await,
        }
    }

    async fn stop(&self, key: &str) -> Result<(), Error> {
        match self {
            Self::Docker(b) => b.stop(key).await,
            Self::Systemd(b) => b.stop(key).await,
        }
    }

    async fn is_running(&self, key: &str) -> Result<bool, Error> {
        match self {
            Self::Docker(b) => b.is_running(key).await,
            Self::Systemd(b) => b.is_running(key).await,
        }
    }

    async fn remove_state(&self, key: &str) -> Result<(), Error> {
        match self {
            Self::Docker(b) => b.remove_state(key).await,
            Self::Systemd(b) => b.remove_state(key).await,
        }
    }

    async fn update(&self, spec: RunnerSpec) -> Result<(), Error> {
        match self {
            Self::Docker(b) => b.update(spec).await,
            Self::Systemd(b) => b.update(spec).await,
        }
    }

    async fn connect_agent(&self, key: &str, wait: Duration) -> Result<AgentStream, Error> {
        connect_agent(&self.state_dir(key), wait).await
    }

    fn local_broker_socket(&self, key: &str) -> Option<PathBuf> {
        Some(self.broker_socket(key))
    }

    async fn list(&self) -> Result<Vec<String>, Error> {
        match self {
            Self::Docker(b) => b.list().await,
            Self::Systemd(b) => b.list().await,
        }
    }
}

pub(crate) fn broker_socket(state: &Path) -> PathBuf {
    state.join("broker.sock")
}

/// guest-agent につながる vsock の Unix ソケット (Firecracker の vsock の UDS)。
pub(crate) fn vsock_path(state: &Path) -> PathBuf {
    state.join("vsock.sock")
}

pub(crate) async fn connect_agent(state: &Path, wait: Duration) -> Result<AgentStream, Error> {
    let stream = chloe_agent_proto::firecracker::connect(
        &vsock_path(state),
        chloe_agent_proto::AGENT_PORT,
        wait,
    )
    .await?;
    Ok(Box::new(stream))
}

/// ランナーが読む方針 (`config/policy.json`) とゲストの環境変数 (`config/guest.env`) を書く。
pub(crate) async fn write_config(state: &Path, spec: &RunnerSpec) -> Result<(), Error> {
    let config = state.join("config");
    tokio::fs::create_dir_all(&config).await?;
    write_policy(state, spec).await?;
    let guest_env: String = spec
        .guest_env
        .iter()
        .map(|(k, v)| format!("{k}={v}\n"))
        .collect();
    tokio::fs::write(config.join("guest.env"), guest_env).await?;
    Ok(())
}

async fn write_policy(state: &Path, spec: &RunnerSpec) -> Result<(), Error> {
    tokio::fs::write(
        state.join("config/policy.json"),
        serde_json::to_vec_pretty(&spec.policy).map_err(|e| Error::Runner(e.to_string()))?,
    )
    .await?;
    Ok(())
}

/// 動いているプロキシに新しい方針を送り、受け付けられたら `config/policy.json` も書き直す
/// (VM の DNS の許可リストは、呼び出し側がこのファイルから作り直させる)。
pub(crate) async fn send_policy(state: &Path, spec: &RunnerSpec) -> Result<(), Error> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let line = serde_json::json!({ "policy": spec.policy }).to_string();
    let mut stream = tokio::net::UnixStream::connect(state.join("egress.sock")).await?;
    stream.write_all(format!("{line}\n").as_bytes()).await?;
    let mut response = String::new();
    BufReader::new(stream).read_line(&mut response).await?;
    if !response.contains("\"ok\":true") {
        return Err(Error::Runner(format!(
            "egress rejected the update: {}",
            response.trim()
        )));
    }
    write_policy(state, spec).await
}

/// 止めたランナーの状態のディレクトリを消す。
pub(crate) async fn remove_state_dir(state: &Path, key: &str) -> Result<(), Error> {
    match tokio::fs::remove_dir_all(state).await {
        Ok(()) => {
            info!(key, "runner state removed");
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_defaults_to_docker() {
        let config: RunnerConfig = toml::from_str(
            r#"
            guest_dir = "build/guest"
            egress_bin = "target/debug/chloe-egress"
            state_root = "build/dev/workspaces"
            "#,
        )
        .unwrap();
        let RunnerConfig::Docker(config) = config else {
            panic!("expected docker");
        };
        assert_eq!(config.image, "chloe-dev-runner");
    }

    #[test]
    fn parses_systemd() {
        let config: RunnerConfig = toml::from_str(r#"kind = "systemd""#).unwrap();
        let RunnerConfig::Systemd(config) = config else {
            panic!("expected systemd");
        };
        assert_eq!(config.env_file, Path::new("/etc/chloe/runner.env"));
    }

    #[test]
    fn rejects_unknown_kind_and_fields() {
        assert!(toml::from_str::<RunnerConfig>(r#"kind = "podman""#).is_err());
        assert!(
            toml::from_str::<RunnerConfig>(
                r#"
                kind = "systemd"
                state_root = "/tmp"
                "#
            )
            .is_err()
        );
    }
}
