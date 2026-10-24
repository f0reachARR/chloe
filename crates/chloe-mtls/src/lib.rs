//! コントロールプレーンと `chloe-node` のあいだの相互 TLS。
//!
//! - コントロールプレーンが CA を持ち、自分と `chloe-node` の証明書を発行する。
//! - 証明書の CN で役割を表す (`chloe-control-plane`、`chloe-node/<名前>`)。どちらも同じ CA が
//!   発行するので、相手の証明書が CA につながるだけでは役割を決められない。受け取った側は
//!   [`Peer`] の役割を確かめる。
//! - `chloe-node` は参加するとき、CA の証明書の指紋を含む参加用のトークンで、最初の接続の
//!   相手を確かめる ([`fetch_ca`])。

use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ConnectInfo;
use axum::{Extension, Router};
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use rcgen::{
    BasicConstraints, CertificateParams, CertificateSigningRequestParams, DistinguishedName,
    DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose, SanType,
};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tracing::debug;

const CA_COMMON_NAME: &str = "chloe CA";
const CONTROL_PLANE_COMMON_NAME: &str = "chloe-control-plane";
const NODE_COMMON_NAME_PREFIX: &str = "chloe-node/";
/// 発行する証明書の有効期間。
const LEAF_VALIDITY: Duration = Duration::from_secs(365 * 24 * 60 * 60);
const CA_VALIDITY: Duration = Duration::from_secs(10 * 365 * 24 * 60 * 60);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("certificate: {0}")]
    Cert(#[from] rcgen::Error),
    #[error("TLS: {0}")]
    Tls(#[from] rustls::Error),
    #[error("HTTP: {0}")]
    Http(#[from] reqwest::Error),
    #[error("{0}")]
    Invalid(String),
}

/// 証明書の持ち主の役割。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    ControlPlane,
    Node(String),
}

impl Role {
    fn common_name(&self) -> String {
        match self {
            Self::ControlPlane => CONTROL_PLANE_COMMON_NAME.into(),
            Self::Node(name) => format!("{NODE_COMMON_NAME_PREFIX}{name}"),
        }
    }

    fn from_common_name(cn: &str) -> Option<Self> {
        if cn == CONTROL_PLANE_COMMON_NAME {
            return Some(Self::ControlPlane);
        }
        let name = cn.strip_prefix(NODE_COMMON_NAME_PREFIX)?;
        valid_node_name(name).then(|| Self::Node(name.to_string()))
    }
}

/// `chloe-node` の名前に使える文字列か (英数字と `-`、1〜63 文字)。
pub fn valid_node_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 63
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// 相互 TLS の接続の相手。[`serve`] がリクエストの拡張に入れる。証明書がなければ `None`。
#[derive(Debug, Clone)]
pub struct Peer(pub Option<Role>);

/// 証明書と鍵と CA の証明書 (どれも PEM)。
#[derive(Clone)]
pub struct Credentials {
    pub cert_pem: String,
    pub key_pem: String,
    pub ca_pem: String,
}

impl Credentials {
    /// `dir` の `cert.pem`、`key.pem`、`ca.pem` を読む。
    pub fn load(dir: &Path) -> Result<Self, Error> {
        Ok(Self {
            cert_pem: std::fs::read_to_string(dir.join("cert.pem"))?,
            key_pem: std::fs::read_to_string(dir.join("key.pem"))?,
            ca_pem: std::fs::read_to_string(dir.join("ca.pem"))?,
        })
    }

    /// `dir` に書く。鍵の権限は 600 にする。
    pub fn save(&self, dir: &Path) -> Result<(), Error> {
        std::fs::create_dir_all(dir)?;
        std::fs::write(dir.join("cert.pem"), &self.cert_pem)?;
        std::fs::write(dir.join("ca.pem"), &self.ca_pem)?;
        write_private(&dir.join("key.pem"), &self.key_pem)
    }
}

/// コントロールプレーンの CA。
pub struct Ca {
    cert_pem: String,
    issuer: Issuer<'static, KeyPair>,
}

impl Ca {
    /// `dir` の `ca.pem` と `ca.key` を読む。なければ作る。
    pub fn load_or_create(dir: &Path) -> Result<Self, Error> {
        let (cert_path, key_path) = (dir.join("ca.pem"), dir.join("ca.key"));
        if cert_path.exists() && key_path.exists() {
            let cert_pem = std::fs::read_to_string(&cert_path)?;
            let key = KeyPair::from_pem(&std::fs::read_to_string(&key_path)?)?;
            let issuer = Issuer::from_ca_cert_pem(&cert_pem, key)?;
            return Ok(Self { cert_pem, issuer });
        }
        let mut params = CertificateParams::default();
        params.distinguished_name = name(CA_COMMON_NAME);
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        set_validity(&mut params, CA_VALIDITY);
        let key = KeyPair::generate()?;
        let cert = params.self_signed(&key)?;
        std::fs::create_dir_all(dir)?;
        write_private(&key_path, &key.serialize_pem())?;
        std::fs::write(&cert_path, cert.pem())?;
        Ok(Self {
            cert_pem: cert.pem(),
            issuer: Issuer::new(params, key),
        })
    }

    pub fn cert_pem(&self) -> &str {
        &self.cert_pem
    }

    /// CA の証明書の指紋 (DER の SHA-256 の 16 進)。参加用のトークンに含める。
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.cert_pem).unwrap_or_default()
    }

    /// 新しい鍵を作り、`role` の証明書を発行する (コントロールプレーン自身に使う)。
    pub fn issue(&self, role: &Role, names: &[String]) -> Result<Credentials, Error> {
        let key = KeyPair::generate()?;
        let cert = leaf_params(role, names)?.signed_by(&key, &self.issuer)?;
        Ok(Credentials {
            cert_pem: cert.pem(),
            key_pem: key.serialize_pem(),
            ca_pem: self.cert_pem.clone(),
        })
    }

    /// 証明書の署名要求 (CSR) の公開鍵に、`role` の証明書を発行する。要求の中の名前や用途は
    /// 使わず、こちらで決めたものにする (`chloe-node` に自分の名前を選ばせない)。
    pub fn sign_request(
        &self,
        csr_pem: &str,
        role: &Role,
        names: &[String],
    ) -> Result<String, Error> {
        let csr = CertificateSigningRequestParams::from_pem(csr_pem)?;
        let cert = leaf_params(role, names)?.signed_by(&csr.public_key, &self.issuer)?;
        Ok(cert.pem())
    }
}

