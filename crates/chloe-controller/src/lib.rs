//! 利用者、ワークスペース、ランナーの管理。
//!
//! ゲートウェイと同じプロセスで使うライブラリ。

mod broker;
pub mod catalog;
pub mod cluster;
mod controller;
pub mod docker;
pub mod runner;
pub mod store;
pub mod workspace;

pub use controller::{Controller, DynamicSecrets, ReconcileReport, Session};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Secrets(#[from] chloe_secrets::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Invalid(String),
    #[error("already exists: {0}")]
    AlreadyExists(String),
    #[error("runner: {0}")]
    Runner(String),
    /// ランナーのホストにつながらない。
    #[error("node unreachable: {0}")]
    Unreachable(String),
}
