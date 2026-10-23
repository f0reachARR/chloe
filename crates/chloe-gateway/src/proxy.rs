//! VM の中で公開したポートの HTTP(S) のサーバーを、ブラウザからの要求に中継する。
//!
//! 公開したポートごとに、ランダムな名前のプロキシのホスト `<名前>.<ドメイン>` (`[api] proxy_url`)
//! を割り当てる。ホスト名からワークスペースやポートは分からず、試して当てることもできない。VM の中のページはエージェントが書き換えられるので、Web の画面と
//! 同じオリジンには置かない。待ち受けは管理 API と同じで、`Host` でプロキシの要求を見分ける
//! ([`dispatch`])。
//!
//! ```text
//! (プロキシのホスト) GET /__chloe/auth?code=&next=   コードをセッションに替え、cookie を渡して next へ送る
//! (プロキシのホスト) *   /*                          公開したポートへ中継する
//! (Web の画面)       GET /api/auth/proxy?name=&next=
//!                        Web の画面のセッションでコードを作り、プロキシのホストへ戻す
//! (Web の画面)       GET /api/auth/continue          ログインのあと、ログインの前に開こうとしたポートへ戻す
//! ```
//!
//! プロキシのホストの cookie (`chloe_proxy`) がない要求は、Web の画面へ送ってログインさせる。
//! cookie のセッションは Web の画面のセッションに結び付けるので、ログアウトすると一緒に消える。
//! `Authorization: Bearer <トークン>` でも開ける。どちらも VM の中のサーバーには渡さない。
//!
//! 中継は要求ごとに、ポート転送と同じく guest-agent に VM の中のポートへの接続を頼み、その上で
//! HTTP/1.1 を話す。本文は溜めずに流し、`Upgrade` (WebSocket) は切り替えたあとのバイト列を中継する。

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};

use anyhow::{Context as _, Result, bail};
use axum::body::Body;
use axum::extract::{Query, Request, State};
use axum::http::header::{self, HeaderMap, HeaderName, HeaderValue};
use axum::http::{Method, StatusCode, Uri, Version};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use bytes::BytesMut;
use chloe_agent_proto::{
    AgentMessage, ClientMessage, ConnectRequest, Connection as AgentConnection, PROTOCOL_VERSION,
};
use chloe_controller::Session;
use chloe_controller::ports::{ProxyBase, PublishedPort, Scheme};
use chloe_controller::workspace::Workspace;
use hyper_util::rt::TokioIo;
use reqwest::Url;
use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tracing::{info, warn};

use crate::api::Via;
use crate::backend::Backend;
use crate::session::{AGENT_CONNECT_WAIT, Shared};
use crate::web::{self, WebConfig};

/// プロキシのホストのセッションの cookie。
pub(crate) const PROXY_COOKIE: &str = "chloe_proxy";
/// ログインの前に開こうとしたポートを覚えておく cookie (Web の画面の `/api/auth` に付ける)。
const RETURN_COOKIE: &str = "chloe_proxy_return";
const RETURN_COOKIE_PATH: &str = "/api/auth";
/// ログインを待つあいだ、開こうとしたポートを覚えておく秒数。
const RETURN_TTL_SECS: i64 = 10 * 60;
/// プロキシのホストでコードを受ける経路。VM の中のサーバーには渡さない。
const AUTH_PATH: &str = "/__chloe/auth";
/// Web の画面からプロキシのホストへ戻すコードの期限。
const CODE_TTL_SECS: i64 = 60;
/// VM の中のサーバーへつなぐ名前。`localhost` だけで待ち受けるサーバーにも届くよう、名前で渡して
/// guest-agent に引かせる (IPv6 の `::1` だけで待ち受けるものもある)。
const UPSTREAM_HOST: &str = "localhost";

/// 中継の前に除くヘッダー (RFC 9110 の hop-by-hop と、それに準じるもの)。
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-connection",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// プロキシのホストの名前が指す、ワークスペースのポート。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Target {
    workspace_id: i64,
    port: u16,
}

/// 確かめた呼び出し元。
struct ProxyCaller {
    login: String,
    via: Via,
}

/// 管理 API と画面より先に、`Host` がプロキシのホストの要求を引き取る。
pub(crate) async fn dispatch(
    State(shared): State<Arc<Shared>>,
    request: Request,
    next: Next,
) -> Response {
    let target = shared.controller.ports().base().and_then(|base| {
        let host = request_host(&request)?;
        Some((base.clone(), base.parse_host(&host)?))
    });
    match target {
        Some((base, name)) => handle(shared, base, &name, request).await,
        None => next.run(request).await,
    }
}