/// 鍵と、その公開鍵の証明書の署名要求 (CSR) を作る。`chloe-node` が参加するときに使う。
pub fn new_key_and_request() -> Result<(String, String), Error> {
    let key = KeyPair::generate()?;
    let csr = CertificateParams::default().serialize_request(&key)?;
    Ok((key.serialize_pem(), csr.pem()?))
}

fn leaf_params(role: &Role, names: &[String]) -> Result<CertificateParams, Error> {
    let mut params = CertificateParams::default();
    params.distinguished_name = name(&role.common_name());
    params.subject_alt_names = names
        .iter()
        .map(|n| match n.parse::<IpAddr>() {
            Ok(ip) => Ok(SanType::IpAddress(ip)),
            Err(_) => Ok(SanType::DnsName(n.clone().try_into()?)),
        })
        .collect::<Result<_, rcgen::Error>>()?;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    // コントロールプレーンと chloe-node は、互いにサーバーにもクライアントにもなる。
    params.extended_key_usages = vec![
        ExtendedKeyUsagePurpose::ServerAuth,
        ExtendedKeyUsagePurpose::ClientAuth,
    ];
    params.use_authority_key_identifier_extension = true;
    set_validity(&mut params, LEAF_VALIDITY);
    Ok(params)
}

fn name(common_name: &str) -> DistinguishedName {
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, common_name);
    name
}

fn set_validity(params: &mut CertificateParams, validity: Duration) {
    let now = time::OffsetDateTime::now_utc();
    // 時計のずれを見込んで、少し前から有効にする。
    params.not_before = now - time::Duration::hours(1);
    params.not_after = now + validity;
}

fn write_private(path: &Path, content: &str) -> Result<(), Error> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(content.as_bytes())?;
    Ok(())
}

