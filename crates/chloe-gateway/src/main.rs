//! SSH の入口。
//!
//! SSH ユーザー名の GitHub のログイン名と、GitHub に登録された公開鍵で利用者を認証し、
//! ワークスペースの VM の guest-agent へシェルやコマンドを中継する。

mod access;
mod api;
mod auth;
mod backend;
mod commands;
mod config;
mod device_flow;
mod github_token;
mod internal;
mod ops;
mod session;
mod tui;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chloe_controller::Controller;
use chloe_controller::cluster::ClusterBackend;
use chloe_controller::store::Store;
use chloe_secrets::{MasterKey, SecretDb};
use clap::Parser;
use russh::keys::ssh_key::LineEnding;
use russh::keys::{Algorithm, PrivateKey};
use russh::server::Server as _;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

use crate::auth::GitHubKeys;
use crate::backend::Backend;
use crate::config::Config;
use crate::session::{Connection, Shared};

#[derive(Parser)]
#[command(about = "chloe の SSH の入口")]
struct Cli {
    /// 設定ファイル (TOML)。
    #[arg(long, env = "CHLOE_GATEWAY_CONFIG")]
    config: PathBuf,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(clap::Subcommand)]
enum Command {
    /// ランナーのホスト (chloe-node) を管理する。
    Node {
        #[command(subcommand)]
        command: NodeCommand,
    },
}

#[derive(clap::Subcommand)]
enum NodeCommand {
    /// ホストを登録し、chloe-node join に渡す参加用のトークンを表示する。
    Add {
        name: String,
        /// コントロールプレーンがこのホストの API を呼ぶ URL (https://host:port)。
        #[arg(long)]
        url: String,
    },
    List,
    /// ホストの登録を消す。そのホストの証明書では、もう問い合わせに答えない。
    Rm {
        name: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config: Config = toml::from_str(
        &std::fs::read_to_string(&cli.config)
            .with_context(|| format!("read {}", cli.config.display()))?,
    )?;
    config
        .policy
        .check()
        .map_err(|e| anyhow::anyhow!("invalid [policy]: {e}"))?;
    if let Some(Command::Node { command }) = cli.command {
        return node_command(&config, command).await;
    }
    anyhow::ensure!(
        auth::is_login(&config.ssh_user),
        "ssh_user must be 1-39 letters, digits and hyphens: {:?}",
        config.ssh_user
    );
    init_logging(config.audit_log.as_deref())?;

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

    let (controller, secrets, cluster) = build_controller(&config).await?;
    let controller = match &config.github_token {
        Some(definition) => {
            let spec = definition
                .clone()
                .into_spec()
                .map_err(|e| anyhow::anyhow!("github_token: {e}"))?;
            controller.with_preset("github_token", spec)?
        }
        None => controller,
    };
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
    // ランナーのプロキシが GitHub のトークンを問い合わせたら、ここで発行して答える。
    let controller = match &github {
        Some(github) => {
            let store = controller.store().clone();
            controller.with_dynamic_secrets(Arc::new(github_token::GitHubTokens::new(
                github.clone(),
                secrets,
                store,
            )))
        }
        None => controller,
    };
    let listen = config.listen;
    let shared = Arc::new(Shared {
        keys: GitHubKeys::new(&config.github_url)?,
        controller,
        github,
        config,
    });
    // 複数のホストで動かすときは、chloe-node からの参加と問い合わせを受ける。
    if let Some((cluster_config, ca, credentials)) = cluster {
        let state = Arc::new(internal::Internal {
            controller: shared.controller.clone(),
            ca,
        });
        let tls = chloe_mtls::server_config(&credentials, chloe_mtls::ClientAuth::Optional)?;
        let listener = tokio::net::TcpListener::bind(cluster_config.listen).await?;
        tracing::info!(listen = %cluster_config.listen, "listening for nodes");
        tokio::spawn(async move {
            if let Err(error) = chloe_mtls::serve(listener, tls, internal::router(state)).await {
                tracing::error!("the internal API stopped: {error}");
            }
        });
    }
    if let Some(api_config) = shared.config.api.clone() {
        start_api(shared.clone(), &api_config).await?;
    }
    // ゲートウェイの再起動の前から動いているランナーを引き取り、そのあとも定期的に突き合わせる。
    let report = shared.controller.reconcile().await?;
    tracing::info!(?report, "reconciled runners");
    let reconciler = shared.clone();
    let interval = Duration::from_secs(shared.config.reconcile_interval_secs.max(5));
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            match reconciler.controller.reconcile().await {
                Ok(report) if report != chloe_controller::ReconcileReport::default() => {
                    tracing::info!(?report, "reconciled runners");
                }
                Ok(_) => {}
                Err(error) => tracing::warn!("failed to reconcile runners: {error:#}"),
            }
        }
    });
    tracing::info!(%listen, "listening");
    let mut server = Server { shared };
    server.run_on_address(ssh_config, listen).await?;
    Ok(())
}

/// 複数のホストで動かすときの、内部 API の設定、CA、コントロールプレーンの証明書。
type ClusterParts = (
    config::ClusterConfig,
    Arc<chloe_mtls::Ca>,
    chloe_mtls::Credentials,
);

async fn open_database(config: &Config) -> Result<sqlx::SqlitePool> {
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
    Ok(pool)
}

