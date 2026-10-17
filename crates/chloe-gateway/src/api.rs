//! HTTP(S) の管理 API。SSH の `chloe` コマンドと同じ操作 ([`crate::ops`]) を JSON で行う。
//!
//! `chloe token create` で作ったトークンを `Authorization: Bearer <token>` で渡す。
//! トークンの権限は、作った利用者の権限と同じ。利用者がログインできなくなったら
//! (Organization から抜けたなど)、トークンも使えない。
//!
//! ```text
//! GET    /v1/me
//! GET    /v1/workspaces
//! POST   /v1/workspaces                 {"name": "acme/web", "root": "persistent", "workspace_disk_gib": 50}
//! GET    /v1/workspaces/{name}          name は SSH と同じ書き方 (acme/dev/tools など)
//! PATCH  /v1/workspaces/{name}          {"root": "ephemeral"}、{"docker_disk_gib": 80} か {"running": false}
//! DELETE /v1/workspaces/{name}[?disks=true]   disks=true ならディスクも消す
//! GET    /v1/secrets[?owner=acme]       シークレットの名前の一覧
//! GET    /v1/secrets/{name}[?owner=..]  シークレットの定義 (値は返さない)
//! PUT    /v1/secrets/{name}[?owner=..]  {"value": "..."} か、定義と値
//! DELETE /v1/secrets/{name}[?owner=..]
//! GET    /v1/policies/{owner}           Organization か team の設定
//! PUT    /v1/policies/{owner}           設定 (JSON)
//! DELETE /v1/policies/{owner}
//! GET    /v1/tokens
//! POST   /v1/tokens                     {"name": "ci", "expires": "30d"}
//! DELETE /v1/tokens/{id}
//! ```

use std::sync::Arc;

use axum::extract::{Path, Query, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use axum::{Extension, Json, Router};
use chloe_controller::catalog::SecretInput;
use chloe_controller::runner::PolicyLayer;
use chloe_controller::workspace::{DiskSizes, RootMode};
use serde::Deserialize;
use tracing::info;

use crate::ops::{self, owner_arg, parse_owner, secret_owner};
use crate::session::Shared;

/// トークンで確かめた呼び出し元。
#[derive(Clone)]
struct Caller {
    login: String,
    token_id: i64,
}

pub fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/v1/me", get(me))
        .route(
            "/v1/workspaces",
            get(list_workspaces).post(create_workspace),
        )
        .route(
            "/v1/workspaces/{*name}",
            get(show_workspace)
                .patch(update_workspace)
                .delete(remove_workspace),
        )
        .route("/v1/secrets", get(list_secrets))
        .route(
            "/v1/secrets/{name}",
            get(show_secret).put(set_secret).delete(remove_secret),
        )
        .route(
            "/v1/policies/{*owner}",
            get(show_policy).put(set_policy).delete(remove_policy),
        )
        .route("/v1/tokens", get(list_tokens).post(create_token))
        .route("/v1/tokens/{id}", delete(revoke_token))
        .layer(middleware::from_fn_with_state(shared.clone(), authenticate))
        .with_state(shared)
}

