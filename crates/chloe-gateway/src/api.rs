//! HTTP(S) の管理 API。SSH の `chloe` コマンドと同じ操作 ([`crate::ops`]) を JSON で行う。
//!
//! `chloe token create` で作ったトークンを `Authorization: Bearer <token>` で渡す。
//! トークンの権限は、作った利用者の権限と同じ。利用者がログインできなくなったら
//! (Organization から抜けたなど)、トークンも使えない。
//!
//! Web の画面 ([`crate::web`]) からは、トークンの代わりにセッションの cookie で呼ぶ。cookie で
//! 来た GET 以外の要求は、`Origin` が公開 URL と一致しなければ断る (CSRF の対策)。cookie では
//! トークンを作らせず、シークレットも登録させない。
//!
//! ```text
//! GET    /api/v1/me
//! GET    /api/v1/events                     依頼の出来事 (SSE)
//! GET    /api/v1/workspaces[?status=true]   status=true なら VM に作業中かを尋ねる (busy)
//! POST   /api/v1/workspaces                 {"name": "acme/web", "root": "persistent", "workspace_disk_gib": 50, "vcpus": 8, "idle_timeout": "2h"}
//! GET    /api/v1/workspaces/{name}[?status=true]   name は SSH と同じ書き方 (acme/dev/tools など)
//! PATCH  /api/v1/workspaces/{name}          {"root": "ephemeral"}、{"docker_disk_gib": 80}、{"memory_mib": 16384}、
//!                                       {"idle_timeout": "never"} か {"running": false}
//! DELETE /api/v1/workspaces/{name}[?disks=true]   disks=true ならディスクも消す
//! GET    /api/v1/secrets[?owner=acme]       シークレットの名前の一覧
//! GET    /api/v1/secrets/{name}[?owner=..]  シークレットの定義 (値は返さない)
//! PUT    /api/v1/secrets/{name}[?owner=..]  {"value": "..."} か、定義と値
//! DELETE /api/v1/secrets/{name}[?owner=..]
//! GET    /api/v1/workspace-policies/{name}  ワークスペースの規則 (PUT、DELETE も同じ形)
//! GET    /api/v1/workspace-requests/{name}[?lines=200]   HTTP の要求のログの末尾 (古い順)
//! GET    /api/v1/policy                     個人の設定 (PUT、DELETE も /api/v1/policies と同じ)
//! GET    /api/v1/policies/{owner}           Organization か team の設定
//! PUT    /api/v1/policies/{owner}           設定 (JSON)
//! DELETE /api/v1/policies/{owner}
//! GET    /api/v1/tokens
//! POST   /api/v1/tokens                     {"name": "ci", "expires": "30d"}
//! DELETE /api/v1/tokens/{id}
//! GET    /api/v1/asks[?all=true]            VM の中のエージェントからの依頼 (all=true なら答えたものも)
//! GET    /api/v1/asks/{id}
//! POST   /api/v1/asks/{id}/answer           {"allow": true}、{"deny": true, "message": "..."} か {"answer": "..."}
//! ```

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, Request, State};
use axum::http::{Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Extension, Json, Router};
use chloe_controller::asks::AskEvent;
use chloe_controller::catalog::SecretInput;
use chloe_controller::runner::{PolicyLayer, WorkspacePolicy};
use chloe_controller::workspace::{DiskSizes, MachineSize, RootMode};
use chloe_secrets::Owner;
use futures::Stream;
use serde::Deserialize;
use tokio::sync::broadcast;
use tracing::info;

use crate::access::personal;
use crate::ops::{self, owner_arg, owner_or_personal, parse_owner};
use crate::session::Shared;
use crate::web;

/// トークンかセッションで確かめた呼び出し元。
#[derive(Clone)]
struct Caller {
    login: String,
    via: Via,
}

#[derive(Clone, Copy)]
enum Via {
    /// 管理 API のトークン (ID)。
    Token(i64),
    /// Web の画面のセッション (ID)。
    Session(i64),
}

