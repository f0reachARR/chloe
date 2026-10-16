//! ランナーの外向き通信のプロキシ。
//!
//! nftables が VM からの 80 番を `--http-listen` に、443 番を `--https-listen` に転送してくる。
//!
//! - 443 番は ClientHello の SNI を読み、中継のみの宛先なら復号せずに SNI のホストへ中継する。
//!   それ以外は、ワークスペースの CA で SNI の証明書をその場で発行して復号する。
//! - 復号した要求と 80 番の要求は、方針 (`chloe-policy`) で判定してから上流へ送る。
//! - 上流の接続先は SNI (平文なら Host) から決め、VM が接続しようとした IP アドレスは使わない。

mod ca;
mod log;
mod service;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use chloe_policy::{Policy, PolicyConfig, TlsDecision};
use clap::Parser;
use rama::error::BoxError;
use rama::extensions::ExtensionsRef;
use rama::graceful::Shutdown;
use rama::http::client::EasyHttpWebClient;
use rama::http::server::HttpServer;
use rama::io::Io;
use rama::net::Protocol;
use rama::net::address::HostWithPort;
use rama::net::proxy::IoForwardService;
use rama::rt::Executor;
use rama::tcp::proxy::IoToProxyBridgeIoLayer;
use rama::tcp::server::TcpListener;
use rama::tls::boring::server::{
    BoringServerConfigExt as _, ServerCertIssuerData, ServerCertIssuerKind, TlsAcceptorLayer,
};
use rama::tls::client::TlsClientConfig;
use rama::tls::server::{SniPrefixedIo, SniRequest, SniRouter, TlsServerConfig};
use rama::{Layer, Service};

use crate::log::{Event, Logger};
use crate::service::{EgressService, IngressSni};

#[derive(Parser)]
#[command(about = "chloe のランナーの外向き通信のプロキシ")]
struct Cli {
    /// 平文の HTTP (VM の 80 番) を受ける。
    #[arg(long, default_value = "0.0.0.0:8080")]
    http_listen: SocketAddr,
    /// TLS (VM の 443 番) を受ける。
    #[arg(long, default_value = "0.0.0.0:8443")]
    https_listen: SocketAddr,
    /// 方針の設定ファイル (JSON)。
    #[arg(long)]
    policy: PathBuf,
    /// CA の証明書と秘密鍵を置くディレクトリ。
    #[arg(long)]
    ca_dir: PathBuf,
    /// 要求のログ (JSON Lines)。
    #[arg(long)]
    log: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let cli = Cli::parse();

    let config: PolicyConfig = serde_json::from_slice(
        &std::fs::read(&cli.policy).with_context(|| format!("read {}", cli.policy.display()))?,
    )?;
    let policy = Arc::new(Policy::new(config, |rule| {
        // 検証用なので、本物の値はランナーの環境変数から読む。
        std::env::var(&rule.secret_env).map_err(|e| format!("{}: {e}", rule.secret_env))
    })?);
    let ca = ca::load_or_generate(&cli.ca_dir)?;
    let logger = Arc::new(Logger::open(cli.log.as_deref())?);

    let shutdown = Shutdown::default();
    let exec = Executor::graceful(shutdown.guard());

    let client = EasyHttpWebClient::connector_builder()
        .with_default_transport_connector()
        .with_default_dns_connector()
        .without_tls_proxy_support()
        .without_proxy_support()
        .with_tls_support_using_boringssl(TlsClientConfig::default_http())
        .with_default_http_connector(exec.clone())
        .with_default_connection_pool()
        .build_client();
    let http_svc = Arc::new(EgressService {
        policy: policy.clone(),
        client,
        logger: logger.clone(),
        ca_pem: ca.cert_pem.clone().into(),
    });

    let tls_config = TlsServerConfig::new()
        .with_cert_issuer(ServerCertIssuerData::new(
            ServerCertIssuerKind::ProvidedCa {
                ca: ca.data,
                leaf: Default::default(),
            },
        ))
        .with_alpn_http_auto();
    let https_svc = TlsAcceptorLayer::new(tls_config)
        .into_layer(HttpServer::auto(exec.clone()).service(http_svc.clone()));

    let http_listener = TcpListener::bind_address(cli.http_listen, exec.clone())
        .await
        .map_err(|e| anyhow::anyhow!("bind http listener: {e}"))?;
    let https_listener = TcpListener::bind_address(cli.https_listen, exec.clone())
        .await
        .map_err(|e| anyhow::anyhow!("bind https listener: {e}"))?;
    tracing::info!(http = %cli.http_listen, https = %cli.https_listen, "listening");

    shutdown.spawn_task(http_listener.serve(HttpServer::auto(exec.clone()).service(http_svc)));
    shutdown.spawn_task(https_listener.serve(SniRouter::new(SniRouterService {
        policy,
        logger,
        https_svc,
        exec,
    })));

    shutdown
        .shutdown_with_limit(std::time::Duration::from_secs(5))
        .await
        .map_err(|e| anyhow::anyhow!("shutdown: {e}"))?;
    Ok(())
}

/// SNI を見て、復号するか中継するかを決める。
#[derive(Clone)]
struct SniRouterService<T> {
    policy: Arc<Policy>,
    logger: Arc<Logger>,
    https_svc: T,
    exec: Executor,
}

impl<T, S> Service<SniRequest<S>> for SniRouterService<T>
where
    S: Io + Unpin + ExtensionsRef,
    T: Service<SniPrefixedIo<S>, Output = (), Error: Into<BoxError>>,
{
    type Output = ();
    type Error = BoxError;

    async fn serve(&self, SniRequest { sni, stream }: SniRequest<S>) -> Result<(), BoxError> {
        let sni_str = sni.as_ref().map(|d| d.to_string());
        if let (Some(sni), TlsDecision::Passthrough) =
            (sni.clone(), self.policy.tls_decision(sni_str.as_deref()))
        {
            self.logger.log(Event::Passthrough { host: sni.as_ref() });
            return IoToProxyBridgeIoLayer::new(HostWithPort {
                host: sni.clone().into(),
                port: Protocol::HTTPS_DEFAULT_PORT,
            })
            .with_connector(rama::dns::client::DnsConnector::new(
                rama::tcp::client::service::TcpConnector::new(),
            ))
            .into_layer(IoForwardService::new(self.exec.clone()))
            .serve(stream)
            .await
            .map(|_| ())
            .map_err(|e| format!("forward passthrough stream for {sni}: {e}").into());
        }
        // SNI のない TLS も復号し、要求の段階で no-sni として遮断する。
        if let Some(sni) = sni {
            stream.extensions().insert(IngressSni(sni));
        }
        self.https_svc.serve(stream).await.map_err(Into::into)
    }
}
