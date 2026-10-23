//! ワークスペースとプレースホルダーのデータベース。

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chloe_secrets::Owner;
use sqlx::{Row, SqlitePool};

use crate::Error;
use crate::catalog::{self, SecretSpec};
use crate::runner::{PolicyLayer, WorkspacePolicy};
use crate::workspace::{DiskSizes, IdleStop, MachineSize, RootMode, Workspace, WorkspaceRef};

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub(crate) fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// ワークスペースを作る。プレースホルダーは使うときに作る ([`Store::ensure_placeholders`])。
    pub async fn create(
        &self,
        target: &WorkspaceRef,
        root_mode: RootMode,
        disks: DiskSizes,
        machine: MachineSize,
        idle_stop: IdleStop,
        created_by: &str,
    ) -> Result<Workspace, Error> {
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            "INSERT INTO workspaces (owner, name, root_mode, workspace_disk_gib, docker_disk_gib, vcpus, memory_mib, idle_timeout_secs, created_by, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )
        .bind(target.owner.to_string())
        .bind(&target.name)
        .bind(root_mode.as_str())
        .bind(disks.workspace_gib)
        .bind(disks.docker_gib)
        .bind(machine.vcpus)
        .bind(machine.memory_mib)
        .bind(idle_stop.to_column())
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
        tx.commit().await?;
        Ok(Workspace {
            id,
            owner: target.owner.clone(),
            name: target.name.clone(),
            root_mode,
            disks,
            machine,
            idle_stop,
        })
    }

    pub async fn find(&self, target: &WorkspaceRef) -> Result<Option<Workspace>, Error> {
        let row = sqlx::query(
            "SELECT id, owner, name, root_mode, workspace_disk_gib, docker_disk_gib, vcpus, memory_mib, idle_timeout_secs FROM workspaces WHERE owner = ?1 AND name = ?2",
        )
        .bind(target.owner.to_string())
        .bind(&target.name)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| workspace_from_row(&row)).transpose()
    }

    pub async fn get(&self, id: i64) -> Result<Option<Workspace>, Error> {
        let row = sqlx::query(
            "SELECT id, owner, name, root_mode, workspace_disk_gib, docker_disk_gib, vcpus, memory_mib, idle_timeout_secs FROM workspaces WHERE id = ?1",
        )
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
            "SELECT id, owner, name, root_mode, workspace_disk_gib, docker_disk_gib, vcpus, memory_mib, idle_timeout_secs FROM workspaces WHERE owner = ?1 ORDER BY name",
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

    pub async fn set_disk_sizes(&self, id: i64, disks: DiskSizes) -> Result<(), Error> {
        sqlx::query(
            "UPDATE workspaces SET workspace_disk_gib = ?2, docker_disk_gib = ?3 WHERE id = ?1",
        )
        .bind(id)
        .bind(disks.workspace_gib)
        .bind(disks.docker_gib)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_machine(&self, id: i64, machine: MachineSize) -> Result<(), Error> {
        sqlx::query("UPDATE workspaces SET vcpus = ?2, memory_mib = ?3 WHERE id = ?1")
            .bind(id)
            .bind(machine.vcpus)
            .bind(machine.memory_mib)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_idle_stop(&self, id: i64, idle_stop: IdleStop) -> Result<(), Error> {
        sqlx::query("UPDATE workspaces SET idle_timeout_secs = ?2 WHERE id = ?1")
            .bind(id)
            .bind(idle_stop.to_column())
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

    /// 持ち主 (個人、Organization か team) の設定。
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

    /// ワークスペースの設定。
    pub async fn workspace_policy(&self, id: i64) -> Result<Option<WorkspacePolicy>, Error> {
        let row = sqlx::query("SELECT config FROM workspace_policies WHERE workspace_id = ?1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| {
            let config: String = row.try_get("config")?;
            serde_json::from_str(&config).map_err(|e| Error::Invalid(e.to_string()))
        })
        .transpose()
    }

    pub async fn set_workspace_policy(
        &self,
        id: i64,
        policy: &WorkspacePolicy,
        updated_by: &str,
    ) -> Result<(), Error> {
        let config = serde_json::to_string(policy).map_err(|e| Error::Invalid(e.to_string()))?;
        sqlx::query(
            "INSERT INTO workspace_policies (workspace_id, config, updated_by, updated_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (workspace_id) DO UPDATE SET config = excluded.config,
                 updated_by = excluded.updated_by, updated_at = excluded.updated_at",
        )
        .bind(id)
        .bind(config)
        .bind(updated_by)
        .bind(now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// ワークスペースの設定を消す。消したら `true`。
    pub async fn delete_workspace_policy(&self, id: i64) -> Result<bool, Error> {
        let result = sqlx::query("DELETE FROM workspace_policies WHERE workspace_id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// ワークスペースの持ち主に重ねる設定の層 (上の層から順)。個人の層を重ねてよいかは
    /// 呼び出し側が決める ([`crate::Controller::owner_layers`])。
    pub async fn policy_layers(&self, owner: &Owner) -> Result<Vec<PolicyLayer>, Error> {
        let chain = self.policy_chain(owner).await?;
        Ok(chain.into_iter().map(|(_, layer)| layer).collect())
    }

    /// [`Store::policy_layers`] を、それぞれの層の持ち主と一緒に返す。
    pub async fn policy_chain(&self, owner: &Owner) -> Result<Vec<(Owner, PolicyLayer)>, Error> {
        let chain = match owner {
            Owner::User(_) | Owner::Org(_) => vec![owner.clone()],
            Owner::Team { org, .. } => vec![Owner::Org(org.clone()), owner.clone()],
        };
        let mut layers = Vec::new();
        for owner in chain {
            if let Some(layer) = self.policy(&owner).await? {
                layers.push((owner, layer));
            }
        }
        Ok(layers)
    }

    /// ワークスペースを置いたホスト。まだ起動していなければ `None`。
    pub async fn workspace_node(&self, id: i64) -> Result<Option<String>, Error> {
        let row = sqlx::query("SELECT node FROM workspaces WHERE id = ?1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(match row {
            Some(row) => row.try_get("node")?,
            None => None,
        })
    }

    pub async fn set_workspace_node(&self, id: i64, node: &str) -> Result<(), Error> {
        sqlx::query("UPDATE workspaces SET node = ?2 WHERE id = ?1")
            .bind(id)
            .bind(node)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// ホストごとの、そこに置いたワークスペースの数 (新しいワークスペースの置き場所を決める)。
    pub async fn placement_counts(&self) -> Result<BTreeMap<String, usize>, Error> {
        let rows = sqlx::query(
            "SELECT node, COUNT(*) AS n FROM workspaces WHERE node IS NOT NULL GROUP BY node",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                let n: i64 = row.try_get("n")?;
                Ok((row.try_get("node")?, n as usize))
            })
            .collect()
    }

    /// `node` で動いていると記録したランナーのワークスペースの ID。
    pub async fn recorded_runners_on(&self, node: &str) -> Result<Vec<i64>, Error> {
        let rows =
            sqlx::query("SELECT workspace_id FROM runners WHERE node = ?1 ORDER BY workspace_id")
                .bind(node)
                .fetch_all(&self.pool)
                .await?;
        rows.iter()
            .map(|row| row.try_get("workspace_id").map_err(Into::into))
            .collect()
    }

    pub async fn nodes(&self) -> Result<Vec<Node>, Error> {
        let rows = sqlx::query("SELECT name, url, joined_at FROM nodes ORDER BY name")
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|row| {
                Ok(Node {
                    name: row.try_get("name")?,
                    url: row.try_get("url")?,
                    joined: row.try_get::<Option<i64>, _>("joined_at")?.is_some(),
                })
            })
            .collect()
    }

    pub async fn node(&self, name: &str) -> Result<Option<Node>, Error> {
        Ok(self.nodes().await?.into_iter().find(|n| n.name == name))
    }

    /// ホストを登録し、一度だけ使える参加用のトークン (の秘密の部分) を返す。登録済みなら
    /// URL を変え、新しいトークンを作る (証明書を作り直すとき)。
    pub async fn add_node(&self, name: &str, url: &str, ttl_secs: i64) -> Result<String, Error> {
        sqlx::query(
            "INSERT INTO nodes (name, url) VALUES (?1, ?2)
             ON CONFLICT (name) DO UPDATE SET url = excluded.url",
        )
        .bind(name)
        .bind(url)
        .execute(&self.pool)
        .await?;
        let secret = random_token();
        sqlx::query("INSERT INTO node_joins (token_hash, name, expires_at) VALUES (?1, ?2, ?3)")
            .bind(token_hash(&secret))
            .bind(name)
            .bind(now() + ttl_secs)
            .execute(&self.pool)
            .await?;
        Ok(secret)
    }

    /// 参加用のトークンを使う。期限内の未使用のトークンなら消して、ホストの名前を返す。
    pub async fn consume_join(&self, secret: &str) -> Result<Option<Node>, Error> {
        let row =
            sqlx::query("DELETE FROM node_joins WHERE token_hash = ?1 RETURNING name, expires_at")
                .bind(token_hash(secret))
                .fetch_optional(&self.pool)
                .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let expires_at: i64 = row.try_get("expires_at")?;
        if expires_at < now() {
            return Ok(None);
        }
        let name: String = row.try_get("name")?;
        sqlx::query("UPDATE nodes SET joined_at = ?2 WHERE name = ?1")
            .bind(&name)
            .bind(now())
            .execute(&self.pool)
            .await?;
        self.node(&name).await
    }

    /// ホストの登録を消す。消したホストの証明書では、もう問い合わせに答えない。
    pub async fn remove_node(&self, name: &str) -> Result<bool, Error> {
        let result = sqlx::query("DELETE FROM nodes WHERE name = ?1")
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 管理 API のトークンを作り、(ID, トークン) を返す。トークンはここでしか見えない。
    pub async fn create_api_token(
        &self,
        login: &str,
        name: &str,
        ttl_secs: Option<i64>,
    ) -> Result<(i64, String), Error> {
        let token = format!("chloe_{}", random_token());
        let row = sqlx::query(
            "INSERT INTO api_tokens (login, name, token_hash, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5) RETURNING id",
        )
        .bind(login)
        .bind(name)
        .bind(token_hash(&token))
        .bind(now())
        .bind(ttl_secs.map(|ttl| now() + ttl))
        .fetch_one(&self.pool)
        .await?;
        Ok((row.try_get("id")?, token))
    }

    /// 有効な (失効しておらず、期限内の) トークンの持ち主を返し、使った時刻を記録する。
    pub async fn authenticate_api_token(&self, token: &str) -> Result<Option<ApiToken>, Error> {
        let row = sqlx::query(
            "UPDATE api_tokens SET last_used_at = ?2
             WHERE token_hash = ?1 AND revoked_at IS NULL
               AND (expires_at IS NULL OR expires_at > ?2)
             RETURNING id, login, name, created_at, expires_at, last_used_at",
        )
        .bind(token_hash(token))
        .bind(now())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| api_token(&row)).transpose()
    }

    /// 利用者の、失効していないトークンの一覧。
    pub async fn api_tokens(&self, login: &str) -> Result<Vec<ApiToken>, Error> {
        let rows = sqlx::query(
            "SELECT id, login, name, created_at, expires_at, last_used_at FROM api_tokens
             WHERE login = ?1 AND revoked_at IS NULL ORDER BY id",
        )
        .bind(login)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(api_token).collect()
    }

    /// 利用者のトークンを失効させる。失効させたら `true`。
    pub async fn revoke_api_token(&self, login: &str, id: i64) -> Result<bool, Error> {
        let result = sqlx::query(
            "UPDATE api_tokens SET revoked_at = ?3
             WHERE id = ?1 AND login = ?2 AND revoked_at IS NULL",
        )
        .bind(id)
        .bind(login)
        .bind(now())
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Web の画面のセッションを作り、(ID, セッションの値) を返す。値は cookie にだけ置く。
    pub async fn create_web_session(
        &self,
        login: &str,
        ttl_secs: i64,
    ) -> Result<(i64, String), Error> {
        let session = random_token();
        let row = sqlx::query(
            "INSERT INTO web_sessions (login, session_hash, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4) RETURNING id",
        )
        .bind(login)
        .bind(token_hash(&session))
        .bind(now())
        .bind(now() + ttl_secs)
        .fetch_one(&self.pool)
        .await?;
        Ok((row.try_get("id")?, session))
    }

    /// 期限内のセッションを返し、使った時刻を記録する。
    pub async fn authenticate_web_session(
        &self,
        session: &str,
    ) -> Result<Option<WebSession>, Error> {
        let row = sqlx::query(
            "UPDATE web_sessions SET last_used_at = ?2
             WHERE session_hash = ?1 AND expires_at > ?2
             RETURNING id, login, created_at, expires_at, last_used_at",
        )
        .bind(token_hash(session))
        .bind(now())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| web_session(&row)).transpose()
    }

    /// 利用者の、期限内のセッションの一覧。
    pub async fn web_sessions(&self, login: &str) -> Result<Vec<WebSession>, Error> {
        let rows = sqlx::query(
            "SELECT id, login, created_at, expires_at, last_used_at FROM web_sessions
             WHERE login = ?1 AND expires_at > ?2 ORDER BY id",
        )
        .bind(login)
        .bind(now())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(web_session).collect()
    }

    /// 利用者のセッションを消す。消したら `true`。
    pub async fn revoke_web_session(&self, login: &str, id: i64) -> Result<bool, Error> {
        let result = sqlx::query("DELETE FROM web_sessions WHERE id = ?1 AND login = ?2")
            .bind(id)
            .bind(login)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// セッションの値で消す (ログアウト)。期限の切れたセッションもここで片付ける。
    pub async fn end_web_session(&self, session: &str) -> Result<(), Error> {
        sqlx::query("DELETE FROM web_sessions WHERE session_hash = ?1 OR expires_at <= ?2")
            .bind(token_hash(session))
            .bind(now())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 一度だけ使えるログインのコードを作る。
    pub async fn create_web_login_link(&self, login: &str, ttl_secs: i64) -> Result<String, Error> {
        let code = random_token();
        sqlx::query("DELETE FROM web_login_links WHERE expires_at <= ?1")
            .bind(now())
            .execute(&self.pool)
            .await?;
        sqlx::query(
            "INSERT INTO web_login_links (code_hash, login, expires_at) VALUES (?1, ?2, ?3)",
        )
        .bind(token_hash(&code))
        .bind(login)
        .bind(now() + ttl_secs)
        .execute(&self.pool)
        .await?;
        Ok(code)
    }

    /// ログインのコードを使い、期限内なら利用者を返す。使ったコードは消すので、二度は使えない。
    pub async fn redeem_web_login_link(&self, code: &str) -> Result<Option<String>, Error> {
        let row = sqlx::query(
            "DELETE FROM web_login_links WHERE code_hash = ?1 RETURNING login, expires_at",
        )
        .bind(token_hash(code))
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let expires_at: i64 = row.try_get("expires_at")?;
        Ok((expires_at > now())
            .then(|| row.try_get("login"))
            .transpose()?)
    }

    /// SSH の公開鍵を利用者に結び付け、鍵が結び付いている利用者を返す。すでにほかの利用者に
    /// 結び付いていれば、結び付け直さずにその利用者を返す。
    pub async fn bind_ssh_key(
        &self,
        login: &str,
        fingerprint: &str,
        public_key: &str,
    ) -> Result<String, Error> {
        sqlx::query(
            "INSERT INTO ssh_keys (fingerprint, login, public_key, created_at)
             VALUES (?1, ?2, ?3, ?4) ON CONFLICT (fingerprint) DO NOTHING",
        )
        .bind(fingerprint)
        .bind(login)
        .bind(public_key)
        .bind(now())
        .execute(&self.pool)
        .await?;
        let row = sqlx::query("SELECT login FROM ssh_keys WHERE fingerprint = ?1")
            .bind(fingerprint)
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get("login")?)
    }

    /// 公開鍵が結び付いている利用者。
    pub async fn ssh_key_login(&self, fingerprint: &str) -> Result<Option<String>, Error> {
        let row = sqlx::query("SELECT login FROM ssh_keys WHERE fingerprint = ?1")
            .bind(fingerprint)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| row.try_get("login"))
            .transpose()
            .map_err(Into::into)
    }

    /// 公開鍵で入ったことを記録する。
    pub async fn touch_ssh_key(&self, fingerprint: &str) -> Result<(), Error> {
        sqlx::query("UPDATE ssh_keys SET last_used_at = ?2 WHERE fingerprint = ?1")
            .bind(fingerprint)
            .bind(now())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 利用者に結び付いた公開鍵の一覧。
    pub async fn ssh_keys(&self, login: &str) -> Result<Vec<SshKey>, Error> {
        let rows = sqlx::query(
            "SELECT fingerprint, login, public_key, created_at, last_used_at FROM ssh_keys
             WHERE login = ?1 ORDER BY created_at, fingerprint",
        )
        .bind(login)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(SshKey {
                    fingerprint: row.try_get("fingerprint")?,
                    login: row.try_get("login")?,
                    public_key: row.try_get("public_key")?,
                    created_at: row.try_get("created_at")?,
                    last_used_at: row.try_get("last_used_at")?,
                })
            })
            .collect()
    }

    /// 利用者の公開鍵の結び付けを消す。消したら `true`。
    pub async fn remove_ssh_key(&self, login: &str, fingerprint: &str) -> Result<bool, Error> {
        let result = sqlx::query("DELETE FROM ssh_keys WHERE fingerprint = ?1 AND login = ?2")
            .bind(fingerprint)
            .bind(login)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 定義に合わせてプレースホルダーを用意し、(シークレット, 欄) からプレースホルダーへの対応を返す。
    /// まだないか、形の変わった欄のプレースホルダーを作る。ほかはワークスペースごとに同じものを使い続ける。
    pub async fn ensure_placeholders(
        &self,
        id: i64,
        specs: &[(String, SecretSpec)],
    ) -> Result<BTreeMap<(String, String), String>, Error> {
        let rows = sqlx::query(
            "SELECT secret_name, field, template, placeholder FROM placeholders WHERE workspace_id = ?1",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        let mut existing = BTreeMap::new();
        for row in rows {
            let key: (String, String) = (row.try_get("secret_name")?, row.try_get("field")?);
            let value: (String, String) = (row.try_get("template")?, row.try_get("placeholder")?);
            existing.insert(key, value);
        }
        let mut placeholders = BTreeMap::new();
        for (name, spec) in specs {
            for (field, field_spec) in &spec.fields {
                let key = (name.clone(), field.clone());
                let template = &field_spec.placeholder;
                let placeholder = match existing.get(&key) {
                    Some((t, placeholder)) if t == template => placeholder.clone(),
                    _ => {
                        // 同時に作っても、先に書いたほうを使う。
                        sqlx::query(
                            "INSERT INTO placeholders (workspace_id, secret_name, field, template, placeholder)
                             VALUES (?1, ?2, ?3, ?4, ?5)
                             ON CONFLICT (workspace_id, secret_name, field) DO UPDATE SET
                                template = excluded.template,
                                placeholder = excluded.placeholder
                             WHERE placeholders.template <> excluded.template",
                        )
                        .bind(id)
                        .bind(name)
                        .bind(field)
                        .bind(template)
                        .bind(new_placeholder(template))
                        .execute(&self.pool)
                        .await?;
                        sqlx::query_scalar(
                            "SELECT placeholder FROM placeholders
                             WHERE workspace_id = ?1 AND secret_name = ?2 AND field = ?3",
                        )
                        .bind(id)
                        .bind(name)
                        .bind(field)
                        .fetch_one(&self.pool)
                        .await?
                    }
                };
                placeholders.insert(key, placeholder);
            }
        }
        Ok(placeholders)
    }

    /// シークレットのプレースホルダーを捨てる。次に用意するときに作り直す。
    pub async fn rotate_placeholder(&self, id: i64, secret_name: &str) -> Result<(), Error> {
        sqlx::query("DELETE FROM placeholders WHERE workspace_id = ?1 AND secret_name = ?2")
            .bind(id)
            .bind(secret_name)
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
        disks: DiskSizes {
            workspace_gib: row.try_get("workspace_disk_gib")?,
            docker_gib: row.try_get("docker_disk_gib")?,
        },
        machine: MachineSize {
            vcpus: row.try_get("vcpus")?,
            memory_mib: row.try_get("memory_mib")?,
        },
        idle_stop: IdleStop::from_column(row.try_get("idle_timeout_secs")?),
    })
}

/// 形の `{id}` を "chloe" と乱数に置き換えたプレースホルダー。
fn new_placeholder(template: &str) -> String {
    let random: [u8; 24] = rand::random();
    let id = format!(
        "chloe{}",
        URL_SAFE_NO_PAD.encode(random).replace(['-', '_'], "x")
    );
    template.replacen(catalog::ID, &id, 1)
}

/// 管理 API のトークン (値は含まない)。時刻は UNIX 時刻の秒。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ApiToken {
    pub id: i64,
    pub login: String,
    pub name: String,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub last_used_at: Option<i64>,
}

/// Web の画面のセッション (値は含まない)。時刻は UNIX 時刻の秒。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WebSession {
    pub id: i64,
    pub login: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub last_used_at: Option<i64>,
}

fn web_session(row: &sqlx::sqlite::SqliteRow) -> Result<WebSession, Error> {
    Ok(WebSession {
        id: row.try_get("id")?,
        login: row.try_get("login")?,
        created_at: row.try_get("created_at")?,
        expires_at: row.try_get("expires_at")?,
        last_used_at: row.try_get("last_used_at")?,
    })
}

/// 利用者に結び付いた SSH の公開鍵。時刻は UNIX 時刻の秒。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SshKey {
    /// `SHA256:...` の形の指紋。
    pub fingerprint: String,
    pub login: String,
    /// OpenSSH の形の公開鍵。
    pub public_key: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

fn api_token(row: &sqlx::sqlite::SqliteRow) -> Result<ApiToken, Error> {
    Ok(ApiToken {
        id: row.try_get("id")?,
        login: row.try_get("login")?,
        name: row.try_get("name")?,
        created_at: row.try_get("created_at")?,
        expires_at: row.try_get("expires_at")?,
        last_used_at: row.try_get("last_used_at")?,
    })
}

/// ランナーのホスト。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub name: String,
    pub url: String,
    /// 参加用のトークンで証明書を受け取ったか。
    pub joined: bool,
}

/// 推測できないトークン (32 バイトの乱数の URL 向け Base64)。
pub fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::fill(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// トークンのハッシュ (SHA-256 の 16 進)。トークンは十分に長い乱数なので、遅いハッシュは要らない。
pub fn token_hash(token: &str) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
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
            .create(
                &api,
                RootMode::Ephemeral,
                DiskSizes::default(),
                MachineSize::default(),
                IdleStop::Default,
                "alice",
            )
            .await
            .unwrap();
        assert!(matches!(
            store
                .create(
                    &api,
                    RootMode::Ephemeral,
                    DiskSizes::default(),
                    MachineSize::default(),
                    IdleStop::Default,
                    "alice"
                )
                .await,
            Err(Error::AlreadyExists(_))
        ));
        assert_eq!(store.find(&api).await.unwrap(), Some(ws.clone()));
        assert_eq!(ws.disks, DiskSizes::default());
        let disks = DiskSizes {
            workspace_gib: Some(50),
            docker_gib: None,
        };
        store.set_disk_sizes(ws.id, disks).await.unwrap();
        let ws = store.get(ws.id).await.unwrap().unwrap();
        assert_eq!(ws.disks, disks);

        let org = WorkspaceRef::parse("fortefibre/web", "alice").unwrap();
        store
            .create(
                &org,
                RootMode::Persistent,
                DiskSizes::default(),
                MachineSize::default(),
                IdleStop::Default,
                "alice",
            )
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

        let presets = [(
            "anthropic_api_key".to_string(),
            catalog::spec_for_test("anthropic_api_key"),
        )];
        store.ensure_placeholders(ws.id, &presets).await.unwrap();
        store.delete(ws.id).await.unwrap();
        assert_eq!(store.find(&api).await.unwrap(), None);
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM placeholders")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert_eq!(left, 0);
    }

    #[tokio::test]
    async fn ssh_keys_bind_to_one_user() {
        let store = Store::new(pool().await);
        assert_eq!(store.ssh_key_login("SHA256:a").await.unwrap(), None);
        let bound = store
            .bind_ssh_key("alice", "SHA256:a", "ssh-ed25519 AAAA")
            .await
            .unwrap();
        assert_eq!(bound, "alice");
        // ほかの利用者には結び付け直さない。
        let bound = store
            .bind_ssh_key("bob", "SHA256:a", "ssh-ed25519 AAAA")
            .await
            .unwrap();
        assert_eq!(bound, "alice");
        assert_eq!(
            store.ssh_key_login("SHA256:a").await.unwrap().as_deref(),
            Some("alice")
        );
        store.touch_ssh_key("SHA256:a").await.unwrap();
        let keys = store.ssh_keys("alice").await.unwrap();
        assert_eq!(keys.len(), 1);
        assert!(keys[0].last_used_at.is_some());
        assert!(store.ssh_keys("bob").await.unwrap().is_empty());
        assert!(!store.remove_ssh_key("bob", "SHA256:a").await.unwrap());
        assert!(store.remove_ssh_key("alice", "SHA256:a").await.unwrap());
        assert_eq!(store.ssh_key_login("SHA256:a").await.unwrap(), None);
    }

    #[tokio::test]
    async fn api_tokens_expire_and_revoke() {
        let store = Store::new(pool().await);
        let (id, token) = store.create_api_token("alice", "ci", None).await.unwrap();
        assert!(token.starts_with("chloe_"));
        let found = store.authenticate_api_token(&token).await.unwrap().unwrap();
        assert_eq!((found.id, found.login.as_str()), (id, "alice"));
        assert!(found.last_used_at.is_some());
        assert_eq!(
            store.authenticate_api_token("chloe_wrong").await.unwrap(),
            None
        );
        let (_, expired) = store
            .create_api_token("alice", "old", Some(-1))
            .await
            .unwrap();
        assert_eq!(store.authenticate_api_token(&expired).await.unwrap(), None);
        assert_eq!(store.api_tokens("alice").await.unwrap().len(), 2);
        // ほかの利用者のトークンは失効させられない。
        assert!(!store.revoke_api_token("bob", id).await.unwrap());
        assert!(store.revoke_api_token("alice", id).await.unwrap());
        assert_eq!(store.authenticate_api_token(&token).await.unwrap(), None);
        assert_eq!(store.api_tokens("alice").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn web_sessions_expire_and_revoke() {
        let store = Store::new(pool().await);
        let (id, session) = store.create_web_session("alice", 3600).await.unwrap();
        let found = store
            .authenticate_web_session(&session)
            .await
            .unwrap()
            .unwrap();
        assert_eq!((found.id, found.login.as_str()), (id, "alice"));
        assert!(found.last_used_at.is_some());
        assert_eq!(store.authenticate_web_session("wrong").await.unwrap(), None);
        let (_, expired) = store.create_web_session("alice", -1).await.unwrap();
        assert_eq!(
            store.authenticate_web_session(&expired).await.unwrap(),
            None
        );
        assert_eq!(store.web_sessions("alice").await.unwrap().len(), 1);

        assert!(!store.revoke_web_session("bob", id).await.unwrap());
        assert!(store.revoke_web_session("alice", id).await.unwrap());
        assert_eq!(
            store.authenticate_web_session(&session).await.unwrap(),
            None
        );

        let (_, other) = store.create_web_session("alice", 3600).await.unwrap();
        store.end_web_session(&other).await.unwrap();
        assert_eq!(store.authenticate_web_session(&other).await.unwrap(), None);
        assert!(store.web_sessions("alice").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn web_login_links_work_once() {
        let store = Store::new(pool().await);
        let code = store.create_web_login_link("alice", 300).await.unwrap();
        assert_eq!(store.redeem_web_login_link("wrong").await.unwrap(), None);
        assert_eq!(
            store.redeem_web_login_link(&code).await.unwrap().as_deref(),
            Some("alice")
        );
        assert_eq!(store.redeem_web_login_link(&code).await.unwrap(), None);
        let expired = store.create_web_login_link("alice", -1).await.unwrap();
        assert_eq!(store.redeem_web_login_link(&expired).await.unwrap(), None);
    }

    #[tokio::test]
    async fn nodes_join_once_and_place_workspaces() {
        let store = Store::new(pool().await);
        let secret = store.add_node("n1", "https://n1:8445", 600).await.unwrap();
        assert!(!store.nodes().await.unwrap()[0].joined);
        assert_eq!(store.consume_join("wrong").await.unwrap(), None);
        let node = store.consume_join(&secret).await.unwrap().unwrap();
        assert_eq!(node.name, "n1");
        assert!(node.joined);
        assert_eq!(
            store.consume_join(&secret).await.unwrap(),
            None,
            "used once"
        );
        // 期限の切れたトークンは使えない。
        let expired = store.add_node("n2", "https://n2:8445", -1).await.unwrap();
        assert_eq!(store.consume_join(&expired).await.unwrap(), None);

        let ws = store
            .create(
                &WorkspaceRef::parse("a", "alice").unwrap(),
                RootMode::Ephemeral,
                DiskSizes::default(),
                MachineSize::default(),
                IdleStop::Default,
                "alice",
            )
            .await
            .unwrap();
        assert_eq!(store.workspace_node(ws.id).await.unwrap(), None);
        store.set_workspace_node(ws.id, "n1").await.unwrap();
        assert_eq!(
            store.workspace_node(ws.id).await.unwrap().as_deref(),
            Some("n1")
        );
        assert_eq!(store.placement_counts().await.unwrap()["n1"], 1);
        store.record_runner(ws.id, "n1").await.unwrap();
        assert_eq!(store.recorded_runners_on("n1").await.unwrap(), [ws.id]);
        assert!(store.recorded_runners_on("n2").await.unwrap().is_empty());
        assert!(store.remove_node("n2").await.unwrap());
        assert_eq!(store.nodes().await.unwrap().len(), 1);
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
            rules: vec![chloe_policy::PathRuleConfig {
                name: None,
                hosts: vec![host.into()],
                methods: vec![],
                paths: vec![],
                action: chloe_policy::Action::Allow,
            }],
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
            layers
                .into_iter()
                .flat_map(|l| l.rules)
                .flat_map(|r| r.hosts)
                .collect()
        };
        assert_eq!(
            hosts(store.policy_layers(&team).await.unwrap()),
            ["org.example", "team2.example"]
        );
        assert_eq!(
            hosts(store.policy_layers(&org).await.unwrap()),
            ["org.example"]
        );
        // 個人のワークスペースには、その人の層だけを重ねる。
        let alice = Owner::User("alice".into());
        assert!(store.policy_layers(&alice).await.unwrap().is_empty());
        store
            .set_policy(&alice, &layer("alice.example"), "alice")
            .await
            .unwrap();
        assert_eq!(
            hosts(store.policy_layers(&alice).await.unwrap()),
            ["alice.example"]
        );
        assert!(
            store
                .policy_layers(&Owner::User("bob".into()))
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
    async fn workspace_policies_go_with_their_workspace() {
        let store = Store::new(pool().await);
        let target = WorkspaceRef::parse("acme/web", "alice").unwrap();
        let ws = store
            .create(
                &target,
                RootMode::Ephemeral,
                DiskSizes::default(),
                MachineSize::default(),
                IdleStop::Default,
                "alice",
            )
            .await
            .unwrap();
        let policy = |host: &str| WorkspacePolicy {
            rules: vec![chloe_policy::PathRuleConfig {
                name: None,
                hosts: vec![host.into()],
                methods: vec![],
                paths: vec![],
                action: chloe_policy::Action::Allow,
            }],
        };
        assert_eq!(store.workspace_policy(ws.id).await.unwrap(), None);
        store
            .set_workspace_policy(ws.id, &policy("a.example"), "alice")
            .await
            .unwrap();
        store
            .set_workspace_policy(ws.id, &policy("b.example"), "alice")
            .await
            .unwrap();
        assert_eq!(
            store.workspace_policy(ws.id).await.unwrap(),
            Some(policy("b.example"))
        );
        assert!(store.delete_workspace_policy(ws.id).await.unwrap());
        assert!(!store.delete_workspace_policy(ws.id).await.unwrap());

        // ワークスペースを消すと、その設定も消える。同じ名前で作り直しても引き継がない。
        store
            .set_workspace_policy(ws.id, &policy("a.example"), "alice")
            .await
            .unwrap();
        store.delete(ws.id).await.unwrap();
        let again = store
            .create(
                &target,
                RootMode::Ephemeral,
                DiskSizes::default(),
                MachineSize::default(),
                IdleStop::Default,
                "alice",
            )
            .await
            .unwrap();
        assert_eq!(store.workspace_policy(again.id).await.unwrap(), None);
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workspace_policies")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert_eq!(left, 0);
    }

    #[tokio::test]
    async fn placeholders_are_stable_per_workspace() {
        let store = Store::new(pool().await);
        let a = store
            .create(
                &WorkspaceRef::parse("a", "alice").unwrap(),
                RootMode::Ephemeral,
                DiskSizes::default(),
                MachineSize::default(),
                IdleStop::Default,
                "alice",
            )
            .await
            .unwrap();
        let b = store
            .create(
                &WorkspaceRef::parse("b", "alice").unwrap(),
                RootMode::Ephemeral,
                DiskSizes::default(),
                MachineSize::default(),
                IdleStop::Default,
                "alice",
            )
            .await
            .unwrap();
        let presets: Vec<(String, SecretSpec)> = catalog::FORMER_PRESETS
            .iter()
            .map(|name| (name.to_string(), catalog::spec_for_test(name)))
            .collect();
        let key = |name: &str| (name.to_string(), catalog::VALUE.to_string());
        let first = store.ensure_placeholders(a.id, &presets).await.unwrap();
        assert_eq!(
            first,
            store.ensure_placeholders(a.id, &presets).await.unwrap()
        );
        assert_ne!(
            first,
            store.ensure_placeholders(b.id, &presets).await.unwrap()
        );
        assert!(first[&key("anthropic_api_key")].starts_with("sk-ant-api03-chloe"));
        assert!(first[&key("github_token")].starts_with("ghu_chloe"));

        store
            .rotate_placeholder(a.id, "anthropic_api_key")
            .await
            .unwrap();
        let rotated = store.ensure_placeholders(a.id, &presets).await.unwrap();
        assert_ne!(
            first[&key("anthropic_api_key")],
            rotated[&key("anthropic_api_key")]
        );
        assert_eq!(
            first[&key("openai_api_key")],
            rotated[&key("openai_api_key")]
        );

        // 形を変えた欄だけを作り直す。
        let mut changed = presets.clone();
        changed[0]
            .1
            .fields
            .get_mut(catalog::VALUE)
            .unwrap()
            .placeholder = "x_{id}_y".into();
        let after = store.ensure_placeholders(a.id, &changed).await.unwrap();
        let name = &changed[0].0;
        assert!(after[&key(name)].starts_with("x_chloe") && after[&key(name)].ends_with("_y"));
        assert_eq!(
            after[&key("openai_api_key")],
            rotated[&key("openai_api_key")]
        );
    }

    /// 0006 より前に作ったプレースホルダーを、組み込みの種類ではそのまま使い続ける。
    #[tokio::test]
    async fn keeps_placeholders_made_before_fields() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        chloe_secrets::MIGRATOR.run_to(5, &pool).await.unwrap();
        sqlx::query(
            "INSERT INTO workspaces (id, owner, name, created_by, created_at) VALUES (1, 'user:alice', 'a', 'alice', 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let old: Vec<(String, String)> = catalog::FORMER_PRESETS
            .iter()
            .map(|name| {
                let template = &catalog::spec_for_test(name).fields[catalog::VALUE].placeholder;
                (name.to_string(), new_placeholder(template))
            })
            .collect();
        for (name, placeholder) in &old {
            sqlx::query(
                "INSERT INTO placeholders (workspace_id, secret_name, placeholder) VALUES (1, ?1, ?2)",
            )
            .bind(name)
            .bind(placeholder)
            .execute(&pool)
            .await
            .unwrap();
        }
        chloe_secrets::MIGRATOR.run(&pool).await.unwrap();
        let presets: Vec<(String, SecretSpec)> = catalog::FORMER_PRESETS
            .iter()
            .map(|name| (name.to_string(), catalog::spec_for_test(name)))
            .collect();
        let now = Store::new(pool)
            .ensure_placeholders(1, &presets)
            .await
            .unwrap();
        for (name, placeholder) in old {
            assert_eq!(now[&(name, catalog::VALUE.to_string())], placeholder);
        }
    }
}
