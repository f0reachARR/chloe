//! ワークスペースとその名前。
//!
//! SSH では、ワークスペースを持ち主に応じて次のように書く。
//!
//! - 個人：`name` (接続した利用者のもの)
//! - Organization：`org/name`
//! - team：`org/team/name`

use std::fmt;
use std::str::FromStr;

use chloe_secrets::Owner;

use crate::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RootMode {
    /// 共通イメージに tmpfs を重ね、起動のたびに捨てる。
    Ephemeral,
    /// 共通イメージに重ねる層をワークスペースのディスクに残す。
    Persistent,
}

impl RootMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ephemeral => "ephemeral",
            Self::Persistent => "persistent",
        }
    }
}

impl FromStr for RootMode {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "ephemeral" => Ok(Self::Ephemeral),
            "persistent" => Ok(Self::Persistent),
            _ => Err(Error::Invalid(format!("unknown root mode: {s}"))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    pub id: i64,
    pub owner: Owner,
    pub name: String,
    pub root_mode: RootMode,
}

impl Workspace {
    /// SSH で書く形の名前。
    pub fn display_name(&self) -> String {
        WorkspaceRef {
            owner: self.owner.clone(),
            name: self.name.clone(),
        }
        .to_string()
    }

    /// ランナーやディレクトリの名前に使う、ワークスペースごとに一意な識別子。
    pub fn key(&self) -> String {
        format!("ws-{}", self.id)
    }
}

/// 持ち主と名前の組。データベースにまだないワークスペースも指せる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRef {
    pub owner: Owner,
    pub name: String,
}

impl WorkspaceRef {
    /// SSH で書かれた名前を、接続した利用者 `login` から見て解釈する。
    pub fn parse(spec: &str, login: &str) -> Result<Self, Error> {
        let parts: Vec<&str> = spec.split('/').collect();
        let invalid = || Error::Invalid(format!("invalid workspace: {spec}"));
        let (owner, name) = match parts.as_slice() {
            [name] => (format!("user:{login}"), *name),
            [org, name] => (format!("org:{org}"), *name),
            [org, team, name] => (format!("team:{org}/{team}"), *name),
            _ => return Err(invalid()),
        };
        validate_name(name)?;
        Ok(Self {
            owner: owner.parse().map_err(|_| invalid())?,
            name: name.to_ascii_lowercase(),
        })
    }
}

impl fmt::Display for WorkspaceRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.owner {
            Owner::User(_) => write!(f, "{}", self.name),
            Owner::Org(org) => write!(f, "{org}/{}", self.name),
            Owner::Team { org, team } => write!(f, "{org}/{team}/{}", self.name),
        }
    }
}

pub fn validate_name(name: &str) -> Result<(), Error> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && !name.starts_with('-');
    if valid {
        Ok(())
    } else {
        Err(Error::Invalid(format!("invalid workspace name: {name}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ssh_names() {
        let personal = WorkspaceRef::parse("api", "alice").unwrap();
        assert_eq!(personal.owner, Owner::User("alice".into()));
        assert_eq!(personal.to_string(), "api");

        let org = WorkspaceRef::parse("ForteFibre/api", "alice").unwrap();
        assert_eq!(org.owner, Owner::Org("fortefibre".into()));
        assert_eq!(org.to_string(), "fortefibre/api");

        let team = WorkspaceRef::parse("fortefibre/dev/api", "alice").unwrap();
        assert_eq!(team.to_string(), "fortefibre/dev/api");

        for bad in ["", "a/b/c/d", "../x", "-x", "a b", "org/"] {
            assert!(WorkspaceRef::parse(bad, "alice").is_err(), "{bad}");
        }
    }
}
