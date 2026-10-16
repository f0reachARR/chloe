//! ランナーのプロキシの問い合わせに答える GitHub のトークン。
//!
//! 個人のワークスペースには Device Flow で得たユーザートークンを (期限が近ければ更新して)、
//! アプリの Organization の team と Organization のワークスペースにはインストールトークンを答える。

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chloe_controller::DynamicSecrets;
use chloe_controller::workspace::Workspace;
use chloe_github::{GitHubApp, UserToken};
use chloe_secrets::{ExposeSecret, Owner, SecretDb, SecretString};
use futures::future::BoxFuture;
use tracing::{info, warn};

/// 個人のユーザートークンを置くシークレットの名前。値は [`UserToken`] の JSON。
pub const USER_TOKEN_SECRET: &str = "github_user_token";
/// ユーザートークンは、失効までこれより短くなったら更新してから答える。
const USER_TOKEN_REFRESH_MARGIN: Duration = Duration::from_secs(5 * 60);
const KIND: &str = "github_token";

pub struct GitHubTokens {
    github: Arc<GitHubApp>,
    secrets: SecretDb,
    /// ユーザートークンの更新を一つずつ進める。リフレッシュトークンは更新のたびに入れ替わるので、
    /// 同時に更新すると片方が失敗する。
    refresh: tokio::sync::Mutex<()>,
}

impl GitHubTokens {
    pub fn new(github: Arc<GitHubApp>, secrets: SecretDb) -> Self {
        Self {
            github,
            secrets,
            refresh: tokio::sync::Mutex::new(()),
        }
    }

    fn is_app_org(&self, org: &str) -> bool {
        org.eq_ignore_ascii_case(self.github.organization())
    }

    async fn user_token(&self, login: &str) -> Result<Option<SecretString>> {
        let Some(token) = load_user_token(&self.secrets, login).await? else {
            return Ok(None);
        };
        if !token.expires_within(USER_TOKEN_REFRESH_MARGIN) {
            return Ok(Some(token.access_token.into()));
        }
        let _guard = self.refresh.lock().await;
        // 錠を待つあいだに、ほかの問い合わせが更新を済ませているかもしれない。
        let Some(token) = load_user_token(&self.secrets, login).await? else {
            return Ok(None);
        };
        if !token.expires_within(USER_TOKEN_REFRESH_MARGIN) {
            return Ok(Some(token.access_token.into()));
        }
        let refresh = token.refresh_token.context(
            "the GitHub token expired and cannot be refreshed; run `chloe github login`",
        )?;
        let token =
            self.github.refresh_user_token(&refresh).await.context(
                "refresh the GitHub token (run `chloe github login` if this keeps failing)",
            )?;
        store_user_token(&self.secrets, login, &token).await?;
        info!(login, "refreshed the GitHub user token");
        Ok(Some(token.access_token.into()))
    }
}

impl DynamicSecrets for GitHubTokens {
    fn kinds<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, Vec<String>> {
        Box::pin(async move {
            let available = match &workspace.owner {
                // Device Flow でユーザートークンを得た利用者のワークスペースだけ。
                Owner::User(_) => matches!(
                    self.secrets.list(&workspace.owner).await,
                    Ok(names) if names.iter().any(|n| n == USER_TOKEN_SECRET)
                ),
                Owner::Org(org) | Owner::Team { org, .. } => self.is_app_org(org),
            };
            if available {
                vec![KIND.to_string()]
            } else {
                vec![]
            }
        })
    }

    fn resolve<'a>(
        &'a self,
        workspace: &'a Workspace,
        kind: &'a str,
    ) -> BoxFuture<'a, Result<Option<SecretString>, chloe_controller::Error>> {
        Box::pin(async move {
            if kind != KIND {
                return Ok(None);
            }
            let result = match &workspace.owner {
                Owner::User(login) => self.user_token(login).await,
                Owner::Org(org) | Owner::Team { org, .. } if self.is_app_org(org) => self
                    .github
                    .installation_token()
                    .await
                    .map(Some)
                    .map_err(Into::into),
                _ => Ok(None),
            };
            result.map_err(|error| {
                warn!(workspace = %workspace.display_name(), "no GitHub token: {error:#}");
                chloe_controller::Error::Runner(format!("{error:#}"))
            })
        })
    }
}

pub async fn load_user_token(secrets: &SecretDb, login: &str) -> Result<Option<UserToken>> {
    let Some(value) = secrets.get(&personal(login), USER_TOKEN_SECRET).await? else {
        return Ok(None);
    };
    Ok(Some(
        serde_json::from_str(value.expose_secret()).context("the stored GitHub token is broken")?,
    ))
}

pub async fn store_user_token(secrets: &SecretDb, login: &str, token: &UserToken) -> Result<()> {
    let value = serde_json::to_string(token)?;
    secrets
        .set(&personal(login), USER_TOKEN_SECRET, &value.into())
        .await?;
    Ok(())
}

pub fn personal(login: &str) -> Owner {
    Owner::User(login.to_ascii_lowercase())
}