impl Caller {
    /// Web の画面のセッションで来た呼び出しには、させない操作を断る。
    fn refuse_session(&self, what: &str) -> Result<(), ops::Error> {
        match self.via {
            Via::Token(_) => Ok(()),
            Via::Session(_) => Err(ops::Error::Forbidden(format!(
                "{what} is not allowed from the web UI; use `ssh ... chloe` or an API token"
            ))),
        }
    }
}

pub fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/api/v1/me", get(me))
        .route("/api/v1/events", get(events))
        .route(
            "/api/v1/workspaces",
            get(list_workspaces).post(create_workspace),
        )
        .route(
            "/api/v1/workspaces/{*name}",
            get(show_workspace)
                .patch(update_workspace)
                .delete(remove_workspace),
        )
        .route("/api/v1/secrets", get(list_secrets))
        .route(
            "/api/v1/secrets/{name}",
            get(show_secret).put(set_secret).delete(remove_secret),
        )
        .route(
            "/api/v1/workspace-policies/{*name}",
            get(show_workspace_policy)
                .put(set_workspace_policy)
                .delete(remove_workspace_policy),
        )
        .route(
            "/api/v1/workspace-requests/{*name}",
            get(workspace_requests),
        )
        .route(
            "/api/v1/policy",
            get(show_own_policy)
                .put(set_own_policy)
                .delete(remove_own_policy),
        )
        .route(
            "/api/v1/policies/{*owner}",
            get(show_policy).put(set_policy).delete(remove_policy),
        )
        .route("/api/v1/tokens", get(list_tokens).post(create_token))
        .route("/api/v1/tokens/{id}", delete(revoke_token))
        .route("/api/v1/asks", get(list_asks))
        .route("/api/v1/asks/{id}", get(show_ask))
        .route("/api/v1/asks/{id}/answer", post(answer_ask))
        .layer(middleware::from_fn_with_state(shared.clone(), authenticate))
        .with_state(shared)
}

pub(crate) fn error(status: StatusCode, message: impl std::fmt::Display) -> Response {
    (
        status,
        Json(serde_json::json!({ "error": message.to_string() })),
    )
        .into_response()
}

impl IntoResponse for ops::Error {
    fn into_response(self) -> Response {
        let status = match &self {
            ops::Error::Invalid(_) => StatusCode::BAD_REQUEST,
            ops::Error::Forbidden(_) => StatusCode::FORBIDDEN,
            ops::Error::NotFound(_) => StatusCode::NOT_FOUND,
            ops::Error::Conflict(_) => StatusCode::CONFLICT,
            ops::Error::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        error(status, self)
    }
}

/// トークンかセッションの cookie を確かめ、呼び出しを監査ログに残す (本文は残さない)。
async fn authenticate(
    State(shared): State<Arc<Shared>>,
    mut request: Request,
    next: Next,
) -> Response {
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim);
    let session = match token {
        Some(_) => None,
        None => web::session_cookie(request.headers()),
    };
    let (login, via) = if let Some(token) = token {
        match shared
            .controller
            .store()
            .authenticate_api_token(token)
            .await
        {
            Ok(Some(found)) => (found.login, Via::Token(found.id)),
            Ok(None) => {
                info!(target: "audit", via = "api", method = %request.method(), path = request.uri().path(), "rejected an API token");
                return error(
                    StatusCode::UNAUTHORIZED,
                    "unknown, expired or revoked token",
                );
            }
            Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e),
        }
    } else if let Some(session) = session
        && let Some(web) = web::WebConfig::of(&shared)
    {
        // cookie はブラウザが勝手に付けるので、ほかのサイトから送らせた要求を断る。
        let safe = matches!(*request.method(), Method::GET | Method::HEAD);
        if !safe && !web.same_origin(request.headers()) {
            return error(
                StatusCode::FORBIDDEN,
                "the request did not come from the web UI (Origin does not match)",
            );
        }
        match shared
            .controller
            .store()
            .authenticate_web_session(&session)
            .await
        {
            Ok(Some(found)) => (found.login, Via::Session(found.id)),
            Ok(None) => return error(StatusCode::UNAUTHORIZED, "the session has expired"),
            Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e),
        }
    } else {
        return error(
            StatusCode::UNAUTHORIZED,
            "pass a token as Authorization: Bearer <token>",
        );
    };
    match shared.is_allowed(&login).await {
        Ok(true) => {}
        Ok(false) => {
            return error(StatusCode::FORBIDDEN, "the user can no longer log in");
        }
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
    }
    let (method, path) = (request.method().clone(), request.uri().path().to_string());
    let caller = Caller { login, via };
    request.extensions_mut().insert(caller.clone());
    let response = next.run(request).await;
    let status = response.status().as_u16();
    match caller.via {
        Via::Token(token) => info!(
            target: "audit",
            via = "api",
            login = caller.login,
            token,
            %method,
            path,
            status,
            "management command"
        ),
        Via::Session(session) => info!(
            target: "audit",
            via = "web",
            login = caller.login,
            session,
            %method,
            path,
            status,
            "management command"
        ),
    }
    response
}

