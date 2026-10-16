//! 誰がログインでき、どのワークスペースに入れるか。GitHub のトークンをどう用意するか。
//!
//! GitHub App を設定していれば、アプリの Organization のメンバーがログインでき、
//! Organization と team のワークスペースには GitHub 上の所属で入れる。
//! 設定の `allowed_users` と `memberships` は、それに加えて許す (開発用)。

use std::time::Duration;

use anyhow::{Context, Result};
use chloe_controller::workspace::Workspace;
use chloe_github::UserToken;
use chloe_secrets::{ExposeSecret, Owner, SecretString};
use tracing::info;

use crate::session::Shared;

/// 個人のユーザートークンを置くシークレットの名前。値は [`UserToken`] の JSON。
pub const USER_TOKEN_SECRET: &str = "github_user_token";
/// ユーザートークンは、失効までこれより短くなったら更新してから渡す。
const USER_TOKEN_REFRESH_MARGIN: Duration = Duration::from_secs(60 * 60);

impl Shared {
    pub async fn is_allowed(&self, login: &str) -> Result<bool> {
        if self.config.is_allowed(login) {
            return Ok(true);
        }
        match &self.github {
            Some(github) => Ok(github.is_org_member(login).await?),
            None => Ok(false),
        }
    }

    /// 利用者が入れるワークスペースの持ち主 (本人と、所属する team や Organization)。
    pub async fn owners_for(&self, login: &str) -> Result<Vec<Owner>> {
        let mut owners = vec![personal(login)];
        // 所属を確かめる候補は、ワークスペースを持つ持ち主と設定にある持ち主に限る。
        let mut candidates = self.controller.store().owners().await?;
        candidates.extend(
            self.config
                .memberships
                .keys()
                .filter_map(|o| o.parse().ok()),
        );
        if let Some(github) = &self.github {
            candidates.push(Owner::Org(github.organization().to_ascii_lowercase()));
        }
        candidates.sort();
        candidates.dedup();
        for owner in candidates {
            if !matches!(owner, Owner::User(_)) && self.can_access(login, &owner).await? {
                owners.push(owner);
            }
        }
        Ok(owners)
    }

    pub async fn can_access(&self, login: &str, owner: &Owner) -> Result<bool> {
        if *owner == personal(login) {
            return Ok(true);
        }
        if self
            .config
            .memberships
            .get(&owner.to_string())
            .is_some_and(|members| members.iter().any(|m| m.eq_ignore_ascii_case(login)))
        {
            return Ok(true);
        }
        let Some(github) = &self.github else {
            return Ok(false);
        };
        let app_org = github.organization();
        Ok(match owner {
            Owner::User(_) => false,
            Owner::Org(org) if org.eq_ignore_ascii_case(app_org) => {
                github.is_org_member(login).await?
            }
            Owner::Team { org, team } if org.eq_ignore_ascii_case(app_org) => {
                github.is_team_member(team, login).await?
            }
            // アプリの Organization 以外の所属は確かめられない。
            _ => false,
        })
    }

    /// ワークスペースのランナーに渡す GitHub のトークン。
    ///
    /// 個人のワークスペースには Device Flow で得たユーザートークンを (期限が近ければ更新して)、
    /// team と Organization のワークスペースにはアプリのインストールトークンを渡す。
    pub async fn github_token_for(&self, workspace: &Workspace) -> Result<Option<SecretString>> {
        let Some(github) = &self.github else {
            return Ok(None);
        };
        match &workspace.owner {
            Owner::User(login) => {
                let Some(mut token) = self.load_user_token(login).await? else {
                    return Ok(None);
                };
                if token.expires_within(USER_TOKEN_REFRESH_MARGIN) {
                    let refresh = token
                        .refresh_token
                        .clone()
                        .context("the GitHub token expired and cannot be refreshed; run `chloe github login`")?;
                    token = github.refresh_user_token(&refresh).await.context(
                        "refresh the GitHub token (run `chloe github login` if this keeps failing)",
                    )?;
                    self.store_user_token(login, &token).await?;
                    info!(login, "refreshed the GitHub user token");
                }
                Ok(Some(token.access_token.into()))
            }
            Owner::Org(org) | Owner::Team { org, .. }
                if org.eq_ignore_ascii_case(github.organization()) =>
            {
                Ok(Some(github.installation_token().await?))
            }
            _ => Ok(None),
        }
    }

    pub async fn load_user_token(&self, login: &str) -> Result<Option<UserToken>> {
        let Some(value) = self
            .controller
            .secrets()
            .get(&personal(login), USER_TOKEN_SECRET)
            .await?
        else {
            return Ok(None);
        };
        Ok(Some(
            serde_json::from_str(value.expose_secret())
                .context("the stored GitHub token is broken")?,
        ))
    }

    pub async fn store_user_token(&self, login: &str, token: &UserToken) -> Result<()> {
        let value = serde_json::to_string(token)?;
        self.controller
            .secrets()
            .set(&personal(login), USER_TOKEN_SECRET, &value.into())
            .await?;
        Ok(())
    }

    pub async fn delete_user_token(&self, login: &str) -> Result<bool> {
        Ok(self
            .controller
            .secrets()
            .delete(&personal(login), USER_TOKEN_SECRET)
            .await?)
    }
}

pub fn personal(login: &str) -> Owner {
    Owner::User(login.to_ascii_lowercase())
}
