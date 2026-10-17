//! 管理の操作 (ワークスペース、シークレット、設定、API のトークン)。
//!
//! SSH の `chloe` コマンド ([`crate::commands`]) と HTTP の管理 API ([`crate::api`]) の両方から
//! 使う。権限の確認はここで行い、結果は型のある値で返す (表示は呼ぶ側が決める)。

use chloe_controller::catalog::{self, SecretInput, SecretSpec};
use chloe_controller::runner::{PolicyLayer, most_specific};
use chloe_controller::store::ApiToken;
use chloe_controller::workspace::{RootMode, Workspace, WorkspaceRef};
use chloe_secrets::Owner;
use serde::Serialize;

use crate::access::personal;
use crate::github_token::USER_TOKEN_SECRET;
use crate::session::Shared;

/// 操作の失敗。HTTP の API では種類からステータスを決める。
#[derive(Debug)]
pub enum Error {
    /// 指定が正しくない。
    Invalid(String),
    /// 権限がない。
    Forbidden(String),
    /// ない (権限がなくて見えないものも含む)。
    NotFound(String),
    /// すでにある。
    Conflict(String),
    Internal(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(m)
            | Self::Forbidden(m)
            | Self::NotFound(m)
            | Self::Conflict(m)
            | Self::Internal(m) => f.write_str(m),
        }
    }
}

impl From<chloe_controller::Error> for Error {
    fn from(error: chloe_controller::Error) -> Self {
        match error {
            chloe_controller::Error::AlreadyExists(m) => {
                Self::Conflict(format!("already exists: {m}"))
            }
            chloe_controller::Error::Invalid(m) => Self::Invalid(m),
            other => Self::Internal(other.to_string()),
        }
    }
}

impl From<anyhow::Error> for Error {
    fn from(error: anyhow::Error) -> Self {
        Self::Internal(format!("{error:#}"))
    }
}

