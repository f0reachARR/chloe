//! `chloe` で始まる管理コマンド。ゲートウェイが受け取り、VM には渡さない。
//!
//! ```text
//! chloe ws list
//! chloe ws create NAME [--root persistent|ephemeral]
//! chloe ws root NAME persistent|ephemeral
//! chloe ws stop NAME
//! chloe ws rm NAME
//! chloe secret list [--owner OWNER]
//! chloe secret set [--owner OWNER] KIND   (値は標準入力から読む)
//! chloe secret rm [--owner OWNER] KIND
//! chloe policy show OWNER
//! chloe policy set OWNER                  (TOML を標準入力から読む)
//! chloe policy rm OWNER
//! chloe github login         (Device Flow で GitHub のトークンを得る)
//! chloe github status
//! chloe github logout
//! ```
//!
//! ワークスペースの NAME は SSH と同じ書き方 (`name`、`org/name`、`org/team/name`)。
//! OWNER は Organization (`org`) か team (`org/team`)。シークレットは、OWNER を省くと
//! 接続した利用者の個人のものを扱う。

use chloe_controller::catalog;
use chloe_controller::runner::{PolicyLayer, most_specific};
use chloe_controller::workspace::{RootMode, WorkspaceRef};
use chloe_secrets::Owner;

use crate::session::Shared;

pub struct Output {
    pub stdout: String,
    pub stderr: String,
    pub code: u32,
}

impl Output {
    fn ok(stdout: impl Into<String>) -> Self {
        Self {
            stdout: stdout.into(),
            stderr: String::new(),
            code: 0,
        }
    }

    fn error(message: impl Into<String>) -> Self {
        Self {
            stdout: String::new(),
            stderr: format!("chloe: {}\n", message.into()),
            code: 1,
        }
    }
}

/// コマンドが `chloe` で始まる管理コマンドなら、その引数を返す。
pub fn parse(command: &str) -> Option<Vec<String>> {
    let mut words = command.split_whitespace();
    if words.next() != Some("chloe") {
        return None;
    }
    Some(words.map(str::to_string).collect())
}

/// 標準入力を最後まで読んでから実行するコマンドか。
pub fn needs_stdin(args: &[String]) -> bool {
    matches!(args, [a, b, ..] if (a == "secret" || a == "policy") && b == "set")
}

pub async fn run(shared: &Shared, login: &str, args: &[String], stdin: &[u8]) -> Output {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["ws", "list"] => ws_list(shared, login).await,
        ["ws", "create", name] => ws_create(shared, login, name, None).await,
        ["ws", "create", name, "--root", mode] => match mode.parse() {
            Ok(mode) => ws_create(shared, login, name, Some(mode)).await,
            Err(e) => Err(e.to_string()),
        },
        ["ws", "root", name, mode] => match mode.parse() {
            Ok(mode) => ws_root(shared, login, name, mode).await,
            Err(e) => Err(e.to_string()),
        },
        ["ws", "stop", name] => ws_stop(shared, login, name).await,
        ["ws", "rm", name] => ws_rm(shared, login, name).await,
        ["secret", "list"] => secret_list(shared, login, None).await,
        ["secret", "list", "--owner", owner] => secret_list(shared, login, Some(owner)).await,
        ["secret", "set", kind] => secret_set(shared, login, None, kind, stdin).await,
        ["secret", "set", "--owner", owner, kind] => {
            secret_set(shared, login, Some(owner), kind, stdin).await
        }
        ["secret", "rm", kind] => secret_rm(shared, login, None, kind).await,
        ["secret", "rm", "--owner", owner, kind] => {
            secret_rm(shared, login, Some(owner), kind).await
        }
        ["policy", "show", owner] => policy_show(shared, login, owner).await,
        ["policy", "set", owner] => policy_set(shared, login, owner, stdin).await,
        ["policy", "rm", owner] => policy_rm(shared, login, owner).await,
        ["github", "status"] => github_status(shared, login).await,
        ["github", "logout"] => github_logout(shared, login).await,
        [] | ["help"] => Ok(help()),
        _ => Err(format!(
            "unknown command: chloe {}\n{}",
            args.join(" "),
            help()
        )),
    };
    match result {
        Ok(stdout) => Output::ok(stdout),
        Err(message) => Output::error(message),
    }
}

