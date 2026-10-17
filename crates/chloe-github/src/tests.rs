use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

struct Request {
    method: String,
    path: String,
    authorization: Option<String>,
    body: String,
}

/// GitHub のふりをする HTTP サーバー。`handler` が (ステータス, 本文) を返す。
async fn stub<F>(handler: F) -> String
where
    F: Fn(Request) -> (u16, String) + Send + Sync + 'static,
{
    let handler = Arc::new(handler);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let handler = handler.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0; 4096];
                // ヘッダーと、Content-Length の分の本文を読む。
                let (head, body) = loop {
                    let n = socket.read(&mut chunk).await.unwrap();
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    if let Some((head, body)) = text.split_once("\r\n\r\n") {
                        let len = head
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if body.len() >= len || n == 0 {
                            break (head.to_string(), body.to_string());
                        }
                    }
                };
                let mut lines = head.lines();
                let mut first = lines.next().unwrap().split_whitespace();
                let method = first.next().unwrap().to_string();
                let path = first.next().unwrap().to_string();
                let authorization = lines.find_map(|l| {
                    let (k, v) = l.split_once(": ")?;
                    k.eq_ignore_ascii_case("authorization")
                        .then(|| v.to_string())
                });
                let (status, body) = handler(Request {
                    method,
                    path,
                    authorization,
                    body,
                });
                let response = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            });
        }
    });
    format!("http://{addr}")
}

async fn app(url: &str) -> GitHubApp {
    let key = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/test-app-key.pem");
    let config = AppConfig {
        app_id: 123,
        client_id: "Iv-test".into(),
        organization: None,
        private_key: format!("file:{key}"),
        api_url: url.into(),
        web_url: url.into(),
    };
    GitHubApp::new(config, &Resolver { connect: None })
        .await
        .unwrap()
}

