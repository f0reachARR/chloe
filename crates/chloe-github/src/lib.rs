//! GitHub App を使った認証と所属の確認。
//!
//! - アプリの JWT で、アプリをインストールした Organization ごとにインストールのトークン
//!   (インストールトークン) を発行する。所属と役割の確認と、共有ワークスペースの GitHub の
//!   認証情報に使う。
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
/// アプリをインストールした Organization の一覧を覚えておく時間。
const INSTALLATIONS_TTL: Duration = Duration::from_secs(300);
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
    #[error("the GitHub App is not installed on {0}")]
    NotInstalled(String),
}

/// `~/.config/chloe/github-app.toml` の形。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    pub app_id: u64,
    pub client_id: String,
    /// 使う Organization を一つに限るときに指定する (`organizations` の一つだけの書き方)。
    #[serde(default)]
    pub organization: Option<String>,
    /// 使う Organization を限るときに指定する。`organization` とどちらもなければ、アプリを
    /// インストールしたすべての Organization を使う。アプリを公開して (だれでもインストール
    /// できるようにして) 使うときは、ここで限る。
    #[serde(default)]
    pub organizations: Option<Vec<String>>,
    /// 秘密鍵 (PEM) への参照 (`file:`、`op://` など)。
    pub private_key: String,
    /// client secret への参照 (`file:`、`env:`、`op://` など)。Web の画面のログイン (OAuth の
    /// Web のフロー) に使う。なければ Web のフローを使わない (Device Flow には要らない)。
    #[serde(default)]
    pub client_secret: Option<String>,
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
    /// 設定で使う Organization を限っていれば、`org` がそのどれかか。限っていなければ `true`。
    fn allows(&self, org: &str) -> bool {
        let listed: Vec<&String> = self
            .organization
            .iter()
            .chain(self.organizations.iter().flatten())
            .collect();
        listed.is_empty() || listed.iter().any(|o| o.eq_ignore_ascii_case(org))
    }

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

    /// まだ使えるか。失効していても、リフレッシュトークンが有効なら更新して使える。
    pub fn is_usable(&self) -> bool {
        let now = now_secs();
        let alive = |at: Option<u64>| at.is_none_or(|at| at > now);
        match &self.refresh_token {
            Some(_) => alive(self.refresh_expires_at),
            None => alive(self.expires_at),
        }
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
    client_secret: Option<SecretString>,
    /// アプリをインストールした Organization (小文字) とインストールの ID、調べた時刻。
    installations: Mutex<Option<(HashMap<String, u64>, Instant)>>,
    /// インストールと範囲ごとのインストールトークンと、その期限。
    tokens: Mutex<HashMap<String, (String, Instant)>>,
    /// 所属の確認の結果 (役割。所属していなければ `None`) と、確かめた時刻。
    memberships: Mutex<HashMap<String, (Option<String>, Instant)>>,
}

/// インストールトークンの範囲。既定ではインストールのすべてのリポジトリと権限。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenScope {
    /// リポジトリの名前 (Organization の名前を含まない)。`None` ならすべて。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repositories: Option<Vec<String>>,
    /// 権限の名前と水準 (`contents = "read"` など)。`None` ならインストールの権限すべて。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<std::collections::BTreeMap<String, String>>,
}

/// Organization での役割。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrgRole {
    Admin,
    Member,
}

/// team での役割。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeamRole {
    Maintainer,
    Member,
}

#[derive(Deserialize)]
struct InstallationEntry {
    id: u64,
    account: Account,
}