/// 要求のホスト (HTTP/2 では URI に、HTTP/1.1 では `Host` にある)。
fn request_host(request: &Request) -> Option<String> {
    request
        .uri()
        .authority()
        .map(|a| a.to_string())
        .or_else(|| {
            let host = request.headers().get(header::HOST)?.to_str().ok()?;
            Some(host.to_string())
        })
}

fn text(status: StatusCode, message: impl std::fmt::Display) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        format!("chloe: {message}\n"),
    )
        .into_response()
}

/// 名前で公開したポートを探す。ないか取り消したなら 404。
async fn find_port(shared: &Shared, name: &str) -> Result<PublishedPort, Box<Response>> {
    match shared.controller.ports().find(name).await {
        Ok(Some(published)) => Ok(published),
        Ok(None) => Err(Box::new(text(
            StatusCode::NOT_FOUND,
            "nothing is published at this address",
        ))),
        Err(e) => Err(Box::new(text(StatusCode::INTERNAL_SERVER_ERROR, e))),
    }
}

async fn handle(shared: Arc<Shared>, base: ProxyBase, name: &str, request: Request) -> Response {
    let published = match find_port(&shared, name).await {
        Ok(published) => published,
        Err(response) => return *response,
    };
    let target = Target {
        workspace_id: published.workspace_id,
        port: published.port,
    };
    if request.uri().path() == AUTH_PATH {
        return redeem(&shared, &base, name, target, request.uri()).await;
    }
    let (caller, strip_authorization) = match authenticate(&shared, name, request.headers()).await {
        Ok(Some(found)) => found,
        Ok(None) => return to_login(&shared, name, &request),
        Err(response) => return *response,
    };
    if let Via::Session(session) = caller.via
        && let Some(reason) = cross_site(request.method(), request.headers(), &base.origin(name))
    {
        warn!(target: "audit", via = "web", login = caller.login, session, workspace_id = target.workspace_id, port = target.port, method = %request.method(), "refused a proxy request from another site: {reason}");
        return text(
            StatusCode::FORBIDDEN,
            format!("refused a request from another site ({reason})"),
        );
    }
    let workspace = match authorize(&shared, &caller, target).await {
        Ok(workspace) => workspace,
        Err(response) => return *response,
    };
    let method = request.method().clone();
    let request = upstream_request(
        request,
        &base,
        name,
        (published.scheme, target.port),
        strip_authorization,
    );
    let response = match relay(&shared, &workspace, &published, request).await {
        Ok(response) => response,
        Err(error) => {
            warn!(login = caller.login, workspace = %workspace.display_name(), port = target.port, "proxy failed: {error:#}");
            text(
                StatusCode::BAD_GATEWAY,
                format!(
                    "nothing answered on port {} ({}) in the workspace {}: {error:#}",
                    target.port,
                    published.scheme.as_str(),
                    workspace.display_name()
                ),
            )
        }
    };
    let status = response.status().as_u16();
    let workspace = workspace.display_name();
    match caller.via {
        Via::Token(token) => {
            info!(target: "audit", via = "api", login = caller.login, token, workspace, port = target.port, %method, status, "proxied a request")
        }
        Via::Session(session) => {
            info!(target: "audit", via = "web", login = caller.login, session, workspace, port = target.port, %method, status, "proxied a request")
        }
    }
    response
}

/// トークンか、プロキシのホストの cookie を確かめる。どちらもなければ `None`。トークンで確かめたら、
/// `Authorization` を VM に渡さないよう `true` を添える。
async fn authenticate(
    shared: &Shared,
    name: &str,
    headers: &HeaderMap,
) -> Result<Option<(ProxyCaller, bool)>, Box<Response>> {
    let store = shared.controller.store();
    let internal =
        |e: chloe_controller::Error| Box::new(text(StatusCode::INTERNAL_SERVER_ERROR, e));
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim);
    // chloe のトークンでなければ、VM の中のアプリのものとみなしてそのまま渡す。
    if let Some(token) = bearer
        && let Some(found) = store
            .authenticate_api_token(token)
            .await
            .map_err(internal)?
    {
        let caller = ProxyCaller {
            login: found.login,
            via: Via::Token(found.id),
        };
        return Ok(Some((caller, true)));
    }
    let Some(session) = web::cookie_value(headers, PROXY_COOKIE).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    let found = store
        .authenticate_proxy_session(&session, name)
        .await
        .map_err(internal)?;
    Ok(found.map(|web| {
        let caller = ProxyCaller {
            login: web.login,
            via: Via::Session(web.id),
        };
        (caller, false)
    }))
}