/// acme (インストール 42) と beta (インストール 43) にインストールしたアプリのふり。
/// alice は acme の管理者で team dev の maintainer、bob は beta のメンバー、carol は招待中。
fn two_orgs(
    issued: Arc<AtomicUsize>,
    scoped: Arc<Mutex<Vec<String>>>,
) -> impl Fn(Request) -> (u16, String) + Send + Sync + 'static {
    move |req| {
        let auth = req.authorization.as_deref().unwrap_or_default();
        let acme = auth == "Bearer ghs_acme";
        let beta = auth == "Bearer ghs_beta";
        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/app/installations?per_page=100&page=1") => {
                assert!(auth.starts_with("Bearer ey"), "an app JWT");
                (
                    200,
                    r#"[{"id":42,"account":{"login":"Acme","type":"Organization"}},
                        {"id":43,"account":{"login":"beta","type":"Organization"}},
                        {"id":44,"account":{"login":"someone","type":"User"}}]"#
                        .into(),
                )
            }
            ("POST", "/app/installations/42/access_tokens") => {
                issued.fetch_add(1, Ordering::SeqCst);
                if !req.body.is_empty() {
                    scoped.lock().unwrap().push(req.body.clone());
                    return (
                        201,
                        r#"{"token":"ghs_acme_scoped","expires_at":"2099-01-01T00:00:00Z"}"#.into(),
                    );
                }
                (
                    201,
                    r#"{"token":"ghs_acme","expires_at":"2099-01-01T00:00:00Z"}"#.into(),
                )
            }
            ("POST", "/app/installations/43/access_tokens") => (
                201,
                r#"{"token":"ghs_beta","expires_at":"2099-01-01T00:00:00Z"}"#.into(),
            ),
            ("GET", "/orgs/acme/memberships/alice") if acme => {
                (200, r#"{"state":"active","role":"admin"}"#.into())
            }
            ("GET", "/orgs/acme/memberships/carol") if acme => {
                (200, r#"{"state":"pending","role":"member"}"#.into())
            }
            ("GET", "/orgs/beta/memberships/bob") if beta => {
                (200, r#"{"state":"active","role":"member"}"#.into())
            }
            ("GET", "/orgs/acme/teams/dev/memberships/alice") if acme => {
                (200, r#"{"state":"active","role":"maintainer"}"#.into())
            }
            ("GET", "/orgs/acme/teams/dev/memberships/dave") if acme => {
                (200, r#"{"state":"active","role":"member"}"#.into())
            }
            _ => (404, "{}".into()),
        }
    }
}

#[tokio::test]
async fn installations_tokens_and_roles() {
    let issued = Arc::new(AtomicUsize::new(0));
    let scoped = Arc::new(Mutex::new(Vec::new()));
    let url = stub(two_orgs(issued.clone(), scoped.clone())).await;
    let app = app(&url).await;
    assert_eq!(app.organizations().await.unwrap(), ["acme", "beta"]);
    assert!(app.is_installed("ACME").await.unwrap());
    assert!(
        !app.is_installed("someone").await.unwrap(),
        "only organizations"
    );
    assert_eq!(
        app.installation_token("acme", &TokenScope::default())
            .await
            .unwrap()
            .expose_secret(),
        "ghs_acme"
    );
    assert!(matches!(
        app.installation_token("gamma", &TokenScope::default())
            .await,
        Err(Error::NotInstalled(_))
    ));

    assert_eq!(
        app.org_role("acme", "alice").await.unwrap(),
        Some(OrgRole::Admin)
    );
    assert_eq!(
        app.org_role("beta", "bob").await.unwrap(),
        Some(OrgRole::Member)
    );
    assert!(!app.is_org_member("acme", "bob").await.unwrap());
    assert!(
        !app.is_org_member("acme", "carol").await.unwrap(),
        "pending"
    );
    assert!(
        !app.is_org_member("gamma", "alice").await.unwrap(),
        "not installed"
    );
    assert!(app.is_member_of_any("bob").await.unwrap());
    assert!(!app.is_member_of_any("carol").await.unwrap());
    assert_eq!(
        app.team_role("acme", "dev", "alice").await.unwrap(),
        Some(TeamRole::Maintainer)
    );
    assert_eq!(
        app.team_role("acme", "dev", "dave").await.unwrap(),
        Some(TeamRole::Member)
    );
    assert!(!app.is_team_member("acme", "dev", "bob").await.unwrap());
    assert_eq!(
        issued.load(Ordering::SeqCst),
        1,
        "the token is reused until it nearly expires"
    );

    // 範囲を指定すると、その範囲のトークンを別に発行して使い回す。
    let scope = TokenScope {
        repositories: Some(vec!["app".into()]),
        permissions: Some([("contents".to_string(), "read".to_string())].into()),
    };
    for _ in 0..2 {
        assert_eq!(
            app.installation_token("acme", &scope)
                .await
                .unwrap()
                .expose_secret(),
            "ghs_acme_scoped"
        );
    }
    assert_eq!(issued.load(Ordering::SeqCst), 2);
    let body: serde_json::Value = serde_json::from_str(&scoped.lock().unwrap()[0]).unwrap();
    assert_eq!(
        body,
        serde_json::json!({"repositories": ["app"], "permissions": {"contents": "read"}})
    );
}

/// 設定で Organization を一つに限ると、ほかのインストールは使わない。
#[tokio::test]
async fn organization_setting_limits_installations() {
    let url = stub(two_orgs(Arc::default(), Arc::default())).await;
    let key = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/test-app-key.pem");
    let config = AppConfig {
        app_id: 123,
        client_id: "Iv-test".into(),
        organization: Some("acme".into()),
        private_key: format!("file:{key}"),
        api_url: url.clone(),
        web_url: url,
    };
    let app = GitHubApp::new(config, &Resolver { connect: None })
        .await
        .unwrap();
    assert_eq!(app.organizations().await.unwrap(), ["acme"]);
    assert!(!app.is_member_of_any("bob").await.unwrap());
}

#[tokio::test]
async fn device_flow_and_refresh() {
    let polls = Arc::new(AtomicUsize::new(0));
    let counter = polls.clone();
    let url = stub(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/login/device/code") => {
            assert!(req.body.contains("client_id=Iv-test"));
            (
                200,
                r#"{"device_code":"dc1","user_code":"ABCD-1234","verification_uri":"https://github.com/login/device","expires_in":900,"interval":5}"#.into(),
            )
        }
        ("POST", "/login/oauth/access_token") if req.body.contains("grant_type=refresh_token") => {
            assert!(req.body.contains("refresh_token=ghr_1"));
            assert!(!req.body.contains("client_secret"));
            (200, r#"{"access_token":"ghu_2","expires_in":28800,"refresh_token":"ghr_2","refresh_token_expires_in":15897600}"#.into())
        }
        ("POST", "/login/oauth/access_token") => {
            assert!(req.body.contains("device_code=dc1"));
            match counter.fetch_add(1, Ordering::SeqCst) {
                0 => (200, r#"{"error":"authorization_pending"}"#.into()),
                1 => (200, r#"{"error":"slow_down","interval":10}"#.into()),
                _ => (200, r#"{"access_token":"ghu_1","expires_in":28800,"refresh_token":"ghr_1","refresh_token_expires_in":15897600}"#.into()),
            }
        }
        ("GET", "/user") if req.authorization.as_deref() == Some("Bearer ghu_1") => {
            (200, r#"{"login":"Alice"}"#.into())
        }
        _ => (404, "{}".into()),
    })
    .await;
    let app = app(&url).await;
    let device = app.start_device_flow().await.unwrap();
    assert_eq!(device.user_code, "ABCD-1234");
    assert!(matches!(
        app.poll_device_flow(&device).await.unwrap(),
        Poll::Pending { interval: 5 }
    ));
    assert!(matches!(
        app.poll_device_flow(&device).await.unwrap(),
        Poll::Pending { interval: 10 }
    ));
    let Poll::Token(token) = app.poll_device_flow(&device).await.unwrap() else {
        panic!("expected a token");
    };
    assert_eq!(token.access_token, "ghu_1");
    assert!(!token.expires_within(Duration::from_secs(60)));
    assert!(token.expires_within(Duration::from_secs(9 * 3600)));
    assert_eq!(app.user_login(&token.access_token).await.unwrap(), "Alice");

    let refreshed = app
        .refresh_user_token(token.refresh_token.as_deref().unwrap())
        .await
        .unwrap();
    assert_eq!(refreshed.access_token, "ghu_2");
    assert_eq!(refreshed.refresh_token.as_deref(), Some("ghr_2"));
}

#[test]
fn parses_github_timestamps() {
    assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(parse_rfc3339("2026-10-16T12:00:00Z"), Some(1_792_152_000));
    assert_eq!(parse_rfc3339("2026-10-16 12:00:00"), None);
}
