//! ランナーの外向き通信のプロキシ。
//!
//! nftables が VM からの 80 番を `--http-listen` に、443 番を `--https-listen` に転送してくる。
//!
//! - 443 番は ClientHello の SNI を読み、中継のみの宛先なら復号せずに SNI のホストへ中継する。
//!   それ以外は、ワークスペースの CA で SNI の証明書をその場で発行して復号する。
//! - 復号した要求と 80 番の要求は、方針 (`chloe-policy`) で判定してから上流へ送る。
//! - 上流の接続先は SNI (平文なら Host) から決め、VM が接続しようとした IP アドレスは使わない。

mod ca;
mod control;
mod log;
mod secrets;
mod service;

use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use chloe_policy::{PolicyConfig, TlsDecision};
use clap::Parser;
use rama::error::BoxError;
use rama::extensions::ExtensionsRef;
use rama::graceful::Shutdown;
use rama::http::client::EasyHttpWebClient;
use rama::http::layer::remove_header::{RemoveRequestHeaderLayer, RemoveResponseHeaderLayer};
use rama::http::layer::upgrade::mitm::HttpUpgradeMitmRelayLayer;
use rama::http::server::HttpServer;
use rama::http::ws::handshake::matcher::{
    HttpWebSocketRelayServiceRequestMatcher, WebSocketMatcher,
};
use rama::http::ws::handshake::mitm::{
    WebSocketRelayInput, WebSocketRelayOutput, WebSocketRelayService,
};
use rama::io::Io;
use rama::layer::HijackLayer;
use rama::net::Protocol;
use rama::net::address::HostWithPort;
use rama::net::proxy::IoForwardService;
use rama::rt::Executor;
use rama::service::service_fn;
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
    /// 方針を動いたまま入れ替えるための制御用の Unix ソケット。
    #[arg(long)]
    control: Option<PathBuf>,
    /// 本物の値を問い合わせるコントロールプレーンの Unix ソケット。指定しなければ、
    /// 開発用にこのプロセスの環境変数から読む。
    #[arg(long)]
    broker: Option<PathBuf>,
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
    let policy = control::SharedPolicy::new(chloe_policy::Policy::new(config));
    let secrets = match &cli.broker {
        Some(path) => secrets::SecretSource::Broker(path.clone()),
        None => secrets::SecretSource::Env,
    };
    let ca = ca::load_or_generate(&cli.ca_dir)?;
    let logger = Arc::new(Logger::open(cli.log.as_deref())?);

    let shutdown = Shutdown::default();
    let exec = Executor::graceful(shutdown.guard());

    let web_client = EasyHttpWebClient::connector_builder()
        .with_default_transport_connector()
        .with_default_dns_connector()
        .without_tls_proxy_support()
        .without_proxy_support()
        .with_tls_support_using_boringssl(TlsClientConfig::default_http())
        .with_default_http_connector(exec.clone())
        .with_default_connection_pool()
        .build_client();
    // WebSocket の要求は Upgrade のヘッダーを残したまま送り、確立後のストリームを中継する。
    // それ以外の要求では hop-by-hop のヘッダーを取り除く。
    let client = HijackLayer::new(
        WebSocketMatcher::new(),
        HttpUpgradeMitmRelayLayer::new(
            exec.clone(),
            HttpWebSocketRelayServiceRequestMatcher::new(WebSocketRelayService::new(service_fn(
                relay_websocket_message,
            ))),
        )
        .into_layer(web_client.clone()),
    )
    .into_layer(
        (
            RemoveResponseHeaderLayer::hop_by_hop(),
            RemoveRequestHeaderLayer::hop_by_hop(),
        )
            .into_layer(web_client),
    );
    let http_svc = Arc::new(EgressService {
        policy: policy.clone(),
        client,
        logger: logger.clone(),
        ca_pem: ca.cert_pem.clone().into(),
        secrets,
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

    if let Some(path) = cli.control.clone() {
        let policy = policy.clone();
        shutdown.spawn_task(async move {
            if let Err(error) = control::serve(&path, policy).await {
                tracing::error!("control socket failed: {error:#}");
            }
        });
    }
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

/// WebSocket のメッセージはそのまま反対側へ送る。
async fn relay_websocket_message(
    input: WebSocketRelayInput,
) -> Result<WebSocketRelayOutput, Infallible> {
    Ok(WebSocketRelayOutput {
        messages: vec![input.message],
        extensions: input.extensions,
    })
}

/// SNI を見て、復号するか中継するかを決める。
#[derive(Clone)]
struct SniRouterService<T> {
    policy: control::SharedPolicy,
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
        if let (Some(sni), TlsDecision::Passthrough) = (
            sni.clone(),
            self.policy.current().tls_decision(sni_str.as_deref()),
        ) {
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