fn help() -> String {
    let kinds: Vec<&str> = catalog::KINDS.iter().map(|k| k.name).collect();
    format!(
        "usage:\n  chloe ws list\n  chloe ws create NAME [--root persistent|ephemeral]\n  chloe ws root NAME persistent|ephemeral\n  chloe ws stop NAME\n  chloe ws rm NAME\n  chloe secret list [--owner OWNER]\n  chloe secret set [--owner OWNER] KIND   (reads the value from stdin)\n  chloe secret rm [--owner OWNER] KIND\n  chloe policy show OWNER\n  chloe policy set OWNER   (reads TOML from stdin)\n  chloe policy rm OWNER\n  chloe github login|status|logout\nOWNER is an organization (org) or a team (org/team).\nsecret kinds: {}\n",
        kinds.join(", ")
    )
}

async fn ws_list(shared: &Shared, login: &str) -> Result<String, String> {
    let workspaces = shared
        .controller
        .store()
        .list(
            &shared
                .owners_for(login)
                .await
                .map_err(|e| format!("{e:#}"))?,
        )
        .await
        .map_err(|e| e.to_string())?;
    let running = shared
        .controller
        .running()
        .await
        .map_err(|e| e.to_string())?;
    Ok(workspaces
        .iter()
        .map(|ws| {
            let state = if running.iter().any(|r| r.id == ws.id) {
                "running"
            } else {
                "stopped"
            };
            format!(
                "{}\t{}\t{state}\n",
                ws.display_name(),
                ws.root_mode.as_str()
            )
        })
        .collect())
}

async fn ws_create(
    shared: &Shared,
    login: &str,
    name: &str,
    mode: Option<RootMode>,
) -> Result<String, String> {
    let target = WorkspaceRef::parse(name, login).map_err(|e| e.to_string())?;
    if !shared
        .can_access(login, &target.owner)
        .await
        .map_err(|e| format!("{e:#}"))?
    {
        return Err(format!("you are not a member of {}", target.owner));
    }
    // 指定がなければ、Organization と team の設定の既定を使う。
    let mode = match mode {
        Some(mode) => mode,
        None => {
            let layers = shared
                .controller
                .store()
                .policy_layers(&target.owner)
                .await
                .map_err(|e| e.to_string())?;
            most_specific(&layers, |l| l.root.as_ref()).unwrap_or(RootMode::Ephemeral)
        }
    };
    let ws = shared
        .controller
        .store()
        .create(&target, mode, login)
        .await
        .map_err(|e| e.to_string())?;
    Ok(format!("created {}\n", ws.display_name()))
}

async fn find(
    shared: &Shared,
    login: &str,
    name: &str,
) -> Result<chloe_controller::workspace::Workspace, String> {
    let target = WorkspaceRef::parse(name, login).map_err(|e| e.to_string())?;
    let not_found = || format!("no such workspace: {name}");
    if !shared
        .can_access(login, &target.owner)
        .await
        .map_err(|e| format!("{e:#}"))?
    {
        return Err(not_found());
    }
    shared
        .controller
        .store()
        .find(&target)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(not_found)
}

async fn ws_root(
    shared: &Shared,
    login: &str,
    name: &str,
    mode: RootMode,
) -> Result<String, String> {
    let ws = find(shared, login, name).await?;
    shared
        .controller
        .store()
        .set_root_mode(ws.id, mode)
        .await
        .map_err(|e| e.to_string())?;
    Ok(format!(
        "{} will use the {} root from the next start (run `chloe ws stop {}` to restart now)\n",
        ws.display_name(),
        mode.as_str(),
        ws.display_name()
    ))
}

async fn ws_stop(shared: &Shared, login: &str, name: &str) -> Result<String, String> {
    let ws = find(shared, login, name).await?;
    shared
        .controller
        .stop(&ws)
        .await
        .map_err(|e| e.to_string())?;
    Ok(format!("stopped {}\n", ws.display_name()))
}

async fn ws_rm(shared: &Shared, login: &str, name: &str) -> Result<String, String> {
    let ws = find(shared, login, name).await?;
    shared
        .controller
        .stop(&ws)
        .await
        .map_err(|e| e.to_string())?;
    shared
        .controller
        .store()
        .delete(ws.id)
        .await
        .map_err(|e| e.to_string())?;
    Ok(format!(
        "removed {} (its disks are kept under the runner state directory as {})\n",
        ws.display_name(),
        ws.key()
    ))
}

