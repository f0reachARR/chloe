use axum::routing::get;

use super::*;

async fn whoami(Extension(peer): Extension<Peer>) -> String {
    format!("{:?}", peer.0)
}

async fn peer_addr(ConnectInfo(addr): ConnectInfo<SocketAddr>) -> String {
    addr.ip().to_string()
}

/// `router` を相互 TLS で待ち受け、URL を返す。
async fn start(credentials: &Credentials, client_auth: ClientAuth, router: Router) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let config = server_config(credentials, client_auth).unwrap();
    tokio::spawn(serve(listener, config, router));
    format!("https://127.0.0.1:{}", addr.port())
}

fn names() -> Vec<String> {
    vec!["127.0.0.1".into(), "localhost".into()]
}

#[tokio::test]
async fn roles_are_read_from_certificates() {
    let dir = tempfile::tempdir().unwrap();
    let ca = Ca::load_or_create(dir.path()).unwrap();
    // 読み直しても同じ CA になる。
    assert_eq!(
        Ca::load_or_create(dir.path()).unwrap().fingerprint(),
        ca.fingerprint()
    );
    let control = ca.issue(&Role::ControlPlane, &names()).unwrap();
    let (key_pem, csr_pem) = new_key_and_request().unwrap();
    let node = Credentials {
        cert_pem: ca
            .sign_request(&csr_pem, &Role::Node("n1".into()), &names())
            .unwrap(),
        key_pem,
        ca_pem: ca.cert_pem().to_string(),
    };

    let router = Router::new().route("/whoami", get(whoami));
    let url = start(&control, ClientAuth::Optional, router.clone()).await;
    let ask = |client: reqwest::Client| {
        let url = url.clone();
        async move {
            client
                .get(format!("{url}/whoami"))
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap()
        }
    };
    assert_eq!(ask(client(&node).unwrap()).await, r#"Some(Node("n1"))"#);
    assert_eq!(ask(client(&control).unwrap()).await, "Some(ControlPlane)");
    assert_eq!(ask(client_trusting(ca.cert_pem()).unwrap()).await, "None");

    // 証明書を求める待ち受けには、証明書のないクライアントはつなげない。
    let strict = start(&control, ClientAuth::Required, router).await;
    assert!(
        client_trusting(ca.cert_pem())
            .unwrap()
            .get(format!("{strict}/whoami"))
            .send()
            .await
            .is_err()
    );

    // ほかの CA が発行した証明書は受け付けない。
    let other_dir = tempfile::tempdir().unwrap();
    let other = Ca::load_or_create(other_dir.path()).unwrap();
    let mut stranger = other.issue(&Role::ControlPlane, &names()).unwrap();
    stranger.ca_pem = ca.cert_pem().to_string();
    assert!(
        client(&stranger)
            .unwrap()
            .get(format!("{url}/whoami"))
            .send()
            .await
            .is_err()
    );
}

#[tokio::test]
async fn ca_is_fetched_only_with_the_right_fingerprint() {
    let dir = tempfile::tempdir().unwrap();
    let ca = Ca::load_or_create(dir.path()).unwrap();
    let control = ca.issue(&Role::ControlPlane, &names()).unwrap();
    let ca_pem = ca.cert_pem().to_string();
    let router = Router::new().route("/internal/v1/ca", get(move || async move { ca_pem }));
    let url = start(&control, ClientAuth::Optional, router).await;
    assert_eq!(
        fetch_ca(&url, &ca.fingerprint()).await.unwrap(),
        ca.cert_pem()
    );
    assert!(fetch_ca(&url, &"0".repeat(64)).await.is_err());
}

#[tokio::test]
async fn plain_tls_does_not_ask_for_client_certificates() {
    let dir = tempfile::tempdir().unwrap();
    let ca = Ca::load_or_create(dir.path()).unwrap();
    let server = ca.issue(&Role::ControlPlane, &names()).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let config = tls_server_config(&server.cert_pem, &server.key_pem).unwrap();
    tokio::spawn(serve(
        listener,
        config,
        Router::new()
            .route("/whoami", get(whoami))
            .route("/peer", get(peer_addr)),
    ));
    // 証明書を出すクライアントでも、役割は読まない (求めていないので送られない)。
    for client in [
        client_trusting(ca.cert_pem()).unwrap(),
        client(&server).unwrap(),
    ] {
        let body = client
            .get(format!("https://127.0.0.1:{port}/whoami"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(body, "None");
    }
    // 接続元のアドレスを、axum の ConnectInfo で読める。
    let peer = client_trusting(ca.cert_pem())
        .unwrap()
        .get(format!("https://127.0.0.1:{port}/peer"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(peer, "127.0.0.1");
}

#[test]
fn node_names() {
    assert!(valid_node_name("node-1"));
    assert!(!valid_node_name(""));
    assert!(!valid_node_name("a/b"));
    assert_eq!(
        Role::from_common_name("chloe-node/n1"),
        Some(Role::Node("n1".into()))
    );
    assert_eq!(Role::from_common_name("chloe-node/../x"), None);
    assert_eq!(Role::from_common_name("someone"), None);
}