async fn me(Extension(caller): Extension<Caller>) -> Response {
    Json(match caller.via {
        Via::Token(id) => serde_json::json!({ "login": caller.login, "token": id }),
        Via::Session(id) => serde_json::json!({ "login": caller.login, "session": id }),
    })
    .into_response()
}

/// 依頼の出来事を流す長さ。これを過ぎたら閉じ、クライアントにつなぎ直させる。つなぎ直すときに
/// トークンやセッションを確かめ直すので、失効したものがいつまでも受け続けることはない。
const EVENTS_LIFETIME: Duration = Duration::from_secs(15 * 60);

/// 依頼が出た、答えた (取り下げた) という出来事を SSE で流す。呼び出し元が入れる
/// ワークスペースの出来事だけを送る。取りこぼしたら `lagged` を送るので、一覧を読み直す。
async fn events(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
) -> Response {
    let receiver = shared.controller.asks().subscribe();
    let deadline = tokio::time::Instant::now() + EVENTS_LIFETIME;
    let stream = ask_events(shared, caller.login, receiver, deadline);
    (
        // 手前のリバースプロキシ (nginx) に、溜めずに流させる。
        [("x-accel-buffering", "no")],
        Sse::new(stream).keep_alive(KeepAlive::default()),
    )
        .into_response()
}

fn ask_events(
    shared: Arc<Shared>,
    login: String,
    receiver: broadcast::Receiver<AskEvent>,
    deadline: tokio::time::Instant,
) -> impl Stream<Item = Result<Event, Infallible>> {
    futures::stream::unfold(receiver, move |mut receiver| {
        let (shared, login) = (shared.clone(), login.clone());
        async move {
            loop {
                let event = match tokio::time::timeout_at(deadline, receiver.recv()).await {
                    Ok(Ok(event)) => event,
                    Ok(Err(broadcast::error::RecvError::Lagged(_))) => {
                        return Some((Ok(Event::default().event("lagged").data("{}")), receiver));
                    }
                    Ok(Err(broadcast::error::RecvError::Closed)) | Err(_) => return None,
                };
                if let Some(data) = visible_event(&shared, &login, &event).await {
                    return Some((Ok(Event::default().event("ask").data(data)), receiver));
                }
            }
        }
    })
}

/// 利用者が入れるワークスペースの出来事なら、送る JSON を返す。
async fn visible_event(shared: &Shared, login: &str, event: &AskEvent) -> Option<String> {
    let (kind, workspace_id, id) = match *event {
        AskEvent::Created { workspace_id, id } => ("created", workspace_id, id),
        AskEvent::Resolved { workspace_id, id } => ("resolved", workspace_id, id),
    };
    let ws = shared
        .controller
        .store()
        .get(workspace_id)
        .await
        .ok()
        .flatten()?;
    if !shared.can_access(login, &ws.owner).await.unwrap_or(false) {
        return None;
    }
    Some(serde_json::json!({ "type": kind, "id": id, "workspace": ws.display_name() }).to_string())
}

#[derive(Deserialize)]
struct StatusQuery {
    /// 動いているワークスペースの VM に、作業中かを尋ねる。
    #[serde(default)]
    status: bool,
}

