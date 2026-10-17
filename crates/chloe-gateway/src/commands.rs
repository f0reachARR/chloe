//! `chloe` で始まる管理コマンド。ゲートウェイが受け取り、VM には渡さない。
//!
//! ```text
//! chloe ws list
//! chloe ws create NAME [--root persistent|ephemeral]
//! chloe ws root NAME persistent|ephemeral
//! chloe ws stop NAME
//! chloe ws rm NAME
//! chloe secret list
//! chloe secret set KIND      (値は標準入力から読む)
//! chloe secret rm KIND
//! chloe github login         (Device Flow で GitHub のトークンを得る)
//! chloe github status
//! chloe github logout
//! ```
//!
//! ワークスペースの NAME は SSH と同じ書き方 (`name`、`org/name`、`org/team/name`)。
//! シークレットは、今は接続した利用者の個人のものだけを扱う。

use chloe_controller::catalog;
use chloe_controller::workspace::{RootMode, WorkspaceRef};

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
    matches!(args, [a, b, ..] if a == "secret" && b == "set")
}

pub async fn run(shared: &Shared, login: &str, args: &[String], stdin: &[u8]) -> Output {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["ws", "list"] => ws_list(shared, login).await,
        ["ws", "create", name] => ws_create(shared, login, name, RootMode::Ephemeral).await,
        ["ws", "create", name, "--root", mode] => match mode.parse() {
            Ok(mode) => ws_create(shared, login, name, mode).await,
            Err(e) => Err(e.to_string()),
        },
        ["ws", "root", name, mode] => match mode.parse() {
            Ok(mode) => ws_root(shared, login, name, mode).await,
            Err(e) => Err(e.to_string()),
        },
        ["ws", "stop", name] => ws_stop(shared, login, name).await,
        ["ws", "rm", name] => ws_rm(shared, login, name).await,
        ["secret", "list"] => secret_list(shared, login).await,
        ["secret", "set", kind] => secret_set(shared, login, kind, stdin).await,
        ["secret", "rm", kind] => secret_rm(shared, login, kind).await,
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
        "usage:\n  chloe ws list\n  chloe ws create NAME [--root persistent|ephemeral]\n  chloe ws root NAME persistent|ephemeral\n  chloe ws stop NAME\n  chloe ws rm NAME\n  chloe secret list\n  chloe secret set KIND   (reads the value from stdin)\n  chloe secret rm KIND\n  chloe github login|status|logout\nsecret kinds: {}\n",
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
    mode: RootMode,
) -> Result<String, String> {
    let target = WorkspaceRef::parse(name, login).map_err(|e| e.to_string())?;
    if !shared
        .can_access(login, &target.owner)
        .await
        .map_err(|e| format!("{e:#}"))?
    {
        return Err(format!("you are not a member of {}", target.owner));
    }
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

async fn secret_list(shared: &Shared, login: &str) -> Result<String, String> {
    let names = shared
        .controller
        .secrets()
        .list(&personal(login))
        .await
        .map_err(|e| e.to_string())?;
    Ok(names.iter().map(|n| format!("{n}\n")).collect())
}

async fn secret_set(
    shared: &Shared,
    login: &str,
    kind: &str,
    stdin: &[u8],
) -> Result<String, String> {
    known_kind(kind)?;
    let value =
        String::from_utf8(stdin.to_vec()).map_err(|_| "the value must be UTF-8".to_string())?;
    let value = value.trim_end_matches(['\n', '\r']);
    if value.is_empty() {
        return Err("the value is empty (pass it on stdin)".into());
    }
    shared
        .controller
        .secrets()
        .set(&personal(login), kind, &value.to_string().into())
        .await
        .map_err(|e| e.to_string())?;
    shared.refresh_running(Some(&personal(login))).await;
    Ok(format!(
        "saved {kind} (used from the next request; in a running workspace, shells opened from now on get its placeholder)\n"
    ))
}

async fn secret_rm(shared: &Shared, login: &str, kind: &str) -> Result<String, String> {
    known_kind(kind)?;
    let removed = shared
        .controller
        .secrets()
        .delete(&personal(login), kind)
        .await
        .map_err(|e| e.to_string())?;
    if removed {
        shared.refresh_running(Some(&personal(login))).await;
        Ok(format!("removed {kind}\n"))
    } else {
        Err(format!("no such secret: {kind}"))
    }
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
        assert!(!needs_stdin(&["ws".into(), "list".into()]));
    }
}