/// PEM の証明書の指紋 (DER の SHA-256 の 16 進)。
pub fn fingerprint(cert_pem: &str) -> Result<String, Error> {
    let der = CertificateDer::from_pem_slice(cert_pem.as_bytes())
        .map_err(|e| Error::Invalid(format!("certificate PEM: {e}")))?;
    Ok(Sha256::digest(der.as_ref())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// 証明書 (DER) の CN から役割を読む。
pub fn role_of(cert: &CertificateDer<'_>) -> Option<Role> {
    let (_, parsed) = x509_parser::parse_x509_certificate(cert.as_ref()).ok()?;
    let cn = parsed.subject().iter_common_name().next()?.as_str().ok()?;
    Role::from_common_name(cn)
}

/// クライアントの証明書を求めるか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientAuth {
    /// CA が発行した証明書がなければ接続を断る。
    Required,
    /// 証明書がなくても接続できる (参加の前の `chloe-node` のため)。ハンドラーが [`Peer`] を
    /// 確かめる。証明書を出したなら、CA が発行したものでなければ断る。
    Optional,
}

/// 相互 TLS の待ち受けの設定。
pub fn server_config(
    credentials: &Credentials,
    client_auth: ClientAuth,
) -> Result<Arc<rustls::ServerConfig>, Error> {
    let mut roots = rustls::RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(credentials.ca_pem.as_bytes()) {
        let cert = cert.map_err(|e| Error::Invalid(format!("CA PEM: {e}")))?;
        roots.add(cert)?;
    }
    let builder = WebPkiClientVerifier::builder(Arc::new(roots));
    let verifier = match client_auth {
        ClientAuth::Required => builder.build(),
        ClientAuth::Optional => builder.allow_unauthenticated().build(),
    }
    .map_err(|e| Error::Invalid(format!("client verifier: {e}")))?;
    let certs = CertificateDer::pem_slice_iter(credentials.cert_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| Error::Invalid(format!("certificate PEM: {e}")))?;
    let key = PrivateKeyDer::from_pem_slice(credentials.key_pem.as_bytes())
        .map_err(|e| Error::Invalid(format!("key PEM: {e}")))?;
    let mut config = rustls::ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(certs, key)?;
    // guest-agent への接続を Upgrade で中継するので、HTTP/1.1 にする。
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// クライアントの証明書を求めない TLS の待ち受けの設定 (利用者向けの管理 API など)。
pub fn tls_server_config(
    cert_pem: &str,
    key_pem: &str,
) -> Result<Arc<rustls::ServerConfig>, Error> {
    let certs = CertificateDer::pem_slice_iter(cert_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| Error::Invalid(format!("certificate PEM: {e}")))?;
    let key = PrivateKeyDer::from_pem_slice(key_pem.as_bytes())
        .map_err(|e| Error::Invalid(format!("key PEM: {e}")))?;
    let mut config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// 相互 TLS で `router` を提供する。接続ごとに、相手の役割を [`Peer`] としてリクエストに入れる。
pub async fn serve(
    listener: TcpListener,
    config: Arc<rustls::ServerConfig>,
    router: Router,
) -> Result<(), Error> {
    let acceptor = tokio_rustls::TlsAcceptor::from(config);
    loop {
        let (tcp, addr) = listener.accept().await?;
        let acceptor = acceptor.clone();
        let router = router.clone();
        tokio::spawn(async move {
            let tls = match acceptor.accept(tcp).await {
                Ok(tls) => tls,
                Err(error) => {
                    debug!(%addr, "TLS handshake failed: {error}");
                    return;
                }
            };
            let peer = Peer(
                tls.get_ref()
                    .1
                    .peer_certificates()
                    .and_then(|certs| certs.first())
                    .and_then(role_of),
            );
            // 接続元のアドレスは、axum の `ConnectInfo<SocketAddr>` で読めるようにする。
            let service = TowerToHyperService::new(
                router
                    .layer(Extension(peer))
                    .layer(Extension(ConnectInfo::<SocketAddr>(addr))),
            );
            if let Err(error) = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(tls), service)
                .with_upgrades()
                .await
            {
                debug!(%addr, "connection failed: {error}");
            }
        });
    }
}

/// 相互 TLS の HTTP のクライアント。CA が発行した証明書の相手にだけつなぐ。
pub fn client(credentials: &Credentials) -> Result<reqwest::Client, Error> {
    let identity = reqwest::Identity::from_pem(
        format!("{}\n{}", credentials.cert_pem, credentials.key_pem).as_bytes(),
    )?;
    Ok(reqwest::Client::builder()
        .tls_certs_only([reqwest::Certificate::from_pem(
            credentials.ca_pem.as_bytes(),
        )?])
        .identity(identity)
        // guest-agent への接続を Upgrade で中継するので、HTTP/1.1 にする。
        .http1_only()
        .connect_timeout(Duration::from_secs(10))
        .build()?)
}

/// 参加の前に、コントロールプレーンの CA の証明書を取ってくる。この接続では相手の証明書を
/// 確かめられないので、取ってきた CA の指紋が参加用のトークンの指紋と同じかで確かめる。
pub async fn fetch_ca(server: &str, expected_fingerprint: &str) -> Result<String, Error> {
    let insecure = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .http1_only()
        .connect_timeout(Duration::from_secs(10))
        .build()?;
    let ca_pem = insecure
        .get(format!("{server}/internal/v1/ca"))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    if fingerprint(&ca_pem)? != expected_fingerprint.to_ascii_lowercase() {
        return Err(Error::Invalid(
            "the control plane's CA does not match the join token".into(),
        ));
    }
    Ok(ca_pem)
}

/// 参加の前の `chloe-node` が、確かめた CA の証明書だけを信頼して使うクライアント。
pub fn client_trusting(ca_pem: &str) -> Result<reqwest::Client, Error> {
    Ok(reqwest::Client::builder()
        .tls_certs_only([reqwest::Certificate::from_pem(ca_pem.as_bytes())?])
        .http1_only()
        .connect_timeout(Duration::from_secs(10))
        .build()?)
}

#[cfg(test)]
mod tests;
