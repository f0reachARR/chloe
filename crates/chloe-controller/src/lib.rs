//! 利用者、ワークスペース、ランナーの管理。
//!
//! ゲートウェイと同じプロセスで使うライブラリ。

pub mod catalog;
mod controller;
pub mod docker;
pub mod runner;
pub mod store;
pub mod workspace;

pub use controller::{Controller, Session};

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
}
