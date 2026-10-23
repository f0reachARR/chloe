//! `chloe-node` から受ける内部 API (相互 TLS)。
//!
//! ```text
//! GET  /internal/v1/ca      CA の証明書 (参加の前の chloe-node が指紋で確かめる)
//! POST /internal/v1/join    参加用のトークンと署名要求で、chloe-node の証明書を発行する
//! POST /internal/v1/broker  ランナーのプロキシの本物の値の問い合わせ (chloe-node が中継する)
//! GET  /internal/v1/tunnel  chloe-node が張るトンネル (Upgrade: chloe-tunnel)。コントロール
//!                           プレーンは、この上で chloe-node の API を呼ぶ
//! ```
//!
//! 参加の前の `chloe-node` は証明書を持たないので、待ち受けはクライアントの証明書がなくても
//! 接続を受け、問い合わせはハンドラーで `chloe-node` の証明書を求める。

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chloe_controller::Controller;
use chloe_controller::store::Store;
use chloe_mtls::tunnel::{TUNNEL_PATH, TunnelEvent, Tunnels};
use chloe_mtls::{Ca, Peer, Role};
use serde::Deserialize;
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::{info, warn};

use crate::backend::Backend;

pub struct Internal {
    pub controller: Controller<Backend>,
    pub ca: Arc<Ca>,
    pub tunnels: Tunnels,
}

pub fn router(state: Arc<Internal>) -> Router {
    Router::new()
        .route("/internal/v1/ca", get(ca))
        .route("/internal/v1/join", post(join))
        .route("/internal/v1/broker", post(broker))
        .route(TUNNEL_PATH, get(tunnel))
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
    // ホストはサーバーにならない (トンネルを張る側) ので、証明書にホスト名は入れない。
    match state
        .ca
        .sign_request(&request.csr, &Role::Node(node.name.clone()), &[])
    {
        Ok(cert_pem) => {
            info!(target: "audit", node = node.name, "a node joined");
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

/// 登録して参加したホストのトンネルを受ける。同じホストが張り直したら、新しいトンネルを使う。
async fn tunnel(
    State(state): State<Arc<Internal>>,
    Extension(peer): Extension<Peer>,
    mut request: Request,
) -> Response {
    let Some(Role::Node(name)) = peer.0 else {
        return refuse(StatusCode::FORBIDDEN, "a node certificate is required");
    };
    let store = state.controller.store();
    if !matches!(store.node(&name).await, Ok(Some(node)) if node.joined) {
        warn!(target: "audit", node = name, "refused a tunnel from an unregistered node");
        return refuse(StatusCode::FORBIDDEN, "the node is not registered");
    }
    let (response, serving) = state.tunnels.accept(&name, &mut request);
    if let Some(serving) = serving {
        tokio::spawn(serving);
    }
    response
}

/// トンネルがつながったことと切れたことを、起きた順にデータベースに書く (`node list` で出す)。
pub async fn record_connections(store: Store, mut events: UnboundedReceiver<TunnelEvent>) {
    while let Some((name, connected)) = events.recv().await {
        if connected {
            info!(target: "audit", node = name, "a node connected");
        } else {
            info!(target: "audit", node = name, "a node disconnected");
        }
        if let Err(error) = store.set_node_connected(&name, connected).await {
            warn!(
                node = name,
                "failed to record the node's connection: {error}"
            );
        }
    }
}
