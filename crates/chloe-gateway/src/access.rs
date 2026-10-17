//! 誰がログインでき、どのワークスペースに入れるか。
//!
//! GitHub App を設定していれば、アプリをインストールしたいずれかの Organization のメンバーが
//! ログインでき、Organization と team のワークスペースには GitHub 上の所属で入れる。
//! 設定の `allowed_users` と `memberships` は、それに加えて許す (開発用)。

use anyhow::Result;
use chloe_controller::workspace::Workspace;
use chloe_github::UserToken;
use chloe_secrets::Owner;

pub use crate::github_token::personal;
use crate::github_token::{USER_TOKEN_SECRET, load_user_token, store_user_token};
use crate::session::Shared;

impl Shared {
    pub async fn is_allowed(&self, login: &str) -> Result<bool> {
        if self.config.is_allowed(login) {
            return Ok(true);
        }
        match &self.github {
            Some(github) => Ok(github.is_member_of_any(login).await?),
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
            candidates.extend(github.organizations().await?.into_iter().map(Owner::Org));
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
        // アプリをインストールしていない Organization の所属は確かめられない (false になる)。
        Ok(match owner {
            Owner::User(_) => false,
            Owner::Org(org) => github.is_org_member(org, login).await?,
            Owner::Team { org, team } => github.is_team_member(org, team, login).await?,
        })
    }

    /// 動いているランナーの方針を作り直して送る (使えるシークレットの種類が変わったとき)。
    pub async fn refresh_workspace(&self, workspace: &Workspace) -> Result<bool> {
        Ok(self.controller.refresh(workspace).await?)
    }

    /// 動いているランナーのうち、`owner` のもの (`None` ならすべて) の方針を作り直す。
    /// 失敗したワークスペースはログに残して続ける。
    pub async fn refresh_running(&self, owner: Option<&Owner>) -> usize {
        let workspaces = match self.controller.running().await {
            Ok(workspaces) => workspaces,
            Err(error) => {
                tracing::warn!("failed to list running workspaces: {error:#}");
                return 0;
            }
        };
        let mut refreshed = 0;
        for workspace in workspaces {
            if owner.is_some_and(|owner| *owner != workspace.owner) {
                continue;
            }
            match self.refresh_workspace(&workspace).await {
                Ok(true) => refreshed += 1,
                Ok(false) => {}
                Err(error) => tracing::warn!(
                    workspace = %workspace.display_name(),
                    "failed to refresh the runner: {error:#}"
                ),
            }
        }
        refreshed
    }

    pub async fn load_user_token(&self, login: &str) -> Result<Option<UserToken>> {
        load_user_token(self.controller.secrets(), login).await
    }

    pub async fn store_user_token(&self, login: &str, token: &UserToken) -> Result<()> {
        store_user_token(self.controller.secrets(), login, token).await
    }

    pub async fn delete_user_token(&self, login: &str) -> Result<bool> {
        Ok(self
            .controller
            .secrets()
            .delete(&personal(login), USER_TOKEN_SECRET)
            .await?)
    }
}
