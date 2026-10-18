//! ランナーの置き場所の実装の選択。設定に `[cluster]` があれば `chloe-node` のホストに、
//! なければ同じホスト (Docker か systemd) に置く。

use std::path::PathBuf;
use std::time::Duration;

use chloe_controller::Error;
use chloe_controller::cluster::ClusterBackend;
use chloe_controller::local::LocalBackend;
use chloe_controller::runner::{AgentStream, RunnerBackend, RunnerSpec};

pub enum Backend {
    Local(LocalBackend),
    Cluster(ClusterBackend),
}

impl RunnerBackend for Backend {
    async fn start(&self, spec: RunnerSpec) -> Result<(), Error> {
        match self {
            Self::Local(b) => b.start(spec).await,
            Self::Cluster(b) => b.start(spec).await,
        }
    }

    async fn stop(&self, key: &str) -> Result<(), Error> {
        match self {
            Self::Local(b) => b.stop(key).await,
            Self::Cluster(b) => b.stop(key).await,
        }
    }

    async fn is_running(&self, key: &str) -> Result<bool, Error> {
        match self {
            Self::Local(b) => b.is_running(key).await,
            Self::Cluster(b) => b.is_running(key).await,
        }
    }

    async fn remove_state(&self, key: &str) -> Result<(), Error> {
        match self {
            Self::Local(b) => b.remove_state(key).await,
            Self::Cluster(b) => b.remove_state(key).await,
        }
    }

    async fn update(&self, spec: RunnerSpec) -> Result<(), Error> {
        match self {
            Self::Local(b) => b.update(spec).await,
            Self::Cluster(b) => b.update(spec).await,
        }
    }

    async fn connect_agent(&self, key: &str, wait: Duration) -> Result<AgentStream, Error> {
        match self {
            Self::Local(b) => b.connect_agent(key, wait).await,
            Self::Cluster(b) => b.connect_agent(key, wait).await,
        }
    }

    fn local_broker_socket(&self, key: &str) -> Option<PathBuf> {
        match self {
            Self::Local(b) => b.local_broker_socket(key),
            Self::Cluster(b) => b.local_broker_socket(key),
        }
    }

    async fn tail_requests(
        &self,
        key: &str,
        lines: usize,
    ) -> Result<Vec<chloe_audit::Record>, Error> {
        match self {
            Self::Local(b) => b.tail_requests(key, lines).await,
            Self::Cluster(b) => b.tail_requests(key, lines).await,
        }
    }

    async fn list(&self) -> Result<Vec<String>, Error> {
        match self {
            Self::Local(b) => b.list().await,
            Self::Cluster(b) => b.list().await,
        }
    }

    async fn node_of(&self, key: &str) -> Result<String, Error> {
        match self {
            Self::Local(b) => b.node_of(key).await,
            Self::Cluster(b) => b.node_of(key).await,
        }
    }
}
