//! SSH の利用者の認証。
//!
//! SSH ユーザー名は `name` か `name+workspace` の形。`name` が GitHub のログイン名なら、その
//! 利用者として扱い、提示された公開鍵を GitHub に登録された鍵 (`https://github.com/{login}.keys`)
//! と照合する。`name` が予約したユーザー名 (既定は `chloe`) か GitHub に存在しない名前なら、
//! 誰でもよい名前として扱い、利用者を公開鍵の結び付けか Device Flow で決める。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use russh::keys::{HashAlg, PublicKey};

/// GitHub の公開鍵を覚えておく時間。
const KEY_CACHE_TTL: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshUser {
    /// `+` の前の部分。GitHub のログイン名か、予約したユーザー名か、GitHub にない名前。
    pub name: String,
    pub workspace: Option<String>,
}

/// SSH ユーザー名を、名前とワークスペースに分ける。
pub fn parse_user(user: &str) -> Result<SshUser> {
    let (name, workspace) = match user.split_once('+') {
        Some((name, workspace)) => (name, Some(workspace)),
        None => (user, None),
    };
    // GitHub のログイン名は英数字とハイフンだけからなる。URL に埋め込むので厳しく確かめる。
    if !is_login(name) {
        bail!("invalid GitHub login: {name:?}");
    }
    if workspace.is_some_and(|w| w.is_empty()) {
        bail!("empty workspace name");
    }
    Ok(SshUser {
        name: name.to_string(),
        workspace: workspace.map(str::to_string),
    })
}

/// GitHub のログイン名として書ける形か。
pub fn is_login(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 39
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// SSH ユーザー名の名前が指す利用者。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claim {
    /// GitHub のログイン名 (小文字)。
    Login(String),
    /// 予約したユーザー名か、GitHub に存在しない名前。利用者は鍵か Device Flow で決まる。
    Anyone,
}

/// 公開鍵の確認の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyOutcome {
    Accept,
    /// 鍵は通すが、Device Flow を済ませないと入れない。
    DeviceFlow,
    Reject,
}

/// 公開鍵の確認の結果を決める。
///
/// - `owner`：鍵の持ち主。GitHub に登録された鍵か、Device Flow で結び付けた鍵なら分かる。
/// - `device_flow`：Device Flow が使えるか (GitHub App を設定しているか)。使えなければ、
///   GitHub のログイン名と登録された鍵だけで入れる。
/// - `token_usable`：持ち主の GitHub のユーザートークンがまだ使えるか。
pub fn key_outcome(
    claim: &Claim,
    owner: Option<&str>,
    device_flow: bool,
    token_usable: bool,
) -> KeyOutcome {
    match claim {
        Claim::Login(login) if owner != Some(login.as_str()) => KeyOutcome::Reject,
        Claim::Login(_) if !device_flow || token_usable => KeyOutcome::Accept,
        Claim::Login(_) => KeyOutcome::DeviceFlow,
        Claim::Anyone if !device_flow => KeyOutcome::Reject,
        Claim::Anyone if owner.is_some() && token_usable => KeyOutcome::Accept,
        Claim::Anyone => KeyOutcome::DeviceFlow,
    }
}

/// 公開鍵の指紋 (`SHA256:...`)。
pub fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

/// 公開鍵の OpenSSH の形 (コメントを除く)。
pub fn openssh(key: &PublicKey) -> String {
    let mut key = key.clone();
    key.set_comment("");
    key.to_openssh().unwrap_or_default()
}

/// ログイン名ごとの鍵と取得した時刻。GitHub に存在しない名前は `None`。
type KeyCache = HashMap<String, (Instant, Option<Vec<PublicKey>>)>;

/// GitHub に登録された公開鍵を取得して照合する。
pub struct GitHubKeys {
    client: reqwest::Client,
    base_url: String,
    cache: Mutex<KeyCache>,
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

    /// `login` が GitHub に存在するか。
    pub async fn exists(&self, login: &str) -> Result<bool> {
        Ok(self.keys(login).await?.is_some())
    }

    /// `key` が `login` の GitHub アカウントに登録された鍵か。
    pub async fn is_registered(&self, login: &str, key: &PublicKey) -> Result<bool> {
        let keys = self.keys(login).await?.unwrap_or_default();
        Ok(keys.iter().any(|k| k.key_data() == key.key_data()))
    }

    /// 登録された鍵。GitHub に存在しない名前なら `None`。取得に失敗したら (存在しないとは
    /// 決めつけずに) 誤りを返す。
    async fn keys(&self, login: &str) -> Result<Option<Vec<PublicKey>>> {
        let cache_key = login.to_ascii_lowercase();
        if let Some((fetched, keys)) = self.cache.lock().unwrap().get(&cache_key)
            && fetched.elapsed() < KEY_CACHE_TTL
        {
            return Ok(keys.clone());
        }
        let url = format!("{}/{login}.keys", self.base_url);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("fetch {url}"))?;
        let keys = if response.status() == reqwest::StatusCode::NOT_FOUND {
            None
        } else {
            let body = response
                .error_for_status()
                .with_context(|| format!("fetch {url}"))?
                .text()
                .await?;
            Some(parse_keys(&body))
        };
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

    const KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGwRxCZHvlSy0SMhrWa9sTKUJpPyTpyM+O3ux5V0pNqH";

    #[test]
    fn parses_user_and_workspace() {
        assert_eq!(
            parse_user("alice").unwrap(),
            SshUser {
                name: "alice".into(),
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
        let body = format!("{KEY}\n\nnot a key\n");
        assert_eq!(parse_keys(&body).len(), 1);
    }

    #[test]
    fn formats_keys() {
        let key = PublicKey::from_openssh(&format!("{KEY} alice@laptop")).unwrap();
        assert!(fingerprint(&key).starts_with("SHA256:"));
        assert_eq!(openssh(&key), KEY);
    }

    #[test]
    fn decides_by_key_owner_and_token() {
        use KeyOutcome::*;
        let alice = Claim::Login("alice".into());
        // GitHub のログイン名では、その利用者の鍵でなければ断る。
        assert_eq!(key_outcome(&alice, None, true, false), Reject);
        assert_eq!(key_outcome(&alice, Some("bob"), true, true), Reject);
        // 鍵が合っても、トークンがなければ Device Flow に進める。
        assert_eq!(key_outcome(&alice, Some("alice"), true, false), DeviceFlow);
        assert_eq!(key_outcome(&alice, Some("alice"), true, true), Accept);
        // Device Flow が使えなければ、鍵だけで入れる。
        assert_eq!(key_outcome(&alice, Some("alice"), false, false), Accept);

        // 誰でもよい名前では、結び付いた鍵とトークンがあれば入れ、なければ Device Flow に進める。
        assert_eq!(key_outcome(&Claim::Anyone, None, true, false), DeviceFlow);
        assert_eq!(
            key_outcome(&Claim::Anyone, Some("alice"), true, false),
            DeviceFlow
        );
        assert_eq!(
            key_outcome(&Claim::Anyone, Some("alice"), true, true),
            Accept
        );
        assert_eq!(
            key_outcome(&Claim::Anyone, Some("alice"), false, true),
            Reject
        );
    }
}
