//! シークレットの保管先。
//!
//! - [`SecretDb`]：chloe のデータベースに暗号化して置くシークレット。持ち主と名前で引く。
//! - [`connect::ConnectClient`]：1Password Connect から `op://` の参照で読むシークレット。
//! - [`Resolver`]：設定に書いた参照 (`op://`、`file:`、`env:`) を値に直す。

pub mod connect;
mod crypto;
mod db;

pub use crypto::MasterKey;
pub use db::SecretDb;
pub use secrecy::{ExposeSecret, SecretString};

use std::fmt;
use std::str::FromStr;

/// データベースのマイグレーション。chloe のすべてのテーブルを含む。
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("failed to decrypt the secret {0}")]
    Decrypt(String),
    #[error("invalid master key: {0}")]
    MasterKey(String),
    #[error("invalid owner: {0}")]
    Owner(String),
    #[error("invalid secret reference: {0}")]
    Reference(String),
    #[error("1Password Connect: {0}")]
    Connect(String),
    #[error("secret not found: {0}")]
    NotFound(String),
}

/// シークレットやワークスペースの持ち主。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Owner {
    User(String),
    Team { org: String, team: String },
    Org(String),
}

impl fmt::Display for Owner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User(login) => write!(f, "user:{login}"),
            Self::Team { org, team } => write!(f, "team:{org}/{team}"),
            Self::Org(org) => write!(f, "org:{org}"),
        }
    }
}

impl FromStr for Owner {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || Error::Owner(s.to_string());
        let (kind, rest) = s.split_once(':').ok_or_else(invalid)?;
        let valid = |name: &str| {
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        };
        match kind {
            "user" if valid(rest) => Ok(Self::User(rest.to_ascii_lowercase())),
            "org" if valid(rest) => Ok(Self::Org(rest.to_ascii_lowercase())),
            "team" => {
                let (org, team) = rest.split_once('/').ok_or_else(invalid)?;
                // team の slug には英数字とハイフンのほかにアンダースコアも入りうる。
                let valid_team = !team.is_empty()
                    && team
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
                if valid(org) && valid_team {
                    Ok(Self::Team {
                        org: org.to_ascii_lowercase(),
                        team: team.to_ascii_lowercase(),
                    })
                } else {
                    Err(invalid())
                }
            }
            _ => Err(invalid()),
        }
    }
}

/// 設定に書いたシークレットの参照を値に直す。
///
/// - `op://<vault>/<item>/<field>`：1Password Connect から読む。
/// - `file:<path>`：ファイルの中身を読む (開発用)。末尾の改行は取る。
/// - `env:<name>`：環境変数を読む (開発用)。
pub struct Resolver {
    pub connect: Option<connect::ConnectClient>,
}

impl Resolver {
    pub async fn resolve(&self, reference: &str) -> Result<SecretString, Error> {
        if let Some(rest) = reference.strip_prefix("op://") {
            let connect = self.connect.as_ref().ok_or_else(|| {
                Error::Reference(format!("{reference}: 1Password Connect is not configured"))
            })?;
            let mut parts = rest.splitn(3, '/');
            let (Some(vault), Some(item), Some(field)) = (parts.next(), parts.next(), parts.next())
            else {
                return Err(Error::Reference(reference.to_string()));
            };
            return connect.field(vault, item, field).await;
        }
        if let Some(path) = reference.strip_prefix("file:") {
            let value = tokio::fs::read_to_string(path).await?;
            return Ok(value.trim_end_matches(['\n', '\r']).to_string().into());
        }
        if let Some(name) = reference.strip_prefix("env:") {
            return std::env::var(name)
                .map(Into::into)
                .map_err(|_| Error::NotFound(reference.to_string()));
        }
        Err(Error::Reference(reference.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_roundtrip() {
        for s in ["user:alice", "team:fortefibre/dev_team", "org:fortefibre"] {
            assert_eq!(s.parse::<Owner>().unwrap().to_string(), s);
        }
        assert_eq!(
            "user:Alice".parse::<Owner>().unwrap(),
            Owner::User("alice".into())
        );
        for s in [
            "alice",
            "user:",
            "user:a/b",
            "team:org",
            "group:x",
            "user:../x",
        ] {
            assert!(s.parse::<Owner>().is_err(), "{s}");
        }
    }

    #[tokio::test]
    async fn resolves_file_and_env() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        std::fs::write(&path, "value-from-file\n").unwrap();
        let resolver = Resolver { connect: None };
        let value = resolver
            .resolve(&format!("file:{}", path.display()))
            .await
            .unwrap();
        assert_eq!(value.expose_secret(), "value-from-file");
        assert!(resolver.resolve("op://v/i/f").await.is_err());
        assert!(resolver.resolve("plain").await.is_err());
    }
}