use crate::access::personal;

async fn github_status(shared: &Shared, login: &str) -> Result<String, String> {
    if shared.github.is_none() {
        return Ok("the GitHub App is not configured\n".into());
    }
    match shared
        .load_user_token(login)
        .await
        .map_err(|e| format!("{e:#}"))?
    {
        Some(token) => Ok(match token.expires_at {
            Some(at) => format!(
                "signed in (the token is refreshed automatically; it expires at UNIX time {at})\n"
            ),
            None => "signed in\n".into(),
        }),
        None => Ok("not signed in (run `chloe github login`)\n".into()),
    }
}

async fn github_logout(shared: &Shared, login: &str) -> Result<String, String> {
    let removed = shared
        .delete_user_token(login)
        .await
        .map_err(|e| format!("{e:#}"))?;
    Ok(if removed {
        "signed out (running workspaces keep the old token until they stop)\n".into()
    } else {
        "not signed in\n".into()
    })
}

fn known_kind(kind: &str) -> Result<(), String> {
    match catalog::kind(kind) {
        Some(_) => Ok(()),
        None => Err(format!("unknown secret kind: {kind}\n{}", help())),
    }
}

/// `--owner` で指定した持ち主。省けば接続した利用者の個人のもの。
fn secret_owner(login: &str, owner: Option<&str>) -> Result<Owner, String> {
    match owner {
        None => Ok(personal(login)),
        Some(owner) => parse_owner(owner),
    }
}

/// Organization (`org`) か team (`org/team`)。
fn parse_owner(arg: &str) -> Result<Owner, String> {
    let text = match arg.split_once('/') {
        None => format!("org:{arg}"),
        Some(_) => format!("team:{arg}"),
    };
    text.to_ascii_lowercase()
        .parse()
        .map_err(|_| format!("invalid owner: {arg} (use org or org/team)"))
}

/// 持ち主の名前を、コマンドで指定する書き方で表す。
fn owner_arg(owner: &Owner) -> String {
    match owner {
        Owner::User(login) => login.clone(),
        Owner::Org(org) => org.clone(),
        Owner::Team { org, team } => format!("{org}/{team}"),
    }
}

async fn secret_list(shared: &Shared, login: &str, owner: Option<&str>) -> Result<String, String> {
    let owner = secret_owner(login, owner)?;
    // 名前だけなので、その持ち主のワークスペースに入れる人は見られる。
    if !shared
        .can_access(login, &owner)
        .await
        .map_err(|e| format!("{e:#}"))?
    {
        return Err(format!("you are not a member of {}", owner_arg(&owner)));
    }
    let names = shared
        .controller
        .secrets()
        .list(&owner)
        .await
        .map_err(|e| e.to_string())?;
    Ok(names.iter().map(|n| format!("{n}\n")).collect())
}

async fn check_secret_manager(shared: &Shared, login: &str, owner: &Owner) -> Result<(), String> {
    if shared
        .can_manage_secrets(login, owner)
        .await
        .map_err(|e| format!("{e:#}"))?
    {
        Ok(())
    } else {
        Err(format!(
            "you cannot change the secrets of {} (organization admins, and team maintainers for a team, can)",
            owner_arg(owner)
        ))
    }
}

async fn secret_set(
    shared: &Shared,
    login: &str,
    owner: Option<&str>,
    kind: &str,
    stdin: &[u8],
) -> Result<String, String> {
    known_kind(kind)?;
    let owner = secret_owner(login, owner)?;
    check_secret_manager(shared, login, &owner).await?;
    let value =
        String::from_utf8(stdin.to_vec()).map_err(|_| "the value must be UTF-8".to_string())?;
    let value = value.trim_end_matches(['\n', '\r']);
    if value.is_empty() {
        return Err("the value is empty (pass it on stdin)".into());
    }
    shared
        .controller
        .secrets()
        .set(&owner, kind, &value.to_string().into())
        .await
        .map_err(|e| e.to_string())?;
    shared.refresh_running_under(&owner).await;
    Ok(format!(
        "saved {kind} for {} (used from the next request; in a running workspace, shells opened from now on get its placeholder)\n",
        owner_arg(&owner)
    ))
}

