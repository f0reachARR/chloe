//! 開発用のランナーの実装。ワークスペースごとに `chloe-dev-runner` のコンテナを起動する。
//!
//! ワークスペースの状態は `{state_root}/{key}/` に置く (ワークスペースと Docker のディスク、
//! vsock、CA、ログ、本物の値の問い合わせ用ソケット)。本物の値はランナーに渡さない。

use std::path::PathBuf;
use std::time::Duration;

use chloe_agent_proto::AGENT_PORT;
use tokio::process::Command;
use tracing::info;

use crate::Error;
use crate::runner::{AgentStream, RunnerBackend, RunnerSpec};

/// 開発用のランナー (chloe-dev-runner のコンテナ) の設定。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DockerConfig {
    #[serde(default = "default_image")]
    pub image: String,
    pub guest_dir: PathBuf,
    pub egress_bin: PathBuf,
    pub state_root: PathBuf,
    #[serde(default = "default_name_prefix")]
    pub name_prefix: String,
}

fn default_image() -> String {
    "chloe-dev-runner".into()
}

fn default_name_prefix() -> String {
    "chloe-".into()
}

impl DockerConfig {
    /// 設定のパスを絶対パスにして、Docker の実装を作る (コンテナに渡すパスは絶対パスが要る)。
    pub fn backend(&self) -> std::io::Result<DockerBackend> {
        Ok(DockerBackend {
            image: self.image.clone(),
            guest_dir: std::path::absolute(&self.guest_dir)?,
            egress_bin: std::path::absolute(&self.egress_bin)?,
            state_root: std::path::absolute(&self.state_root)?,
            name_prefix: self.name_prefix.clone(),
        })
    }
}

pub struct DockerBackend {
    /// ランナーのコンテナのイメージ。
    pub image: String,
    /// カーネルと rootfs を置いたディレクトリ。
    pub guest_dir: PathBuf,
    /// コンテナに渡す chloe-egress のバイナリ。
    pub egress_bin: PathBuf,
    /// ワークスペースごとの状態を置くディレクトリ。
    pub state_root: PathBuf,
    /// コンテナの名前の接頭辞。
    pub name_prefix: String,
}

impl DockerBackend {
    fn container(&self, key: &str) -> String {
        format!("{}{key}", self.name_prefix)
    }

    pub fn state_dir(&self, key: &str) -> PathBuf {
        self.state_root.join(key)
    }

    /// ランナーのプロキシが本物の値を問い合わせる Unix ソケット。
    pub fn broker_socket(&self, key: &str) -> PathBuf {
        self.state_dir(key).join("broker.sock")
    }

    /// guest-agent につながる vsock の Unix ソケット (Firecracker の vsock の UDS)。
    pub fn vsock_path(&self, key: &str) -> PathBuf {
        self.state_dir(key).join("vsock.sock")
    }
}

impl RunnerBackend for DockerBackend {
    async fn start(&self, spec: RunnerSpec) -> Result<(), Error> {
        let state = self.state_dir(&spec.key);
        let config = state.join("config");
        tokio::fs::create_dir_all(&config).await?;
        tokio::fs::write(
            config.join("policy.json"),
            serde_json::to_vec_pretty(&spec.policy).map_err(|e| Error::Runner(e.to_string()))?,
        )
        .await?;
        let guest_env: String = spec
            .guest_env
            .iter()
            .map(|(k, v)| format!("{k}={v}\n"))
            .collect();
        tokio::fs::write(config.join("guest.env"), guest_env).await?;

        // 止まりきっていない古いコンテナが残っていれば片づける。
        let _ = Command::new("docker")
            .args(["rm", "-f", &self.container(&spec.key)])
            .output()
            .await;

        let mut command = Command::new("docker");
        command
            .args(["run", "-d", "--rm", "--name", &self.container(&spec.key)])
            .args([
                "--privileged",
                "--device",
                "/dev/kvm",
                "--device",
                "/dev/net/tun",
            ])
            .args(["-e", "CHLOE_NET_MODE=proxy"])
            .args(["-e", "CHLOE_POLICY=/state/config/policy.json"])
            .args(["-e", "CHLOE_GUEST_ENV=/state/config/guest.env"])
            .args([
                "-e",
                &format!("CHLOE_ROOT_MODE={}", spec.root_mode.as_str()),
            ])
            .args(["-v", &format!("{}:/guest:ro", self.guest_dir.display())])
            .args(["-v", &format!("{}:/state", state.display())])
            .args([
                "-v",
                &format!(
                    "{}:/opt/chloe/bin/chloe-egress:ro",
                    self.egress_bin.display()
                ),
            ]);
        command.arg(&self.image);
        let output = command.output().await?;
        if !output.status.success() {
            return Err(Error::Runner(format!(
                "docker run failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        info!(
            key = spec.key,
            container = self.container(&spec.key),
            "runner container started"
        );
        Ok(())
    }

    async fn stop(&self, key: &str) -> Result<(), Error> {
        let output = Command::new("docker")
            .args(["rm", "-f", &self.container(key)])
            .output()
            .await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.contains("No such container") {
                return Err(Error::Runner(format!(
                    "docker rm failed: {}",
                    stderr.trim()
                )));
            }
        }
        Ok(())
    }

    async fn update(&self, spec: RunnerSpec) -> Result<(), Error> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let line = serde_json::json!({ "policy": spec.policy }).to_string();
        let socket = self.state_dir(&spec.key).join("egress.sock");
        let mut stream = tokio::net::UnixStream::connect(&socket).await?;
        stream.write_all(format!("{line}\n").as_bytes()).await?;
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response).await?;
        if response.contains("\"ok\":true") {
            // VM の DNS が問い合わせる名前の一覧も、新しい方針から作り直す。
            let policy = self.state_dir(&spec.key).join("config/policy.json");
            let json = serde_json::to_vec_pretty(&spec.policy)
                .map_err(|e| Error::Runner(e.to_string()))?;
            tokio::fs::write(&policy, json).await?;
            let output = Command::new("docker")
                .args(["exec", &self.container(&spec.key)])
                .args(["/opt/chloe/dns-update.sh", "/state/config/policy.json"])
                .output()
                .await?;
            if !output.status.success() {
                return Err(Error::Runner(format!(
                    "failed to update the DNS allowlist: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
            Ok(())
        } else {
            Err(Error::Runner(format!(
                "egress rejected the update: {}",
                response.trim()
            )))
        }
    }

    async fn connect_agent(&self, key: &str, wait: Duration) -> Result<AgentStream, Error> {
        let stream =
            chloe_agent_proto::firecracker::connect(&self.vsock_path(key), AGENT_PORT, wait)
                .await?;
        Ok(Box::new(stream))
    }

    fn local_broker_socket(&self, key: &str) -> Option<PathBuf> {
        Some(self.broker_socket(key))
    }

    async fn list(&self) -> Result<Vec<String>, Error> {
        let output = Command::new("docker")
            .args([
                "ps",
                "--filter",
                &format!("name=^{}", self.name_prefix),
                "--format",
                "{{.Names}}",
            ])
            .output()
            .await?;
        if !output.status.success() {
            return Err(Error::Runner(format!(
                "docker ps failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|name| name.strip_prefix(&self.name_prefix))
            .map(str::to_string)
            .collect())
    }

    async fn is_running(&self, key: &str) -> Result<bool, Error> {
        let output = Command::new("docker")
            .args(["inspect", "-f", "{{.State.Running}}", &self.container(key)])
            .output()
            .await?;
        Ok(output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "true")
    }
}
