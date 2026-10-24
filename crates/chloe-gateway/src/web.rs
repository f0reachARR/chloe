//! Web の画面のログインと、画面のファイルの配信。`[api] public_url` があるときだけ出す。
//!
//! ゲートウェイが処理する経路は `/api` の下に置き、ほかは画面 (SPA、`[api] web_root`) に任せる。
//!
//! ```text
//! GET  /api/auth/methods            使えるログインの方法 ({"github": true, "link": true})
//! GET  /api/auth/github             GitHub の承認へ送る (GitHub App に client secret があるとき)
//! GET  /api/auth/github/callback    GitHub から戻り、ログインして / へ送る (失敗は /login?error=...)
//! POST /api/auth/link               {"code": "..."} (`chloe web login` のコード) でログインする
//! POST /api/auth/logout
//! GET  /*                           web_root のファイル。ないパスには index.html を返す
//! ```
//!
//! ログインすると、セッションの値を cookie (`HttpOnly`、`SameSite=Lax`) で渡し、データベースには
//! ハッシュだけを置く。権限は SSH と同じ。GitHub のユーザートークンはログイン名を知るために
//! 一度だけ使い、残さない。

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use axum::extract::{Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use chloe_controller::store::random_token;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use tower_http::services::{ServeDir, ServeFile};
use tracing::{info, warn};
use utoipa::ToSchema;

use crate::api::{ErrorBody, error};
use crate::config::ApiConfig;
use crate::proxy;
use crate::session::Shared;

const SESSION_COOKIE: &str = "chloe_session";
const STATE_COOKIE: &str = "chloe_oauth_state";
const GITHUB_CALLBACK: &str = "/api/auth/github/callback";
/// GitHub の承認から戻るまで待つ秒数。
const STATE_TTL_SECS: i64 = 10 * 60;

/// Web の画面の設定 (`[api] public_url` から作る)。
pub struct WebConfig {
    public_url: Url,
    session_ttl_secs: i64,
}

impl WebConfig {
    fn new(api: &ApiConfig) -> Result<Option<Self>> {
        let Some(text) = &api.public_url else {
            return Ok(None);
        };
        let public_url = Url::parse(text).with_context(|| format!("api.public_url: {text}"))?;
        if !matches!(public_url.scheme(), "http" | "https") || public_url.host().is_none() {
            bail!("api.public_url must be an http(s) URL: {text}");
        }
        // 画面は `/` から置くので、リバースプロキシでもパスを付けずに出す。
        if public_url.path() != "/" || public_url.query().is_some() {
            bail!("api.public_url must not have a path: {text}");
        }
        Ok(Some(Self {
            public_url,
            session_ttl_secs: api.session_ttl_secs,
        }))
    }

    /// 起動時に設定を確かめる。
    pub fn check(api: &ApiConfig) -> Result<()> {
        Self::new(api)?;
        if let Some(proxy_url) = &api.proxy_url {
            chloe_controller::ports::ProxyBase::parse(proxy_url)?;
            if api.public_url.is_none() {
                bail!(
                    "api.proxy_url needs api.public_url (opening a port signs in through the web UI)"
                );
            }
        }
        if let Some(root) = &api.web_root
            && !root.join("index.html").is_file()
        {
            bail!("api.web_root has no index.html: {}", root.display());
        }
        Ok(())
    }

    /// ゲートウェイの設定から作る。Web の画面を出さないなら `None`。
    pub fn of(shared: &Shared) -> Option<Self> {
        Self::new(shared.config.api.as_ref()?).ok().flatten()
    }

    fn origin(&self) -> String {
        self.public_url.origin().ascii_serialization()
    }

    pub(crate) fn url(&self, path: &str) -> String {
        format!("{}{path}", self.origin())
    }

    /// 要求の `Origin` が画面と同じか。`Origin` のない要求は同じとみなさない。
    pub fn same_origin(&self, headers: &HeaderMap) -> bool {
        headers
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|origin| origin == self.origin())
    }

    pub(crate) fn cookie(&self, name: &str, value: &str, path: &str, max_age: i64) -> HeaderValue {
        let secure = if self.public_url.scheme() == "https" {
            "; Secure"
        } else {
            ""
        };
        let cookie = format!(
            "{name}={value}; Path={path}; Max-Age={max_age}; HttpOnly; SameSite=Lax{secure}"
        );
        HeaderValue::from_str(&cookie).expect("cookie values are URL-safe")
    }
}

