//! 複数のランナーのホスト (`chloe-node`) にランナーを置く実装。
//!
//! ワークスペースは、最初に起動したホストに結びつける (ディスクがそのホストにあるので)。
//! まだ置いていないワークスペースは、置いたワークスペースが少ないホストに置く。
//! ホストへの頼みは、ホストが張ったトンネルの上の HTTP/1.1 で送る (`chloe-node` の API)。
//! コントロールプレーンからホストへは接続を始めない ([`chloe_mtls::tunnel`])。
//!
//! つながらないホスト (トンネルのないホスト) のランナーは、止まったとはみなさない。止まったと
//! みなすと、reconcile がそのホストで動いているランナーの把握と記録を消してしまうので、記録の
//! とおり動いているものとして扱う。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use bytes::Bytes;
use chloe_mtls::tunnel::Tunnels;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode, header};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tracing::warn;

use crate::Error;
use crate::runner::{AgentStream, RunnerBackend, RunnerSpec};
use crate::store::{Node, Store};

/// guest-agent への接続の Upgrade で使うプロトコルの名前 (`chloe-node` と同じ)。
const AGENT_UPGRADE: &str = "chloe-agent";

pub struct ClusterBackend {
    store: Store,
    /// ホストが張ったトンネル。
    tunnels: Tunnels,
    /// 最後の一覧で、ランナーを返したホスト。置き場所の記録がないランナー (持ち主のない
    /// ランナー) を止めるときに使う。
    seen: Mutex<HashMap<String, String>>,
}

#[derive(serde::Deserialize)]
struct Status {
    running: bool,
}

impl ClusterBackend {
    pub fn new(store: Store, tunnels: Tunnels) -> Self {
        Self {
            store,
            tunnels,
            seen: Mutex::new(HashMap::new()),
        }
    }

    fn workspace_id(key: &str) -> Result<i64, Error> {
        key.strip_prefix("ws-")
            .and_then(|id| id.parse().ok())
            .ok_or_else(|| Error::Invalid(format!("invalid runner key: {key}")))
    }

    /// ワークスペースを置いたホスト。置いていなければ `None`。
    async fn placed(&self, key: &str) -> Result<Option<Node>, Error> {
        let Some(name) = self.store.workspace_node(Self::workspace_id(key)?).await? else {
            return Ok(None);
        };
        match self.store.node(&name).await? {
            Some(node) => Ok(Some(node)),
            None => Err(Error::Runner(format!(
                "{key} is placed on {name}, which is no longer registered"
            ))),
        }
    }

    async fn placed_or_error(&self, key: &str) -> Result<Node, Error> {
        self.placed(key)
            .await?
            .ok_or_else(|| Error::Runner(format!("{key} is not placed on any node")))
    }

    /// まだ置いていないワークスペースの置き場所を決めて記録する。参加したホストのうち、
    /// 置いたワークスペースがもっとも少ないもの (同じなら名前の順で先のもの)。
    async fn place(&self, key: &str) -> Result<Node, Error> {
        if let Some(node) = self.placed(key).await? {
            return Ok(node);
        }
        let counts = self.store.placement_counts().await?;
        let node = self
            .store
            .nodes()
            .await?
            .into_iter()
            .filter(|n| n.joined)
            .min_by_key(|n| (counts.get(&n.name).copied().unwrap_or(0), n.name.clone()))
            .ok_or_else(|| Error::Runner("no node has joined the control plane".into()))?;
        self.store
            .set_workspace_node(Self::workspace_id(key)?, &node.name)
            .await?;
        Ok(node)
    }

    /// トンネルで要求を送る。トンネルがないか、途中で切れたら [`Error::Unreachable`]。
    async fn send(
        &self,
        node: &Node,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<Incoming>, Error> {
        self.tunnels
            .request(&node.name, request)
            .await
            .map_err(|e| Error::Unreachable(format!("{}: {e}", node.name)))
    }

    /// 要求を送り、成功した応答の本文を返す。
    async fn call(&self, node: &Node, request: Request<Full<Bytes>>) -> Result<Bytes, Error> {
        let response = self.send(node, request).await?;
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .map_err(|e| Error::Unreachable(format!("{}: {e}", node.name)))?
            .to_bytes();
        if status.is_success() {
            return Ok(body);
        }
        Err(Error::Runner(format!(
            "{}: {status}: {}",
            node.name,
            String::from_utf8_lossy(&body)
                .chars()
                .take(300)
                .collect::<String>()
        )))
    }

    async fn call_json<T: DeserializeOwned>(
        &self,
        node: &Node,
        request: Request<Full<Bytes>>,
    ) -> Result<T, Error> {
        let body = self.call(node, request).await?;
        serde_json::from_slice(&body).map_err(|e| Error::Runner(format!("{}: {e}", node.name)))
    }

    /// つながらないホストのランナーは、記録のとおり動いているものとして扱う。
    async fn recorded_running(&self, node: &Node, key: &str) -> Result<bool, Error> {
        let id = Self::workspace_id(key)?;
        Ok(self
            .store
            .recorded_runners_on(&node.name)
            .await?
            .contains(&id))
    }
}

fn request(method: Method, path: &str) -> Request<Full<Bytes>> {
    with_body(method, path, Full::default())
}

fn with_json(
    method: Method,
    path: &str,
    body: &impl Serialize,
) -> Result<Request<Full<Bytes>>, Error> {
    let body = serde_json::to_vec(body).map_err(|e| Error::Runner(e.to_string()))?;
    let mut request = with_body(method, path, Full::new(body.into()));
    request
        .headers_mut()
        .insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
    Ok(request)
}

fn with_body(method: Method, path: &str, body: Full<Bytes>) -> Request<Full<Bytes>> {
    let mut request = Request::new(body);
    *request.method_mut() = method;
    // パスはランナーの識別子 (ws-<番号>) と数だけで組むので、URI として読めないことはない。
    *request.uri_mut() = path.parse().unwrap_or_default();
    request
}

impl RunnerBackend for ClusterBackend {
    async fn start(&self, spec: RunnerSpec) -> Result<(), Error> {
        let node = self.place(&spec.key).await?;
        self.call(&node, with_json(Method::POST, "/v1/runners", &spec)?)
            .await?;
        Ok(())
    }