#[derive(Deserialize)]
struct Account {
    login: String,
    #[serde(rename = "type")]
    kind: String,
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

/// Organization と team の所属 (`/orgs/{org}/memberships/{user}`、
/// `/orgs/{org}/teams/{team}/memberships/{user}`)。
#[derive(Deserialize)]
struct Membership {
    state: String,
    role: String,
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
        let client_secret = match &config.client_secret {
            Some(reference) => Some(resolver.resolve(reference).await?),
            None => None,
        };
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .user_agent("chloe")
                .build()?,
            config,
            key,
            client_secret,
            installations: Mutex::new(None),
            tokens: Mutex::new(HashMap::new()),
            memberships: Mutex::new(HashMap::new()),
        })
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

    /// アプリをインストールした Organization (小文字) とインストールの ID。
    /// 設定に `organization` があれば、その Organization だけを使う。
    async fn installations(&self) -> Result<HashMap<String, u64>, Error> {
        if let Some((installations, checked)) = self.installations.lock().unwrap().as_ref()
            && checked.elapsed() < INSTALLATIONS_TTL
        {
            return Ok(installations.clone());
        }
        let jwt = self.jwt()?;
        let mut installations = HashMap::new();
        for page in 1.. {
            let path = format!("/app/installations?per_page=100&page={page}");
            let entries: Vec<InstallationEntry> =
                json(self.api(reqwest::Method::GET, &path).bearer_auth(&jwt)).await?;
            let last = entries.len() < 100;
            for entry in entries {
                let org = entry.account.login.to_ascii_lowercase();
                let wanted = self.config.allows(&org);
                if entry.account.kind == "Organization" && wanted {
                    installations.insert(org, entry.id);
                }
            }
            if last {
                break;
            }
        }
        *self.installations.lock().unwrap() = Some((installations.clone(), Instant::now()));
        Ok(installations)
    }

    /// アプリをインストールした Organization の名前 (小文字、並べたもの)。
    pub async fn organizations(&self) -> Result<Vec<String>, Error> {
        let mut orgs: Vec<String> = self.installations().await?.into_keys().collect();
        orgs.sort();
        Ok(orgs)
    }

    /// アプリを `org` にインストールしているか。
    pub async fn is_installed(&self, org: &str) -> Result<bool, Error> {
        Ok(self
            .installations()
            .await?
            .contains_key(&org.to_ascii_lowercase()))
    }

    /// `org` へのインストールの、`scope` の範囲のトークン。期限が近くなるまで使い回す。
    pub async fn installation_token(
        &self,
        org: &str,
        scope: &TokenScope,
    ) -> Result<SecretString, Error> {
        let id = *self
            .installations()
            .await?
            .get(&org.to_ascii_lowercase())
            .ok_or_else(|| Error::NotInstalled(org.to_string()))?;
        let body = serde_json::to_string(scope).unwrap_or_default();
        let cache_key = format!("{id}:{body}");
        if let Some((token, expires)) = self.tokens.lock().unwrap().get(&cache_key)
            && Instant::now() + TOKEN_REFRESH_MARGIN < *expires
        {
            return Ok(token.clone().into());
        }
        let path = format!("/app/installations/{id}/access_tokens");
        let mut request = self
            .api(reqwest::Method::POST, &path)
            .bearer_auth(self.jwt()?);
        if *scope != TokenScope::default() {
            request = request
                .header("content-type", "application/json")
                .body(body);
        }
        let token: InstallationToken = json(request).await?;
        let lifetime = parse_rfc3339(&token.expires_at)
            .map(|at| Duration::from_secs(at.saturating_sub(now_secs())))
            .unwrap_or(Duration::from_secs(3600));
        self.tokens
            .lock()
            .unwrap()
            .insert(cache_key, (token.token.clone(), Instant::now() + lifetime));
        Ok(token.token.into())
    }

    /// 利用者が、アプリをインストールしたいずれかの Organization のメンバーか。
    pub async fn is_member_of_any(&self, login: &str) -> Result<bool, Error> {
        for org in self.organizations().await? {
            if self.is_org_member(&org, login).await? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// 利用者が `org` のメンバーか。アプリをインストールしていない Organization では `false`。
    pub async fn is_org_member(&self, org: &str, login: &str) -> Result<bool, Error> {
        Ok(self.org_role(org, login).await?.is_some())
    }

    /// 利用者の `org` での役割。メンバーでなければ (招待を承諾していなければ) `None`。
    pub async fn org_role(&self, org: &str, login: &str) -> Result<Option<OrgRole>, Error> {
        let path = format!("/orgs/{org}/memberships/{login}");
        let role = self
            .membership(org, format!("org:{org}:{login}"), path)
            .await?;
        Ok(role.map(|role| match role.as_str() {
            "admin" => OrgRole::Admin,
            _ => OrgRole::Member,
        }))
    }

    /// 利用者が `org` の team のメンバーか。
    pub async fn is_team_member(&self, org: &str, team: &str, login: &str) -> Result<bool, Error> {
        Ok(self.team_role(org, team, login).await?.is_some())
    }

    /// 利用者の team での役割。メンバーでなければ `None`。
    pub async fn team_role(
        &self,
        org: &str,
        team: &str,
        login: &str,
    ) -> Result<Option<TeamRole>, Error> {
        let path = format!("/orgs/{org}/teams/{team}/memberships/{login}");
        let role = self
            .membership(org, format!("team:{org}/{team}:{login}"), path)
            .await?;
        Ok(role.map(|role| match role.as_str() {
            "maintainer" => TeamRole::Maintainer,
            _ => TeamRole::Member,
        }))
    }

    /// 所属を確かめ、有効な所属なら役割を返す。結果はしばらく覚えておく。
    async fn membership(
        &self,
        org: &str,
        key: String,
        path: String,
    ) -> Result<Option<String>, Error> {
        let key = key.to_ascii_lowercase();
        if let Some((role, checked)) = self.memberships.lock().unwrap().get(&key)
            && checked.elapsed() < MEMBERSHIP_TTL
        {
            return Ok(role.clone());
        }
        if !self.is_installed(org).await? {
            return Ok(None);
        }
        let token = self.installation_token(org, &TokenScope::default()).await?;
        let response = self
            .api(reqwest::Method::GET, &path)
            .bearer_auth(token.expose_secret())
            .send()
            .await?;
        let role = match response.status() {
            StatusCode::OK => {
                let membership: Membership = response.json().await?;
                (membership.state == "active").then_some(membership.role)
            }
            StatusCode::NOT_FOUND => None,
            status => return Err(Error::Api(path, status.to_string())),
        };
        self.memberships
            .lock()
            .unwrap()
            .insert(key, (role.clone(), Instant::now()));
        Ok(role)
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

    /// Web のフローを使えるか (client secret があるか)。
    pub fn has_web_flow(&self) -> bool {
        self.client_secret.is_some()
    }

    /// Web のフローで利用者を送る GitHub の承認の URL。`redirect_uri` は App の設定の
    /// Callback URL と一致しなければならない。
    pub fn authorize_url(&self, redirect_uri: &str, state: &str) -> Result<String, Error> {
        let url = reqwest::Url::parse_with_params(
            &format!("{}/login/oauth/authorize", self.config.web_url),
            &[
                ("client_id", self.config.client_id.as_str()),
                ("redirect_uri", redirect_uri),
                ("state", state),
            ],
        )
        .map_err(|e| Error::Config(format!("web_url: {e}")))?;
        Ok(url.into())
    }

    /// Web のフローのコールバックで受けたコードを、ユーザートークンに換える。
    pub async fn exchange_code(&self, code: &str, redirect_uri: &str) -> Result<UserToken, Error> {
        let secret = self
            .client_secret
            .as_ref()
            .ok_or_else(|| Error::Config("no client_secret for the web sign-in".into()))?;
        let response = self
            .token_request(&[
                ("client_id", self.config.client_id.as_str()),
                ("client_secret", secret.expose_secret()),
                ("code", code),
                ("redirect_uri", redirect_uri),
            ])
            .await?;
        if let Some(error) = response.error {
            return Err(Error::Api(
                "code".into(),
                response.error_description.unwrap_or(error),
            ));
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