/// `chloe web login` の URL。
pub fn login_link(shared: &Shared, code: &str) -> Option<String> {
    Some(WebConfig::of(shared)?.url(&format!("/login/link?code={code}")))
}

/// 要求の cookie から、名前が `name` の値を探す。
pub(crate) fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value.to_string())
}

/// 要求のセッションの cookie の値。
pub fn session_cookie(headers: &HeaderMap) -> Option<String> {
    cookie_value(headers, SESSION_COOKIE).filter(|v| !v.is_empty())
}

pub fn router(shared: Arc<Shared>) -> Router {
    if WebConfig::of(&shared).is_none() {
        return Router::new();
    }
    let web_root = shared.config.api.as_ref().and_then(|a| a.web_root.clone());
    let router = Router::new()
        .route("/api/auth/methods", get(methods))
        .route("/api/auth/github", get(github_start))
        .route(GITHUB_CALLBACK, get(github_callback))
        .route("/api/auth/link", post(link))
        .route("/api/auth/logout", post(logout))
        .route("/api/auth/proxy", get(proxy::start))
        .route("/api/auth/continue", get(proxy::resume))
        // 管理 API にもログインにもない `/api` の経路は、画面ではなく 404 を返す。
        .route("/api/{*rest}", any(api_not_found))
        .with_state(shared);
    match web_root {
        Some(root) => router.merge(static_files(&root)),
        None => router,
    }
}

async fn api_not_found() -> Response {
    error(StatusCode::NOT_FOUND, "no such API path")
}

/// 画面のファイル。ないパスには `index.html` を返し、画面のルーティングに任せる。ただし
/// `/assets/` は 404 にする (古い版のファイルを求められたときに、長くキャッシュさせる
/// `/assets/` の名前で `index.html` を返さないように)。
fn static_files(root: &Path) -> Router {
    let serve = ServeDir::new(root).fallback(ServeFile::new(root.join("index.html")));
    Router::new()
        .nest_service("/assets", ServeDir::new(root.join("assets")))
        .fallback_service(serve)
        .layer(middleware::from_fn(static_headers))
}

/// 画面のファイルに付けるヘッダー。Vite が `/assets/` に出すファイルは名前に中身のハッシュが
/// 付くので長くキャッシュさせ、ほか (`index.html`、Service Worker など) は毎回確かめさせる。
async fn static_headers(request: Request, next: Next) -> Response {
    let hashed = request.uri().path().starts_with("/assets/");
    let mut response = next.run(request).await;
    let cache = if hashed && response.status().is_success() {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    // スクリプトは画面のファイルだけを使い、ほかのサイトの枠に入れさせない。Mantine は CSS の
    // 変数を style 要素で入れるので、スタイルにはインラインを許す。
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
             img-src 'self' data:; frame-ancestors 'none'; form-action 'self'; base-uri 'none'",
        ),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}

/// 使えるログインの方法。
#[derive(Serialize, ToSchema)]
pub(crate) struct AuthMethods {
    /// GitHub のログイン (`/api/auth/github`) を使えるか。
    github: bool,
    /// `chloe web login` の URL でログインできるか (いつも使える)。
    link: bool,
}

#[utoipa::path(get, path = "/api/auth/methods", tag = "auth", security(()), responses((status = 200, body = AuthMethods)))]
pub(crate) async fn methods(State(shared): State<Arc<Shared>>) -> Response {
    let github = shared.github.as_ref().is_some_and(|g| g.has_web_flow());
    Json(AuthMethods { github, link: true }).into_response()
}

/// ログインの画面に戻して、失敗を伝える。
fn back_to_login(message: &str) -> Response {
    let mut url = Url::parse("http://localhost/login").expect("a valid URL");
    url.query_pairs_mut().append_pair("error", message);
    Redirect::to(&format!(
        "{}?{}",
        url.path(),
        url.query().unwrap_or_default()
    ))
    .into_response()
}

/// セッションを作る。ログインできない利用者なら、伝える言葉を返す。
async fn create_session(
    shared: &Shared,
    web: &WebConfig,
    login: &str,
    method: &str,
) -> Result<HeaderValue, (StatusCode, String)> {
    match shared.is_allowed(login).await {
        Ok(true) => {}
        Ok(false) => {
            warn!(target: "audit", via = "web", login, method, "web sign-in refused: the user may not log in");
            return Err((
                StatusCode::FORBIDDEN,
                format!("{login} may not use this server"),
            ));
        }
        Err(e) => return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))),
    }
    let (id, session) = shared
        .controller
        .store()
        .create_web_session(login, web.session_ttl_secs)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    info!(target: "audit", via = "web", login, session = id, method, "signed in to the web UI");
    Ok(web.cookie(SESSION_COOKIE, &session, "/", web.session_ttl_secs))
}