    async fn stop(&self, key: &str) -> Result<(), Error> {
        let node = match self.placed(key).await? {
            Some(node) => node,
            // ワークスペースのないランナーは、一覧で返したホストで止める。
            None => {
                let seen = self.seen.lock().unwrap().get(key).cloned();
                match seen {
                    Some(name) => match self.store.node(&name).await? {
                        Some(node) => node,
                        None => return Ok(()),
                    },
                    None => return Ok(()),
                }
            }
        };
        self.call(
            &node,
            request(Method::DELETE, &format!("/v1/runners/{key}")),
        )
        .await?;
        Ok(())
    }

    async fn is_running(&self, key: &str) -> Result<bool, Error> {
        let Some(node) = self.placed(key).await? else {
            return Ok(false);
        };
        let status = self
            .call_json::<Status>(&node, request(Method::GET, &format!("/v1/runners/{key}")))
            .await;
        match status {
            Ok(status) => Ok(status.running),
            Err(Error::Unreachable(message)) => {
                warn!(key, "cannot tell whether the runner is running: {message}");
                self.recorded_running(&node, key).await
            }
            Err(error) => Err(error),
        }
    }

    async fn remove_state(&self, key: &str) -> Result<(), Error> {
        // 置いていないワークスペースには、状態もない。
        let Some(node) = self.placed(key).await? else {
            return Ok(());
        };
        let path = format!("/v1/runners/{key}/state");
        self.call(&node, request(Method::DELETE, &path)).await?;
        Ok(())
    }

    async fn update(&self, spec: RunnerSpec) -> Result<(), Error> {
        let node = self.placed_or_error(&spec.key).await?;
        let path = format!("/v1/runners/{}", spec.key);
        self.call(&node, with_json(Method::PUT, &path, &spec)?)
            .await?;
        Ok(())
    }

    async fn connect_agent(&self, key: &str, wait: Duration) -> Result<AgentStream, Error> {
        let node = self.placed_or_error(key).await?;
        let mut request = request(
            Method::GET,
            &format!("/v1/runners/{key}/agent?wait_ms={}", wait.as_millis()),
        );
        let headers = request.headers_mut();
        headers.insert(header::CONNECTION, "upgrade".parse().unwrap());
        headers.insert(header::UPGRADE, AGENT_UPGRADE.parse().unwrap());
        // VM の起動を待つ時間に、余裕を足す。
        let response =
            tokio::time::timeout(wait + Duration::from_secs(10), self.send(&node, request))
                .await
                .map_err(|_| {
                    Error::Unreachable(format!("{}: the agent connection timed out", node.name))
                })??;
        if response.status() != StatusCode::SWITCHING_PROTOCOLS {
            let status = response.status();
            let body = response
                .into_body()
                .collect()
                .await
                .map(|b| String::from_utf8_lossy(&b.to_bytes()).trim().to_string())
                .unwrap_or_default();
            return Err(Error::Runner(format!(
                "{}: the agent connection failed: {status}: {body}",
                node.name,
            )));
        }
        let upgraded = hyper::upgrade::on(response)
            .await
            .map_err(|e| Error::Unreachable(format!("{}: {e}", node.name)))?;
        Ok(Box::new(hyper_util::rt::TokioIo::new(upgraded)))
    }

    async fn tail_requests(
        &self,
        key: &str,
        lines: usize,
    ) -> Result<Vec<chloe_audit::Record>, Error> {
        // 一度も起動していないワークスペースには、ログがない。
        let Some(node) = self.placed(key).await? else {
            return Ok(Vec::new());
        };
        let path = format!("/v1/runners/{key}/requests?lines={lines}");
        self.call_json(&node, request(Method::GET, &path)).await
    }

    fn local_broker_socket(&self, _key: &str) -> Option<PathBuf> {
        // ホストの chloe-node が待ち受け、コントロールプレーンの内部 API に中継する。
        None
    }

    async fn list(&self) -> Result<Vec<String>, Error> {
        let mut keys = Vec::new();
        for node in self.store.nodes().await? {
            if !node.joined {
                continue;
            }
            let listed = self
                .call_json::<Vec<String>>(&node, request(Method::GET, "/v1/runners"))
                .await;
            match listed {
                Ok(listed) => {
                    let mut seen = self.seen.lock().unwrap();
                    for key in &listed {
                        seen.insert(key.clone(), node.name.clone());
                    }
                    keys.extend(listed);
                }
                Err(Error::Unreachable(message)) => {
                    // 空とみなすと、reconcile がそのホストのランナーを片づけてしまう。
                    warn!(
                        node = node.name,
                        "the node is not reachable; keeping its runners: {message}"
                    );
                    keys.extend(
                        self.store
                            .recorded_runners_on(&node.name)
                            .await?
                            .into_iter()
                            .map(|id| format!("ws-{id}")),
                    );
                }
                Err(error) => return Err(error),
            }
        }
        Ok(keys)
    }

    async fn node_of(&self, key: &str) -> Result<String, Error> {
        Ok(self.placed_or_error(key).await?.name)
    }
}
