use axum::routing::get;

use super::*;

async fn whoami(Extension(peer): Extension<Peer>) -> String {
    format!("{:?}", peer.0)
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
        Router::new().route("/whoami", get(whoami)),
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

/// コントロールプレーンの待ち受けに、トンネルを受ける経路を足す。ホストの証明書の相手だけを受ける。
fn tunnel_router(tunnels: tunnel::Tunnels) -> Router {
    Router::new().route(
        tunnel::TUNNEL_PATH,
        get(
            move |Extension(peer): Extension<Peer>, mut request: axum::extract::Request| async move {
                let Some(Role::Node(name)) = peer.0 else {
                    return axum::response::Response::builder()
                        .status(403)
                        .body(axum::body::Body::from("a node certificate is required"))
                        .unwrap();
                };
                let (response, serving) = tunnels.accept(&name, &mut request);
                if let Some(serving) = serving {
                    tokio::spawn(serving);
                }
                response
            },
        ),
    )
}

async fn echo(mut request: axum::extract::Request) -> axum::response::Response {
    let upgrade = hyper::upgrade::on(&mut request);
    tokio::spawn(async move {
        let upgraded = upgrade.await.unwrap();
        let (mut read, mut write) = tokio::io::split(TokioIo::new(upgraded));
        tokio::io::copy(&mut read, &mut write).await.unwrap();
    });
    axum::response::Response::builder()
        .status(101)
        .header("connection", "upgrade")
        .header("upgrade", "echo")
        .body(axum::body::Body::empty())
        .unwrap()
}

fn node_credentials(ca: &Ca, name: &str) -> Credentials {
    let (key_pem, csr_pem) = new_key_and_request().unwrap();
    Credentials {
        cert_pem: ca
            .sign_request(&csr_pem, &Role::Node(name.into()), &[])
            .unwrap(),
        key_pem,
        ca_pem: ca.cert_pem().to_string(),
    }
}

async fn wait_connected(tunnels: &tunnel::Tunnels, name: &str, connected: bool) {
    for _ in 0..100 {
        if tunnels.is_connected(name) == connected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("{name} did not become connected={connected}");
}

#[tokio::test]
async fn the_control_plane_calls_a_node_through_its_tunnel() {
    use http_body_util::BodyExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let dir = tempfile::tempdir().unwrap();
    let ca = Ca::load_or_create(dir.path()).unwrap();
    let control = ca.issue(&Role::ControlPlane, &names()).unwrap();
    let tunnels = tunnel::Tunnels::new();
    let url = start(
        &control,
        ClientAuth::Optional,
        tunnel_router(tunnels.clone()),
    )
    .await;
    let get_whoami = |tunnels: tunnel::Tunnels| async move {
        let request = hyper::Request::get("/whoami")
            .body(http_body_util::Full::default())
            .unwrap();
        let response = tunnels.request("n1", request).await?;
        let body = response.into_body().collect().await.unwrap().to_bytes();
        Ok::<_, Error>(String::from_utf8(body.to_vec()).unwrap())
    };
    assert!(matches!(
        get_whoami(tunnels.clone()).await,
        Err(Error::NotConnected)
    ));

    let node = node_credentials(&ca, "n1");
    let node_router = Router::new()
        .route("/whoami", get(whoami))
        .route("/echo", get(echo));
    let dialing = {
        let (url, node, router) = (url.clone(), node.clone(), node_router.clone());
        tokio::spawn(async move { tunnel::dial(&url, &node, router, || {}).await })
    };
    wait_connected(&tunnels, "n1", true).await;
    // ホストは、トンネルの上の要求をコントロールプレーンからのものとして受ける。
    assert_eq!(
        get_whoami(tunnels.clone()).await.unwrap(),
        "Some(ControlPlane)"
    );

    // Upgrade した接続も、トンネルの上で使える。
    let request = hyper::Request::get("/echo")
        .header("connection", "upgrade")
        .header("upgrade", "echo")
        .body(http_body_util::Full::default())
        .unwrap();
    let response = tunnels.request("n1", request).await.unwrap();
    assert_eq!(response.status(), 101);
    let mut upgraded = TokioIo::new(hyper::upgrade::on(response).await.unwrap());
    upgraded.write_all(b"hello").await.unwrap();
    let mut buf = [0u8; 5];
    upgraded.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"hello");

    // 同じホストが張り直したら、新しいトンネルを使う。
    let again = {
        let (url, node, router) = (url.clone(), node.clone(), node_router.clone());
        tokio::spawn(async move { tunnel::dial(&url, &node, router, || {}).await })
    };
    let finished = tokio::time::timeout(Duration::from_secs(5), dialing)
        .await
        .expect("the old tunnel closes when the node dials again");
    assert!(finished.unwrap().is_ok());
    assert_eq!(
        get_whoami(tunnels.clone()).await.unwrap(),
        "Some(ControlPlane)"
    );

    // ホストがいなくなったら、つながっていないことになる。
    again.abort();
    let _ = again.await;
    wait_connected(&tunnels, "n1", false).await;
    assert!(matches!(
        get_whoami(tunnels.clone()).await,
        Err(Error::NotConnected)
    ));
}

#[tokio::test]
async fn tunnels_need_the_right_certificates() {
    let dir = tempfile::tempdir().unwrap();
    let ca = Ca::load_or_create(dir.path()).unwrap();
    let control = ca.issue(&Role::ControlPlane, &names()).unwrap();
    let tunnels = tunnel::Tunnels::new();
    let url = start(
        &control,
        ClientAuth::Optional,
        tunnel_router(tunnels.clone()),
    )
    .await;
    let router = Router::new().route("/whoami", get(whoami));

    // コントロールプレーンの証明書でトンネルを求めても、ホストとしては受けない。
    let refused = tunnel::dial(&url, &control, router.clone(), || {}).await;
    assert!(
        matches!(&refused, Err(Error::Tunnel(m)) if m.contains("403")),
        "{refused:?}"
    );

    // ホストは、コントロールプレーンの証明書を出さない相手にはトンネルを張らない (同じ CA の
    // 証明書で、名前が合っていても)。
    let pretender = ca.issue(&Role::Node("n2".into()), &names()).unwrap();
    let fake = start(
        &pretender,
        ClientAuth::Optional,
        tunnel_router(tunnel::Tunnels::new()),
    )
    .await;
    let node = node_credentials(&ca, "n1");
    let refused = tunnel::dial(&fake, &node, router, || {}).await;
    assert!(
        matches!(&refused, Err(Error::Invalid(m)) if m.contains("control plane")),
        "{refused:?}"
    );
}
