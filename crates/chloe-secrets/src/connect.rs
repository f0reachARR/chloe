//! 1Password Connect の REST API のクライアント。
//!
//! `op://<vault>/<item>/<field>` の vault と item は、名前でも ID でもよい。
//! 名前のときは Connect の検索 (`filter`) で ID を引く。field はラベルか ID で探す。

use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;

use crate::Error;

pub struct ConnectClient {
    client: reqwest::Client,
    base_url: String,
    token: SecretString,
}

#[derive(Deserialize)]
struct Named {
    id: String,
}

#[derive(Deserialize)]
struct Item {
    #[serde(default)]
    fields: Vec<Field>,
}

#[derive(Deserialize)]
struct Field {
    id: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    value: Option<String>,
}

impl ConnectClient {
    pub fn new(base_url: &str, token: SecretString) -> Result<Self, Error> {
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .map_err(|e| Error::Connect(e.to_string()))?,
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
        })
    }

    /// vault の item の field の値を読む。
    pub async fn field(&self, vault: &str, item: &str, field: &str) -> Result<SecretString, Error> {
        let vault_id = match is_id(vault) {
            true => vault.to_string(),
            false => self.find("/v1/vaults", "name", vault).await?,
        };
        let items = format!("/v1/vaults/{vault_id}/items");
        let item_id = match is_id(item) {
            true => item.to_string(),
            false => self.find(&items, "title", item).await?,
        };
        let item: Item = self.get(&format!("{items}/{item_id}"), None).await?;
        item.fields
            .into_iter()
            .find(|f| f.id == field || f.label.as_deref() == Some(field))
            .and_then(|f| f.value)
            .map(Into::into)
            .ok_or_else(|| Error::NotFound(format!("op://{vault}/{}/{field}", item_id)))
    }

    async fn find(&self, path: &str, attribute: &str, name: &str) -> Result<String, Error> {
        let filter = format!("{attribute} eq \"{}\"", name.replace('"', ""));
        let found: Vec<Named> = self.get(path, Some(&filter)).await?;
        match found.as_slice() {
            [one] => Ok(one.id.clone()),
            [] => Err(Error::NotFound(format!("{path}: {name}"))),
            _ => Err(Error::Connect(format!(
                "{path}: {name} is ambiguous, use its ID"
            ))),
        }
    }

    async fn get<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        filter: Option<&str>,
    ) -> Result<T, Error> {
        let mut request = self
            .client
            .get(format!("{}{path}", self.base_url))
            .bearer_auth(self.token.expose_secret());
        if let Some(filter) = filter {
            request = request.query(&[("filter", filter)]);
        }
        let response = request
            .send()
            .await
            .map_err(|e| Error::Connect(e.to_string()))?;
        if !response.status().is_success() {
            return Err(Error::Connect(format!("{path}: {}", response.status())));
        }
        response
            .json()
            .await
            .map_err(|e| Error::Connect(e.to_string()))
    }
}

/// 1Password の ID は小文字の英数字 26 文字。
fn is_id(s: &str) -> bool {
    s.len() == 26
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const VAULT_ID: &str = "vvvvvvvvvvvvvvvvvvvvvvvvv1";
    const ITEM_ID: &str = "iiiiiiiiiiiiiiiiiiiiiiiii1";

    /// Connect のふりをする HTTP サーバー。トークンが違えば 401 を返す。
    async fn stub() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut buf = vec![0; 4096];
                    let n = socket.read(&mut buf).await.unwrap();
                    let request = String::from_utf8_lossy(&buf[..n]).to_string();
                    let target = request.split_whitespace().nth(1).unwrap_or("").to_string();
                    let authorized = request.contains("authorization: Bearer test-token")
                        || request.contains("Authorization: Bearer test-token");
                    let (status, body) = if !authorized {
                        ("401 Unauthorized", "{}".to_string())
                    } else if target.starts_with("/v1/vaults?filter=") {
                        let found = target.contains("Dev");
                        (
                            "200 OK",
                            if found {
                                format!(r#"[{{"id":"{VAULT_ID}","name":"Dev"}}]"#)
                            } else {
                                "[]".into()
                            },
                        )
                    } else if target.starts_with(&format!("/v1/vaults/{VAULT_ID}/items?filter=")) {
                        (
                            "200 OK",
                            format!(r#"[{{"id":"{ITEM_ID}","title":"alice"}}]"#),
                        )
                    } else if target == format!("/v1/vaults/{VAULT_ID}/items/{ITEM_ID}") {
                        ("200 OK", r#"{"id":"x","fields":[{"id":"f1","label":"anthropic_api_key","value":"sk-from-connect"},{"id":"f2","label":"empty"}]}"#.into())
                    } else {
                        ("404 Not Found", "{}".into())
                    };
                    let response = format!(
                        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    socket.write_all(response.as_bytes()).await.unwrap();
                });
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn reads_field_by_names_and_ids() {
        let url = stub().await;
        let client = ConnectClient::new(&url, "test-token".to_string().into()).unwrap();
        let value = client
            .field("Dev", "alice", "anthropic_api_key")
            .await
            .unwrap();
        assert_eq!(value.expose_secret(), "sk-from-connect");
        let value = client.field(VAULT_ID, ITEM_ID, "f1").await.unwrap();
        assert_eq!(value.expose_secret(), "sk-from-connect");
    }

    #[tokio::test]
    async fn reports_missing_and_unauthorized() {
        let url = stub().await;
        let client = ConnectClient::new(&url, "test-token".to_string().into()).unwrap();
        assert!(client.field("Nope", "alice", "x").await.is_err());
        assert!(client.field("Dev", "alice", "empty").await.is_err());
        let wrong = ConnectClient::new(&url, "wrong".to_string().into()).unwrap();
        assert!(
            wrong
                .field("Dev", "alice", "anthropic_api_key")
                .await
                .is_err()
        );
    }
}
