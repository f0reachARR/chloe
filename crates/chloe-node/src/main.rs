//! ランナーのホストに置く常駐プロセス。
//!
//! コントロールプレーンに相互 TLS でトンネルを張り、その上で、このホストのランナーの起動、
//! 停止、一覧、方針の入れ替え、guest-agent への接続の中継を受ける。待ち受けはしないので、
//! コントロールプレーンからこのホストへ届かなくてもよい。トンネルが切れたら張り直す。
//! ランナーのプロキシからの本物の値の問い合わせは、ランナーごとの Unix ソケットで受けて
//! コントロールプレーンへ中継する。
//!
//! ```text
//! chloe-node join --server https://control-plane:8444 --token <参加用のトークン> --dir <証明書の置き場所>
//! chloe-node run --config node.toml
//! ```

mod api;
mod relay;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chloe_controller::local::RunnerConfig;
use chloe_controller::runner::RunnerBackend;
use chloe_mtls::Credentials;
use clap::{Parser, Subcommand};
use serde::Deserialize;
use tracing::{info, warn};

#[derive(Parser)]
#[command(about = "chloe のランナーのホストの常駐プロセス")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// コントロールプレーンに参加し、このホストの証明書を受け取る。
    Join {
        /// コントロールプレーンの内部 API の URL。
        #[arg(long)]
        server: String,
        /// `chloe-gateway node add` が表示した参加用のトークン。
        #[arg(long, env = "CHLOE_NODE_JOIN_TOKEN")]
        token: String,
        /// 証明書と鍵を置くディレクトリ。
        #[arg(long)]
        dir: PathBuf,
    },
    /// コントロールプレーンにトンネルを張り、その上で頼みを受ける。
    Run {
        #[arg(long, env = "CHLOE_NODE_CONFIG")]
        config: PathBuf,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    /// 段階 22 より前の設定 (コントロールプレーンからの API の待ち受け)。もう待ち受けないので、
    /// 書いてあっても警告を出して使わない。
    #[serde(default)]
    listen: Option<SocketAddr>,
    /// `join` で受け取った証明書と鍵のディレクトリ。
    credentials: PathBuf,
    /// コントロールプレーンの内部 API の URL (トンネルを張り、本物の値の問い合わせを中継する先)。
    control_plane: String,
    runner: RunnerConfig,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    match Cli::parse().command {
        Command::Join { server, token, dir } => join(&server, &token, &dir).await,
        Command::Run { config } => run(&config).await,
    }
}

#[derive(Deserialize)]
struct Joined {
    name: String,
    cert_pem: String,
}

async fn join(server: &str, token: &str, dir: &Path) -> Result<()> {
    let server = server.trim_end_matches('/');
    let (secret, fingerprint) = token
        .rsplit_once('.')
        .context("the join token must look like <secret>.<CA fingerprint>")?;
    let ca_pem = chloe_mtls::fetch_ca(server, fingerprint).await?;
    let (key_pem, csr_pem) = chloe_mtls::new_key_and_request()?;
    let response = chloe_mtls::client_trusting(&ca_pem)?
        .post(format!("{server}/internal/v1/join"))
        .json(&serde_json::json!({ "token": secret, "csr": csr_pem }))
        .send()
        .await?;
    if !response.status().is_success() {
        bail!(
            "the control plane refused to join: {}",
            response.text().await.unwrap_or_default().trim()
        );
    }
    let joined: Joined = response.json().await?;
    Credentials {
        cert_pem: joined.cert_pem,
        key_pem,
        ca_pem,
    }
    .save(dir)?;
    println!(
        "joined as {}; credentials are in {}",
        joined.name,
        dir.display()
    );
    Ok(())
}

/// トンネルを張り直す間隔。張れないあいだは倍にしていき、張れたら最初に戻す。
const MIN_RETRY: Duration = Duration::from_secs(1);
const MAX_RETRY: Duration = Duration::from_secs(30);

async fn run(path: &Path) -> Result<()> {
    let config: Config = toml::from_str(
        &std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?,
    )?;
    let credentials = Credentials::load(&config.credentials).with_context(|| {
        format!(
            "read the credentials in {} (run `chloe-node join` first)",
            config.credentials.display()
        )
    })?;
    let state = Arc::new(api::NodeState {
        backend: config.runner.backend()?,
        control_plane: chloe_mtls::client(&credentials)?,
        control_plane_url: config.control_plane.trim_end_matches('/').to_string(),
        relays: Default::default(),
    });
    // 再起動の前から動いているランナーの問い合わせを、また中継する。
    for key in state.backend.list().await? {
        relay::start(&state, &key)?;
    }
    if let Some(listen) = config.listen {
        warn!(
            %listen,
            "ignoring listen in the configuration: chloe-node connects to the control plane and no longer listens"
        );
    }
    let router = api::router(state);
    let mut retry = MIN_RETRY;
    loop {
        let mut connected = false;
        let result =
            chloe_mtls::tunnel::dial(&config.control_plane, &credentials, router.clone(), || {
                info!(
                    control_plane = config.control_plane,
                    "connected to the control plane"
                );
                connected = true;
            })
            .await;
        match result {
            Ok(()) => warn!("the tunnel to the control plane closed"),
            Err(error) => warn!(
                control_plane = config.control_plane,
                "the tunnel to the control plane failed: {error}"
            ),
        }
        if connected {
            retry = MIN_RETRY;
        }
        tokio::time::sleep(retry).await;
        retry = (retry * 2).min(MAX_RETRY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// リポジトリに置いた設定の例が読めること。
    #[test]
    fn parses_shipped_example() {
        let config: Config = toml::from_str(include_str!("../../../deploy/node.toml")).unwrap();
        assert!(matches!(config.runner, RunnerConfig::Systemd(_)));
    }
}