async fn build_controller(
    config: &Config,
) -> Result<(Controller<Backend>, SecretDb, Option<ClusterParts>)> {
    let pool = open_database(config).await?;
    let master = MasterKey::load_or_generate(&config.master_key)
        .with_context(|| format!("load {}", config.master_key.display()))?;
    let (backend, cluster) = match (&config.runner, &config.cluster) {
        (Some(runner), None) => (Backend::Docker(runner.backend()?), None),
        (None, Some(cluster)) => {
            let ca = Arc::new(chloe_mtls::Ca::load_or_create(&cluster.pki_dir)?);
            // 起動のたびにコントロールプレーンの証明書を発行し直す (鍵はファイルに置かない)。
            let credentials = ca.issue(&chloe_mtls::Role::ControlPlane, &cluster.server_names)?;
            let client = chloe_mtls::client(&credentials)?;
            (
                Backend::Cluster(ClusterBackend::new(Store::new(pool.clone()), client)),
                Some((cluster.clone(), ca, credentials)),
            )
        }
        _ => anyhow::bail!("write exactly one of [runner] and [cluster] in the configuration"),
    };
    let secrets = SecretDb::new(pool.clone(), master);
    let controller = Controller::new(
        Store::new(pool),
        secrets.clone(),
        backend,
        config.policy.clone(),
        Duration::from_secs(config.idle_timeout_secs),
    )
    .with_activity_wait(
        Duration::from_secs(config.activity_poll_secs.max(1)),
        Duration::from_secs(config.max_busy_wait_secs),
    );
    Ok((controller, secrets, cluster))
}

/// HTTP(S) の管理 API で待ち受ける。
async fn start_api(shared: Arc<Shared>, config: &config::ApiConfig) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    let router = api::router(shared);
    match (&config.tls_cert, &config.tls_key) {
        (Some(cert), Some(key)) => {
            let tls = chloe_mtls::tls_server_config(
                &std::fs::read_to_string(cert)
                    .with_context(|| format!("read {}", cert.display()))?,
                &std::fs::read_to_string(key).with_context(|| format!("read {}", key.display()))?,
            )?;
            tracing::info!(listen = %config.listen, "listening for the management API (HTTPS)");
            tokio::spawn(async move {
                if let Err(error) = chloe_mtls::serve(listener, tls, router).await {
                    tracing::error!("the management API stopped: {error}");
                }
            });
        }
        (None, None) => {
            tracing::info!(listen = %config.listen, "listening for the management API (HTTP)");
            tokio::spawn(async move {
                if let Err(error) = axum::serve(listener, router).await {
                    tracing::error!("the management API stopped: {error}");
                }
            });
        }
        _ => anyhow::bail!("write both api.tls_cert and api.tls_key, or neither"),
    }
    Ok(())
}

/// `chloe-gateway node ...`。
async fn node_command(config: &Config, command: NodeCommand) -> Result<()> {
    let cluster = config
        .cluster
        .as_ref()
        .context("write [cluster] in the configuration to use nodes")?;
    let store = Store::new(open_database(config).await?);
    match command {
        NodeCommand::Add { name, url } => {
            anyhow::ensure!(
                chloe_mtls::valid_node_name(&name),
                "a node name is 1-63 letters, digits and hyphens"
            );
            let parsed = reqwest::Url::parse(&url).context("invalid URL")?;
            anyhow::ensure!(
                parsed.scheme() == "https" && parsed.host_str().is_some(),
                "the URL must be https://host:port"
            );
            let ca = chloe_mtls::Ca::load_or_create(&cluster.pki_dir)?;
            let secret = store.add_node(&name, &url, cluster.join_ttl_secs).await?;
            println!("{secret}.{}", ca.fingerprint());
            eprintln!(
                "run on the node (the token works once, for {} seconds):\n  chloe-node join --server https://<control plane>:{} --token <the token above> --dir <dir>",
                cluster.join_ttl_secs,
                cluster.listen.port()
            );
        }
        NodeCommand::List => {
            for node in store.nodes().await? {
                let state = if node.joined {
                    "joined"
                } else {
                    "waiting to join"
                };
                println!("{}\t{}\t{state}", node.name, node.url);
            }
        }
        NodeCommand::Rm { name } => {
            // 置いたワークスペースが残っていると、そのワークスペースを扱えなくなる。
            let placed = store
                .placement_counts()
                .await?
                .get(&name)
                .copied()
                .unwrap_or(0);
            anyhow::ensure!(
                placed == 0,
                "{placed} workspace(s) are placed on {name}; remove them (chloe ws rm) before removing the node"
            );
            anyhow::ensure!(store.remove_node(&name).await?, "no such node: {name}");
            println!("removed {name}");
        }
    }
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

/// 先頭の `~/` をホームディレクトリに直す。
fn expand_home(path: &Path) -> PathBuf {
    match (path.strip_prefix("~"), std::env::var_os("HOME")) {
        (Ok(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => path.to_path_buf(),
    }
}

/// 標準エラーへのログと、`audit` を target にしたイベントだけを JSON Lines で書く監査ログ。
fn init_logging(audit_log: Option<&Path>) -> Result<()> {
    use tracing_subscriber::Layer as _;
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;

    let stderr = tracing_subscriber::fmt::layer().with_filter(
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
    );
    let audit = match audit_log {
        Some(path) => {
            use std::os::unix::fs::OpenOptionsExt as _;
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(path)
                .with_context(|| format!("open the audit log {}", path.display()))?;
            Some(
                tracing_subscriber::fmt::layer()
                    .json()
                    .flatten_event(true)
                    .with_current_span(false)
                    .with_span_list(false)
                    .with_target(false)
                    .with_writer(std::sync::Mutex::new(file))
                    .with_filter(
                        tracing_subscriber::filter::Targets::new()
                            .with_target("audit", tracing::Level::INFO),
                    ),
            )
        }
        None => None,
    };
    tracing_subscriber::registry()
        .with(stderr)
        .with(audit)
        .init();
    Ok(())
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