async fn secret_rm(
    shared: &Shared,
    login: &str,
    owner: Option<&str>,
    kind: &str,
) -> Result<String, String> {
    known_kind(kind)?;
    let owner = secret_owner(login, owner)?;
    check_secret_manager(shared, login, &owner).await?;
    let removed = shared
        .controller
        .secrets()
        .delete(&owner, kind)
        .await
        .map_err(|e| e.to_string())?;
    if removed {
        shared.refresh_running_under(&owner).await;
        Ok(format!("removed {kind} from {}\n", owner_arg(&owner)))
    } else {
        Err(format!("no such secret: {kind}"))
    }
}

async fn check_policy_admin(shared: &Shared, login: &str, owner: &Owner) -> Result<(), String> {
    if shared
        .can_manage_policy(login, owner)
        .await
        .map_err(|e| format!("{e:#}"))?
    {
        Ok(())
    } else {
        Err(format!(
            "only the administrators of the organization can change the settings of {}",
            owner_arg(owner)
        ))
    }
}

async fn policy_show(shared: &Shared, login: &str, owner: &str) -> Result<String, String> {
    let owner = parse_owner(owner)?;
    // 設定 (宛先と規則) は秘密ではないので、その持ち主のワークスペースに入れる人は見られる。
    if !shared
        .can_access(login, &owner)
        .await
        .map_err(|e| format!("{e:#}"))?
        && !shared
            .can_manage_policy(login, &owner)
            .await
            .map_err(|e| format!("{e:#}"))?
    {
        return Err(format!("you are not a member of {}", owner_arg(&owner)));
    }
    match shared
        .controller
        .store()
        .policy(&owner)
        .await
        .map_err(|e| e.to_string())?
    {
        Some(layer) => toml::to_string(&layer).map_err(|e| e.to_string()),
        None => Ok(format!("# no settings for {}\n", owner_arg(&owner))),
    }
}

async fn policy_set(
    shared: &Shared,
    login: &str,
    owner: &str,
    stdin: &[u8],
) -> Result<String, String> {
    let owner = parse_owner(owner)?;
    check_policy_admin(shared, login, &owner).await?;
    let text = std::str::from_utf8(stdin).map_err(|_| "the settings must be UTF-8".to_string())?;
    let layer: PolicyLayer = toml::from_str(text).map_err(|e| format!("invalid settings: {e}"))?;
    shared
        .controller
        .store()
        .set_policy(&owner, &layer, login)
        .await
        .map_err(|e| e.to_string())?;
    let refreshed = shared.refresh_running_under(&owner).await;
    Ok(format!(
        "saved the settings of {} ({refreshed} running workspace(s) updated)\n",
        owner_arg(&owner)
    ))
}

async fn policy_rm(shared: &Shared, login: &str, owner: &str) -> Result<String, String> {
    let owner = parse_owner(owner)?;
    check_policy_admin(shared, login, &owner).await?;
    let removed = shared
        .controller
        .store()
        .delete_policy(&owner)
        .await
        .map_err(|e| e.to_string())?;
    if !removed {
        return Err(format!("no settings for {}", owner_arg(&owner)));
    }
    let refreshed = shared.refresh_running_under(&owner).await;
    Ok(format!(
        "removed the settings of {} ({refreshed} running workspace(s) updated)\n",
        owner_arg(&owner)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_management_commands() {
        assert_eq!(
            parse("chloe ws list"),
            Some(vec!["ws".to_string(), "list".to_string()])
        );
        assert_eq!(parse("chloe"), Some(vec![]));
        assert_eq!(parse("chloe-other"), None);
        assert_eq!(parse("ls"), None);
        assert!(needs_stdin(&["secret".into(), "set".into(), "x".into()]));
        assert!(needs_stdin(&["policy".into(), "set".into(), "acme".into()]));
        assert!(!needs_stdin(&["ws".into(), "list".into()]));
    }

    #[test]
    fn parses_owners() {
        assert_eq!(parse_owner("Acme"), Ok(Owner::Org("acme".into())));
        assert_eq!(
            parse_owner("acme/dev"),
            Ok(Owner::Team {
                org: "acme".into(),
                team: "dev".into()
            })
        );
        assert!(parse_owner("../x").is_err());
        assert!(parse_owner("a/b/c").is_err());
        assert_eq!(owner_arg(&parse_owner("acme/dev").unwrap()), "acme/dev");
    }
}
