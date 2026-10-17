//! `chloe-node` から受ける内部 API (相互 TLS)。
//!
//! ```text
//! GET  /internal/v1/ca      CA の証明書 (参加の前の chloe-node が指紋で確かめる)
//! POST /internal/v1/join    参加用のトークンと署名要求で、chloe-node の証明書を発行する
//! POST /internal/v1/broker  ランナーのプロキシの本物の値の問い合わせ (chloe-node が中継する)
//! ```
//!
//! 参加の前の `chloe-node` は証明書を持たないので、待ち受けはクライアントの証明書がなくても
//! 接続を受け、問い合わせはハンドラーで `chloe-node` の証明書を求める。

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chloe_controller::Controller;
use chloe_mtls::{Ca, Peer, Role};
use serde::Deserialize;
use tracing::{info, warn};

use crate::backend::Backend;

pub struct Internal {
    pub controller: Controller<Backend>,
    pub ca: Arc<Ca>,
}

pub fn router(state: Arc<Internal>) -> Router {
    Router::new()
        .route("/internal/v1/ca", get(ca))
        .route("/internal/v1/join", post(join))
        .route("/internal/v1/broker", post(broker))
        .with_state(state)
}

fn refuse(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

async fn ca(State(state): State<Arc<Internal>>) -> String {
    state.ca.cert_pem().to_string()
}

#[derive(Deserialize)]
struct JoinRequest {
    token: String,
    csr: String,
}

async fn join(State(state): State<Arc<Internal>>, Json(request): Json<JoinRequest>) -> Response {
    let store = state.controller.store();
    let node = match store.consume_join(&request.token).await {
        Ok(Some(node)) => node,
        Ok(None) => {
            warn!(target: "audit", "refused to join with an unknown or expired token");
            return refuse(StatusCode::FORBIDDEN, "unknown or expired join token");
        }
        Err(error) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    };
    // コントロールプレーンは node add で登録した URL のホスト名でつなぐので、その名前で発行する。
    let host = match reqwest::Url::parse(&node.url).ok().and_then(|url| {
        url.host_str()
            .map(|h| h.trim_matches(['[', ']']).to_string())
    }) {
        Some(host) => host,
        None => return refuse(StatusCode::BAD_REQUEST, "the node URL has no host"),
    };
    match state
        .ca
        .sign_request(&request.csr, &Role::Node(node.name.clone()), &[host])
    {
        Ok(cert_pem) => {
            info!(target: "audit", node = node.name, url = node.url, "a node joined");
            Json(serde_json::json!({ "name": node.name, "cert_pem": cert_pem })).into_response()
        }
        Err(error) => refuse(StatusCode::BAD_REQUEST, &error.to_string()),
    }
}

#[derive(Deserialize)]
struct BrokerRequest {
    key: String,
    question: String,
}

/// 中継してきた `chloe-node` に置いたランナーの問い合わせにだけ答える。
async fn broker(
    State(state): State<Arc<Internal>>,
    Extension(peer): Extension<Peer>,
    Json(request): Json<BrokerRequest>,
) -> Response {
    let Some(Role::Node(name)) = peer.0 else {
        return refuse(StatusCode::FORBIDDEN, "a node certificate is required");
    };
    let store = state.controller.store();
    let placed = match request
        .key
        .strip_prefix("ws-")
        .and_then(|id| id.parse::<i64>().ok())
    {
        Some(id) => store.workspace_node(id).await.ok().flatten(),
        None => None,
    };
    let registered = matches!(store.node(&name).await, Ok(Some(node)) if node.joined);
    if !registered || placed.as_deref() != Some(name.as_str()) {
        warn!(
            target: "audit",
            node = name,
            key = request.key,
            "refused a secret question for a runner on another node"
        );
        return refuse(
            StatusCode::FORBIDDEN,
            "the runner is not placed on this node",
        );
    }
    state
        .controller
        .answer_secret(&request.key, &request.question)
        .await
        .into_response()
}
