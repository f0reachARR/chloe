//! `chloe-node` からコントロールプレーンへ張るトンネル。
//!
//! `chloe-node` がコントロールプレーンの内部 API に相互 TLS でつなぎ、[`TUNNEL_PATH`] を
//! `Upgrade: chloe-tunnel` で求める。101 のあと、その接続を yamux で多重化する。ストリームを
//! 開くのはコントロールプレーンだけで、`chloe-node` は受けたストリームごとに、自分の API を
//! HTTP/1.1 で答える。コントロールプレーンからホストへは接続を始めないので、ホストが NAT や
//! ファイアウォールの内側にあっても使える。
//!
//! ```text
//! chloe-node ──TLS──> コントロールプレーン   GET /internal/v1/tunnel (Upgrade: chloe-tunnel)
//!            <═yamux═                       ストリームごとに HTTP/1.1 の要求 (Upgrade も使える)
//! ```

use std::collections::{HashMap, VecDeque};
use std::future::poll_fn;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;

use axum::{Extension, Router};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header;
use hyper::upgrade::Upgraded;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio_util::compat::{FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};
use tracing::debug;

use crate::{Credentials, Error, Peer, Role, client_config, keepalive, role_of};

/// トンネルを求める内部 API のパス。
pub const TUNNEL_PATH: &str = "/internal/v1/tunnel";
/// トンネルの Upgrade で使うプロトコルの名前。
pub const TUNNEL_UPGRADE: &str = "chloe-tunnel";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

fn yamux_config() -> yamux::Config {
    let mut config = yamux::Config::default();
    // 相手が受信のウィンドウを広げさせて使わせられるメモリを、1 GiB から 256 MiB に抑える
    // (ストリームの数の上限 × 256 KiB より小さくはできない)。
    config.set_max_num_streams(1024);
    config.set_max_connection_receive_window(Some(256 << 20));
    config
}

fn tunnel_error(error: impl std::fmt::Display) -> Error {
    Error::Tunnel(error.to_string())
}

type Opener = oneshot::Sender<yamux::Stream>;

struct Handle {
    id: u64,
    open: mpsc::UnboundedSender<Opener>,
}

/// コントロールプレーンの側。ホストの名前ごとに、張られたトンネルを持つ。
#[derive(Clone, Default)]
pub struct Tunnels {
    handles: Arc<Mutex<HashMap<String, Handle>>>,
    next_id: Arc<AtomicU64>,
}

impl Tunnels {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_connected(&self, name: &str) -> bool {
        self.handles.lock().unwrap().contains_key(name)
    }

    /// `name` のホストがトンネルを求めた要求に答える。`Upgrade: chloe-tunnel` がなければ 400 を
    /// 返す。あれば 101 を返し、Upgrade した接続を閉じるまでトンネルとして動かす future を
    /// 添える。future は、閉じたときにまだこれが `name` のトンネルだったら `true` を返す
    /// (`false` なら、ホストが張り直して新しいトンネルに替わっている)。同じホストの古い
    /// トンネルは、新しいトンネルが動き始めたときに閉じる。
    pub fn accept(
        &self,
        name: &str,
        request: &mut axum::extract::Request,
    ) -> (
        axum::response::Response,
        Option<impl Future<Output = bool> + Send + 'static>,
    ) {
        let wants_upgrade = request
            .headers()
            .get(header::UPGRADE)
            .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(TUNNEL_UPGRADE.as_bytes()));
        if !wants_upgrade {
            let response = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(axum::body::Body::from("expected Upgrade: chloe-tunnel"))
                .unwrap_or_default();
            return (response, None);
        }
        let upgrade = hyper::upgrade::on(request);
        let (tunnels, name) = (self.clone(), name.to_string());
        let serving = async move {
            match upgrade.await {
                Ok(upgraded) => tunnels.serve(&name, upgraded).await,
                Err(error) => {
                    debug!(node = name, "the tunnel upgrade failed: {error}");
                    false
                }
            }
        };
        let response = Response::builder()
            .status(StatusCode::SWITCHING_PROTOCOLS)
            .header(header::CONNECTION, "upgrade")
            .header(header::UPGRADE, TUNNEL_UPGRADE)
            .body(axum::body::Body::empty())
            .unwrap_or_default();
        (response, Some(serving))
    }

    async fn serve(&self, name: &str, upgraded: Upgraded) -> bool {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (open, opens) = mpsc::unbounded_channel();
        // 古い Handle を落とすと、古いトンネルは頼みを受けられなくなって閉じる。
        self.handles
            .lock()
            .unwrap()
            .insert(name.to_string(), Handle { id, open });
        let connection = yamux::Connection::new(
            TokioIo::new(upgraded).compat(),
            yamux_config(),
            yamux::Mode::Client,
        );
        if let Err(error) = drive(connection, opens).await {
            debug!(node = name, "the tunnel failed: {error}");
        }
        let mut handles = self.handles.lock().unwrap();
        if handles.get(name).is_some_and(|h| h.id == id) {
            handles.remove(name);
            true
        } else {
            false
        }
    }

    /// `name` のホストへのストリームを開く。トンネルがなければ [`Error::NotConnected`]。
    pub async fn open(&self, name: &str) -> Result<yamux::Stream, Error> {
        let (tx, rx) = oneshot::channel();
        self.handles
            .lock()
            .unwrap()
            .get(name)
            .ok_or(Error::NotConnected)?
            .open
            .send(tx)
            .map_err(|_| Error::NotConnected)?;
        rx.await.map_err(|_| Error::NotConnected)
    }

    /// `name` のホストの API に、新しいストリームで一つの要求を送る。`request` の URI はパス
    /// だけにする。応答が 101 なら、`hyper::upgrade::on` で Upgrade した接続を取れる。
    pub async fn request(
        &self,
        name: &str,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<Incoming>, Error> {
        let stream = self.open(name).await?;
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake(TokioIo::new(stream.compat())).await?;
        tokio::spawn(async move {
            if let Err(error) = connection.with_upgrades().await {
                debug!("a request through the tunnel failed: {error}");
            }
        });
        Ok(sender.send_request(request).await?)
    }
}