fn github_unavailable() -> Response {
    error(StatusCode::NOT_FOUND, "GitHub sign-in is not set up")
}

#[utoipa::path(get, path = "/api/auth/github", tag = "auth", security(()), description = "ブラウザで開く。GitHub の承認へ送り、戻ったら `/api/auth/github/callback` がログインして `/` へ送る (失敗は `/login?error=...`)。", responses((status = 303, description = "GitHub の承認へ送る"), (status = "4XX", description = "誤り", body = ErrorBody)))]
pub(crate) async fn github_start(State(shared): State<Arc<Shared>>) -> Response {
    let (Some(web), Some(github)) = (WebConfig::of(&shared), shared.github.as_ref()) else {
        return github_unavailable();
    };
    if !github.has_web_flow() {
        return github_unavailable();
    }
    let state = random_token();
    let url = match github.authorize_url(&web.url(GITHUB_CALLBACK), &state) {
        Ok(url) => url,
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let mut response = Redirect::to(&url).into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        web.cookie(STATE_COOKIE, &state, GITHUB_CALLBACK, STATE_TTL_SECS),
    );
    response
}

#[derive(Deserialize)]
struct Callback {
    code: Option<String>,
    state: Option<String>,
    error_description: Option<String>,
}

async fn github_callback(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    Query(query): Query<Callback>,
) -> Response {
    let (Some(web), Some(github)) = (WebConfig::of(&shared), shared.github.as_ref()) else {
        return github_unavailable();
    };
    if let Some(message) = query.error_description {
        return back_to_login(&format!("GitHub: {message}"));
    }
    // 承認へ送ったブラウザと、戻ってきたブラウザが同じかを確かめる (ログインの CSRF の対策)。
    let expected = cookie_value(&headers, STATE_COOKIE);
    let (Some(code), Some(state)) = (query.code, query.state) else {
        return back_to_login("the sign-in was not completed");
    };
    if expected.as_deref() != Some(state.as_str()) {
        return back_to_login("the sign-in has expired or was started elsewhere; try again");
    }
    let login = match github.exchange_code(&code, &web.url(GITHUB_CALLBACK)).await {
        Ok(token) => github.user_login(&token.access_token).await,
        Err(e) => Err(e),
    };
    let login = match login {
        Ok(login) => login,
        Err(e) => {
            warn!("GitHub sign-in for the web UI failed: {e}");
            return back_to_login("GitHub sign-in failed; try again");
        }
    };
    let cookie = match create_session(&shared, &web, &login, "github").await {
        Ok(cookie) => cookie,
        Err((_, message)) => return back_to_login(&message),
    };
    // ログインの前に公開したポートを開こうとしていれば、そこへ戻す。
    let mut response = Redirect::to("/api/auth/continue").into_response();
    let headers = response.headers_mut();
    headers.append(header::SET_COOKIE, cookie);
    headers.append(
        header::SET_COOKIE,
        web.cookie(STATE_COOKIE, "", GITHUB_CALLBACK, 0),
    );
    response
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct LinkCode {
    code: String,
}

/// `chloe web login` のコードでログインする。画面は、コードの付いた URL を開いただけでは
/// 呼ばず、ボタンを押してから呼ぶ (リンクのプレビューを作るボットに使い切られないように)。
#[utoipa::path(post, path = "/api/auth/link", tag = "auth", security(()), request_body = LinkCode, responses((status = 204, description = "ログインした (セッションの cookie を渡す)"), (status = "4XX", description = "誤り", body = ErrorBody)))]
pub(crate) async fn link(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    Json(body): Json<LinkCode>,
) -> Response {
    let Some(web) = WebConfig::of(&shared) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !web.same_origin(&headers) {
        return error(
            StatusCode::FORBIDDEN,
            "the request did not come from the web UI (Origin does not match)",
        );
    }
    let login = match shared
        .controller
        .store()
        .redeem_web_login_link(&body.code)
        .await
    {
        Ok(Some(login)) => login,
        Ok(None) => {
            return error(
                StatusCode::FORBIDDEN,
                "the link has expired or was already used; run chloe web login again",
            );
        }
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    match create_session(&shared, &web, &login, "link").await {
        Ok(cookie) => (StatusCode::NO_CONTENT, [(header::SET_COOKIE, cookie)]).into_response(),
        Err((status, message)) => error(status, message),
    }
}

#[utoipa::path(post, path = "/api/auth/logout", tag = "auth", responses((status = 204, description = "ログアウトした"), (status = "4XX", description = "誤り", body = ErrorBody)))]
pub(crate) async fn logout(State(shared): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    let Some(web) = WebConfig::of(&shared) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !web.same_origin(&headers) {
        return error(
            StatusCode::FORBIDDEN,
            "the request did not come from the web UI (Origin does not match)",
        );
    }
    if let Some(session) = session_cookie(&headers)
        && let Err(e) = shared.controller.store().end_web_session(&session).await
    {
        warn!("failed to end a web session: {e}");
    }
    (
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, web.cookie(SESSION_COOKIE, "", "/", 0))],
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api(public_url: Option<&str>) -> ApiConfig {
        ApiConfig {
            listen: "127.0.0.1:8080".parse().unwrap(),
            tls_cert: None,
            tls_key: None,
            public_url: public_url.map(Into::into),
            web_root: None,
            proxy_url: None,
            session_ttl_secs: 3600,
        }
    }

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(*name, value.parse().unwrap());
        }
        headers
    }

    #[test]
    fn public_url_must_be_an_origin() {
        assert!(WebConfig::new(&api(None)).unwrap().is_none());
        assert!(
            WebConfig::new(&api(Some("https://chloe.example.com")))
                .unwrap()
                .is_some()
        );
        assert!(
            WebConfig::new(&api(Some("http://127.0.0.1:8080/")))
                .unwrap()
                .is_some()
        );
        assert!(WebConfig::new(&api(Some("https://chloe.example.com/chloe"))).is_err());
        assert!(WebConfig::new(&api(Some("ftp://chloe.example.com"))).is_err());
        assert!(WebConfig::new(&api(Some("chloe.example.com"))).is_err());
    }

    #[test]
    fn checks_the_origin() {
        let web = WebConfig::new(&api(Some("https://chloe.example.com:8443")))
            .unwrap()
            .unwrap();
        assert!(web.same_origin(&headers(&[("origin", "https://chloe.example.com:8443")])));
        assert!(!web.same_origin(&headers(&[("origin", "https://evil.example.com")])));
        assert!(!web.same_origin(&headers(&[("origin", "http://chloe.example.com:8443")])));
        assert!(!web.same_origin(&headers(&[("origin", "null")])));
        assert!(!web.same_origin(&HeaderMap::new()));
        assert_eq!(
            web.url(GITHUB_CALLBACK),
            "https://chloe.example.com:8443/api/auth/github/callback"
        );
    }

    #[test]
    fn cookies_are_secure_only_over_https() {
        let https = WebConfig::new(&api(Some("https://chloe.example.com")))
            .unwrap()
            .unwrap();
        assert_eq!(
            https.cookie(SESSION_COOKIE, "v", "/", 60),
            "chloe_session=v; Path=/; Max-Age=60; HttpOnly; SameSite=Lax; Secure"
        );
        let http = WebConfig::new(&api(Some("http://127.0.0.1:8080")))
            .unwrap()
            .unwrap();
        assert!(
            !http
                .cookie(SESSION_COOKIE, "v", "/", 60)
                .to_str()
                .unwrap()
                .contains("Secure")
        );
    }

    #[test]
    fn reads_cookies() {
        let map = headers(&[
            ("cookie", "a=1; chloe_session=abc"),
            ("cookie", "chloe_oauth_state=s"),
        ]);
        assert_eq!(session_cookie(&map).as_deref(), Some("abc"));
        assert_eq!(cookie_value(&map, STATE_COOKIE).as_deref(), Some("s"));
        assert_eq!(
            session_cookie(&headers(&[("cookie", "chloe_session=")])),
            None
        );
        assert_eq!(session_cookie(&HeaderMap::new()), None);
    }

    #[test]
    fn sends_errors_back_to_the_login_page() {
        let response = back_to_login("a & b");
        assert_eq!(response.headers()[header::LOCATION], "/login?error=a+%26+b");
    }
}
