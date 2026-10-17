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
//! chloe token create NAME [--expires 30d]   (管理 API のトークン。値はこのときだけ表示する)
//! chloe token list
//! chloe token revoke ID
//! chloe github login         (Device Flow で GitHub のトークンを得る)
//! chloe github status
//! chloe github logout
//! ```
//!
//! ワークスペースの NAME は SSH と同じ書き方 (`name`、`org/name`、`org/team/name`)。
//! OWNER は Organization (`org`) か team (`org/team`)。シークレットは、OWNER を省くと
//! 接続した利用者の個人のものを扱う。

use chloe_controller::catalog;
use chloe_controller::runner::PolicyLayer;
use chloe_controller::workspace::RootMode;

use crate::ops::{self, owner_arg, parse_owner, secret_owner};
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
        ["token", "create", name] => token_create(shared, login, name, None).await,
        ["token", "create", name, "--expires", expires] => {
            token_create(shared, login, name, Some(expires)).await
        }
        ["token", "list"] => token_list(shared, login).await,
        ["token", "revoke", id] => token_revoke(shared, login, id).await,
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
        "usage:\n  chloe ws list\n  chloe ws create NAME [--root persistent|ephemeral]\n  chloe ws root NAME persistent|ephemeral\n  chloe ws stop NAME\n  chloe ws rm NAME\n  chloe secret list [--owner OWNER]\n  chloe secret set [--owner OWNER] KIND   (reads the value from stdin)\n  chloe secret rm [--owner OWNER] KIND\n  chloe policy show OWNER\n  chloe policy set OWNER   (reads TOML from stdin)\n  chloe policy rm OWNER\n  chloe token create NAME [--expires 30d]\n  chloe token list\n  chloe token revoke ID\n  chloe github login|status|logout\nOWNER is an organization (org) or a team (org/team).\nsecret kinds: {}\n",
        kinds.join(", ")
    )
}

/// 操作の失敗をコマンドの表示にする。
fn shown(error: ops::Error) -> String {
    match &error {
        ops::Error::Invalid(m) if m.starts_with("unknown secret kind") => {
            format!("{m}\n{}", help())
        }
        _ => error.to_string(),
    }
}

async fn ws_list(shared: &Shared, login: &str) -> Result<String, String> {
    let workspaces = ops::list_workspaces(shared, login).await.map_err(shown)?;
    Ok(workspaces
        .iter()
        .map(|ws| {
            let state = if ws.running { "running" } else { "stopped" };
            format!("{}\t{}\t{state}\n", ws.name, ws.root.as_str())
        })
        .collect())
}

async fn ws_create(
    shared: &Shared,
    login: &str,
    name: &str,
    root: Option<RootMode>,
) -> Result<String, String> {
    let ws = ops::create_workspace(shared, login, name, root)
        .await
        .map_err(shown)?;
    Ok(format!("created {}\n", ws.name))
}

async fn ws_root(
    shared: &Shared,
    login: &str,
    name: &str,
    root: RootMode,
) -> Result<String, String> {
    let ws = ops::set_root(shared, login, name, root)
        .await
        .map_err(shown)?;
    Ok(format!(
        "{} will use the {} root from the next start (run `chloe ws stop {}` to restart now)\n",
        ws.display_name(),
        root.as_str(),
        ws.display_name()
    ))
}

async fn ws_stop(shared: &Shared, login: &str, name: &str) -> Result<String, String> {
    let ws = ops::stop_workspace(shared, login, name)
        .await
        .map_err(shown)?;
    Ok(format!("stopped {}\n", ws.display_name()))
}

async fn ws_rm(shared: &Shared, login: &str, name: &str) -> Result<String, String> {
    let ws = ops::remove_workspace(shared, login, name)
        .await
        .map_err(shown)?;
    Ok(format!(
        "removed {} (its disks are kept under the runner state directory as {})\n",
        ws.display_name(),
        ws.key()
    ))
}

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