impl From<chloe_secrets::Error> for Error {
    fn from(error: chloe_secrets::Error) -> Self {
        Self::Internal(error.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceInfo {
    pub name: String,
    pub root: RootMode,
    pub running: bool,
}

fn info(ws: &Workspace, running: bool) -> WorkspaceInfo {
    WorkspaceInfo {
        name: ws.display_name(),
        root: ws.root_mode,
        running,
    }
}

pub async fn list_workspaces(shared: &Shared, login: &str) -> Result<Vec<WorkspaceInfo>> {
    let workspaces = shared
        .controller
        .store()
        .list(&shared.owners_for(login).await?)
        .await?;
    let running = shared.controller.running().await?;
    Ok(workspaces
        .iter()
        .map(|ws| info(ws, running.iter().any(|r| r.id == ws.id)))
        .collect())
}

/// ワークスペースを作る。`root` がなければ、Organization と team の設定の既定を使う。
pub async fn create_workspace(
    shared: &Shared,
    login: &str,
    name: &str,
    root: Option<RootMode>,
) -> Result<WorkspaceInfo> {
    let target = WorkspaceRef::parse(name, login).map_err(|e| Error::Invalid(e.to_string()))?;
    if !shared.can_access(login, &target.owner).await? {
        return Err(Error::Forbidden(format!(
            "you are not a member of {}",
            target.owner
        )));
    }
    let root = match root {
        Some(root) => root,
        None => {
            let layers = shared
                .controller
                .store()
                .policy_layers(&target.owner)
                .await?;
            most_specific(&layers, |l| l.root.as_ref()).unwrap_or(RootMode::Ephemeral)
        }
    };
    let ws = shared
        .controller
        .store()
        .create(&target, root, login)
        .await?;
    Ok(info(&ws, false))
}

/// 利用者が入れるワークスペースを探す。入れないものは、ないものとして扱う。
pub async fn find_workspace(shared: &Shared, login: &str, name: &str) -> Result<Workspace> {
    let target = WorkspaceRef::parse(name, login).map_err(|e| Error::Invalid(e.to_string()))?;
    let not_found = || Error::NotFound(format!("no such workspace: {name}"));
    if !shared.can_access(login, &target.owner).await? {
        return Err(not_found());
    }
    shared
        .controller
        .store()
        .find(&target)
        .await?
        .ok_or_else(not_found)
}

pub async fn workspace_info(shared: &Shared, login: &str, name: &str) -> Result<WorkspaceInfo> {
    let ws = find_workspace(shared, login, name).await?;
    let running = shared.controller.running().await?;
    Ok(info(&ws, running.iter().any(|r| r.id == ws.id)))
}

pub async fn set_root(
    shared: &Shared,
    login: &str,
    name: &str,
    root: RootMode,
) -> Result<Workspace> {
    let mut ws = find_workspace(shared, login, name).await?;
    shared.controller.store().set_root_mode(ws.id, root).await?;
    ws.root_mode = root;
    Ok(ws)
}

pub async fn stop_workspace(shared: &Shared, login: &str, name: &str) -> Result<Workspace> {
    let ws = find_workspace(shared, login, name).await?;
    shared.controller.stop(&ws).await?;
    Ok(ws)
}

pub async fn remove_workspace(shared: &Shared, login: &str, name: &str) -> Result<Workspace> {
    let ws = find_workspace(shared, login, name).await?;
    shared.controller.stop(&ws).await?;
    shared.controller.store().delete(ws.id).await?;
    Ok(ws)
}

/// Organization (`org`) か team (`org/team`)。
pub fn parse_owner(arg: &str) -> Result<Owner> {
    let text = match arg.split_once('/') {
        None => format!("org:{arg}"),
        Some(_) => format!("team:{arg}"),
    };
    text.to_ascii_lowercase()
        .parse()
        .map_err(|_| Error::Invalid(format!("invalid owner: {arg} (use org or org/team)")))
}

/// シークレットの持ち主。指定がなければ利用者の個人のもの。
pub fn secret_owner(login: &str, owner: Option<&str>) -> Result<Owner> {
    match owner {
        None => Ok(personal(login)),
        Some(owner) => parse_owner(owner),
    }
}

/// 持ち主の名前を、コマンドで指定する書き方で表す。
pub fn owner_arg(owner: &Owner) -> String {
    match owner {
        Owner::User(login) => login.clone(),
        Owner::Org(org) => org.clone(),
        Owner::Team { org, team } => format!("{org}/{team}"),
    }
}

/// 利用者が登録や削除をできるシークレットの名前か。
pub fn check_secret_name(name: &str) -> Result<()> {
    catalog::check_name(name).map_err(Error::Invalid)?;
    if name == USER_TOKEN_SECRET {
        return Err(Error::Invalid(format!(
            "{name} is managed by chloe github login"
        )));
    }
    Ok(())
}

/// シークレットの名前の一覧。名前だけなので、その持ち主のワークスペースに入れる人は見られる。
pub async fn list_secrets(shared: &Shared, login: &str, owner: &Owner) -> Result<Vec<String>> {
    if !shared.can_access(login, owner).await? {
        return Err(Error::Forbidden(format!(
            "you are not a member of {}",
            owner_arg(owner)
        )));
    }
    Ok(shared.controller.secrets().list(owner).await?)
}

pub async fn check_secret_manager(shared: &Shared, login: &str, owner: &Owner) -> Result<()> {
    if shared.can_manage_secrets(login, owner).await? {
        Ok(())
    } else {
        Err(Error::Forbidden(format!(
            "you cannot change the secrets of {} (organization admins, and team maintainers for a team, can)",
            owner_arg(owner)
        )))
    }
}

/// 登録したシークレットの定義。組み込みの種類なら `builtin` が真になる。
#[derive(Debug, Serialize)]
pub struct SecretView {
    pub name: String,
    pub owner: String,
    pub builtin: bool,
    pub spec: SecretSpec,
}

/// シークレットの定義を見る。値は返さないので、その持ち主のワークスペースに入れる人は見られる。
pub async fn show_secret(
    shared: &Shared,
    login: &str,
    owner: &Owner,
    name: &str,
) -> Result<SecretView> {
    check_secret_name(name)?;
    if !shared.can_access(login, owner).await? {
        return Err(Error::Forbidden(format!(
            "you are not a member of {}",
            owner_arg(owner)
        )));
    }
    let Some((_, stored)) = shared
        .controller
        .secrets()
        .get_with_spec(owner, name)
        .await?
    else {
        return Err(Error::NotFound(format!("no such secret: {name}")));
    };
    let spec = stored_spec(name, stored.as_deref())?;
    Ok(SecretView {
        name: name.to_string(),
        owner: owner_arg(owner),
        builtin: stored.is_none(),
        spec,
    })
}

pub async fn set_secret(
    shared: &Shared,
    login: &str,
    owner: &Owner,
    name: &str,
    input: SecretInput,
) -> Result<()> {
    check_secret_name(name)?;
    check_secret_manager(shared, login, owner).await?;
    let secrets = shared.controller.secrets();
    // 値だけを変えるときは、同じ持ち主に置いた定義を使う。
    let existing = secrets
        .get_with_spec(owner, name)
        .await?
        .map(|(_, stored)| stored);
    let (value, spec) = prepare_secret(name, input, existing)?;
    secrets
        .set_with_spec(owner, name, &value.into(), spec.as_deref())
        .await?;
    shared.refresh_running_under(owner).await;
    Ok(())
}

/// 登録する値 (保存する形) と定義の文字列を決める。
///
/// `existing` は同じ持ち主にある行の定義 (行がなければ `None`、組み込みの種類なら `Some(None)`)。
/// 定義を渡さなければ、ある行の定義をそのままの文字列で使い続ける (定義は追加認証データに入るため)。
fn prepare_secret(
    name: &str,
    input: SecretInput,
    existing: Option<Option<String>>,
) -> Result<(String, Option<String>)> {
    let (spec, values) = input.into_parts().map_err(Error::Invalid)?;
    let (spec, text) = match spec {
        Some(_) if catalog::preset(name).is_some() => {
            return Err(Error::Invalid(format!(
                "{name} is a built-in secret; it takes only a value"
            )));
        }
        Some(spec) => {
            spec.validate().map_err(Error::Invalid)?;
            let text = serde_json::to_string(&spec).map_err(|e| Error::Invalid(e.to_string()))?;
            (spec, Some(text))
        }
        None => match existing {
            Some(stored) => (stored_spec(name, stored.as_deref())?, stored),
            None => match catalog::preset(name) {
                Some(spec) => (spec, None),
                None => {
                    return Err(Error::Invalid(format!(
                        "{name} has no definition for this owner yet; pass one with --spec"
                    )));
                }
            },
        },
    };
    let value = spec.encode_values(&values).map_err(Error::Invalid)?;
    Ok((value, text))
}

fn stored_spec(name: &str, stored: Option<&str>) -> Result<SecretSpec> {
    match stored {
        Some(text) => serde_json::from_str(text)
            .map_err(|e| Error::Invalid(format!("the definition of {name} is broken: {e}"))),
        None => {
            catalog::preset(name).ok_or_else(|| Error::Invalid(format!("{name} has no definition")))
        }
    }
}

pub async fn remove_secret(shared: &Shared, login: &str, owner: &Owner, name: &str) -> Result<()> {
    check_secret_name(name)?;
    check_secret_manager(shared, login, owner).await?;
    if !shared.controller.secrets().delete(owner, name).await? {
        return Err(Error::NotFound(format!("no such secret: {name}")));
    }
    shared.refresh_running_under(owner).await;
    Ok(())
}

pub async fn check_policy_admin(shared: &Shared, login: &str, owner: &Owner) -> Result<()> {
    if shared.can_manage_policy(login, owner).await? {
        Ok(())
    } else {
        Err(Error::Forbidden(format!(
            "only the administrators of the organization can change the settings of {}",
            owner_arg(owner)
        )))
    }
}

/// 設定を読む。設定 (宛先と規則) は秘密ではないので、その持ち主のワークスペースに入れる人と
/// 管理者は見られる。
pub async fn show_policy(
    shared: &Shared,
    login: &str,
    owner: &Owner,
) -> Result<Option<PolicyLayer>> {
    if !shared.can_access(login, owner).await? && !shared.can_manage_policy(login, owner).await? {
        return Err(Error::Forbidden(format!(
            "you are not a member of {}",
            owner_arg(owner)
        )));
    }
    Ok(shared.controller.store().policy(owner).await?)
}

/// 設定を置き換え、動いているランナーに送る。送ったランナーの数を返す。
pub async fn set_policy(
    shared: &Shared,
    login: &str,
    owner: &Owner,
    layer: &PolicyLayer,
) -> Result<usize> {
    check_policy_admin(shared, login, owner).await?;
    shared
        .controller
        .store()
        .set_policy(owner, layer, login)
        .await?;
    Ok(shared.refresh_running_under(owner).await)
}

pub async fn remove_policy(shared: &Shared, login: &str, owner: &Owner) -> Result<usize> {
    check_policy_admin(shared, login, owner).await?;
    if !shared.controller.store().delete_policy(owner).await? {
        return Err(Error::NotFound(format!(
            "no settings for {}",
            owner_arg(owner)
        )));
    }
    Ok(shared.refresh_running_under(owner).await)
}

/// `30d`、`12h` のような期間を秒にする。
pub fn parse_duration(text: &str) -> Result<i64> {
    let invalid = || Error::Invalid(format!("invalid duration: {text} (use e.g. 30d or 12h)"));
    let (number, unit) = text.split_at(text.len().saturating_sub(1));
    let number: i64 = number.parse().map_err(|_| invalid())?;
    let unit = match unit {
        "d" => 24 * 60 * 60,
        "h" => 60 * 60,
        _ => return Err(invalid()),
    };
    if number <= 0 {
        return Err(invalid());
    }
    Ok(number * unit)
}

/// 管理 API のトークンを作る。トークンの値はこのときにしか見えない。
pub async fn create_token(
    shared: &Shared,
    login: &str,
    name: &str,
    ttl_secs: Option<i64>,
) -> Result<(i64, String)> {
    if name.is_empty() || name.len() > 64 {
        return Err(Error::Invalid("a token name is 1-64 characters".into()));
    }
    Ok(shared
        .controller
        .store()
        .create_api_token(login, name, ttl_secs)
        .await?)
}

pub async fn list_tokens(shared: &Shared, login: &str) -> Result<Vec<ApiToken>> {
    Ok(shared.controller.store().api_tokens(login).await?)
}

pub async fn revoke_token(shared: &Shared, login: &str, id: i64) -> Result<()> {
    if shared
        .controller
        .store()
        .revoke_api_token(login, id)
        .await?
    {
        Ok(())
    } else {
        Err(Error::NotFound(format!("no such token: {id}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_owners() {
        assert_eq!(parse_owner("Acme").unwrap(), Owner::Org("acme".into()));
        assert_eq!(
            parse_owner("acme/dev").unwrap(),
            Owner::Team {
                org: "acme".into(),
                team: "dev".into()
            }
        );
        assert!(parse_owner("../x").is_err());
        assert!(parse_owner("a/b/c").is_err());
        assert_eq!(owner_arg(&parse_owner("acme/dev").unwrap()), "acme/dev");
    }

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration("30d").unwrap(), 30 * 86400);
        assert_eq!(parse_duration("12h").unwrap(), 12 * 3600);
        assert!(parse_duration("0d").is_err());
        assert!(parse_duration("5m").is_err());
        assert!(parse_duration("d").is_err());
    }

    const HF: &str = r#"
        env = ["HF_TOKEN"]
        placeholder = "hf_{id}"
        value = "hf-real"
        [[inject]]
        hosts = ["huggingface.co"]
        header = "authorization"
    "#;

    fn input(text: &str) -> SecretInput {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn prepares_secrets() {
        // 組み込みの種類は値だけで登録できる。
        let (value, spec) =
            prepare_secret("anthropic_api_key", SecretInput::value("sk"), None).unwrap();
        assert_eq!((value.as_str(), spec), ("sk", None));
        // 組み込みの種類には定義を渡せない。
        assert!(prepare_secret("anthropic_api_key", input(HF), None).is_err());
        // 定義のない名前は、値だけでは登録できない。
        assert!(prepare_secret("hf_token", SecretInput::value("x"), None).is_err());

        let (value, spec) = prepare_secret("hf_token", input(HF), None).unwrap();
        assert_eq!(value, "hf-real");
        let spec = spec.unwrap();
        // 値だけを変えるときは、ある定義をそのままの文字列で使う。
        let (value, kept) = prepare_secret(
            "hf_token",
            SecretInput::value("hf-new"),
            Some(Some(spec.clone())),
        )
        .unwrap();
        assert_eq!((value.as_str(), kept), ("hf-new", Some(spec)));

        // 欄がいくつもあるシークレットの値だけを変える。
        let aws = r#"
            [fields.access_key_id]
            env = ["AWS_ACCESS_KEY_ID"]
            value = "AKID"
            [fields.secret_access_key]
            value = "s"
            [[sign]]
            scheme = "aws-sigv4"
            hosts = ["*.amazonaws.com"]
        "#;
        let (_, spec) = prepare_secret("aws", input(aws), None).unwrap();
        let values_only = r#"
            [fields.access_key_id]
            value = "AKID2"
            [fields.secret_access_key]
            value = "s2"
        "#;
        let (value, kept) = prepare_secret("aws", input(values_only), Some(spec.clone())).unwrap();
        assert_eq!(kept, spec);
        assert!(value.contains("AKID2"));
        assert!(prepare_secret("aws", SecretInput::value("x"), Some(spec)).is_err());
    }

    #[test]
    fn reserves_internal_names() {
        assert!(check_secret_name(USER_TOKEN_SECRET).is_err());
        assert!(check_secret_name("Bad-Name").is_err());
        check_secret_name("hf_token").unwrap();
    }
}
