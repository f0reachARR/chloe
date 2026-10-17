//! ワークスペースとプレースホルダーのデータベース。

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chloe_secrets::Owner;
use sqlx::{Row, SqlitePool};

use crate::Error;
use crate::catalog;
use crate::runner::PolicyLayer;
use crate::workspace::{RootMode, Workspace, WorkspaceRef};

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// ワークスペースを作り、シークレットの種類ごとのプレースホルダーを決める。
    pub async fn create(
        &self,
        target: &WorkspaceRef,
        root_mode: RootMode,
        created_by: &str,
    ) -> Result<Workspace, Error> {
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            "INSERT INTO workspaces (owner, name, root_mode, created_by, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(target.owner.to_string())
        .bind(&target.name)
        .bind(root_mode.as_str())
        .bind(created_by)
        .bind(now())
        .execute(&mut *tx)
        .await;
        let id = match result {
            Ok(result) => result.last_insert_rowid(),
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                return Err(Error::AlreadyExists(target.to_string()));
            }
            Err(e) => return Err(e.into()),
        };
        for kind in catalog::KINDS {
            sqlx::query(
                "INSERT INTO placeholders (workspace_id, secret_name, placeholder) VALUES (?1, ?2, ?3)",
            )
            .bind(id)
            .bind(kind.name)
            .bind(new_placeholder(kind.placeholder_prefix))
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(Workspace {
            id,
            owner: target.owner.clone(),
            name: target.name.clone(),
            root_mode,
        })
    }

    pub async fn find(&self, target: &WorkspaceRef) -> Result<Option<Workspace>, Error> {
        let row = sqlx::query(
            "SELECT id, owner, name, root_mode FROM workspaces WHERE owner = ?1 AND name = ?2",
        )
        .bind(target.owner.to_string())
        .bind(&target.name)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| workspace_from_row(&row)).transpose()
    }

    pub async fn get(&self, id: i64) -> Result<Option<Workspace>, Error> {
        let row = sqlx::query("SELECT id, owner, name, root_mode FROM workspaces WHERE id = ?1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| workspace_from_row(&row)).transpose()
    }

    /// 持ち主のどれかに属するワークスペースの一覧。
    pub async fn list(&self, owners: &[Owner]) -> Result<Vec<Workspace>, Error> {
        let mut workspaces = Vec::new();
        for owner in owners {
            let rows = sqlx::query(
                "SELECT id, owner, name, root_mode FROM workspaces WHERE owner = ?1 ORDER BY name",
            )
            .bind(owner.to_string())
            .fetch_all(&self.pool)
            .await?;
            for row in rows {
                workspaces.push(workspace_from_row(&row)?);
            }
        }
        Ok(workspaces)
    }

    /// ワークスペースを持つ持ち主の一覧。
    pub async fn owners(&self) -> Result<Vec<Owner>, Error> {
        let rows = sqlx::query("SELECT DISTINCT owner FROM workspaces ORDER BY owner")
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|row| {
                let owner: String = row.try_get("owner")?;
                owner
                    .parse()
                    .map_err(|e: chloe_secrets::Error| Error::Invalid(e.to_string()))
            })
            .collect()
    }

    pub async fn delete(&self, id: i64) -> Result<(), Error> {
        sqlx::query("DELETE FROM workspaces WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_root_mode(&self, id: i64, root_mode: RootMode) -> Result<(), Error> {
        sqlx::query("UPDATE workspaces SET root_mode = ?2 WHERE id = ?1")
            .bind(id)
            .bind(root_mode.as_str())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// ランナーが動き始めたことを記録する。
    pub async fn record_runner(&self, workspace_id: i64, node: &str) -> Result<(), Error> {
        sqlx::query(
            "INSERT INTO runners (workspace_id, node, started_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (workspace_id) DO UPDATE SET node = excluded.node, started_at = excluded.started_at",
        )
        .bind(workspace_id)
        .bind(node)
        .bind(now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn remove_runner(&self, workspace_id: i64) -> Result<(), Error> {
        sqlx::query("DELETE FROM runners WHERE workspace_id = ?1")
            .bind(workspace_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 動いていると記録したランナーのワークスペースの ID。
    pub async fn recorded_runners(&self) -> Result<Vec<i64>, Error> {
        let rows = sqlx::query("SELECT workspace_id FROM runners ORDER BY workspace_id")
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|row| row.try_get("workspace_id").map_err(Into::into))
            .collect()
    }

    /// 持ち主 (Organization か team) の設定。
    pub async fn policy(&self, owner: &Owner) -> Result<Option<PolicyLayer>, Error> {
        let row = sqlx::query("SELECT config FROM policies WHERE owner = ?1")
            .bind(owner.to_string())
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| {
            let config: String = row.try_get("config")?;
            serde_json::from_str(&config).map_err(|e| Error::Invalid(e.to_string()))
        })
        .transpose()
    }

    pub async fn set_policy(
        &self,
        owner: &Owner,
        layer: &PolicyLayer,
        updated_by: &str,
    ) -> Result<(), Error> {
        let config = serde_json::to_string(layer).map_err(|e| Error::Invalid(e.to_string()))?;
        sqlx::query(
            "INSERT INTO policies (owner, config, updated_by, updated_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (owner) DO UPDATE SET config = excluded.config,
                 updated_by = excluded.updated_by, updated_at = excluded.updated_at",
        )
        .bind(owner.to_string())
        .bind(config)
        .bind(updated_by)
        .bind(now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 設定を消す。消したら `true`。
    pub async fn delete_policy(&self, owner: &Owner) -> Result<bool, Error> {
        let result = sqlx::query("DELETE FROM policies WHERE owner = ?1")
            .bind(owner.to_string())
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// ワークスペースの持ち主に重ねる設定の層 (上の層から順)。個人のワークスペースには重ねない。
    pub async fn policy_layers(&self, owner: &Owner) -> Result<Vec<PolicyLayer>, Error> {
        let chain = match owner {
            Owner::User(_) => vec![],
            Owner::Org(_) => vec![owner.clone()],
            Owner::Team { org, .. } => vec![Owner::Org(org.clone()), owner.clone()],
        };
        let mut layers = Vec::new();
        for owner in chain {
            if let Some(layer) = self.policy(&owner).await? {
                layers.push(layer);
            }
        }
        Ok(layers)
    }

    /// シークレットの名前からプレースホルダーへの対応。
    pub async fn placeholders(&self, id: i64) -> Result<BTreeMap<String, String>, Error> {
        let rows = sqlx::query(
            "SELECT secret_name, placeholder FROM placeholders WHERE workspace_id = ?1",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| Ok((row.try_get("secret_name")?, row.try_get("placeholder")?)))
            .collect()
    }

    /// プレースホルダーを作り直す。
    pub async fn rotate_placeholder(&self, id: i64, secret_name: &str) -> Result<(), Error> {
        let kind = catalog::kind(secret_name)
            .ok_or_else(|| Error::Invalid(format!("unknown secret: {secret_name}")))?;
        sqlx::query(
            "INSERT INTO placeholders (workspace_id, secret_name, placeholder) VALUES (?1, ?2, ?3)
             ON CONFLICT (workspace_id, secret_name) DO UPDATE SET placeholder = excluded.placeholder",
        )
        .bind(id)
        .bind(secret_name)
        .bind(new_placeholder(kind.placeholder_prefix))
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

fn workspace_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Workspace, Error> {
    let owner: String = row.try_get("owner")?;
    let root_mode: String = row.try_get("root_mode")?;
    Ok(Workspace {
        id: row.try_get("id")?,
        owner: owner
            .parse()
            .map_err(|e: chloe_secrets::Error| Error::Invalid(e.to_string()))?,
        name: row.try_get("name")?,
        root_mode: root_mode.parse()?,
    })
}

/// 接頭辞のあとに "chloe" と乱数を続けたプレースホルダー。
fn new_placeholder(prefix: &str) -> String {
    let random: [u8; 24] = rand::random();
    format!(
        "{prefix}chloe{}",
        URL_SAFE_NO_PAD.encode(random).replace(['-', '_'], "x")
    )
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    pub async fn pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        chloe_secrets::MIGRATOR.run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn create_find_list_delete() {
        let store = Store::new(pool().await);
        let api = WorkspaceRef::parse("api", "alice").unwrap();
        let ws = store
            .create(&api, RootMode::Ephemeral, "alice")
            .await
            .unwrap();
        assert!(matches!(
            store.create(&api, RootMode::Ephemeral, "alice").await,
            Err(Error::AlreadyExists(_))
        ));
        assert_eq!(store.find(&api).await.unwrap(), Some(ws.clone()));

        let org = WorkspaceRef::parse("fortefibre/web", "alice").unwrap();
        store
            .create(&org, RootMode::Persistent, "alice")
            .await
            .unwrap();
        let listed = store
            .list(&[Owner::User("alice".into()), Owner::Org("fortefibre".into())])
            .await
            .unwrap();
        assert_eq!(listed.len(), 2);
        assert!(
            store
                .list(&[Owner::User("bob".into())])
                .await
                .unwrap()
                .is_empty()
        );

        store.delete(ws.id).await.unwrap();
        assert_eq!(store.find(&api).await.unwrap(), None);
        assert!(store.placeholders(ws.id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn policy_layers_follow_the_owner_chain() {
        let store = Store::new(pool().await);
        let org = Owner::Org("acme".into());
        let team = Owner::Team {
            org: "acme".into(),
            team: "dev".into(),
        };
        let layer = |host: &str| PolicyLayer {
            allow: vec![host.into()],
            ..Default::default()
        };
        store
            .set_policy(&org, &layer("org.example"), "alice")
            .await
            .unwrap();
        store
            .set_policy(&team, &layer("team.example"), "alice")
            .await
            .unwrap();
        // 上書きできる。
        store
            .set_policy(&team, &layer("team2.example"), "alice")
            .await
            .unwrap();
        assert_eq!(
            store.policy(&team).await.unwrap(),
            Some(layer("team2.example"))
        );

        let hosts = |layers: Vec<PolicyLayer>| -> Vec<String> {
            layers.into_iter().flat_map(|l| l.allow).collect()
        };
        assert_eq!(
            hosts(store.policy_layers(&team).await.unwrap()),
            ["org.example", "team2.example"]
        );
        assert_eq!(
            hosts(store.policy_layers(&org).await.unwrap()),
            ["org.example"]
        );
        // 個人のワークスペースには重ねない。
        assert!(
            store
                .policy_layers(&Owner::User("alice".into()))
                .await
                .unwrap()
                .is_empty()
        );
        // ほかの Organization の team には、acme の設定は重ならない。
        let other = Owner::Team {
            org: "beta".into(),
            team: "dev".into(),
        };
        assert!(store.policy_layers(&other).await.unwrap().is_empty());

        assert!(store.delete_policy(&org).await.unwrap());
        assert!(!store.delete_policy(&org).await.unwrap());
        assert_eq!(
            hosts(store.policy_layers(&team).await.unwrap()),
            ["team2.example"]
        );
    }

    #[tokio::test]
    async fn placeholders_are_stable_per_workspace() {
        let store = Store::new(pool().await);
        let a = store
            .create(
                &WorkspaceRef::parse("a", "alice").unwrap(),
                RootMode::Ephemeral,
                "alice",
            )
            .await
            .unwrap();
        let b = store
            .create(
                &WorkspaceRef::parse("b", "alice").unwrap(),
                RootMode::Ephemeral,
                "alice",
            )
            .await
            .unwrap();
        let first = store.placeholders(a.id).await.unwrap();
        assert_eq!(first, store.placeholders(a.id).await.unwrap());
        assert_ne!(first, store.placeholders(b.id).await.unwrap());
        assert!(first["anthropic_api_key"].starts_with("sk-ant-api03-chloe"));
        assert!(first["github_token"].starts_with("ghu_chloe"));

        store
            .rotate_placeholder(a.id, "anthropic_api_key")
            .await
            .unwrap();
        let rotated = store.placeholders(a.id).await.unwrap();
        assert_ne!(first["anthropic_api_key"], rotated["anthropic_api_key"]);
        assert_eq!(first["openai_api_key"], rotated["openai_api_key"]);
    }
}
