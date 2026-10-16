//! chloe のデータベースに置くシークレット。

use std::time::{SystemTime, UNIX_EPOCH};

use secrecy::{ExposeSecret, SecretString};
use sqlx::{Row, SqlitePool};

use crate::{Error, MasterKey, Owner};

pub struct SecretDb {
    pool: SqlitePool,
    master: MasterKey,
}

impl SecretDb {
    pub fn new(pool: SqlitePool, master: MasterKey) -> Self {
        Self { pool, master }
    }

    pub async fn set(&self, owner: &Owner, name: &str, value: &SecretString) -> Result<(), Error> {
        let aad = aad(owner, name);
        let (wrapped_key, ciphertext) = self.master.seal(&aad, value.expose_secret().as_bytes())?;
        let now = now();
        sqlx::query(
            "INSERT INTO secrets (owner, name, master_key_id, wrapped_key, ciphertext, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
             ON CONFLICT (owner, name) DO UPDATE SET
                master_key_id = excluded.master_key_id,
                wrapped_key = excluded.wrapped_key,
                ciphertext = excluded.ciphertext,
                updated_at = excluded.updated_at",
        )
        .bind(owner.to_string())
        .bind(name)
        .bind(self.master.id())
        .bind(wrapped_key)
        .bind(ciphertext)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get(&self, owner: &Owner, name: &str) -> Result<Option<SecretString>, Error> {
        let row = sqlx::query(
            "SELECT wrapped_key, ciphertext FROM secrets WHERE owner = ?1 AND name = ?2",
        )
        .bind(owner.to_string())
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else { return Ok(None) };
        let wrapped_key: Vec<u8> = row.try_get("wrapped_key")?;
        let ciphertext: Vec<u8> = row.try_get("ciphertext")?;
        let aad = aad(owner, name);
        let value = self.master.open(&aad, &wrapped_key, &ciphertext)?;
        let value =
            String::from_utf8(value).map_err(|_| Error::Decrypt(format!("{owner}/{name}")))?;
        Ok(Some(value.into()))
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

fn aad(owner: &Owner, name: &str) -> Vec<u8> {
    format!("chloe-secret:v1:{owner}:{name}").into_bytes()
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
}
