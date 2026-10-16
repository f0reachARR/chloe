//! SSH の利用者の認証。
//!
//! SSH ユーザー名は `login` か `login+workspace` の形で、`login` を GitHub のログイン名として扱う。
//! 提示された公開鍵を、GitHub に登録された鍵 (`https://github.com/{login}.keys`) と照合する。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use russh::keys::PublicKey;

/// GitHub の公開鍵を覚えておく時間。
const KEY_CACHE_TTL: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshUser {
    pub login: String,
    pub workspace: Option<String>,
}

/// SSH ユーザー名を GitHub のログイン名とワークスペースに分ける。
pub fn parse_user(user: &str) -> Result<SshUser> {
    let (login, workspace) = match user.split_once('+') {
        Some((login, workspace)) => (login, Some(workspace)),
        None => (user, None),
    };
    // GitHub のログイン名は英数字とハイフンだけからなる。URL に埋め込むので厳しく確かめる。
    if login.is_empty()
        || login.len() > 39
        || !login.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        bail!("invalid GitHub login: {login:?}");
    }
    if workspace.is_some_and(|w| w.is_empty()) {
        bail!("empty workspace name");
    }
    Ok(SshUser {
        login: login.to_string(),
        workspace: workspace.map(str::to_string),
    })
}

/// GitHub に登録された公開鍵を取得して照合する。
pub struct GitHubKeys {
    client: reqwest::Client,
    base_url: String,
    cache: Mutex<HashMap<String, (Instant, Vec<PublicKey>)>>,
}

impl GitHubKeys {
    pub fn new(base_url: &str) -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .user_agent("chloe-gateway")
                .build()?,
            base_url: base_url.trim_end_matches('/').to_string(),
            cache: Mutex::new(HashMap::new()),
        })
    }

    /// `key` が `login` の GitHub アカウントに登録された鍵か。
    pub async fn is_registered(&self, login: &str, key: &PublicKey) -> Result<bool> {
        let keys = self.keys(login).await?;
        Ok(keys.iter().any(|k| k.key_data() == key.key_data()))
    }

    async fn keys(&self, login: &str) -> Result<Vec<PublicKey>> {
        let cache_key = login.to_ascii_lowercase();
        if let Some((fetched, keys)) = self.cache.lock().unwrap().get(&cache_key)
            && fetched.elapsed() < KEY_CACHE_TTL
        {
            return Ok(keys.clone());
        }
        let url = format!("{}/{login}.keys", self.base_url);
        let body = self
            .client
            .get(&url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .with_context(|| format!("fetch {url}"))?
            .text()
            .await?;
        let keys = parse_keys(&body);
        self.cache
            .lock()
            .unwrap()
            .insert(cache_key, (Instant::now(), keys.clone()));
        Ok(keys)
    }
}

/// `authorized_keys` と同じ形式の行から公開鍵を読む。読めない行は飛ばす。
fn parse_keys(body: &str) -> Vec<PublicKey> {
    body.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| PublicKey::from_openssh(line).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_user_and_workspace() {
        assert_eq!(
            parse_user("alice").unwrap(),
            SshUser {
                login: "alice".into(),
                workspace: None
            }
        );
        assert_eq!(
            parse_user("alice+myorg/api").unwrap().workspace.as_deref(),
            Some("myorg/api")
        );
        assert!(parse_user("../etc").is_err());
        assert!(parse_user("alice+").is_err());
        assert!(parse_user("").is_err());
    }

    #[test]
    fn parses_github_keys() {
        let body = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGwRxCZHvlSy0SMhrWa9sTKUJpPyTpyM+O3ux5V0pNqH\n\nnot a key\n";
        assert_eq!(parse_keys(body).len(), 1);
    }
}
