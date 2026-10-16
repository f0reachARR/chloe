//! 開発用のランナーの実装。ワークスペースごとに `chloe-dev-runner` のコンテナを起動する。
//!
//! ワークスペースの状態は `{state_root}/{key}/` に置く (ワークスペースと Docker のディスク、
//! vsock、CA、ログ)。プロキシに渡す本物の値はコンテナの環境変数で渡すので、
//! `docker inspect` で見える。本番のランナーでは別の渡し方にする。

use std::path::PathBuf;

use chloe_secrets::ExposeSecret;
use tokio::process::Command;
use tracing::info;

use crate::Error;
use crate::runner::{RunnerBackend, RunnerSpec};

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
}

impl RunnerBackend for DockerBackend {
    async fn start(&self, spec: RunnerSpec) -> Result<PathBuf, Error> {
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
        // 本物の値はコマンドの引数に書かず、docker の環境から渡す (`-e NAME` は値を環境から読む)。
        for (name, value) in &spec.secrets {
            command.args(["-e", name]).env(name, value.expose_secret());
        }
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
        Ok(state.join("vsock.sock"))
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
        let secrets: std::collections::BTreeMap<&str, &str> = spec
            .secrets
            .iter()
            .map(|(k, v)| (k.as_str(), v.expose_secret()))
            .collect();
        let line = serde_json::json!({ "policy": spec.policy, "secrets": secrets }).to_string();
        let socket = self.state_dir(&spec.key).join("egress.sock");
        let mut stream = tokio::net::UnixStream::connect(&socket).await?;
        stream.write_all(format!("{line}\n").as_bytes()).await?;
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response).await?;
        if response.contains("\"ok\":true") {
            Ok(())
        } else {
            Err(Error::Runner(format!(
                "egress rejected the update: {}",
                response.trim()
            )))
        }
    }

    async fn is_running(&self, key: &str) -> Result<bool, Error> {
        let output = Command::new("docker")
            .args(["inspect", "-f", "{{.State.Running}}", &self.container(key)])
            .output()
            .await?;
        Ok(output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "true")
    }
}