/// cookie で来た要求が、ほかのサイトのページから送らせたものなら、その理由を返す。
///
/// プロキシのホストはどれも同じドメインの下にあり、ブラウザから見ると互いに同じサイトなので、
/// `SameSite=Lax` の cookie は、ほかのワークスペースの VM の中のページからの要求にも付く。
/// WebSocket には CORS がなく、ほかのページからでも中身を読めてしまう。そこで、
///
/// - ほかのオリジンからの要求 (`Sec-Fetch-Site` が `same-site` か `cross-site`) は、ページを開く
///   GET (`Sec-Fetch-Mode: navigate`) だけを通す。読み込み、`fetch`、WebSocket は断る。
/// - WebSocket の切り替えと、GET と HEAD のほかの要求は、`Origin` があればこのホストのものに限る
///   (`Sec-Fetch-*` を送らないブラウザのため)。
///
/// `Origin` も `Sec-Fetch-Site` もない要求は、ブラウザでないものとして通す。
fn cross_site(method: &Method, headers: &HeaderMap, own_origin: &str) -> Option<String> {
    let value = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let navigation = matches!(*method, Method::GET | Method::HEAD)
        && value("sec-fetch-mode") == Some("navigate")
        && !headers.contains_key(header::UPGRADE);
    if let Some(site @ ("same-site" | "cross-site")) = value("sec-fetch-site")
        && !navigation
    {
        return Some(format!("Sec-Fetch-Site: {site}"));
    }
    let unsafe_request =
        !matches!(*method, Method::GET | Method::HEAD) || headers.contains_key(header::UPGRADE);
    match value("origin") {
        Some(origin) if unsafe_request && origin != own_origin => Some(format!("Origin: {origin}")),
        _ => None,
    }
}

