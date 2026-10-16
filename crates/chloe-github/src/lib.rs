//! GitHub App を使った認証と所属の確認。
//!
//! - アプリの JWT で、Organization へのインストールのトークン (インストールトークン) を発行する。
//!   所属の確認と、共有ワークスペースの GitHub の認証情報に使う。
//! - Device Flow で利用者を認証し、ユーザートークンを得る。個人ワークスペースの GitHub の
//!   認証情報に使う。ユーザートークンは 8 時間で失効し、リフレッシュトークンで更新する。

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chloe_secrets::{ExposeSecret, Resolver, SecretString};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

/// 所属の確認の結果を覚えておく時間。
const MEMBERSHIP_TTL: Duration = Duration::from_secs(300);
/// インストールトークンは、失効までこれより短くなったら発行し直す。
const TOKEN_REFRESH_MARGIN: Duration = Duration::from_secs(300);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("GitHub: {0}")]
    Http(#[from] reqwest::Error),
    #[error("GitHub API {0}: {1}")]
    Api(String, String),
    #[error("invalid GitHub App configuration: {0}")]
    Config(String),
    #[error(transparent)]
    Secrets(#[from] chloe_secrets::Error),
}

/// `~/.config/chloe/github-app.toml` の形。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    pub app_id: u64,
    pub client_id: String,
    pub organization: String,
    /// 秘密鍵 (PEM) への参照 (`file:`、`op://` など)。
    pub private_key: String,
    #[serde(default = "default_api_url")]
    pub api_url: String,
    #[serde(default = "default_web_url")]
    pub web_url: String,
}

fn default_api_url() -> String {
    "https://api.github.com".into()
}

fn default_web_url() -> String {
    "https://github.com".into()
}

impl AppConfig {
    pub fn load(path: &Path) -> Result<Self, Error> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::Config(format!("{}: {e}", path.display())))?;
        toml::from_str(&text).map_err(|e| Error::Config(format!("{}: {e}", path.display())))
    }
}

/// Device Flow の始まりで利用者に見せるもの。
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

/// ユーザートークンとその期限 (UNIX 時刻の秒)。
#[derive(Clone, Serialize, Deserialize)]
pub struct UserToken {
    pub access_token: String,
    pub expires_at: Option<u64>,
    pub refresh_token: Option<String>,
    pub refresh_expires_at: Option<u64>,
}

impl UserToken {
    /// 失効まで `margin` より短いか。期限のないトークンは失効しない。
    pub fn expires_within(&self, margin: Duration) -> bool {
        self.expires_at
            .is_some_and(|at| at <= now_secs() + margin.as_secs())
    }
}

pub enum Poll {
    /// 利用者がまだ承認していない。`interval` 秒待ってから問い合わせる。
    Pending {
        interval: u64,
    },
    Token(UserToken),
    /// 利用者が拒否したか、コードが失効した。
    Failed(String),
}

pub struct GitHubApp {
    http: reqwest::Client,
    config: AppConfig,
    key: EncodingKey,
    installation: Mutex<Option<(u64, String, Instant)>>,
    memberships: Mutex<HashMap<String, (bool, Instant)>>,
}

#[derive(Deserialize)]
struct Installation {
    id: u64,
}

#[derive(Deserialize)]
struct InstallationToken {
    token: String,
    expires_at: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    expires_in: Option<u64>,
    refresh_token: Option<String>,
    refresh_token_expires_in: Option<u64>,
    error: Option<String>,
    error_description: Option<String>,
    interval: Option<u64>,
}

#[derive(Deserialize)]
struct User {
    login: String,
}

#[derive(Deserialize)]
struct TeamMembership {
    state: String,
}

#[derive(Serialize)]
struct Claims {
    iat: u64,
    exp: u64,
    iss: String,
}

