//! SSH の入口。
//!
//! SSH ユーザー名の GitHub のログイン名と、GitHub に登録された公開鍵で利用者を認証し、
//! ワークスペースの VM の guest-agent へシェルやコマンドを中継する。

mod auth;
mod config;
mod session;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use russh::keys::ssh_key::LineEnding;
use russh::keys::{Algorithm, PrivateKey};
use russh::server::Server as _;

use crate::auth::GitHubKeys;
use crate::config::Config;
use crate::session::{Connection, Shared};

#[derive(Parser)]
#[command(about = "chloe の SSH の入口")]
struct Cli {
    /// 設定ファイル (TOML)。
    #[arg(long, env = "CHLOE_GATEWAY_CONFIG")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let cli = Cli::parse();
    let config: Config = toml::from_str(
        &std::fs::read_to_string(&cli.config)
            .with_context(|| format!("read {}", cli.config.display()))?,
    )?;

    let host_key = load_or_generate_host_key(&config.host_key)?;
    let ssh_config = Arc::new(russh::server::Config {
        methods: russh::MethodSet::from(&[russh::MethodKind::PublicKey][..]),
        auth_rejection_time: Duration::from_secs(1),
        auth_rejection_time_initial: Some(Duration::from_secs(0)),
        keys: vec![host_key],
        keepalive_interval: Some(Duration::from_secs(30)),
        ..Default::default()
    });

    let listen = config.listen;
    let shared = Arc::new(Shared {
        keys: GitHubKeys::new(&config.github_url)?,
        config,
    });
    tracing::info!(%listen, "listening");
    let mut server = Server { shared };
    server.run_on_address(ssh_config, listen).await?;
    Ok(())
}

struct Server {
    shared: Arc<Shared>,
}

impl russh::server::Server for Server {
    type Handler = Connection;

    fn new_client(&mut self, peer: Option<std::net::SocketAddr>) -> Connection {
        Connection::new(self.shared.clone(), peer)
    }
}

fn load_or_generate_host_key(path: &Path) -> Result<PrivateKey> {
    if !path.exists() {
        let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        key.write_openssh_file(path, LineEnding::LF)
            .with_context(|| format!("write host key to {}", path.display()))?;
        tracing::info!(path = %path.display(), "generated a host key");
    }
    russh::keys::load_secret_key(path, None)
        .with_context(|| format!("load host key from {}", path.display()))
}