/// トンネルの読み書きを進め、ストリームを開く頼みに答える。頼む側がいなくなったら閉じる。
async fn drive<T>(
    mut connection: yamux::Connection<T>,
    mut opens: mpsc::UnboundedReceiver<Opener>,
) -> Result<(), yamux::ConnectionError>
where
    T: futures::AsyncRead + futures::AsyncWrite + Unpin,
{
    let mut waiting: VecDeque<Opener> = VecDeque::new();
    let replaced = poll_fn(|cx| {
        loop {
            loop {
                match opens.poll_recv(cx) {
                    Poll::Ready(Some(opener)) => waiting.push_back(opener),
                    Poll::Ready(None) => return Poll::Ready(Ok(true)),
                    Poll::Pending => break,
                }
            }
            while !waiting.is_empty() {
                match connection.poll_new_outbound(cx) {
                    Poll::Ready(Ok(stream)) => {
                        if let Some(opener) = waiting.pop_front() {
                            let _ = opener.send(stream);
                        }
                    }
                    Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                    Poll::Pending => break,
                }
            }
            // ホストからストリームを開くことはないので、来たら捨てる。
            match connection.poll_next_inbound(cx) {
                Poll::Ready(Some(Ok(_))) => continue,
                Poll::Ready(Some(Err(error))) => return Poll::Ready(Err(error)),
                Poll::Ready(None) => return Poll::Ready(Ok(false)),
                Poll::Pending => return Poll::Pending,
            }
        }
    })
    .await?;
    if replaced {
        poll_fn(|cx| connection.poll_close(cx)).await?;
    }
    Ok(())
}

/// `chloe-node` の側。`control_plane` (内部 API の URL) にトンネルを張り、切れるまで、
/// トンネルの上の要求に `router` で答える。張れなければすぐにエラーを返す。張れたら
/// `connected` を呼び、切れたときに返る。
///
/// つないだ相手がコントロールプレーンの証明書を出したことを確かめるので、`router` には
/// コントロールプレーンの [`Peer`] を入れて渡す。
pub async fn dial(
    control_plane: &str,
    credentials: &Credentials,
    router: Router,
    connected: impl FnOnce(),
) -> Result<(), Error> {
    let url = reqwest::Url::parse(control_plane)
        .map_err(|e| Error::Invalid(format!("the control plane URL: {e}")))?;
    let host = url
        .host_str()
        .ok_or_else(|| Error::Invalid("the control plane URL has no host".into()))?
        .trim_matches(['[', ']'])
        .to_string();
    let port = url.port_or_known_default().unwrap_or(443);
    let tcp = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host.as_str(), port)))
        .await
        .map_err(|_| tunnel_error(format!("connecting to {host}:{port} timed out")))??;
    keepalive(&tcp)?;
    let server_name =
        ServerName::try_from(host.clone()).map_err(|e| Error::Invalid(e.to_string()))?;
    let tls = tokio_rustls::TlsConnector::from(client_config(credentials)?)
        .connect(server_name, tcp)
        .await?;
    let role = tls
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|certs| certs.first())
        .and_then(role_of);
    if role != Some(Role::ControlPlane) {
        return Err(Error::Invalid(
            "the peer did not present the control plane's certificate".into(),
        ));
    }

    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(tls)).await?;
    tokio::spawn(async move {
        if let Err(error) = connection.with_upgrades().await {
            debug!("the tunnel request failed: {error}");
        }
    });
    let request = Request::get(TUNNEL_PATH)
        .header(header::HOST, &host)
        .header(header::CONNECTION, "upgrade")
        .header(header::UPGRADE, TUNNEL_UPGRADE)
        .body(Full::<Bytes>::default())
        .map_err(tunnel_error)?;
    let response = sender.send_request(request).await?;
    if response.status() != StatusCode::SWITCHING_PROTOCOLS {
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .map(|b| String::from_utf8_lossy(&b.to_bytes()).trim().to_string())
            .unwrap_or_default();
        return Err(tunnel_error(format!(
            "the control plane refused the tunnel: {status}: {body}"
        )));
    }
    let upgraded = hyper::upgrade::on(response).await?;
    connected();

    let mut connection = yamux::Connection::new(
        TokioIo::new(upgraded).compat(),
        yamux_config(),
        yamux::Mode::Server,
    );
    let router = router.layer(Extension(Peer(Some(Role::ControlPlane))));
    loop {
        match poll_fn(|cx| connection.poll_next_inbound(cx)).await {
            Some(Ok(stream)) => {
                let service = TowerToHyperService::new(router.clone());
                tokio::spawn(async move {
                    if let Err(error) = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream.compat()), service)
                        .with_upgrades()
                        .await
                    {
                        debug!("a request from the control plane failed: {error}");
                    }
                });
            }
            Some(Err(error)) => return Err(tunnel_error(error)),
            None => return Ok(()),
        }
    }
}