impl GitHubApp {
    pub async fn new(config: AppConfig, resolver: &Resolver) -> Result<Self, Error> {
        let pem = resolver.resolve(&config.private_key).await?;
        let key = EncodingKey::from_rsa_pem(pem.expose_secret().as_bytes())
            .map_err(|e| Error::Config(format!("private key: {e}")))?;
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .user_agent("chloe")
                .build()?,
            config,
            key,
            installation: Mutex::new(None),
            memberships: Mutex::new(HashMap::new()),
        })
    }

    pub fn organization(&self) -> &str {
        &self.config.organization
    }

    /// アプリとして API を呼ぶための JWT。時計のずれを見込んで 60 秒前から有効にする。
    fn jwt(&self) -> Result<String, Error> {
        let now = now_secs();
        let claims = Claims {
            iat: now - 60,
            exp: now + 9 * 60,
            iss: self.config.app_id.to_string(),
        };
        jsonwebtoken::encode(&Header::new(Algorithm::RS256), &claims, &self.key)
            .map_err(|e| Error::Config(format!("sign JWT: {e}")))
    }

    fn api(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.config.api_url))
            .header("accept", "application/vnd.github+json")
            .header("x-github-api-version", "2022-11-28")
    }

    /// アプリをインストールしたアカウントの名前の一覧 (設定の確認用)。
    pub async fn installed_accounts(&self) -> Result<Vec<String>, Error> {
        #[derive(Deserialize)]
        struct Account {
            login: String,
        }
        #[derive(Deserialize)]
        struct Entry {
            account: Account,
        }
        let entries: Vec<Entry> = json(
            self.api(reqwest::Method::GET, "/app/installations")
                .bearer_auth(self.jwt()?),
        )
        .await?;
        Ok(entries.into_iter().map(|e| e.account.login).collect())
    }

    /// Organization へのインストールのトークン。期限が近くなるまで使い回す。
    pub async fn installation_token(&self) -> Result<SecretString, Error> {
        if let Some((_, token, expires)) = self.installation.lock().unwrap().as_ref()
            && Instant::now() + TOKEN_REFRESH_MARGIN < *expires
        {
            return Ok(token.clone().into());
        }
        let jwt = self.jwt()?;
        let path = format!("/orgs/{}/installation", self.config.organization);
        let installation: Installation =
            json(self.api(reqwest::Method::GET, &path).bearer_auth(&jwt)).await?;
        let path = format!("/app/installations/{}/access_tokens", installation.id);
        let token: InstallationToken =
            json(self.api(reqwest::Method::POST, &path).bearer_auth(&jwt)).await?;
        let lifetime = parse_rfc3339(&token.expires_at)
            .map(|at| Duration::from_secs(at.saturating_sub(now_secs())))
            .unwrap_or(Duration::from_secs(3600));
        *self.installation.lock().unwrap() = Some((
            installation.id,
            token.token.clone(),
            Instant::now() + lifetime,
        ));
        Ok(token.token.into())
    }

    /// 利用者がアプリの Organization のメンバーか。
    pub async fn is_org_member(&self, login: &str) -> Result<bool, Error> {
        let org = self.config.organization.clone();
        let path = format!("/orgs/{org}/members/{login}");
        self.cached_membership(format!("org:{org}:{login}"), || async move {
            let token = self.installation_token().await?;
            let response = self
                .api(reqwest::Method::GET, &path)
                .bearer_auth(token.expose_secret())
                .send()
                .await?;
            match response.status() {
                StatusCode::NO_CONTENT => Ok(true),
                StatusCode::NOT_FOUND | StatusCode::FOUND => Ok(false),
                status => Err(Error::Api(path.clone(), status.to_string())),
            }
        })
        .await
    }

    /// 利用者がアプリの Organization の team のメンバーか。
    pub async fn is_team_member(&self, team: &str, login: &str) -> Result<bool, Error> {
        let org = self.config.organization.clone();
        let path = format!("/orgs/{org}/teams/{team}/memberships/{login}");
        self.cached_membership(format!("team:{org}/{team}:{login}"), || async move {
            let token = self.installation_token().await?;
            let response = self
                .api(reqwest::Method::GET, &path)
                .bearer_auth(token.expose_secret())
                .send()
                .await?;
            match response.status() {
                StatusCode::OK => Ok(response.json::<TeamMembership>().await?.state == "active"),
                StatusCode::NOT_FOUND => Ok(false),
                status => Err(Error::Api(path.clone(), status.to_string())),
            }
        })
        .await
    }

    async fn cached_membership<F, Fut>(&self, key: String, check: F) -> Result<bool, Error>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<bool, Error>>,
    {
        let key = key.to_ascii_lowercase();
        if let Some((member, checked)) = self.memberships.lock().unwrap().get(&key)
            && checked.elapsed() < MEMBERSHIP_TTL
        {
            return Ok(*member);
        }
        let member = check().await?;
        self.memberships
            .lock()
            .unwrap()
            .insert(key, (member, Instant::now()));
        Ok(member)
    }

    /// Device Flow を始める。
    pub async fn start_device_flow(&self) -> Result<DeviceCode, Error> {
        let url = format!("{}/login/device/code", self.config.web_url);
        json(
            self.http
                .post(url)
                .header("accept", "application/json")
                .form(&[("client_id", self.config.client_id.as_str())]),
        )
        .await
    }

    /// 利用者が承認したかを一度だけ問い合わせる。
    pub async fn poll_device_flow(&self, device: &DeviceCode) -> Result<Poll, Error> {
        let response = self
            .token_request(&[
                ("client_id", self.config.client_id.as_str()),
                ("device_code", device.device_code.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .await?;
        Ok(match response.error.as_deref() {
            None => Poll::Token(user_token(response)?),
            Some("authorization_pending") => Poll::Pending {
                interval: device.interval,
            },
            Some("slow_down") => Poll::Pending {
                interval: response.interval.unwrap_or(device.interval + 5),
            },
            Some(error) => Poll::Failed(
                response
                    .error_description
                    .unwrap_or_else(|| error.to_string()),
            ),
        })
    }

    /// リフレッシュトークンでユーザートークンを更新する。Device Flow で得たトークンは
    /// client secret なしで更新できる。
    pub async fn refresh_user_token(&self, refresh_token: &str) -> Result<UserToken, Error> {
        let response = self
            .token_request(&[
                ("client_id", self.config.client_id.as_str()),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
            ])
            .await?;
        if let Some(error) = response.error {
            return Err(Error::Api("refresh".into(), error));
        }
        user_token(response)
    }

    async fn token_request(&self, form: &[(&str, &str)]) -> Result<TokenResponse, Error> {
        let url = format!("{}/login/oauth/access_token", self.config.web_url);
        json(
            self.http
                .post(url)
                .header("accept", "application/json")
                .form(form),
        )
        .await
    }

    /// ユーザートークンの持ち主のログイン名。
    pub async fn user_login(&self, access_token: &str) -> Result<String, Error> {
        let user: User = json(
            self.api(reqwest::Method::GET, "/user")
                .bearer_auth(access_token),
        )
        .await?;
        Ok(user.login)
    }
}

fn user_token(response: TokenResponse) -> Result<UserToken, Error> {
    let now = now_secs();
    let access_token = response
        .access_token
        .ok_or_else(|| Error::Api("token".into(), "no access_token in the response".into()))?;
    Ok(UserToken {
        access_token,
        expires_at: response.expires_in.map(|s| now + s),
        refresh_token: response.refresh_token,
        refresh_expires_at: response.refresh_token_expires_in.map(|s| now + s),
    })
}

async fn json<T: serde::de::DeserializeOwned>(
    request: reqwest::RequestBuilder,
) -> Result<T, Error> {
    let response = request.send().await?;
    let status = response.status();
    let url = response.url().path().to_string();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(Error::Api(
            url,
            format!("{status}: {}", body.chars().take(200).collect::<String>()),
        ));
    }
    Ok(response.json().await?)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// GitHub が返す `2026-10-16T12:00:00Z` の形の時刻を UNIX 時刻の秒にする。
fn parse_rfc3339(s: &str) -> Option<u64> {
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>());
    let (year, month, day) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);
    let mut t = time.split(':').map(|p| p.parse::<i64>());
    let (hour, minute, second) = (t.next()?.ok()?, t.next()?.ok()?, t.next()?.ok()?);
    // 1970-01-01 からの日数 (Howard Hinnant の days_from_civil)。
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    u64::try_from(days * 86400 + hour * 3600 + minute * 60 + second).ok()
}

#[cfg(test)]
mod tests;
