//! 管理の操作 (ワークスペース、シークレット、設定、API のトークン)。
//!
//! SSH の `chloe` コマンド ([`crate::commands`]) と HTTP の管理 API ([`crate::api`]) の両方から
//! 使う。権限の確認はここで行い、結果は型のある値で返す (表示は呼ぶ側が決める)。

use chloe_controller::asks::{Ask, AskBody, AskStatus, Resolution};
use chloe_controller::catalog::{self, SecretInput, SecretSpec};
use chloe_controller::runner::{
    PolicyLayer, RuleOrigin, WorkspacePolicy, label_rules, most_specific,
};
use chloe_controller::store::{ApiToken, SshKey};
use chloe_controller::workspace::{
    DiskSizes, IdleStop, MachineSize, RootMode, Workspace, WorkspaceRef,
};
use chloe_policy::{Action, PathRuleConfig};
use chloe_secrets::{ExposeSecret, Owner};
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
    /// ワークスペースのディスクの大きさ (GiB)。既定を当てはめたもの。
    pub workspace_disk_gib: u32,
    /// Docker のディスクの大きさ (GiB)。既定を当てはめたもの。
    pub docker_disk_gib: u32,
    /// VM の vCPU の数。既定を当てはめたもの。
    pub vcpus: u32,
    /// VM のメモリ (MiB)。既定を当てはめたもの。
    pub memory_mib: u32,
    /// 最後の接続が閉じてから止めるまでの秒数。既定を当てはめたもの。止めないなら `None`。
    pub idle_timeout_secs: Option<u64>,
}

fn info(shared: &Shared, ws: &Workspace, running: bool) -> WorkspaceInfo {
    let disks = shared.controller.disks();
    let machine = shared.controller.machine();
    WorkspaceInfo {
        name: ws.display_name(),
        root: ws.root_mode,
        running,
        workspace_disk_gib: disks.workspace_gib(ws.disks),
        docker_disk_gib: disks.docker_gib(ws.disks),
        vcpus: machine.vcpus(ws.machine),
        memory_mib: machine.memory_mib(ws.machine),
        idle_timeout_secs: ws
            .idle_stop
            .timeout(shared.controller.idle_timeout())
            .map(|d| d.as_secs()),
    }
}

/// 作るワークスペースの設定。書かなかったものは既定を使う。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NewWorkspace {
    /// なければ、持ち主 (個人、Organization と team) の設定の既定。
    pub root: Option<RootMode>,
    pub disks: DiskSizes,
    pub machine: MachineSize,
    pub idle_stop: IdleStop,
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
        .map(|ws| info(shared, ws, running.iter().any(|r| r.id == ws.id)))
        .collect())
}

