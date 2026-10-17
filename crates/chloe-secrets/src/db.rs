//! chloe のデータベースに置くシークレット。

use std::time::{SystemTime, UNIX_EPOCH};

use secrecy::{ExposeSecret, SecretString};
use sqlx::{Row, SqlitePool};

use crate::{Error, MasterKey, Owner};

/// 複製しても同じデータベースとマスター鍵を指す。
#[derive(Clone)]
pub struct SecretDb {
    pool: SqlitePool,
    master: std::sync::Arc<MasterKey>,
}

impl SecretDb {
    pub fn new(pool: SqlitePool, master: MasterKey) -> Self {
        Self {
            pool,
            master: std::sync::Arc::new(master),
        }
    }

    pub async fn set(&self, owner: &Owner, name: &str, value: &SecretString) -> Result<(), Error> {
        self.set_with_spec(owner, name, value, None).await
    }

    /// 値と定義を置く。定義は暗号化しないが、追加認証データに入れるので書き換えると復号できなくなる。
    pub async fn set_with_spec(
        &self,
        owner: &Owner,
        name: &str,
        value: &SecretString,
        spec: Option<&str>,
    ) -> Result<(), Error> {
        let aad = aad(owner, name, spec);
        let (wrapped_key, ciphertext) = self.master.seal(&aad, value.expose_secret().as_bytes())?;
        let now = now();
        sqlx::query(
            "INSERT INTO secrets (owner, name, master_key_id, wrapped_key, ciphertext, spec, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
             ON CONFLICT (owner, name) DO UPDATE SET
                master_key_id = excluded.master_key_id,
                wrapped_key = excluded.wrapped_key,
                ciphertext = excluded.ciphertext,
                spec = excluded.spec,
                updated_at = excluded.updated_at",
        )
        .bind(owner.to_string())
        .bind(name)
        .bind(self.master.id())
        .bind(wrapped_key)
        .bind(ciphertext)
        .bind(spec)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get(&self, owner: &Owner, name: &str) -> Result<Option<SecretString>, Error> {
        Ok(self
            .get_with_spec(owner, name)
            .await?
            .map(|(value, _)| value))
    }

    /// 値と定義を返す。定義は値の復号で確かめたものになる。
    pub async fn get_with_spec(
        &self,
        owner: &Owner,
        name: &str,
    ) -> Result<Option<(SecretString, Option<String>)>, Error> {
        let row = sqlx::query(
            "SELECT wrapped_key, ciphertext, spec FROM secrets WHERE owner = ?1 AND name = ?2",
        )
        .bind(owner.to_string())
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else { return Ok(None) };
        let wrapped_key: Vec<u8> = row.try_get("wrapped_key")?;
        let ciphertext: Vec<u8> = row.try_get("ciphertext")?;
        let spec: Option<String> = row.try_get("spec")?;
        let aad = aad(owner, name, spec.as_deref());
        let value = self.master.open(&aad, &wrapped_key, &ciphertext)?;
        let value =
            String::from_utf8(value).map_err(|_| Error::Decrypt(format!("{owner}/{name}")))?;
        Ok(Some((value.into(), spec)))
    }

    /// 持ち主のシークレットの名前の一覧。値は返さない。
    pub async fn list(&self, owner: &Owner) -> Result<Vec<String>, Error> {
        let rows = sqlx::query("SELECT name FROM secrets WHERE owner = ?1 ORDER BY name")
            .bind(owner.to_string())
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|row| row.try_get("name").map_err(Into::into))
            .collect()
    }

    pub async fn delete(&self, owner: &Owner, name: &str) -> Result<bool, Error> {
        let result = sqlx::query("DELETE FROM secrets WHERE owner = ?1 AND name = ?2")
            .bind(owner.to_string())
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }
}