async fn secret_list(shared: &Shared, login: &str, owner: Option<&str>) -> Result<String, String> {
    let owner = secret_owner(login, owner).map_err(shown)?;
    let names = ops::list_secrets(shared, login, &owner)
        .await
        .map_err(shown)?;
    Ok(names.iter().map(|n| format!("{n}\n")).collect())
}

async fn secret_set(
    shared: &Shared,
    login: &str,
    owner: Option<&str>,
    kind: &str,
    stdin: &[u8],
) -> Result<String, String> {
    let owner = secret_owner(login, owner).map_err(shown)?;
    ops::known_kind(kind).map_err(shown)?;
    ops::check_secret_manager(shared, login, &owner)
        .await
        .map_err(shown)?;
    let value =
        String::from_utf8(stdin.to_vec()).map_err(|_| "the value must be UTF-8".to_string())?;
    let value = value.trim_end_matches(['\n', '\r']);
    if value.is_empty() {
        return Err("the value is empty (pass it on stdin)".into());
    }
    ops::set_secret(shared, login, &owner, kind, value)
        .await
        .map_err(shown)?;
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
    let owner = secret_owner(login, owner).map_err(shown)?;
    ops::remove_secret(shared, login, &owner, kind)
        .await
        .map_err(shown)?;
    Ok(format!("removed {kind} from {}\n", owner_arg(&owner)))
}

async fn policy_show(shared: &Shared, login: &str, owner: &str) -> Result<String, String> {
    let owner = parse_owner(owner).map_err(shown)?;
    match ops::show_policy(shared, login, &owner)
        .await
        .map_err(shown)?
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
    let owner = parse_owner(owner).map_err(shown)?;
    let text = std::str::from_utf8(stdin).map_err(|_| "the settings must be UTF-8".to_string())?;
    // 権限のない利用者には、設定の書き方の誤りより先に権限がないことを伝える。
    ops::check_policy_admin(shared, login, &owner)
        .await
        .map_err(shown)?;
    let layer: PolicyLayer = toml::from_str(text).map_err(|e| format!("invalid settings: {e}"))?;
    let refreshed = ops::set_policy(shared, login, &owner, &layer)
        .await
        .map_err(shown)?;
    Ok(format!(
        "saved the settings of {} ({refreshed} running workspace(s) updated)\n",
        owner_arg(&owner)
    ))
}

async fn policy_rm(shared: &Shared, login: &str, owner: &str) -> Result<String, String> {
    let owner = parse_owner(owner).map_err(shown)?;
    let refreshed = ops::remove_policy(shared, login, &owner)
        .await
        .map_err(shown)?;
    Ok(format!(
        "removed the settings of {} ({refreshed} running workspace(s) updated)\n",
        owner_arg(&owner)
    ))
}

async fn token_create(
    shared: &Shared,
    login: &str,
    name: &str,
    expires: Option<&str>,
) -> Result<String, String> {
    let ttl = expires
        .map(ops::parse_duration)
        .transpose()
        .map_err(shown)?;
    let (id, token) = ops::create_token(shared, login, name, ttl)
        .await
        .map_err(shown)?;
    Ok(format!(
        "{token}\n# token {id} ({name}); it is shown only now. Use it as `Authorization: Bearer <token>`.\n"
    ))
}

async fn token_list(shared: &Shared, login: &str) -> Result<String, String> {
    let tokens = ops::list_tokens(shared, login).await.map_err(shown)?;
    Ok(tokens
        .iter()
        .map(|t| {
            let expires = t
                .expires_at
                .map(|at| format!("expires at {at}"))
                .unwrap_or_else(|| "no expiry".into());
            let used = t
                .last_used_at
                .map(|at| format!("last used at {at}"))
                .unwrap_or_else(|| "never used".into());
            format!("{}\t{}\t{expires}\t{used}\n", t.id, t.name)
        })
        .collect())
}

async fn token_revoke(shared: &Shared, login: &str, id: &str) -> Result<String, String> {
    let id: i64 = id.parse().map_err(|_| format!("invalid token id: {id}"))?;
    ops::revoke_token(shared, login, id).await.map_err(shown)?;
    Ok(format!("revoked token {id}\n"))
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
}