/// ワークスペースを作る。書かなかった設定は、ルートの扱いなら持ち主 (個人、Organization と
/// team) の設定の既定、ほかはゲートウェイの設定の既定を使う。
pub async fn create_workspace(
    shared: &Shared,
    login: &str,
    name: &str,
    options: NewWorkspace,
) -> Result<WorkspaceInfo> {
    let target = WorkspaceRef::parse(name, login).map_err(|e| Error::Invalid(e.to_string()))?;
    if !shared.can_access(login, &target.owner).await? {
        return Err(Error::Forbidden(format!(
            "you are not a member of {}",
            target.owner
        )));
    }
    shared.controller.disks().validate_new(options.disks)?;
    shared.controller.machine().validate(options.machine)?;
    let root = match options.root {
        Some(root) => root,
        None => {
            let layers = shared.controller.owner_layers(&target.owner).await?;
            most_specific(&layers, |l| l.root.as_ref()).unwrap_or(RootMode::Ephemeral)
        }
    };
    let ws = shared
        .controller
        .store()
        .create(
            &target,
            root,
            options.disks,
            options.machine,
            options.idle_stop,
            login,
        )
        .await?;
    Ok(info(shared, &ws, false))
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
    Ok(info(shared, &ws, running.iter().any(|r| r.id == ws.id)))
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

/// ディスクの大きさを変える。書かなかったほうは変えない。小さくはできず、次の起動から効く。
pub async fn set_disks(
    shared: &Shared,
    login: &str,
    name: &str,
    disks: DiskSizes,
) -> Result<Workspace> {
    let mut ws = find_workspace(shared, login, name).await?;
    let disks = shared.controller.disks().validate_change(ws.disks, disks)?;
    shared
        .controller
        .store()
        .set_disk_sizes(ws.id, disks)
        .await?;
    ws.disks = disks;
    Ok(ws)
}

/// VM の vCPU の数とメモリを変える。書かなかったほうは変えず、次の起動から効く。
pub async fn set_machine(
    shared: &Shared,
    login: &str,
    name: &str,
    machine: MachineSize,
) -> Result<Workspace> {
    let mut ws = find_workspace(shared, login, name).await?;
    let machine = shared
        .controller
        .machine()
        .validate_change(ws.machine, machine)?;
    shared
        .controller
        .store()
        .set_machine(ws.id, machine)
        .await?;
    ws.machine = machine;
    Ok(ws)
}

/// 最後の接続が閉じてから止めるまでの時間を変える。動いていて接続がなければ、今から数え直す。
pub async fn set_idle_stop(
    shared: &Shared,
    login: &str,
    name: &str,
    idle_stop: IdleStop,
) -> Result<Workspace> {
    let mut ws = find_workspace(shared, login, name).await?;
    shared
        .controller
        .store()
        .set_idle_stop(ws.id, idle_stop)
        .await?;
    shared.controller.reschedule_idle_stop(ws.id);
    ws.idle_stop = idle_stop;
    Ok(ws)
}

pub async fn stop_workspace(shared: &Shared, login: &str, name: &str) -> Result<Workspace> {
    let ws = find_workspace(shared, login, name).await?;
    shared.controller.stop(&ws).await?;
    Ok(ws)
}

/// ワークスペースを消す。`disks` なら、ランナーのホストにあるディスクも消す。
pub async fn remove_workspace(
    shared: &Shared,
    login: &str,
    name: &str,
    disks: bool,
) -> Result<Workspace> {
    let ws = find_workspace(shared, login, name).await?;
    shared.controller.remove(&ws, disks).await?;
    Ok(ws)
}

/// ワークスペースのランナーのプロキシのログ (HTTP の要求) の末尾から、`lines` 件を古い順に
/// 読む。省けば [`chloe_audit::DEFAULT_LINES`] 件。ランナーが止まっていても読める。
pub async fn tail_requests(
    shared: &Shared,
    login: &str,
    name: &str,
    lines: Option<usize>,
) -> Result<Vec<chloe_audit::Record>> {
    let lines = lines.unwrap_or(chloe_audit::DEFAULT_LINES);
    if lines == 0 || lines > chloe_audit::MAX_LINES {
        return Err(Error::Invalid(format!(
            "the number of lines must be between 1 and {}",
            chloe_audit::MAX_LINES
        )));
    }
    let ws = find_workspace(shared, login, name).await?;
    Ok(shared.controller.tail_requests(&ws, lines).await?)
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

/// シークレットや設定の持ち主。指定がなければ利用者の個人のもの。
pub fn owner_or_personal(login: &str, owner: Option<&str>) -> Result<Owner> {
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
    let spec = stored_spec(name, stored.as_deref(), shared.controller.preset(name))?;
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
    let (value, spec) = prepare_secret(name, input, existing, shared.controller.preset(name))?;
    secrets
        .set_with_spec(owner, name, &value.into(), spec.as_deref())
        .await?;
    shared.refresh_running_under(owner).await;
    Ok(())
}

/// 値を書かなかった欄を、登録してある今の値で埋める。値を入れ直さずに定義だけを変えるとき
/// (管理の TUI) に使う。値は呼び出し側に見せずに、そのまま登録し直すためだけに読む。
pub async fn keep_secret_values(
    shared: &Shared,
    login: &str,
    owner: &Owner,
    name: &str,
    input: &mut SecretInput,
) -> Result<()> {
    let single_missing = input.fields.is_empty() && input.value.is_none();
    let missing: Vec<String> = input
        .fields
        .iter()
        .filter(|(_, f)| f.value.is_empty())
        .map(|(n, _)| n.clone())
        .collect();
    if !single_missing && missing.is_empty() {
        return Ok(());
    }
    check_secret_name(name)?;
    check_secret_manager(shared, login, owner).await?;
    let Some((stored_value, stored)) = shared
        .controller
        .secrets()
        .get_with_spec(owner, name)
        .await?
    else {
        return Err(Error::Invalid("enter the value".into()));
    };
    let spec = stored_spec(name, stored.as_deref(), shared.controller.preset(name))?;
    let current = |field: &str| {
        spec.field_value(stored_value.expose_secret(), field)
            .ok_or_else(|| Error::Invalid(format!("enter the value of {field}")))
    };
    if single_missing {
        input.value = Some(current(catalog::VALUE)?);
    }
    for field in missing {
        let value = current(&field)?;
        if let Some(input) = input.fields.get_mut(&field) {
            input.value = value;
        }
    }
    Ok(())
}

/// 登録する値 (保存する形) と定義の文字列を決める。
///
/// `existing` は同じ持ち主にある行の定義 (行がなければ `None`、組み込みの種類なら `Some(None)`)。
/// `preset` は、その名前が組み込みの種類ならその定義。
/// 定義を渡さなければ、ある行の定義をそのままの文字列で使い続ける (定義は追加認証データに入るため)。
fn prepare_secret(
    name: &str,
    input: SecretInput,
    existing: Option<Option<String>>,
    preset: Option<SecretSpec>,
) -> Result<(String, Option<String>)> {
    let (spec, values) = input.into_parts().map_err(Error::Invalid)?;
    let (spec, text) = match spec {
        Some(_) if preset.is_some() => {
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
            Some(stored) => (stored_spec(name, stored.as_deref(), preset)?, stored),
            None => match preset {
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

fn stored_spec(name: &str, stored: Option<&str>, preset: Option<SecretSpec>) -> Result<SecretSpec> {
    match stored {
        Some(text) => serde_json::from_str(text)
            .map_err(|e| Error::Invalid(format!("the definition of {name} is broken: {e}"))),
        None => preset.ok_or_else(|| Error::Invalid(format!("{name} has no definition"))),
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
        return Ok(());
    }
    Err(Error::Forbidden(match owner {
        Owner::User(_) if *owner != personal(login) => {
            "you can only change your own personal settings".to_string()
        }
        Owner::User(_) => "personal settings are disabled on this gateway".to_string(),
        _ => format!(
            "only the administrators of the organization can change the settings of {}",
            owner_arg(owner)
        ),
    }))
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
    // 個人のワークスペースは利用者のトークンを使うので、インストールトークンの範囲は効かない。
    if matches!(owner, Owner::User(_)) && layer.github.is_some() {
        return Err(Error::Invalid(
            "personal settings cannot have github (personal workspaces use your own token)"
                .to_string(),
        ));
    }
    layer
        .check(shared.controller.base_policy())
        .map_err(Error::Invalid)?;
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

pub async fn check_workspace_policy_admin(
    shared: &Shared,
    login: &str,
    workspace: &Workspace,
) -> Result<()> {
    if shared.can_manage_workspace_policy(login, workspace).await? {
        return Ok(());
    }
    Err(Error::Forbidden(match workspace.owner {
        Owner::User(_) => "personal settings are disabled on this gateway".to_string(),
        _ => format!(
            "only the administrators of the organization (or the team's maintainers) can change the rules of {}",
            workspace.display_name()
        ),
    }))
}

/// ワークスペースの設定を読む。ワークスペースに入れる人は見られる。
pub async fn show_workspace_policy(
    shared: &Shared,
    login: &str,
    name: &str,
) -> Result<(Workspace, Option<WorkspacePolicy>)> {
    let ws = find_workspace(shared, login, name).await?;
    let policy = shared.controller.store().workspace_policy(ws.id).await?;
    Ok((ws, policy))
}

/// ワークスペースの設定を置き換え、動いていればランナーに送る。送ったランナーの数を返す。
pub async fn set_workspace_policy(
    shared: &Shared,
    login: &str,
    workspace: &Workspace,
    policy: &WorkspacePolicy,
) -> Result<usize> {
    check_workspace_policy_admin(shared, login, workspace).await?;
    policy
        .check(shared.controller.base_policy())
        .map_err(Error::Invalid)?;
    shared
        .controller
        .store()
        .set_workspace_policy(workspace.id, policy, login)
        .await?;
    Ok(shared.refresh_workspace(workspace).await? as usize)
}

pub async fn remove_workspace_policy(
    shared: &Shared,
    login: &str,
    workspace: &Workspace,
) -> Result<usize> {
    check_workspace_policy_admin(shared, login, workspace).await?;
    if !shared
        .controller
        .store()
        .delete_workspace_policy(workspace.id)
        .await?
    {
        return Err(Error::NotFound(format!(
            "no rules for {}",
            workspace.display_name()
        )));
    }
    Ok(shared.refresh_workspace(workspace).await? as usize)
}

/// 規則を置く場所。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleScope {
    /// ゲートウェイの `[policy]`。設定ファイルで変えるので、見るだけ。
    Gateway,
    /// 持ち主 (個人、Organization か team) の設定。
    Owner(Owner),
    /// ワークスペースの設定。
    Workspace(Workspace),
}

impl RuleScope {
    /// 画面に出す名前。
    pub fn label(&self) -> String {
        match self {
            Self::Gateway => "gateway".to_string(),
            Self::Owner(owner) => owner_arg(owner),
            Self::Workspace(ws) => ws.display_name(),
        }
    }
}

/// 利用者が見られるスコープと、規則を変えられるか。
#[derive(Debug, Clone)]
pub struct ScopeEntry {
    pub scope: RuleScope,
    pub editable: bool,
}

/// 利用者が見られるスコープを、gateway、個人、Organization、その team の順に並べる。
/// それぞれの持ち主のすぐあとに、そのワークスペースを置く。
pub async fn rule_scopes(shared: &Shared, login: &str) -> Result<Vec<ScopeEntry>> {
    let mut owners = shared.owners_for(login).await?;
    owners.sort_by_key(|owner| match owner {
        Owner::User(_) => (0, String::new(), String::new()),
        Owner::Org(org) => (1, org.clone(), String::new()),
        Owner::Team { org, team } => (1, org.clone(), team.clone()),
    });
    let workspaces = shared.controller.store().list(&owners).await?;
    let mut entries = vec![ScopeEntry {
        scope: RuleScope::Gateway,
        editable: false,
    }];
    for owner in owners {
        entries.push(ScopeEntry {
            editable: shared.can_manage_policy(login, &owner).await?,
            scope: RuleScope::Owner(owner.clone()),
        });
        let owned: Vec<&Workspace> = workspaces.iter().filter(|ws| ws.owner == owner).collect();
        // 変えられるかは持ち主だけで決まるので、GitHub に尋ねるのは持ち主ごとに一度にする。
        let editable = match owned.first() {
            Some(ws) => shared.can_manage_workspace_policy(login, ws).await?,
            None => false,
        };
        entries.extend(owned.into_iter().map(|ws| ScopeEntry {
            scope: RuleScope::Workspace(ws.clone()),
            editable,
        }));
    }
    Ok(entries)
}

/// スコープで効く規則と、そのスコープ自身の規則。
#[derive(Debug, Clone)]
pub struct RulesView {
    pub scope: RuleScope,
    pub editable: bool,
    /// 評価する順の規則と、その出どころ。
    pub rules: Vec<(RuleOrigin, PathRuleConfig)>,
    /// スコープ自身の規則 (編集するもの)。
    pub own: Vec<PathRuleConfig>,
}

pub async fn show_rules(shared: &Shared, login: &str, scope: RuleScope) -> Result<RulesView> {
    let controller = &shared.controller;
    let base = controller.base_policy();
    Ok(match &scope {
        RuleScope::Gateway => RulesView {
            rules: label_rules(base, &[], &[]),
            own: base.rules.clone(),
            editable: false,
            scope,
        },
        RuleScope::Owner(owner) => {
            let own = show_policy(shared, login, owner)
                .await?
                .map(|layer| layer.rules)
                .unwrap_or_default();
            RulesView {
                rules: controller.owner_rules(owner).await?,
                own,
                editable: shared.can_manage_policy(login, owner).await?,
                scope,
            }
        }
        RuleScope::Workspace(ws) => {
            let (ws, policy) = show_workspace_policy(shared, login, &ws.display_name()).await?;
            RulesView {
                rules: controller.workspace_rules(&ws).await?,
                own: policy.map(|p| p.rules).unwrap_or_default(),
                editable: shared.can_manage_workspace_policy(login, &ws).await?,
                scope: RuleScope::Workspace(ws),
            }
        }
    })
}

/// スコープ自身の規則を置き換える。持ち主の設定では、規則のほか (ルートの扱いとトークンの
/// 範囲) は残す。規則を空にして何も残らなければ、設定を消す。送ったランナーの数を返す。
pub async fn set_rules(
    shared: &Shared,
    login: &str,
    scope: &RuleScope,
    rules: Vec<PathRuleConfig>,
) -> Result<usize> {
    let store = shared.controller.store();
    match scope {
        RuleScope::Gateway => Err(Error::Invalid(
            "the gateway's rules are set in its configuration file".to_string(),
        )),
        RuleScope::Owner(owner) => {
            update_policy(shared, login, owner, |layer| layer.rules = rules).await
        }
        RuleScope::Workspace(ws) => {
            let ws = find_workspace(shared, login, &ws.display_name()).await?;
            if rules.is_empty() {
                check_workspace_policy_admin(shared, login, &ws).await?;
                if store.workspace_policy(ws.id).await?.is_none() {
                    return Ok(0);
                }
                return remove_workspace_policy(shared, login, &ws).await;
            }
            set_workspace_policy(shared, login, &ws, &WorkspacePolicy { rules }).await
        }
    }
}

/// 持ち主の設定のうち、規則のほか (ルートの扱いとトークンの範囲) を `settings` のものに
/// 置き換える。規則は残す。
pub async fn set_owner_settings(
    shared: &Shared,
    login: &str,
    owner: &Owner,
    settings: PolicyLayer,
) -> Result<usize> {
    update_policy(shared, login, owner, |layer| {
        layer.root = settings.root;
        layer.github = settings.github;
    })
    .await
}

/// 持ち主の設定を `change` で書き換えて保存する。何も残らなければ設定を消す。
async fn update_policy(
    shared: &Shared,
    login: &str,
    owner: &Owner,
    change: impl FnOnce(&mut PolicyLayer),
) -> Result<usize> {
    check_policy_admin(shared, login, owner).await?;
    let existing = shared.controller.store().policy(owner).await?;
    let mut layer = existing.clone().unwrap_or_default();
    change(&mut layer);
    if layer != PolicyLayer::default() {
        return set_policy(shared, login, owner, &layer).await;
    }
    if existing.is_none() {
        return Ok(0);
    }
    remove_policy(shared, login, owner).await
}

/// ディスクの大きさ (`50G`、`50GiB`、`50`。単位は GiB)。
pub fn parse_gib(size: &str) -> Result<u32> {
    let number = size
        .strip_suffix("GiB")
        .or_else(|| size.strip_suffix('G'))
        .unwrap_or(size);
    number
        .parse()
        .map_err(|_| Error::Invalid(format!("invalid size: {size} (write it in GiB, like 50G)")))
}

/// メモリの大きさ (`8G`、`8GiB`、`512M`、`512MiB`) を MiB にする。
pub fn parse_mib(size: &str) -> Result<u32> {
    let invalid = || Error::Invalid(format!("invalid size: {size} (write it like 8G or 512M)"));
    let (number, scale) =
        if let Some(n) = size.strip_suffix("GiB").or_else(|| size.strip_suffix('G')) {
            (n, 1024)
        } else if let Some(n) = size.strip_suffix("MiB").or_else(|| size.strip_suffix('M')) {
            (n, 1)
        } else {
            return Err(invalid());
        };
    let number: u32 = number.parse().map_err(|_| invalid())?;
    number.checked_mul(scale).ok_or_else(invalid)
}

/// MiB を、GiB で割り切れれば `8G`、そうでなければ `512M` の形で表す。
pub fn format_mib(mib: u32) -> String {
    if mib.is_multiple_of(1024) {
        format!("{}G", mib / 1024)
    } else {
        format!("{mib}M")
    }
}

/// 停止までの時間 (`90s`、`30m`、`2h`、`1d`、`never`、`default`)。
pub fn parse_idle_stop(text: &str) -> Result<IdleStop> {
    let invalid = || {
        Error::Invalid(format!(
            "invalid idle timeout: {text} (use e.g. 30m, 2h, never or default)"
        ))
    };
    match text {
        "never" => return Ok(IdleStop::Never),
        "default" => return Ok(IdleStop::Default),
        _ => {}
    }
    let (number, unit) = text.split_at(text.len().saturating_sub(1));
    let number: u32 = number.parse().map_err(|_| invalid())?;
    let unit = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 60 * 60,
        "d" => 24 * 60 * 60,
        _ => return Err(invalid()),
    };
    match number.checked_mul(unit) {
        Some(secs) if secs > 0 => Ok(IdleStop::After(secs)),
        _ => Err(invalid()),
    }
}

/// 停止までの秒数を、割り切れるいちばん大きな単位で表す (`30m`、`2h`)。止めないなら `never`。
pub fn format_idle(secs: Option<u64>) -> String {
    let Some(secs) = secs else {
        return "never".into();
    };
    for (unit, scale) in [("d", 24 * 60 * 60), ("h", 60 * 60), ("m", 60)] {
        if secs >= scale && secs % scale == 0 {
            return format!("{}{unit}", secs / scale);
        }
    }
    format!("{secs}s")
}

/// UNIX 時刻の秒を `YYYY-MM-DD HH:MM` (UTC) にする。
pub fn format_time(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rest = secs.rem_euclid(86_400);
    // 1970-01-01 からの日数を、グレゴリオ暦の日付にする。
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        rest / 3600,
        rest % 3600 / 60
    )
}

/// UNIX 時刻の秒を `YYYY-MM-DD HH:MM:SS` (UTC) にする。
pub fn format_time_secs(secs: i64) -> String {
    format!("{}:{:02}", format_time(secs), secs.rem_euclid(60))
}

/// バイト数を `512B`、`1.5K`、`12M` の形で表す。
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["K", "M", "G", "T"];
    if bytes < 1024 {
        return format!("{bytes}B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value < 10.0 {
        format!("{value:.1}{}", UNITS[unit])
    } else {
        format!("{value:.0}{}", UNITS[unit])
    }
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

/// Device Flow で利用者に結び付けた SSH の公開鍵。
pub async fn list_ssh_keys(shared: &Shared, login: &str) -> Result<Vec<SshKey>> {
    Ok(shared.controller.store().ssh_keys(login).await?)
}

/// SSH の公開鍵の結び付けを消す。その鍵では、予約したユーザー名で入れなくなる。
pub async fn remove_ssh_key(shared: &Shared, login: &str, fingerprint: &str) -> Result<()> {
    if shared
        .controller
        .store()
        .remove_ssh_key(login, fingerprint)
        .await?
    {
        Ok(())
    } else {
        Err(Error::NotFound(format!("no such key: {fingerprint}")))
    }
}

/// エージェントからの依頼と、そのワークスペース。
#[derive(Debug, Clone, Serialize)]
pub struct AskInfo {
    pub workspace: String,
    #[serde(flatten)]
    pub ask: Ask,
    /// 見ている人が通信の許可を承認できるか (答えを待つ通信の許可のときだけ `true` になりうる)。
    pub can_allow: bool,
}

/// 依頼への答え。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// 通信の許可を承認する。
    Allow,
    /// 断る。言葉はエージェントに渡す。
    Deny(Option<String>),
    /// 質問に答える。
    Answer(String),
}

async fn ask_info(shared: &Shared, login: &str, ws: &Workspace, ask: Ask) -> Result<AskInfo> {
    let can_allow = matches!(ask.body, AskBody::Network { .. })
        && ask.status == AskStatus::Pending
        && shared.can_manage_workspace_policy(login, ws).await?;
    Ok(AskInfo {
        workspace: ws.display_name(),
        ask,
        can_allow,
    })
}

/// 利用者が入れるワークスペースの依頼を新しい順に。`all` でなければ答えを待つものだけ。
pub async fn list_asks(shared: &Shared, login: &str, all: bool) -> Result<Vec<AskInfo>> {
    let workspaces = shared
        .controller
        .store()
        .list(&shared.owners_for(login).await?)
        .await?;
    let ids: Vec<i64> = workspaces.iter().map(|ws| ws.id).collect();
    let asks = shared.controller.asks().list(&ids, !all).await?;
    let mut infos = Vec::with_capacity(asks.len());
    for ask in asks {
        if let Some(ws) = workspaces.iter().find(|ws| ws.id == ask.workspace_id) {
            infos.push(ask_info(shared, login, ws, ask).await?);
        }
    }
    Ok(infos)
}

/// 依頼を探す。入れないワークスペースの依頼は、ないものとして扱う。
async fn find_ask(shared: &Shared, login: &str, id: i64) -> Result<(Workspace, Ask)> {
    let not_found = || Error::NotFound(format!("no such ask: {id}"));
    let ask = shared
        .controller
        .asks()
        .get(id)
        .await?
        .ok_or_else(not_found)?;
    let ws = shared
        .controller
        .store()
        .get(ask.workspace_id)
        .await?
        .ok_or_else(not_found)?;
    if !shared.can_access(login, &ws.owner).await? {
        return Err(not_found());
    }
    Ok((ws, ask))
}

pub async fn show_ask(shared: &Shared, login: &str, id: i64) -> Result<AskInfo> {
    let (ws, ask) = find_ask(shared, login, id).await?;
    ask_info(shared, login, &ws, ask).await
}

/// 依頼に答える。通信の許可を承認するときは、ワークスペースの規則を変えられる人でなければ
/// ならず、ワークスペースの規則にその宛先を許す規則を足して、動いているランナーに送る。
/// 断ることと質問に答えることは、ワークスペースに入れる人ならできる。
pub async fn answer_ask(shared: &Shared, login: &str, id: i64, reply: Reply) -> Result<AskInfo> {
    let (ws, ask) = find_ask(shared, login, id).await?;
    if ask.status != AskStatus::Pending {
        return Err(Error::Conflict(format!(
            "ask {id} is already {}",
            ask.status
        )));
    }
    let resolution = resolution_for(&ask.body, reply)?;
    if resolution == Resolution::Allow {
        check_workspace_policy_admin(shared, login, &ws).await?;
        let mut policy = shared
            .controller
            .store()
            .workspace_policy(ws.id)
            .await?
            .unwrap_or_default();
        if let Some(rule) = allow_rule(&ask)
            && !policy.rules.iter().any(|r| r.name == rule.name)
        {
            policy.rules.push(rule);
        }
        set_workspace_policy(shared, login, &ws, &policy).await?;
    }
    let ask = shared
        .controller
        .asks()
        .resolve(id, resolution, login)
        .await
        .map_err(|e| Error::Conflict(e.to_string()))?;
    ask_info(shared, login, &ws, ask).await
}

/// 依頼の種類に合う答えか。通信の許可は承認か断るか、質問は答えるか断るか。
fn resolution_for(body: &AskBody, reply: Reply) -> Result<Resolution> {
    match (body, reply) {
        (AskBody::Network { .. }, Reply::Allow) => Ok(Resolution::Allow),
        (_, Reply::Deny(message)) => Ok(Resolution::Deny(message.filter(|m| !m.trim().is_empty()))),
        (AskBody::Question { .. }, Reply::Answer(text)) if !text.trim().is_empty() => {
            Ok(Resolution::Answer(text))
        }
        (AskBody::Question { .. }, Reply::Answer(_)) => {
            Err(Error::Invalid("the answer is empty".into()))
        }
        (AskBody::Network { .. }, Reply::Answer(_)) => Err(Error::Invalid(
            "a network ask is allowed or denied, not answered".into(),
        )),
        (AskBody::Question { .. }, Reply::Allow) => Err(Error::Invalid(
            "a question is answered or denied, not allowed".into(),
        )),
    }
}

/// 通信の許可を承認したときに、ワークスペースの規則に足す規則。
fn allow_rule(ask: &Ask) -> Option<PathRuleConfig> {
    match &ask.body {
        AskBody::Network {
            host,
            methods,
            paths,
        } => Some(PathRuleConfig {
            name: Some(format!("ask-{}", ask.id)),
            hosts: vec![host.clone()],
            methods: methods.clone(),
            paths: paths.clone(),
            action: Action::Allow,
        }),
        AskBody::Question { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_fit_the_kind_of_ask() {
        let network = AskBody::Network {
            host: "example.com".into(),
            methods: vec!["GET".into()],
            paths: vec![],
        };
        let question = AskBody::Question {
            question: "ok?".into(),
            options: vec![],
        };
        assert_eq!(
            resolution_for(&network, Reply::Allow).unwrap(),
            Resolution::Allow
        );
        assert_eq!(
            resolution_for(&network, Reply::Deny(Some(" ".into()))).unwrap(),
            Resolution::Deny(None)
        );
        assert!(resolution_for(&network, Reply::Answer("yes".into())).is_err());
        assert!(resolution_for(&question, Reply::Allow).is_err());
        assert!(resolution_for(&question, Reply::Answer(" ".into())).is_err());
        assert_eq!(
            resolution_for(&question, Reply::Answer("yes".into())).unwrap(),
            Resolution::Answer("yes".into())
        );

        let ask = Ask {
            id: 7,
            workspace_id: 1,
            body: network,
            reason: None,
            status: AskStatus::Pending,
            answer: None,
            answered_by: None,
            created_at: 0,
            expires_at: 0,
            answered_at: None,
        };
        let rule = allow_rule(&ask).unwrap();
        assert_eq!(rule.name.as_deref(), Some("ask-7"));
        assert_eq!(rule.hosts, ["example.com"]);
        assert_eq!(rule.methods, ["GET"]);
        assert_eq!(rule.action, Action::Allow);
    }

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
    fn formats_times_and_sizes() {
        assert_eq!(format_time(0), "1970-01-01 00:00");
        assert_eq!(format_time(1_792_152_000), "2026-10-16 12:00");
        assert_eq!(format_time_secs(1_792_152_059), "2026-10-16 12:00:59");
        assert_eq!(format_bytes(512), "512B");
        assert_eq!(format_bytes(1536), "1.5K");
        assert_eq!(format_bytes(12 * 1024 * 1024), "12M");
    }

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration("30d").unwrap(), 30 * 86400);
        assert_eq!(parse_duration("12h").unwrap(), 12 * 3600);
        assert!(parse_duration("0d").is_err());
        assert!(parse_duration("5m").is_err());
        assert!(parse_duration("d").is_err());
    }

    #[test]
    fn parses_disk_sizes() {
        assert_eq!(parse_gib("50G").unwrap(), 50);
        assert_eq!(parse_gib("80GiB").unwrap(), 80);
        assert_eq!(parse_gib("30").unwrap(), 30);
        assert!(parse_gib("1T").is_err());
        assert!(parse_gib("-1").is_err());
    }

    #[test]
    fn parses_memory_sizes() {
        assert_eq!(parse_mib("8G").unwrap(), 8192);
        assert_eq!(parse_mib("2GiB").unwrap(), 2048);
        assert_eq!(parse_mib("512M").unwrap(), 512);
        assert_eq!(parse_mib("768MiB").unwrap(), 768);
        // 単位のない数は GiB とも MiB とも読めるので受け付けない。
        assert!(parse_mib("4096").is_err());
        assert!(parse_mib("1T").is_err());
        assert!(parse_mib("9999999G").is_err());
        assert_eq!(format_mib(8192), "8G");
        assert_eq!(format_mib(1536), "1536M");
    }

    #[test]
    fn parses_idle_timeouts() {
        assert_eq!(parse_idle_stop("90s").unwrap(), IdleStop::After(90));
        assert_eq!(parse_idle_stop("30m").unwrap(), IdleStop::After(1800));
        assert_eq!(parse_idle_stop("2h").unwrap(), IdleStop::After(7200));
        assert_eq!(parse_idle_stop("1d").unwrap(), IdleStop::After(86400));
        assert_eq!(parse_idle_stop("never").unwrap(), IdleStop::Never);
        assert_eq!(parse_idle_stop("default").unwrap(), IdleStop::Default);
        for bad in ["0m", "30", "m", "-1h", "1w"] {
            assert!(parse_idle_stop(bad).is_err(), "{bad}");
        }
        assert_eq!(format_idle(Some(1800)), "30m");
        assert_eq!(format_idle(Some(7200)), "2h");
        assert_eq!(format_idle(Some(90)), "90s");
        assert_eq!(format_idle(None), "never");
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
        let github = || catalog::preset("github_token");
        let (value, spec) =
            prepare_secret("github_token", SecretInput::value("ghp"), None, github()).unwrap();
        assert_eq!((value.as_str(), spec), ("ghp", None));
        // 組み込みの種類には定義を渡せない。
        assert!(prepare_secret("github_token", input(HF), None, github()).is_err());
        // 定義のない名前は、値だけでは登録できない (以前は組み込みだった名前も)。
        assert!(prepare_secret("hf_token", SecretInput::value("x"), None, None).is_err());
        assert!(prepare_secret("anthropic_api_key", SecretInput::value("x"), None, None).is_err());

        let (value, spec) = prepare_secret("hf_token", input(HF), None, None).unwrap();
        assert_eq!(value, "hf-real");
        let spec = spec.unwrap();
        // 値だけを変えるときは、ある定義をそのままの文字列で使う。
        let (value, kept) = prepare_secret(
            "hf_token",
            SecretInput::value("hf-new"),
            Some(Some(spec.clone())),
            None,
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
        let (_, spec) = prepare_secret("aws", input(aws), None, None).unwrap();
        let values_only = r#"
            [fields.access_key_id]
            value = "AKID2"
            [fields.secret_access_key]
            value = "s2"
        "#;
        let (value, kept) =
            prepare_secret("aws", input(values_only), Some(spec.clone()), None).unwrap();
        assert_eq!(kept, spec);
        assert!(value.contains("AKID2"));
        assert!(prepare_secret("aws", SecretInput::value("x"), Some(spec), None).is_err());
    }

    #[test]
    fn reserves_internal_names() {
        assert!(check_secret_name(USER_TOKEN_SECRET).is_err());
        assert!(check_secret_name("Bad-Name").is_err());
        check_secret_name("hf_token").unwrap();
    }
}
