//! SSH の入口。
//!
//! SSH ユーザー名の GitHub のログイン名と、GitHub に登録された公開鍵で利用者を認証し、
//! ワークスペースの VM の guest-agent へシェルやコマンドを中継する。

mod access;
mod auth;
mod commands;
mod config;
mod session;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chloe_controller::Controller;
use chloe_controller::docker::DockerBackend;
use chloe_controller::store::Store;
use chloe_secrets::{MasterKey, SecretDb};
use clap::Parser;
use russh::keys::ssh_key::LineEnding;
use russh::keys::{Algorithm, PrivateKey};
use russh::server::Server as _;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

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
        methods: if config.github_app.is_some() {
            russh::MethodSet::from(
                &[
                    russh::MethodKind::PublicKey,
                    russh::MethodKind::KeyboardInteractive,
                ][..],
            )
        } else {
            russh::MethodSet::from(&[russh::MethodKind::PublicKey][..])
        },
        auth_rejection_time: Duration::from_secs(1),
        auth_rejection_time_initial: Some(Duration::from_secs(0)),
        keys: vec![host_key],
        keepalive_interval: Some(Duration::from_secs(30)),
        ..Default::default()
    });

    let controller = build_controller(&config).await?;
    let github = match &config.github_app {
        Some(path) => {
            let app_config = chloe_github::AppConfig::load(&expand_home(path))?;
            let resolver = chloe_secrets::Resolver { connect: None };
            Some(Arc::new(
                chloe_github::GitHubApp::new(app_config, &resolver).await?,
            ))
        }
        None => None,
    };
    let listen = config.listen;
    let shared = Arc::new(Shared {
        keys: GitHubKeys::new(&config.github_url)?,
        controller,
        github,
        config,
    });
    tracing::info!(%listen, "listening");
    let mut server = Server { shared };
    server.run_on_address(ssh_config, listen).await?;
    Ok(())
}

async fn build_controller(config: &Config) -> Result<Controller<DockerBackend>> {
    if let Some(dir) = config.database.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let options = SqliteConnectOptions::new()
        .filename(&config.database)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await
        .with_context(|| format!("open {}", config.database.display()))?;
    chloe_secrets::MIGRATOR.run(&pool).await?;
    let master = MasterKey::load_or_generate(&config.master_key)
        .with_context(|| format!("load {}", config.master_key.display()))?;
    let backend = DockerBackend {
        image: config.runner.image.clone(),
        guest_dir: std::path::absolute(&config.runner.guest_dir)?,
        egress_bin: std::path::absolute(&config.runner.egress_bin)?,
        state_root: std::path::absolute(&config.runner.state_root)?,
        name_prefix: config.runner.name_prefix.clone(),
    };
    Ok(Controller::new(
        Store::new(pool.clone()),
        SecretDb::new(pool, master),
        backend,
        config.policy.clone(),
        Duration::from_secs(config.idle_timeout_secs),
    ))
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

/// 先頭の `~/` をホームディレクトリに直す。
fn expand_home(path: &Path) -> PathBuf {
    match (path.strip_prefix("~"), std::env::var_os("HOME")) {
        (Ok(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => path.to_path_buf(),
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
