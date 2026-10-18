//! 開発用のランナーの実装。ワークスペースごとに `chloe-dev-runner` のコンテナを起動する。
//!
//! 状態の置き場所は [`crate::local`] にある。

use std::path::PathBuf;
use std::time::Duration;

use tokio::process::Command;
use tracing::info;

use crate::Error;
use crate::local;
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

    fn state_dir(&self, key: &str) -> PathBuf {
        self.state_root.join(key)
    }
}

impl RunnerBackend for DockerBackend {
    async fn start(&self, spec: RunnerSpec) -> Result<(), Error> {
        let state = self.state_dir(&spec.key);
        local::write_config(&state, &spec).await?;

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
            ]);
        // なければ chloe-runner-start の既定で作る。
        if let Some(gib) = spec.workspace_disk_gib {
            command.args(["-e", &format!("WORKSPACE_SIZE={gib}G")]);
        }
        if let Some(gib) = spec.docker_disk_gib {
            command.args(["-e", &format!("DOCKER_SIZE={gib}G")]);
        }
        command
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

    async fn remove_state(&self, key: &str) -> Result<(), Error> {
        if self.is_running(key).await? {
            return Err(Error::Runner(format!("{key} is still running")));
        }
        local::remove_state_dir(&self.state_dir(key), key).await
    }

    async fn update(&self, spec: RunnerSpec) -> Result<(), Error> {
        local::send_policy(&self.state_dir(&spec.key), &spec).await?;
        // VM の DNS が問い合わせる名前の一覧も、新しい方針から作り直す。
        let output = Command::new("docker")
            .args(["exec", &self.container(&spec.key)])
            .args([
                "/opt/chloe/chloe-dns-update",
                "/state",
                "/state/config/policy.json",
            ])
            .output()
            .await?;
        if !output.status.success() {
            return Err(Error::Runner(format!(
                "failed to update the DNS allowlist: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(())
    }

    async fn connect_agent(&self, key: &str, wait: Duration) -> Result<AgentStream, Error> {
        local::connect_agent(&self.state_dir(key), wait).await
    }

    fn local_broker_socket(&self, key: &str) -> Option<PathBuf> {
        Some(local::broker_socket(&self.state_dir(key)))
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