/// 利用者がまだログインでき、公開したポートのワークスペースに入れるかを確かめる。
async fn authorize(
    shared: &Shared,
    caller: &ProxyCaller,
    target: Target,
) -> Result<Workspace, Box<Response>> {
    let internal = |e: &dyn std::fmt::Display| {
        Box::new(text(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
    };
    match shared.is_allowed(&caller.login).await {
        Ok(true) => {}
        Ok(false) => {
            return Err(Box::new(text(
                StatusCode::FORBIDDEN,
                "the user can no longer log in",
            )));
        }
        Err(e) => return Err(internal(&format!("{e:#}"))),
    }
    member_workspace(shared, &caller.login, target.workspace_id)
        .await
        .map_err(|e| match e {
            Lookup::NotFound => Box::new(not_published()),
            Lookup::Internal(e) => internal(&e),
        })
}

fn not_published() -> Response {
    text(
        StatusCode::NOT_FOUND,
        "nothing is published at this address",
    )
}

enum Lookup {
    /// ワークスペースがないか、入れない (公開していないときと区別して伝えない)。
    NotFound,
    Internal(String),
}

async fn member_workspace(
    shared: &Shared,
    login: &str,
    workspace_id: i64,
) -> Result<Workspace, Lookup> {
    let internal = |e: &dyn std::fmt::Display| Lookup::Internal(format!("{e:#}"));
    let workspace = shared
        .controller
        .store()
        .get(workspace_id)
        .await
        .map_err(|e| internal(&e))?
        .ok_or(Lookup::NotFound)?;
    if !shared
        .can_access(login, &workspace.owner)
        .await
        .map_err(|e| internal(&e))?
    {
        return Err(Lookup::NotFound);
    }
    Ok(workspace)
}

/// `next` に使える、同じホストの中のパスか (`//host` や `/\host` のような、ほかのホストへ
/// 向かうものは断る)。
fn local_path(next: &str) -> Option<&str> {
    let valid = next.starts_with('/')
        && !next.starts_with("//")
        && !next.starts_with("/\\")
        && !next.chars().any(|c| c.is_control());
    valid.then_some(next)
}

/// ログインの要らない呼び出し元 (ブラウザでない) には 401 を、ブラウザのページの読み込みには
/// Web の画面でのログインへの案内を返す。
fn to_login(shared: &Shared, name: &str, request: &Request) -> Response {
    let browsing = matches!(*request.method(), Method::GET | Method::HEAD)
        && !request.headers().contains_key(header::UPGRADE);
    let Some(web) = WebConfig::of(shared).filter(|_| browsing) else {
        return text(
            StatusCode::UNAUTHORIZED,
            "log in through the chloe web UI, or pass a token as Authorization: Bearer <token>",
        );
    };
    let next = request
        .uri()
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/");
    Redirect::to(&web.url(&start_path(name, next))).into_response()
}

/// Web の画面でコードを作る経路 (`/api/auth/proxy?...`)。
fn start_path(name: &str, next: &str) -> String {
    let mut url = Url::parse("http://localhost/api/auth/proxy").expect("a valid URL");
    url.query_pairs_mut()
        .append_pair("name", name)
        .append_pair("next", next);
    format!("{}?{}", url.path(), url.query().unwrap_or_default())
}

#[derive(Deserialize)]
pub(crate) struct StartQuery {
    /// プロキシのホストの名前。
    name: String,
    #[serde(default)]
    next: Option<String>,
}

/// Web の画面のセッションで、プロキシのホストへ戻すコードを作る。ログインしていなければ、開こうと
/// したポートを cookie に覚えてログインの画面へ送り、ログインのあと [`resume`] で戻す。
pub(crate) async fn start(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    Query(query): Query<StartQuery>,
) -> Response {
    let (Some(web), Some(base)) = (WebConfig::of(&shared), shared.controller.ports().base()) else {
        return text(
            StatusCode::NOT_FOUND,
            "ports are not published on this server",
        );
    };
    let Some(name) = base.parse_host(&base.host(&query.name)) else {
        return not_published();
    };
    let next = query.next.as_deref().and_then(local_path).unwrap_or("/");
    let store = shared.controller.store();
    let session = match web::session_cookie(&headers) {
        Some(value) => match store.authenticate_web_session(&value).await {
            Ok(found) => found,
            Err(e) => return text(StatusCode::INTERNAL_SERVER_ERROR, e),
        },
        None => None,
    };
    let Some(session) = session else {
        let mut response = Redirect::to("/login").into_response();
        let remembered = start_path(&name, next);
        let value = remembered.trim_start_matches("/api/auth/proxy?");
        response.headers_mut().append(
            header::SET_COOKIE,
            web.cookie(RETURN_COOKIE, value, RETURN_COOKIE_PATH, RETURN_TTL_SECS),
        );
        return response;
    };
    let login = session.login;
    match shared.is_allowed(&login).await {
        Ok(true) => {}
        Ok(false) => return text(StatusCode::FORBIDDEN, "the user can no longer log in"),
        Err(e) => return text(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
    }
    let published = match find_port(&shared, &name).await {
        Ok(published) => published,
        Err(response) => return *response,
    };
    match member_workspace(&shared, &login, published.workspace_id).await {
        Ok(_) => {}
        Err(Lookup::NotFound) => return not_published(),
        Err(Lookup::Internal(e)) => return text(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
    let code = match store
        .create_proxy_code(session.id, &name, CODE_TTL_SECS)
        .await
    {
        Ok(code) => code,
        Err(e) => return text(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let mut url = Url::parse(&base.origin(&name))
        .expect("the proxy origin is a valid URL")
        .join(AUTH_PATH)
        .expect("a valid path");
    url.query_pairs_mut()
        .append_pair("code", &code)
        .append_pair("next", next);
    let mut response = Redirect::to(url.as_str()).into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        web.cookie(RETURN_COOKIE, "", RETURN_COOKIE_PATH, 0),
    );
    response
}

/// ログインのあとに開く。ログインの前に開こうとしたポートがあればそこへ、なければ画面へ送る。
pub(crate) async fn resume(headers: HeaderMap) -> Response {
    match web::cookie_value(&headers, RETURN_COOKIE).filter(|v| !v.is_empty()) {
        Some(query) if !query.contains(|c: char| c.is_control() || c == '#') => {
            Redirect::to(&format!("/api/auth/proxy?{query}")).into_response()
        }
        _ => Redirect::to("/").into_response(),
    }
}

#[derive(Deserialize)]
struct RedeemQuery {
    code: String,
    #[serde(default)]
    next: Option<String>,
}

/// プロキシのホストで、Web の画面が作ったコードをセッションに替える。
async fn redeem(
    shared: &Shared,
    base: &ProxyBase,
    name: &str,
    target: Target,
    uri: &Uri,
) -> Response {
    let Ok(Query(query)) = Query::<RedeemQuery>::try_from_uri(uri) else {
        return text(StatusCode::BAD_REQUEST, "the sign-in link is incomplete");
    };
    let store = shared.controller.store();
    let session = match store.redeem_proxy_code(&query.code, name).await {
        Ok(Some(session)) => session,
        Ok(None) => {
            return text(
                StatusCode::FORBIDDEN,
                "the sign-in link has expired or was already used; open the page again",
            );
        }
        Err(e) => return text(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    info!(target: "audit", via = "web", workspace_id = target.workspace_id, port = target.port, "proxy login");
    let next = query.next.as_deref().and_then(local_path).unwrap_or("/");
    let ttl = shared
        .config
        .api
        .as_ref()
        .map(|a| a.session_ttl_secs)
        .unwrap_or_default();
    let mut response = Redirect::to(next).into_response();
    response
        .headers_mut()
        .append(header::SET_COOKIE, proxy_cookie(base, &session, ttl));
    response
}

/// プロキシのホストだけに付く cookie (`Domain` を付けない)。
fn proxy_cookie(base: &ProxyBase, value: &str, max_age: i64) -> HeaderValue {
    let secure = if base.scheme() == "https" {
        "; Secure"
    } else {
        ""
    };
    HeaderValue::from_str(&format!(
        "{PROXY_COOKIE}={value}; Path=/; Max-Age={max_age}; HttpOnly; SameSite=Lax{secure}"
    ))
    .expect("cookie values are URL-safe")
}

/// `Cookie` から chloe の cookie を除く。残りがなければ `None`。
fn strip_cookies(headers: &HeaderMap) -> Option<HeaderValue> {
    let kept: Vec<&str> = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .map(str::trim)
        .filter(|pair| {
            let name = pair.split_once('=').map_or(*pair, |(name, _)| name);
            !pair.is_empty() && name != PROXY_COOKIE
        })
        .collect();
    (!kept.is_empty())
        .then(|| HeaderValue::from_str(&kept.join("; ")).ok())
        .flatten()
}

/// `Connection` で名指しされたヘッダーと、hop-by-hop のヘッダーを除く。
fn remove_hop_by_hop(headers: &mut HeaderMap) {
    let named: Vec<HeaderName> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|name| HeaderName::from_bytes(name.trim().as_bytes()).ok())
        .collect();
    for name in named {
        headers.remove(name);
    }
    for name in HOP_BY_HOP {
        headers.remove(*name);
    }
}

/// VM の中のサーバーへ送る要求に書き換える。
/// `upstream` は VM の中のサーバーとの話し方とポート。
fn upstream_request(
    request: Request,
    base: &ProxyBase,
    name: &str,
    upstream: (Scheme, u16),
    strip_authorization: bool,
) -> Request {
    let (scheme, port) = upstream;
    let (mut parts, body) = request.into_parts();
    let upgrade = parts.headers.get(header::UPGRADE).cloned();
    let cookies = strip_cookies(&parts.headers);
    remove_hop_by_hop(&mut parts.headers);
    let headers = &mut parts.headers;
    headers.remove(header::COOKIE);
    if let Some(cookies) = cookies {
        headers.insert(header::COOKIE, cookies);
    }
    if strip_authorization {
        headers.remove(header::AUTHORIZATION);
    }
    if let Some(upgrade) = upgrade {
        headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
        headers.insert(header::UPGRADE, upgrade);
    }
    // 開発用のサーバーは知らない `Host` を断るので、VM の中で開いたときと同じにする。
    let public_host = base.host(name);
    headers.insert(
        header::HOST,
        HeaderValue::from_str(&format!("{UPSTREAM_HOST}:{port}")).expect("a valid host"),
    );
    // そのページ自身からの要求 (HMR の WebSocket など) の `Origin` も、VM の中で開いたときと
    // 同じにする (Vite などは `Origin` も確かめる)。ほかのオリジンからの要求は、アプリが CORS や
    // CSRF を正しく判断できるよう、書き換えずに渡す。
    let own_origin = base.origin(name);
    if headers
        .get(header::ORIGIN)
        .is_some_and(|origin| origin.as_bytes() == own_origin.as_bytes())
    {
        let local = format!("{}://{UPSTREAM_HOST}:{port}", scheme.as_str());
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_str(&local).expect("a valid origin"),
        );
    }
    headers.insert(
        "x-forwarded-host",
        HeaderValue::from_str(&public_host).expect("a valid host"),
    );
    headers.insert(
        "x-forwarded-proto",
        HeaderValue::from_static(if base.scheme() == "https" {
            "https"
        } else {
            "http"
        }),
    );
    parts.uri = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/")
        .parse()
        .unwrap_or_else(|_| Uri::from_static("/"));
    parts.version = Version::HTTP_11;
    Request::from_parts(parts, body)
}

/// VM の中のサーバーからの応答を、ブラウザへ返す形に書き換える。
fn downstream_response(mut response: Response) -> Response {
    let switching = response.status() == StatusCode::SWITCHING_PROTOCOLS;
    let upgrade = response.headers().get(header::UPGRADE).cloned();
    let headers = response.headers_mut();
    remove_hop_by_hop(headers);
    if switching && let Some(upgrade) = upgrade {
        headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
        headers.insert(header::UPGRADE, upgrade);
    }
    // VM の中のサーバーが chloe の cookie を上書きしないようにする。
    let cookies: Vec<HeaderValue> = headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter(|v| {
            let name = v
                .to_str()
                .ok()
                .and_then(|v| v.split_once('='))
                .map(|(name, _)| name.trim());
            name != Some(PROXY_COOKIE)
        })
        .cloned()
        .collect();
    headers.remove(header::SET_COOKIE);
    for cookie in cookies {
        headers.append(header::SET_COOKIE, cookie);
    }
    response
}

/// 要求を VM の中のサーバーへ送り、応答を返す。応答の本文を流し終えるか、切り替えた接続が
/// 閉じるまで、ワークスペースを止めない。
async fn relay(
    shared: &Shared,
    workspace: &Workspace,
    published: &PublishedPort,
    mut request: Request,
) -> Result<Response> {
    let session = Arc::new(shared.controller.open(workspace).await?);
    let stream = session
        .connect_agent(AGENT_CONNECT_WAIT)
        .await
        .context("the workspace VM is not reachable")?;
    let mut conn = AgentConnection::new(stream);
    conn.send(&ClientMessage::Connect(ConnectRequest {
        version: PROTOCOL_VERSION,
        host: UPSTREAM_HOST.into(),
        port: published.port,
    }))
    .await?;
    match conn.recv::<AgentMessage>().await? {
        Some(AgentMessage::Connected) => {}
        Some(AgentMessage::Error(message)) => bail!("{message}"),
        _ => bail!("guest-agent did not accept the connection"),
    }
    let (io, buffered) = conn.into_raw();
    let io = Prefixed {
        prefix: buffered,
        inner: io,
    };
    let client_upgrade = request
        .headers()
        .contains_key(header::UPGRADE)
        .then(|| hyper::upgrade::on(&mut request));
    let mut response = match published.scheme {
        Scheme::Http => exchange(io, request, session.clone()).await?,
        Scheme::Https => {
            let tls = tokio_rustls::TlsConnector::from(insecure_tls_config())
                .connect(
                    rustls::pki_types::ServerName::try_from(UPSTREAM_HOST)
                        .expect("a valid server name"),
                    io,
                )
                .await
                .context("TLS handshake with the server")?;
            exchange(tls, request, session.clone()).await?
        }
    };
    if response.status() == StatusCode::SWITCHING_PROTOCOLS
        && let Some(client_upgrade) = client_upgrade
    {
        let upstream_upgrade = hyper::upgrade::on(&mut response);
        tokio::spawn(async move {
            let (Ok(client), Ok(upstream)) = tokio::join!(client_upgrade, upstream_upgrade) else {
                return;
            };
            let _ = tokio::io::copy_bidirectional(
                &mut TokioIo::new(client),
                &mut TokioIo::new(upstream),
            )
            .await;
            drop(session);
        });
    }
    Ok(downstream_response(response.map(Body::new)))
}

/// HTTP/1.1 で要求を一つ送る。接続は使い回さず、応答を流し終えたら閉じる。
async fn exchange<I>(
    io: I,
    request: Request,
    session: Arc<Session<Backend>>,
) -> Result<hyper::Response<hyper::body::Incoming>>
where
    I: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(io))
        .await
        .context("HTTP handshake with the server")?;
    tokio::spawn(async move {
        let _ = conn.with_upgrades().await;
        drop(session);
    });
    let response = sender
        .send_request(request)
        .await
        .context("the server did not answer")?;
    Ok(response)
}

/// VM の中の開発用のサーバーは自分で署名した証明書を使うことが多いので、証明書を確かめない。
fn insecure_tls_config() -> Arc<rustls::ClientConfig> {
    static CONFIG: std::sync::OnceLock<Arc<rustls::ClientConfig>> = std::sync::OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let provider = rustls::crypto::CryptoProvider::get_default()
                .cloned()
                .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()));
            let mut config = rustls::ClientConfig::builder_with_provider(provider.clone())
                .with_safe_default_protocol_versions()
                .expect("the default protocol versions")
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(AcceptAnyCertificate(provider)))
                .with_no_client_auth();
            config.alpn_protocols = vec![b"http/1.1".to_vec()];
            Arc::new(config)
        })
        .clone()
}