fn error(status: StatusCode, message: impl std::fmt::Display) -> Response {
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

/// トークンを確かめ、呼び出しを監査ログに残す (本文は残さない)。
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
    let Some(token) = token else {
        return error(
            StatusCode::UNAUTHORIZED,
            "pass a token as Authorization: Bearer <token>",
        );
    };
    let found = match shared
        .controller
        .store()
        .authenticate_api_token(token)
        .await
    {
        Ok(found) => found,
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let Some(found) = found else {
        info!(target: "audit", via = "api", method = %request.method(), path = request.uri().path(), "rejected an API token");
        return error(
            StatusCode::UNAUTHORIZED,
            "unknown, expired or revoked token",
        );
    };
    match shared.is_allowed(&found.login).await {
        Ok(true) => {}
        Ok(false) => {
            return error(
                StatusCode::FORBIDDEN,
                "the token's user can no longer log in",
            );
        }
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
    }
    let (method, path) = (request.method().clone(), request.uri().path().to_string());
    let caller = Caller {
        login: found.login,
        token_id: found.id,
    };
    request.extensions_mut().insert(caller.clone());
    let response = next.run(request).await;
    info!(
        target: "audit",
        via = "api",
        login = caller.login,
        token = caller.token_id,
        %method,
        path,
        status = response.status().as_u16(),
        "management command"
    );
    response
}

async fn me(Extension(caller): Extension<Caller>) -> Response {
    Json(serde_json::json!({ "login": caller.login, "token": caller.token_id })).into_response()
}

async fn list_workspaces(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
) -> Result<Response, ops::Error> {
    Ok(Json(ops::list_workspaces(&shared, &caller.login).await?).into_response())
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
}

async fn create_workspace(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
    Json(body): Json<CreateWorkspace>,
) -> Result<Response, ops::Error> {
    let disks = DiskSizes {
        workspace_gib: body.workspace_disk_gib,
        docker_gib: body.docker_disk_gib,
    };
    let ws = ops::create_workspace(&shared, &caller.login, &body.name, body.root, disks).await?;
    Ok((StatusCode::CREATED, Json(ws)).into_response())
}

async fn show_workspace(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
    Path(name): Path<String>,
) -> Result<Response, ops::Error> {
    Ok(Json(ops::workspace_info(&shared, &caller.login, &name).await?).into_response())
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
    if disks != DiskSizes::default() {
        ops::set_disks(&shared, &caller.login, &name, disks).await?;
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
    let owner = secret_owner(&caller.login, query.owner.as_deref())?;
    let names = ops::list_secrets(&shared, &caller.login, &owner).await?;
    Ok(Json(serde_json::json!({ "owner": owner_arg(&owner), "names": names })).into_response())
}

async fn show_secret(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
    Path(name): Path<String>,
    Query(query): Query<OwnerQuery>,
) -> Result<Response, ops::Error> {
    let owner = secret_owner(&caller.login, query.owner.as_deref())?;
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
    let owner = secret_owner(&caller.login, query.owner.as_deref())?;
    ops::set_secret(&shared, &caller.login, &owner, &name, body).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn remove_secret(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
    Path(name): Path<String>,
    Query(query): Query<OwnerQuery>,
) -> Result<Response, ops::Error> {
    let owner = secret_owner(&caller.login, query.owner.as_deref())?;
    ops::remove_secret(&shared, &caller.login, &owner, &name).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn show_policy(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
    Path(owner): Path<String>,
) -> Result<Response, ops::Error> {
    let owner = parse_owner(&owner)?;
    match ops::show_policy(&shared, &caller.login, &owner).await? {
        Some(layer) => Ok(Json(layer).into_response()),
        None => Err(ops::Error::NotFound(format!(
            "no settings for {}",
            owner_arg(&owner)
        ))),
    }
}

async fn set_policy(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
    Path(owner): Path<String>,
    body: axum::body::Bytes,
) -> Result<Response, ops::Error> {
    let owner = parse_owner(&owner)?;
    // 権限のない利用者には、設定の書き方の誤りより先に権限がないことを伝える。
    ops::check_policy_admin(&shared, &caller.login, &owner).await?;
    let layer: PolicyLayer = serde_json::from_slice(&body)
        .map_err(|e| ops::Error::Invalid(format!("invalid settings: {e}")))?;
    let updated = ops::set_policy(&shared, &caller.login, &owner, &layer).await?;
    Ok(Json(serde_json::json!({ "updated_runners": updated })).into_response())
}

async fn remove_policy(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
    Path(owner): Path<String>,
) -> Result<Response, ops::Error> {
    let owner = parse_owner(&owner)?;
    let updated = ops::remove_policy(&shared, &caller.login, &owner).await?;
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
