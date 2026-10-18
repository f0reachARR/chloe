//! コントロールプレーンから受ける API。相互 TLS で、コントロールプレーンの証明書の相手だけを
//! 受け付ける (同じ CA が発行したほかの `chloe-node` の証明書は断る)。
//!
//! ```text
//! GET    /v1/runners                    動いているランナーの識別子の一覧
//! POST   /v1/runners                    ランナーを起動する (本文は RunnerSpec)
//! GET    /v1/runners/{key}              {"running": bool}
//! PUT    /v1/runners/{key}              動いているランナーの方針を入れ替える (本文は RunnerSpec)
//! DELETE /v1/runners/{key}              ランナーを止める
//! DELETE /v1/runners/{key}/state        止めたランナーの状態 (ディスクを含む) を消す
//! GET    /v1/runners/{key}/requests     プロキシのログの末尾 (?lines=N)。止まっていても読める
//! GET    /v1/runners/{key}/agent        guest-agent への接続 (Upgrade: chloe-agent)
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, Query, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use axum::{Extension, Json, Router};
use chloe_controller::local::LocalBackend;
use chloe_controller::runner::{RunnerBackend, RunnerSpec};
use chloe_mtls::{Peer, Role};
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::relay;

/// guest-agent への接続の Upgrade で使うプロトコルの名前。
pub const AGENT_UPGRADE: &str = "chloe-agent";
/// guest-agent が答えるまで待つ時間の上限。
const MAX_AGENT_WAIT: Duration = Duration::from_secs(120);

pub struct NodeState {
    pub backend: LocalBackend,
    /// コントロールプレーンへの相互 TLS のクライアント。
    pub control_plane: reqwest::Client,
    pub control_plane_url: String,
    /// ランナーごとの問い合わせの中継。
    pub relays: Mutex<HashMap<String, JoinHandle<()>>>,
}

pub fn router(state: Arc<NodeState>) -> Router {
    Router::new()
        .route("/v1/runners", get(list).post(start))
        .route("/v1/runners/{key}", get(status).put(update).delete(stop))
        .route("/v1/runners/{key}/state", delete(remove_state))
        .route("/v1/runners/{key}/requests", get(requests))
        .route("/v1/runners/{key}/agent", get(agent))
        .layer(middleware::from_fn(only_control_plane))
        .with_state(state)
}

async fn only_control_plane(
    Extension(peer): Extension<Peer>,
    request: Request,
    next: Next,
) -> Response {
    if peer.0 != Some(Role::ControlPlane) {
        warn!(peer = ?peer.0, "refused a request from a non-control-plane peer");
        return error(
            StatusCode::FORBIDDEN,
            "only the control plane can use this API",
        );
    }
    next.run(request).await
}

fn error(status: StatusCode, message: impl std::fmt::Display) -> Response {
    (
        status,
        Json(serde_json::json!({ "error": message.to_string() })),
    )
        .into_response()
}

fn failed(error: impl std::fmt::Display) -> Response {
    self::error(StatusCode::INTERNAL_SERVER_ERROR, error)
}

/// ランナーの識別子は `ws-<数字>` だけを受け付ける (パスに使うので)。
fn valid_key(key: &str) -> bool {
    key.strip_prefix("ws-")
        .is_some_and(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()))
}

fn bad_key() -> Response {
    error(StatusCode::BAD_REQUEST, "invalid runner key")
}

async fn list(State(state): State<Arc<NodeState>>) -> Response {
    match state.backend.list().await {
        Ok(keys) => Json(keys).into_response(),
        Err(e) => failed(e),
    }
}

async fn start(State(state): State<Arc<NodeState>>, Json(spec): Json<RunnerSpec>) -> Response {
    if !valid_key(&spec.key) {
        return bad_key();
    }
    let key = spec.key.clone();
    // ランナーのプロキシは起動時に問い合わせ用ソケットがあるかを見るので、先に待ち受ける。
    if let Err(e) = relay::start(&state, &key) {
        return failed(e);
    }
    match state.backend.start(spec).await {
        Ok(()) => {
            info!(key, "runner started");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            relay::stop(&state, &key);
            failed(e)
        }
    }
}

async fn status(State(state): State<Arc<NodeState>>, Path(key): Path<String>) -> Response {
    if !valid_key(&key) {
        return bad_key();
    }
    match state.backend.is_running(&key).await {
        Ok(running) => Json(serde_json::json!({ "running": running })).into_response(),
        Err(e) => failed(e),
    }
}

async fn update(
    State(state): State<Arc<NodeState>>,
    Path(key): Path<String>,
    Json(spec): Json<RunnerSpec>,
) -> Response {
    if !valid_key(&key) {
        return bad_key();
    }
    if spec.key != key {
        return error(StatusCode::BAD_REQUEST, "the key does not match");
    }
    match state.backend.update(spec).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => failed(e),
    }
}

async fn stop(State(state): State<Arc<NodeState>>, Path(key): Path<String>) -> Response {
    if !valid_key(&key) {
        return bad_key();
    }
    let result = state.backend.stop(&key).await;
    relay::stop(&state, &key);
    match result {
        Ok(()) => {
            info!(key, "runner stopped");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => failed(e),
    }
}

async fn remove_state(State(state): State<Arc<NodeState>>, Path(key): Path<String>) -> Response {
    if !valid_key(&key) {
        return bad_key();
    }
    match state.backend.remove_state(&key).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => failed(e),
    }
}

#[derive(Deserialize)]
struct RequestsQuery {
    lines: Option<usize>,
}

async fn requests(
    State(state): State<Arc<NodeState>>,
    Path(key): Path<String>,
    Query(query): Query<RequestsQuery>,
) -> Response {
    if !valid_key(&key) {
        return bad_key();
    }
    let lines = query.lines.unwrap_or(chloe_audit::DEFAULT_LINES);
    match state.backend.tail_requests(&key, lines).await {
        Ok(records) => Json(records).into_response(),
        Err(e) => failed(e),
    }
}

#[derive(Deserialize)]
struct AgentQuery {
    /// VM の guest-agent が答えるまで待つミリ秒。
    #[serde(default)]
    wait_ms: u64,
}

/// guest-agent につないでから 101 を返し、Upgrade した接続とのあいだでバイト列を中継する。
async fn agent(
    State(state): State<Arc<NodeState>>,
    Path(key): Path<String>,
    Query(query): Query<AgentQuery>,
    mut request: Request,
) -> Response {
    if !valid_key(&key) {
        return bad_key();
    }
    let wants_upgrade = request
        .headers()
        .get(header::UPGRADE)
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(AGENT_UPGRADE.as_bytes()));
    if !wants_upgrade {
        return error(StatusCode::BAD_REQUEST, "expected Upgrade: chloe-agent");
    }
    let wait = Duration::from_millis(query.wait_ms).min(MAX_AGENT_WAIT);
    let mut agent = match state.backend.connect_agent(&key, wait).await {
        Ok(agent) => agent,
        Err(e) => return error(StatusCode::BAD_GATEWAY, e),
    };
    let upgrade = hyper::upgrade::on(&mut request);
    tokio::spawn(async move {
        match upgrade.await {
            Ok(upgraded) => {
                let mut upgraded = TokioIo::new(upgraded);
                let _ = tokio::io::copy_bidirectional(&mut upgraded, &mut agent).await;
            }
            Err(error) => warn!(key, "the agent upgrade failed: {error}"),
        }
    });
    Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(header::CONNECTION, "upgrade")
        .header(header::UPGRADE, AGENT_UPGRADE)
        .body(axum::body::Body::empty())
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}
