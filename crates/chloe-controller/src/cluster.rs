//! 複数のランナーのホスト (`chloe-node`) にランナーを置く実装。
//!
//! ワークスペースは、最初に起動したホストに結びつける (ディスクがそのホストにあるので)。
//! まだ置いていないワークスペースは、置いたワークスペースが少ないホストに置く。
//! ホストへの頼みは相互 TLS の HTTPS で送る (`chloe-node` の API)。
//!
//! つながらないホストのランナーは、止まったとはみなさない。止まったとみなすと、reconcile が
//! そのホストで動いているランナーの把握と記録を消してしまうので、記録のとおり動いているものと
//! して扱う。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use reqwest::StatusCode;
use serde::Deserialize;
use tracing::warn;

use crate::Error;
use crate::runner::{AgentStream, RunnerBackend, RunnerSpec};
use crate::store::{Node, Store};

/// guest-agent への接続の Upgrade で使うプロトコルの名前 (`chloe-node` と同じ)。
const AGENT_UPGRADE: &str = "chloe-agent";

pub struct ClusterBackend {
    store: Store,
    /// `chloe-node` への相互 TLS のクライアント (コントロールプレーンの証明書)。
    client: reqwest::Client,
    /// 最後の一覧で、ランナーを返したホスト。置き場所の記録がないランナー (持ち主のない
    /// ランナー) を止めるときに使う。
    seen: Mutex<HashMap<String, String>>,
}

#[derive(Deserialize)]
struct Status {
    running: bool,
}

impl ClusterBackend {
    pub fn new(store: Store, client: reqwest::Client) -> Self {
        Self {
            store,
            client,
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

    async fn call(
        &self,
        node: &Node,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, Error> {
        let response = request
            .send()
            .await
            .map_err(|e| Error::Unreachable(format!("{}: {e}", node.name)))?;
        if response.status().is_success() {
            return Ok(response);
        }
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        Err(Error::Runner(format!(
            "{}: {status}: {}",
            node.name,
            body.chars().take(300).collect::<String>()
        )))
    }

    fn url(node: &Node, path: &str) -> String {
        format!("{}{path}", node.url.trim_end_matches('/'))
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

impl RunnerBackend for ClusterBackend {
    async fn start(&self, spec: RunnerSpec) -> Result<(), Error> {
        let node = self.place(&spec.key).await?;
        let request = self
            .client
            .post(Self::url(&node, "/v1/runners"))
            .json(&spec);
        self.call(&node, request).await?;
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
        let request = self
            .client
            .delete(Self::url(&node, &format!("/v1/runners/{key}")));
        self.call(&node, request).await?;
        Ok(())
    }

    async fn is_running(&self, key: &str) -> Result<bool, Error> {
        let Some(node) = self.placed(key).await? else {
            return Ok(false);
        };
        let request = self
            .client
            .get(Self::url(&node, &format!("/v1/runners/{key}")));
        match self.call(&node, request).await {
            Ok(response) => Ok(response.json::<Status>().await.map_err(http)?.running),
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
        let request = self
            .client
            .delete(Self::url(&node, &format!("/v1/runners/{key}/state")));
        self.call(&node, request).await?;
        Ok(())
    }

    async fn update(&self, spec: RunnerSpec) -> Result<(), Error> {
        let node = self.placed_or_error(&spec.key).await?;
        let request = self
            .client
            .put(Self::url(&node, &format!("/v1/runners/{}", spec.key)))
            .json(&spec);
        self.call(&node, request).await?;
        Ok(())
    }

    async fn connect_agent(&self, key: &str, wait: Duration) -> Result<AgentStream, Error> {
        let node = self.placed_or_error(key).await?;
        let response = self
            .client
            .get(Self::url(
                &node,
                &format!("/v1/runners/{key}/agent?wait_ms={}", wait.as_millis()),
            ))
            .header(reqwest::header::CONNECTION, "upgrade")
            .header(reqwest::header::UPGRADE, AGENT_UPGRADE)
            // VM の起動を待つ時間に、余裕を足す。
            .timeout(wait + Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| Error::Unreachable(format!("{}: {e}", node.name)))?;
        if response.status() != StatusCode::SWITCHING_PROTOCOLS {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(Error::Runner(format!(
                "{}: the agent connection failed: {status}: {}",
                node.name,
                body.trim()
            )));
        }
        Ok(Box::new(response.upgrade().await.map_err(http)?))
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
            let request = self.client.get(Self::url(&node, "/v1/runners"));
            match self.call(&node, request).await {
                Ok(response) => {
                    let listed = response.json::<Vec<String>>().await.map_err(http)?;
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

fn http(error: reqwest::Error) -> Error {
    Error::Runner(error.to_string())
}
