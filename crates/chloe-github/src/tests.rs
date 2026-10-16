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
        organization: "acme".into(),
        private_key: format!("file:{key}"),
        api_url: url.into(),
        web_url: url.into(),
    };
    GitHubApp::new(config, &Resolver { connect: None })
        .await
        .unwrap()
}

#[tokio::test]
async fn installation_token_is_cached_and_used_for_membership() {
    let issued = Arc::new(AtomicUsize::new(0));
    let counter = issued.clone();
    let url = stub(move |req| {
        let installation = req.authorization.as_deref() == Some("Bearer ghs_test");
        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/orgs/acme/installation") => {
                assert!(
                    req.authorization.unwrap().starts_with("Bearer ey"),
                    "an app JWT"
                );
                (200, r#"{"id":42}"#.into())
            }
            ("POST", "/app/installations/42/access_tokens") => {
                counter.fetch_add(1, Ordering::SeqCst);
                (
                    201,
                    r#"{"token":"ghs_test","expires_at":"2099-01-01T00:00:00Z"}"#.into(),
                )
            }
            ("GET", "/orgs/acme/members/alice") if installation => (204, String::new()),
            ("GET", "/orgs/acme/members/bob") if installation => (404, "{}".into()),
            ("GET", "/orgs/acme/teams/dev/memberships/alice") if installation => {
                (200, r#"{"state":"active"}"#.into())
            }
            ("GET", "/orgs/acme/teams/dev/memberships/carol") if installation => {
                (200, r#"{"state":"pending"}"#.into())
            }
            _ => (404, "{}".into()),
        }
    })
    .await;
    let app = app(&url).await;
    assert_eq!(
        app.installation_token().await.unwrap().expose_secret(),
        "ghs_test"
    );
    assert!(app.is_org_member("alice").await.unwrap());
    assert!(!app.is_org_member("bob").await.unwrap());
    assert!(app.is_team_member("dev", "alice").await.unwrap());
    assert!(!app.is_team_member("dev", "carol").await.unwrap());
    assert!(!app.is_team_member("dev", "bob").await.unwrap());
    assert_eq!(
        issued.load(Ordering::SeqCst),
        1,
        "the token is reused until it nearly expires"
    );
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
