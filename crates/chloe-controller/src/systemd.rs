//! ランナーを systemd の unit (`chloe-runner@<key>.service`) で動かす実装。
//!
//! unit (`deploy/systemd/`) は、root でネットワーク namespace を用意し、この実装を動かす利用者
//! (`chloe`) でプロキシ、DNS、Firecracker を起動する。状態の置き場所は unit と同じ
//! `/etc/chloe/runner.env` の `CHLOE_STATE_ROOT` から読む。unit の起動、停止、読み直しは
//! `systemctl` で行い、polkit の規則 (`deploy/polkit/50-chloe.rules`) でこの利用者に許しておく。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Output;

use tokio::process::Command;
use tracing::info;

use crate::Error;
use crate::local;
use crate::runner::{RunnerBackend, RunnerSpec};

const UNIT_PREFIX: &str = "chloe-runner@";
const UNIT_SUFFIX: &str = ".service";

/// systemd で動かすランナーの設定。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemdConfig {
    /// unit と共有するランナーの設定 (`CHLOE_STATE_ROOT` など)。
    #[serde(default = "default_env_file")]
    pub env_file: PathBuf,
}

fn default_env_file() -> PathBuf {
    "/etc/chloe/runner.env".into()
}

impl SystemdConfig {
    pub fn backend(&self) -> std::io::Result<SystemdBackend> {
        let text = std::fs::read_to_string(&self.env_file).map_err(|e| {
            std::io::Error::new(e.kind(), format!("read {}: {e}", self.env_file.display()))
        })?;
        let state_root = parse_env(&text)
            .remove("CHLOE_STATE_ROOT")
            .filter(|v| Path::new(v).is_absolute())
            .ok_or_else(|| {
                std::io::Error::other(format!(
                    "{} has no absolute CHLOE_STATE_ROOT",
                    self.env_file.display()
                ))
            })?;
        Ok(SystemdBackend {
            state_root: state_root.into(),
        })
    }
}

pub struct SystemdBackend {
    /// ワークスペースごとの状態を置くディレクトリ。
    pub state_root: PathBuf,
}

impl SystemdBackend {
    fn state_dir(&self, key: &str) -> PathBuf {
        self.state_root.join(key)
    }

    async fn systemctl(&self, args: &[&str]) -> Result<Output, Error> {
        Ok(Command::new("systemctl")
            .arg("--no-ask-password")
            .args(args)
            .output()
            .await?)
    }

    /// `systemctl` を実行し、失敗したら標準エラーを添えて返す。
    async fn systemctl_ok(&self, args: &[&str]) -> Result<(), Error> {
        let output = self.systemctl(args).await?;
        if output.status.success() {
            Ok(())
        } else {
            Err(Error::Runner(format!(
                "systemctl {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            )))
        }
    }
}

fn unit(key: &str) -> String {
    format!("{UNIT_PREFIX}{key}{UNIT_SUFFIX}")
}

impl RunnerBackend for SystemdBackend {
    async fn start(&self, spec: RunnerSpec) -> Result<(), Error> {
        let state = self.state_dir(&spec.key);
        local::write_config(&state, &spec).await?;
        // ワークスペースごとの値。chloe-runner-start が読む。
        let mut env = format!("CHLOE_ROOT_MODE={}\n", spec.root_mode.as_str());
        if let Some(gib) = spec.workspace_disk_gib {
            env.push_str(&format!("WORKSPACE_SIZE={gib}G\n"));
        }
        if let Some(gib) = spec.docker_disk_gib {
            env.push_str(&format!("DOCKER_SIZE={gib}G\n"));
        }
        tokio::fs::write(state.join("config/runner.env"), env).await?;

        let unit = unit(&spec.key);
        // 止まりきっていない古いランナーが残っていれば止める。
        let _ = self.systemctl(&["stop", &unit]).await;
        self.systemctl_ok(&["start", &unit]).await?;
        info!(key = spec.key, unit, "runner unit started");
        Ok(())
    }

    async fn stop(&self, key: &str) -> Result<(), Error> {
        self.systemctl_ok(&["stop", &unit(key)]).await
    }

    async fn is_running(&self, key: &str) -> Result<bool, Error> {
        let output = self
            .systemctl(&["is-active", "--quiet", &unit(key)])
            .await?;
        Ok(output.status.success())
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
        self.systemctl_ok(&["reload", &unit(&spec.key)])
            .await
            .map_err(|e| Error::Runner(format!("failed to update the DNS allowlist: {e}")))
    }

    async fn connect_agent(
        &self,
        key: &str,
        wait: std::time::Duration,
    ) -> Result<crate::runner::AgentStream, Error> {
        local::connect_agent(&self.state_dir(key), wait).await
    }

    fn local_broker_socket(&self, key: &str) -> Option<PathBuf> {
        Some(local::broker_socket(&self.state_dir(key)))
    }

    async fn list(&self) -> Result<Vec<String>, Error> {
        let pattern = format!("{UNIT_PREFIX}*{UNIT_SUFFIX}");
        let output = self
            .systemctl(&[
                "list-units",
                "--type=service",
                "--state=active,reloading,activating",
                "--plain",
                "--no-legend",
                "--no-pager",
                &pattern,
            ])
            .await?;
        if !output.status.success() {
            return Err(Error::Runner(format!(
                "systemctl list-units failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(parse_units(&String::from_utf8_lossy(&output.stdout)))
    }
}

/// `systemctl list-units --plain --no-legend` の出力から、ランナーの識別子を取り出す。
fn parse_units(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter_map(|unit| unit.strip_prefix(UNIT_PREFIX)?.strip_suffix(UNIT_SUFFIX))
        .map(str::to_string)
        .collect()
}

/// `KEY=VALUE` の行を読む (systemd の EnvironmentFile と sh の両方で読める範囲)。
/// 値の前後の引用符は外す。
fn parse_env(text: &str) -> HashMap<String, String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| {
            let v = v.trim();
            let v = v
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .or_else(|| v.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                .unwrap_or(v);
            (k.trim().to_string(), v.to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_shipped_env_file() {
        let env = parse_env(include_str!("../../../deploy/runner.env"));
        assert_eq!(env["CHLOE_STATE_ROOT"], "/var/lib/chloe/workspaces");
        assert_eq!(env["CHLOE_NET_POOL"], "10.250.0.0/16");
    }

    #[test]
    fn parses_quoted_values() {
        let env = parse_env("A=\"x y\"\nB='z'\n# C=1\n  D = w \n");
        assert_eq!(env["A"], "x y");
        assert_eq!(env["B"], "z");
        assert_eq!(env["D"], "w");
        assert!(!env.contains_key("C"));
    }

    #[test]
    fn parses_units() {
        let output = "chloe-runner@ws-3.service loaded active running chloe runner ws-3\n\
                      chloe-runner@ws-12.service loaded reloading reload chloe runner ws-12\n\
                      other.service loaded active running other\n";
        assert_eq!(parse_units(output), ["ws-3", "ws-12"]);
    }

    #[test]
    fn backend_reads_state_root() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runner.env");
        std::fs::write(&path, "CHLOE_STATE_ROOT=/srv/chloe\n").unwrap();
        let backend = SystemdConfig {
            env_file: path.clone(),
        }
        .backend()
        .unwrap();
        assert_eq!(backend.state_root, Path::new("/srv/chloe"));

        std::fs::write(&path, "CHLOE_STATE_ROOT=relative\n").unwrap();
        assert!(SystemdConfig { env_file: path }.backend().is_err());
    }
}
