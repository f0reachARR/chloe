//! ランナーの置き場所の実装の選択。設定に `[cluster]` があれば `chloe-node` のホストに、
//! なければ同じホストの Docker に置く。

use std::path::PathBuf;
use std::time::Duration;

use chloe_controller::Error;
use chloe_controller::cluster::ClusterBackend;
use chloe_controller::docker::DockerBackend;
use chloe_controller::runner::{AgentStream, RunnerBackend, RunnerSpec};

pub enum Backend {
    Docker(DockerBackend),
    Cluster(ClusterBackend),
}

impl RunnerBackend for Backend {
    async fn start(&self, spec: RunnerSpec) -> Result<(), Error> {
        match self {
            Self::Docker(b) => b.start(spec).await,
            Self::Cluster(b) => b.start(spec).await,
        }
    }

    async fn stop(&self, key: &str) -> Result<(), Error> {
        match self {
            Self::Docker(b) => b.stop(key).await,
            Self::Cluster(b) => b.stop(key).await,
        }
    }

    async fn is_running(&self, key: &str) -> Result<bool, Error> {
        match self {
            Self::Docker(b) => b.is_running(key).await,
            Self::Cluster(b) => b.is_running(key).await,
        }
    }

    async fn update(&self, spec: RunnerSpec) -> Result<(), Error> {
        match self {
            Self::Docker(b) => b.update(spec).await,
            Self::Cluster(b) => b.update(spec).await,
        }
    }

    async fn connect_agent(&self, key: &str, wait: Duration) -> Result<AgentStream, Error> {
        match self {
            Self::Docker(b) => b.connect_agent(key, wait).await,
            Self::Cluster(b) => b.connect_agent(key, wait).await,
        }
    }

    fn local_broker_socket(&self, key: &str) -> Option<PathBuf> {
        match self {
            Self::Docker(b) => b.local_broker_socket(key),
            Self::Cluster(b) => b.local_broker_socket(key),
        }
    }

    async fn list(&self) -> Result<Vec<String>, Error> {
        match self {
            Self::Docker(b) => b.list().await,
            Self::Cluster(b) => b.list().await,
        }
    }

    async fn node_of(&self, key: &str) -> Result<String, Error> {
        match self {
            Self::Docker(b) => b.node_of(key).await,
            Self::Cluster(b) => b.node_of(key).await,
        }
    }
}