#[derive(Debug)]
struct AcceptAnyCertificate(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// guest-agent とのやり取りで読みすぎたバイト列を、接続から読むより先に返す。
struct Prefixed<S> {
    prefix: BytesMut,
    inner: S,
}

impl<S: AsyncRead + Unpin> AsyncRead for Prefixed<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if !self.prefix.is_empty() {
            let n = self.prefix.len().min(buf.remaining());
            let chunk = self.prefix.split_to(n);
            buf.put_slice(&chunk);
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Prefixed<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> ProxyBase {
        ProxyBase::parse("https://preview.example.com").unwrap()
    }

    const NAME: &str = "k3x7q2m5vbtd";

    fn request(pairs: &[(&'static str, &str)]) -> Request {
        let mut builder = Request::builder()
            .uri("https://k3x7q2m5vbtd.preview.example.com/src/main.ts?t=1")
            .version(Version::HTTP_2);
        for (name, value) in pairs {
            builder = builder.header(*name, *value);
        }
        builder.body(Body::empty()).unwrap()
    }

    #[test]
    fn rewrites_the_request_for_the_server() {
        let rewritten = upstream_request(
            request(&[
                ("cookie", "app=1; chloe_proxy=secret"),
                ("cookie", "theme=dark"),
                ("authorization", "Bearer token"),
                ("connection", "keep-alive, x-private"),
                ("x-private", "1"),
                ("te", "trailers"),
                ("accept", "text/html"),
            ]),
            &base(),
            NAME,
            (Scheme::Http, 5173),
            true,
        );
        assert_eq!(rewritten.uri(), "/src/main.ts?t=1");
        assert_eq!(rewritten.version(), Version::HTTP_11);
        let headers = rewritten.headers();
        assert_eq!(headers["host"], "localhost:5173");
        assert_eq!(
            headers["x-forwarded-host"],
            "k3x7q2m5vbtd.preview.example.com"
        );
        assert_eq!(headers["x-forwarded-proto"], "https");
        assert_eq!(headers["cookie"], "app=1; theme=dark");
        assert_eq!(headers["accept"], "text/html");
        for gone in ["authorization", "connection", "x-private", "te"] {
            assert!(!headers.contains_key(gone), "{gone}");
        }
    }

    #[test]
    fn keeps_the_app_authorization_and_upgrade() {
        let rewritten = upstream_request(
            request(&[
                ("cookie", "chloe_proxy=secret"),
                ("authorization", "Bearer app-token"),
                ("connection", "Upgrade"),
                ("upgrade", "websocket"),
                ("origin", "https://k3x7q2m5vbtd.preview.example.com"),
            ]),
            &base(),
            NAME,
            (Scheme::Https, 5173),
            false,
        );
        let headers = rewritten.headers();
        assert!(!headers.contains_key("cookie"));
        assert_eq!(headers["authorization"], "Bearer app-token");
        assert_eq!(headers["connection"], "upgrade");
        assert_eq!(headers["upgrade"], "websocket");
        // そのページ自身の `Origin` は、VM の中で開いたときと同じにする。
        assert_eq!(headers["origin"], "https://localhost:5173");
    }

    #[test]
    fn keeps_other_origins() {
        for origin in [
            "https://p4rz6wq2hnma.preview.example.com",
            "https://evil.example.com",
            "null",
        ] {
            let rewritten = upstream_request(
                request(&[("origin", origin)]),
                &base(),
                NAME,
                (Scheme::Http, 5173),
                false,
            );
            assert_eq!(rewritten.headers()["origin"], origin);
        }
        let rewritten = upstream_request(request(&[]), &base(), NAME, (Scheme::Http, 5173), false);
        assert!(!rewritten.headers().contains_key("origin"));
    }

    #[test]
    fn rewrites_the_response_for_the_browser() {
        let response = Response::builder()
            .header("transfer-encoding", "chunked")
            .header("set-cookie", "chloe_proxy=forged; Path=/")
            .header("set-cookie", "app=1; Path=/")
            .header("content-type", "text/html")
            .body(Body::empty())
            .unwrap();
        let response = downstream_response(response);
        let headers = response.headers();
        assert!(!headers.contains_key("transfer-encoding"));
        let cookies: Vec<_> = headers.get_all("set-cookie").iter().collect();
        assert_eq!(cookies, ["app=1; Path=/"]);
        assert_eq!(headers["content-type"], "text/html");

        let switching = Response::builder()
            .status(StatusCode::SWITCHING_PROTOCOLS)
            .header("connection", "Upgrade")
            .header("upgrade", "websocket")
            .body(Body::empty())
            .unwrap();
        let switching = downstream_response(switching);
        assert_eq!(switching.headers()["upgrade"], "websocket");
        assert_eq!(switching.headers()["connection"], "upgrade");
    }

    #[test]
    fn refuses_requests_from_other_sites() {
        let own = "https://k3x7q2m5vbtd.preview.example.com";
        let other = "https://p4rz6wq2hnma.preview.example.com";
        let check = |method: Method, pairs: &[(&'static str, &str)]| {
            let mut headers = HeaderMap::new();
            for (name, value) in pairs {
                headers.insert(*name, value.parse().unwrap());
            }
            cross_site(&method, &headers, own)
        };
        fn ws<'a>(origin: &'a str, site: &'a str) -> [(&'static str, &'a str); 4] {
            [
                ("upgrade", "websocket"),
                ("origin", origin),
                ("sec-fetch-site", site),
                ("sec-fetch-mode", "websocket"),
            ]
        }
        // 自分のページからの WebSocket、fetch、フォームは通す。
        assert_eq!(check(Method::GET, &ws(own, "same-origin")), None);
        assert_eq!(
            check(
                Method::POST,
                &[("origin", own), ("sec-fetch-site", "same-origin")]
            ),
            None
        );
        // ほかのプロキシのホストからは断る。
        assert!(check(Method::GET, &ws(other, "same-site")).is_some());
        assert!(
            check(
                Method::POST,
                &[("origin", other), ("sec-fetch-site", "same-site")]
            )
            .is_some()
        );
        assert!(
            check(
                Method::GET,
                &[
                    ("sec-fetch-site", "same-site"),
                    ("sec-fetch-mode", "no-cors")
                ]
            )
            .is_some()
        );
        assert!(
            check(
                Method::GET,
                &[("sec-fetch-site", "cross-site"), ("sec-fetch-mode", "cors")]
            )
            .is_some()
        );
        // Sec-Fetch-* を送らないブラウザでも、Origin で断る。
        assert!(check(Method::GET, &[("upgrade", "websocket"), ("origin", other)]).is_some());
        assert!(check(Method::DELETE, &[("origin", "null")]).is_some());
        // ほかのサイトのリンクからページを開くのは通す (Web の画面のリンクなど)。
        assert_eq!(
            check(
                Method::GET,
                &[
                    ("sec-fetch-site", "cross-site"),
                    ("sec-fetch-mode", "navigate")
                ]
            ),
            None
        );
        // ほかのサイトのフォームから送らせる POST は断る。
        assert!(
            check(
                Method::POST,
                &[
                    ("origin", other),
                    ("sec-fetch-site", "same-site"),
                    ("sec-fetch-mode", "navigate")
                ]
            )
            .is_some()
        );
        // ブラウザでないもの (Origin も Sec-Fetch-* もない) は通す。
        assert_eq!(check(Method::POST, &[]), None);
        assert_eq!(check(Method::GET, &[("upgrade", "websocket")]), None);
    }

    #[test]
    fn next_stays_on_the_host() {
        assert_eq!(local_path("/a?b=1"), Some("/a?b=1"));
        for bad in [
            "//evil.test/",
            "/\\evil.test",
            "https://evil.test/",
            "a",
            "/a\r\nb",
        ] {
            assert_eq!(local_path(bad), None, "{bad}");
        }
        assert_eq!(
            start_path(NAME, "/a?b=1&c"),
            "/api/auth/proxy?name=k3x7q2m5vbtd&next=%2Fa%3Fb%3D1%26c"
        );
    }

    #[test]
    fn proxy_cookies_are_host_only() {
        assert_eq!(
            proxy_cookie(&base(), "v", 60),
            "chloe_proxy=v; Path=/; Max-Age=60; HttpOnly; SameSite=Lax; Secure"
        );
        let http = ProxyBase::parse("http://preview.localhost:8080").unwrap();
        assert!(
            !proxy_cookie(&http, "v", 60)
                .to_str()
                .unwrap()
                .contains("Secure")
        );
    }

    #[tokio::test]
    async fn prefixed_reads_the_prefix_first() {
        use tokio::io::AsyncReadExt as _;
        let mut io = Prefixed {
            prefix: BytesMut::from(&b"ab"[..]),
            inner: &b"cd"[..],
        };
        let mut out = String::new();
        io.read_to_string(&mut out).await.unwrap();
        assert_eq!(out, "abcd");
    }
}