async fn list_workspaces(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
    Query(query): Query<StatusQuery>,
) -> Result<Response, ops::Error> {
    Ok(Json(ops::list_workspaces(&shared, &caller.login, query.status).await?).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateWorkspace {
    name: String,
    #[serde(default)]
    root: Option<RootMode>,
    #[serde(default)]
    workspace_disk_gib: Option<u32>,
    #[serde(default)]
    docker_disk_gib: Option<u32>,
    #[serde(default)]
    vcpus: Option<u32>,
    #[serde(default)]
    memory_mib: Option<u32>,
    /// `30m`、`2h`、`never`、`default` (`chloe ws idle` と同じ書き方)。
    #[serde(default)]
    idle_timeout: Option<String>,
}

async fn create_workspace(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
    Json(body): Json<CreateWorkspace>,
) -> Result<Response, ops::Error> {
    let options = ops::NewWorkspace {
        root: body.root,
        disks: DiskSizes {
            workspace_gib: body.workspace_disk_gib,
            docker_gib: body.docker_disk_gib,
        },
        machine: MachineSize {
            vcpus: body.vcpus,
            memory_mib: body.memory_mib,
        },
        idle_stop: match &body.idle_timeout {
            Some(text) => ops::parse_idle_stop(text)?,
            None => Default::default(),
        },
    };
    let ws = ops::create_workspace(&shared, &caller.login, &body.name, options).await?;
    Ok((StatusCode::CREATED, Json(ws)).into_response())
}

async fn show_workspace(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
    Path(name): Path<String>,
    Query(query): Query<StatusQuery>,
) -> Result<Response, ops::Error> {
    let info = ops::workspace_status(&shared, &caller.login, &name, query.status).await?;
    Ok(Json(info).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateWorkspace {
    #[serde(default)]
    root: Option<RootMode>,
    /// ディスクの大きさ (GiB)。大きくするだけで、次の起動から効く。
    #[serde(default)]
    workspace_disk_gib: Option<u32>,
    #[serde(default)]
    docker_disk_gib: Option<u32>,
    /// VM の大きさ。次の起動から効く。
    #[serde(default)]
    vcpus: Option<u32>,
    #[serde(default)]
    memory_mib: Option<u32>,
    /// 最後の接続が閉じてから止めるまで。`30m`、`2h`、`never`、`default`。
    #[serde(default)]
    idle_timeout: Option<String>,
    /// `false` で止める。起動は SSH でつないだときに行うので、`true` は受け付けない。
    #[serde(default)]
    running: Option<bool>,
}

async fn update_workspace(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
    Path(name): Path<String>,
    Json(body): Json<UpdateWorkspace>,
) -> Result<Response, ops::Error> {
    if body.running == Some(true) {
        return Err(ops::Error::Invalid(
            "a workspace starts when you connect to it; only \"running\": false is accepted".into(),
        ));
    }
    let disks = DiskSizes {
        workspace_gib: body.workspace_disk_gib,
        docker_gib: body.docker_disk_gib,
    };
    let machine = MachineSize {
        vcpus: body.vcpus,
        memory_mib: body.memory_mib,
    };
    // 書き方の誤りは、何かを変える前に返す。
    let idle_stop = body
        .idle_timeout
        .as_deref()
        .map(ops::parse_idle_stop)
        .transpose()?;
    if disks != DiskSizes::default() {
        ops::set_disks(&shared, &caller.login, &name, disks).await?;
    }
    if machine != MachineSize::default() {
        ops::set_machine(&shared, &caller.login, &name, machine).await?;
    }
    if let Some(idle_stop) = idle_stop {
        ops::set_idle_stop(&shared, &caller.login, &name, idle_stop).await?;
    }
    if let Some(root) = body.root {
        ops::set_root(&shared, &caller.login, &name, root).await?;
    }
    if body.running == Some(false) {
        ops::stop_workspace(&shared, &caller.login, &name).await?;
    }
    Ok(Json(ops::workspace_info(&shared, &caller.login, &name).await?).into_response())
}

#[derive(Deserialize)]
struct RemoveQuery {
    /// ランナーのホストにあるディスクも消す。
    #[serde(default)]
    disks: bool,
}

async fn remove_workspace(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
    Path(name): Path<String>,
    Query(query): Query<RemoveQuery>,
) -> Result<Response, ops::Error> {
    ops::remove_workspace(&shared, &caller.login, &name, query.disks).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
struct OwnerQuery {
    #[serde(default)]
    owner: Option<String>,
}

async fn list_secrets(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
    Query(query): Query<OwnerQuery>,
) -> Result<Response, ops::Error> {
    let owner = owner_or_personal(&caller.login, query.owner.as_deref())?;
    let names = ops::list_secrets(&shared, &caller.login, &owner).await?;
    Ok(Json(serde_json::json!({ "owner": owner_arg(&owner), "names": names })).into_response())
}

async fn show_secret(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
    Path(name): Path<String>,
    Query(query): Query<OwnerQuery>,
) -> Result<Response, ops::Error> {
    let owner = owner_or_personal(&caller.login, query.owner.as_deref())?;
    let view = ops::show_secret(&shared, &caller.login, &owner, &name).await?;
    Ok(Json(view).into_response())
}

async fn set_secret(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
    Path(name): Path<String>,
    Query(query): Query<OwnerQuery>,
    Json(body): Json<SecretInput>,
) -> Result<Response, ops::Error> {
    caller.refuse_session("setting a secret")?;
    let owner = owner_or_personal(&caller.login, query.owner.as_deref())?;
    ops::set_secret(&shared, &caller.login, &owner, &name, body).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn remove_secret(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
    Path(name): Path<String>,
    Query(query): Query<OwnerQuery>,
) -> Result<Response, ops::Error> {
    let owner = owner_or_personal(&caller.login, query.owner.as_deref())?;
    ops::remove_secret(&shared, &caller.login, &owner, &name).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn show_policy(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
    Path(owner): Path<String>,
) -> Result<Response, ops::Error> {
    respond_policy(&shared, &caller, parse_owner(&owner)?).await
}

async fn set_policy(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
    Path(owner): Path<String>,
    body: axum::body::Bytes,
) -> Result<Response, ops::Error> {
    save_policy(&shared, &caller, parse_owner(&owner)?, &body).await
}

async fn remove_policy(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
    Path(owner): Path<String>,
) -> Result<Response, ops::Error> {
    drop_policy(&shared, &caller, parse_owner(&owner)?).await
}

#[derive(Deserialize)]
struct RequestsQuery {
    lines: Option<usize>,
}

async fn workspace_requests(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
    Path(name): Path<String>,
    Query(query): Query<RequestsQuery>,
) -> Result<Response, ops::Error> {
    let records = ops::tail_requests(&shared, &caller.login, &name, query.lines).await?;
    Ok(Json(records).into_response())
}

#[derive(Deserialize)]
struct AsksQuery {
    #[serde(default)]
    all: bool,
}

async fn list_asks(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
    Query(query): Query<AsksQuery>,
) -> Result<Response, ops::Error> {
    let asks = ops::list_asks(&shared, &caller.login, query.all).await?;
    Ok(Json(asks).into_response())
}

async fn show_ask(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<i64>,
) -> Result<Response, ops::Error> {
    Ok(Json(ops::show_ask(&shared, &caller.login, id).await?).into_response())
}

/// 依頼への答え。`allow`、`deny`、`answer` のどれか一つを書く。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AnswerBody {
    #[serde(default)]
    allow: bool,
    #[serde(default)]
    deny: bool,
    /// 断るときにエージェントへ渡す言葉。
    message: Option<String>,
    answer: Option<String>,
}

async fn answer_ask(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<i64>,
    Json(body): Json<AnswerBody>,
) -> Result<Response, ops::Error> {
    let reply = match (body.allow, body.deny, body.answer) {
        (true, false, None) if body.message.is_none() => ops::Reply::Allow,
        (false, true, None) => ops::Reply::Deny(body.message),
        (false, false, Some(answer)) if body.message.is_none() => ops::Reply::Answer(answer),
        _ => {
            return Err(ops::Error::Invalid(
                "write one of allow, deny (with an optional message) or answer".into(),
            ));
        }
    };
    Ok(Json(ops::answer_ask(&shared, &caller.login, id, reply).await?).into_response())
}

async fn show_workspace_policy(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
    Path(name): Path<String>,
) -> Result<Response, ops::Error> {
    match ops::show_workspace_policy(&shared, &caller.login, &name).await? {
        (_, Some(policy)) => Ok(Json(policy).into_response()),
        (ws, None) => Err(ops::Error::NotFound(format!(
            "no rules for {}",
            ws.display_name()
        ))),
    }
}

async fn set_workspace_policy(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
    Path(name): Path<String>,
    body: axum::body::Bytes,
) -> Result<Response, ops::Error> {
    let ws = ops::find_workspace(&shared, &caller.login, &name).await?;
    // 権限のない利用者には、書き方の誤りより先に権限がないことを伝える。
    ops::check_workspace_policy_admin(&shared, &caller.login, &ws).await?;
    let policy: WorkspacePolicy = serde_json::from_slice(&body)
        .map_err(|e| ops::Error::Invalid(format!("invalid rules: {e}")))?;
    let updated = ops::set_workspace_policy(&shared, &caller.login, &ws, &policy).await?;
    Ok(Json(serde_json::json!({ "updated_runners": updated })).into_response())
}

async fn remove_workspace_policy(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
    Path(name): Path<String>,
) -> Result<Response, ops::Error> {
    let ws = ops::find_workspace(&shared, &caller.login, &name).await?;
    let updated = ops::remove_workspace_policy(&shared, &caller.login, &ws).await?;
    Ok(Json(serde_json::json!({ "updated_runners": updated })).into_response())
}

async fn show_own_policy(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
) -> Result<Response, ops::Error> {
    respond_policy(&shared, &caller, personal(&caller.login)).await
}

async fn set_own_policy(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
    body: axum::body::Bytes,
) -> Result<Response, ops::Error> {
    save_policy(&shared, &caller, personal(&caller.login), &body).await
}

async fn remove_own_policy(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
) -> Result<Response, ops::Error> {
    drop_policy(&shared, &caller, personal(&caller.login)).await
}

async fn respond_policy(
    shared: &Shared,
    caller: &Caller,
    owner: Owner,
) -> Result<Response, ops::Error> {
    match ops::show_policy(shared, &caller.login, &owner).await? {
        Some(layer) => Ok(Json(layer).into_response()),
        None => Err(ops::Error::NotFound(format!(
            "no settings for {}",
            owner_arg(&owner)
        ))),
    }
}

async fn save_policy(
    shared: &Shared,
    caller: &Caller,
    owner: Owner,
    body: &[u8],
) -> Result<Response, ops::Error> {
    // 権限のない利用者には、設定の書き方の誤りより先に権限がないことを伝える。
    ops::check_policy_admin(shared, &caller.login, &owner).await?;
    let layer: PolicyLayer = serde_json::from_slice(body)
        .map_err(|e| ops::Error::Invalid(format!("invalid settings: {e}")))?;
    let updated = ops::set_policy(shared, &caller.login, &owner, &layer).await?;
    Ok(Json(serde_json::json!({ "updated_runners": updated })).into_response())
}

async fn drop_policy(
    shared: &Shared,
    caller: &Caller,
    owner: Owner,
) -> Result<Response, ops::Error> {
    let updated = ops::remove_policy(shared, &caller.login, &owner).await?;
    Ok(Json(serde_json::json!({ "updated_runners": updated })).into_response())
}

async fn list_tokens(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
) -> Result<Response, ops::Error> {
    Ok(Json(ops::list_tokens(&shared, &caller.login).await?).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateToken {
    name: String,
    /// `30d` や `12h`。なければ期限なし。
    #[serde(default)]
    expires: Option<String>,
}

async fn create_token(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
    Json(body): Json<CreateToken>,
) -> Result<Response, ops::Error> {
    // 画面に XSS があっても、期限のあるセッションから期限のないトークンを作らせない。
    caller.refuse_session("creating an API token")?;
    let ttl = body
        .expires
        .as_deref()
        .map(ops::parse_duration)
        .transpose()?;
    let (id, token) = ops::create_token(&shared, &caller.login, &body.name, ttl).await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "id": id, "token": token })),
    )
        .into_response())
}

async fn revoke_token(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<i64>,
) -> Result<Response, ops::Error> {
    ops::revoke_token(&shared, &caller.login, id).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