/// 定義のある行は、保存した定義の文字列そのものを追加認証データに入れる。
fn aad(owner: &Owner, name: &str, spec: Option<&str>) -> Vec<u8> {
    match spec {
        None => format!("chloe-secret:v1:{owner}:{name}").into_bytes(),
        Some(spec) => format!("chloe-secret:v2:{owner}:{name}:{spec}").into_bytes(),
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn db() -> SecretDb {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        crate::MIGRATOR.run(&pool).await.unwrap();
        SecretDb::new(pool, MasterKey::from_bytes("test", &[1; 32]).unwrap())
    }

    fn alice() -> Owner {
        Owner::User("alice".into())
    }

    #[tokio::test]
    async fn set_get_list_delete() {
        let db = db().await;
        db.set(&alice(), "anthropic", &"sk-1".to_string().into())
            .await
            .unwrap();
        db.set(&alice(), "anthropic", &"sk-2".to_string().into())
            .await
            .unwrap();
        db.set(&alice(), "openai", &"sk-o".to_string().into())
            .await
            .unwrap();
        let value = db.get(&alice(), "anthropic").await.unwrap().unwrap();
        assert_eq!(value.expose_secret(), "sk-2");
        assert_eq!(db.list(&alice()).await.unwrap(), ["anthropic", "openai"]);
        assert!(
            db.get(&Owner::User("bob".into()), "anthropic")
                .await
                .unwrap()
                .is_none()
        );
        assert!(db.delete(&alice(), "openai").await.unwrap());
        assert!(!db.delete(&alice(), "openai").await.unwrap());
    }

    #[tokio::test]
    async fn rows_cannot_be_swapped_between_owners() {
        let db = db().await;
        db.set(&alice(), "anthropic", &"alice-secret".to_string().into())
            .await
            .unwrap();
        // 行の持ち主だけを書き換えても、追加認証データが合わないので復号できない。
        sqlx::query("UPDATE secrets SET owner = 'user:bob'")
            .execute(&db.pool)
            .await
            .unwrap();
        assert!(
            db.get(&Owner::User("bob".into()), "anthropic")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn spec_is_authenticated() {
        let db = db().await;
        let spec = r#"{"fields":{"value":{"placeholder":"{id}"}}}"#;
        db.set_with_spec(&alice(), "custom", &"v".to_string().into(), Some(spec))
            .await
            .unwrap();
        let (value, stored) = db.get_with_spec(&alice(), "custom").await.unwrap().unwrap();
        assert_eq!(value.expose_secret(), "v");
        assert_eq!(stored.as_deref(), Some(spec));
        // 定義のない行は前と同じ形で読める。
        db.set(&alice(), "plain", &"p".to_string().into())
            .await
            .unwrap();
        assert_eq!(
            db.get_with_spec(&alice(), "plain")
                .await
                .unwrap()
                .unwrap()
                .1,
            None
        );
        // 定義を書き換えたり消したりすると復号できない。
        for tampered in [Some(spec.replace("{id}", "x{id}")), None] {
            sqlx::query("UPDATE secrets SET spec = ?1 WHERE name = 'custom'")
                .bind(tampered)
                .execute(&db.pool)
                .await
                .unwrap();
            assert!(db.get(&alice(), "custom").await.is_err());
        }
        // 定義のない行に定義を足しても復号できない。
        sqlx::query("UPDATE secrets SET spec = ?1 WHERE name = 'plain'")
            .bind(spec)
            .execute(&db.pool)
            .await
            .unwrap();
        assert!(db.get(&alice(), "plain").await.is_err());
    }

    #[tokio::test]
    async fn reads_rows_written_before_specs() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        crate::MIGRATOR.run_to(5, &pool).await.unwrap();
        let master = MasterKey::from_bytes("test", &[1; 32]).unwrap();
        // 0005 までの形で書いた行。
        let (wrapped_key, ciphertext) = master
            .seal(b"chloe-secret:v1:user:alice:anthropic_api_key", b"old")
            .unwrap();
        sqlx::query(
            "INSERT INTO secrets (owner, name, master_key_id, wrapped_key, ciphertext, created_at, updated_at)
             VALUES ('user:alice', 'anthropic_api_key', 'test', ?1, ?2, 0, 0)",
        )
        .bind(wrapped_key)
        .bind(ciphertext)
        .execute(&pool)
        .await
        .unwrap();
        crate::MIGRATOR.run(&pool).await.unwrap();
        let db = SecretDb::new(pool, master);
        let value = db
            .get(&alice(), "anthropic_api_key")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(value.expose_secret(), "old");
    }
}
