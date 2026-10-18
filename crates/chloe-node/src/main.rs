//! ランナーのホストに置く常駐プロセス。
//!
//! コントロールプレーンからの相互 TLS の API で、このホストのランナーの起動、停止、一覧、
//! 方針の入れ替え、guest-agent への接続の中継を受ける。ランナーのプロキシからの本物の値の
//! 問い合わせは、ランナーごとの Unix ソケットで受けてコントロールプレーンへ中継する。
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

use anyhow::{Context, Result, bail};
use chloe_controller::local::RunnerConfig;
use chloe_controller::runner::RunnerBackend;
use chloe_mtls::{ClientAuth, Credentials};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use tracing::info;

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
    /// API で待ち受ける。
    Run {
        #[arg(long, env = "CHLOE_NODE_CONFIG")]
        config: PathBuf,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    listen: SocketAddr,
    /// `join` で受け取った証明書と鍵のディレクトリ。
    credentials: PathBuf,
    /// コントロールプレーンの内部 API の URL (本物の値の問い合わせを中継する先)。
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
    let tls = chloe_mtls::server_config(&credentials, ClientAuth::Required)?;
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    info!(listen = %config.listen, "listening");
    chloe_mtls::serve(listener, tls, api::router(state)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// リポジトリに置いた設定の例が読めること。
    #[test]
    fn parses_shipped_example() {
        let config: Config = toml::from_str(include_str!("../../../deploy/node.toml")).unwrap();
        assert!(matches!(config.runner, RunnerConfig::Docker(_)));
    }
}
