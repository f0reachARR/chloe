//! `chloe` で始まる管理コマンド。ゲートウェイが受け取り、VM には渡さない。
//!
//! ```text
//! chloe whoami              (接続した利用者の GitHub のログイン名)
//! chloe ws list
//! chloe ws create NAME [--root persistent|ephemeral] [--workspace 50G] [--docker 80G]
//! chloe ws root NAME persistent|ephemeral
//! chloe ws disk NAME [--workspace 50G] [--docker 80G]   (大きくするだけ。次の起動から効く)
//! chloe ws policy show|set|rm NAME        (ワークスペースの規則。set は TOML を標準入力から読む)
//! chloe ws stop NAME
//! chloe ws rm NAME [--disks]   (--disks ならランナーのホストのディスクも消す)
//! chloe secret list [--owner OWNER]
//! chloe secret set [--owner OWNER] KIND   (値は標準入力から読む)
//! chloe secret rm [--owner OWNER] KIND
//! chloe policy show [OWNER]               (OWNER を省くと個人の設定)
//! chloe policy set [OWNER]                (TOML を標準入力から読む)
//! chloe policy rm [OWNER]
//! chloe token create NAME [--expires 30d]   (管理 API のトークン。値はこのときだけ表示する)
//! chloe token list
//! chloe token revoke ID
//! chloe key list            (Device Flow で結び付けた SSH の公開鍵)
//! chloe key rm FINGERPRINT
//! chloe github login         (Device Flow で GitHub のトークンを得る)
//! chloe github status
//! chloe github logout
//! ```
//!
//! ワークスペースの NAME は SSH と同じ書き方 (`name`、`org/name`、`org/team/name`)。
//! OWNER は Organization (`org`) か team (`org/team`)。シークレットと設定は、OWNER を省くと
//! 接続した利用者の個人のものを扱う。

use chloe_controller::catalog::{self, SecretInput};
use chloe_controller::runner::{PolicyLayer, WorkspacePolicy};
use chloe_controller::workspace::{DiskSizes, RootMode};

use crate::ops::{self, owner_arg, owner_or_personal};
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
    match args {
        [a, b, ..] if (a == "secret" || a == "policy") && b == "set" => true,
        [a, b, c, ..] => a == "ws" && b == "policy" && c == "set",
        _ => false,
    }
}

pub async fn run(shared: &Shared, login: &str, args: &[String], stdin: &[u8]) -> Output {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["whoami"] => Ok(format!("{login}\n")),
        ["ws", "list"] => ws_list(shared, login).await,
        ["ws", "create", name, options @ ..] => match parse_ws_options(options, true) {
            Ok((root, disks)) => ws_create(shared, login, name, root, disks).await,
            Err(e) => Err(e),
        },
        ["ws", "root", name, mode] => match mode.parse() {
            Ok(mode) => ws_root(shared, login, name, mode).await,
            Err(e) => Err(e.to_string()),
        },
        ["ws", "disk", name] => ws_disk_show(shared, login, name).await,
        ["ws", "disk", name, options @ ..] => match parse_ws_options(options, false) {
            Ok((_, disks)) => ws_disk(shared, login, name, disks).await,
            Err(e) => Err(e),
        },
        ["ws", "policy", "show", name] => ws_policy_show(shared, login, name).await,
        ["ws", "policy", "set", name] => ws_policy_set(shared, login, name, stdin).await,
        ["ws", "policy", "rm", name] => ws_policy_rm(shared, login, name).await,
        ["ws", "stop", name] => ws_stop(shared, login, name).await,
        ["ws", "rm", name] => ws_rm(shared, login, name, false).await,
        ["ws", "rm", name, "--disks"] => ws_rm(shared, login, name, true).await,
        ["secret", "list"] => secret_list(shared, login, None).await,
        ["secret", "list", "--owner", owner] => secret_list(shared, login, Some(owner)).await,
        ["secret", "set", name] => secret_set(shared, login, None, name, false, stdin).await,
        ["secret", "set", name, "--spec"] => {
            secret_set(shared, login, None, name, true, stdin).await
        }
        ["secret", "set", "--owner", owner, name] => {
            secret_set(shared, login, Some(owner), name, false, stdin).await
        }
        ["secret", "set", "--owner", owner, name, "--spec"] => {
            secret_set(shared, login, Some(owner), name, true, stdin).await
        }
        ["secret", "show", name] => secret_show(shared, login, None, name).await,
        ["secret", "show", "--owner", owner, name] => {
            secret_show(shared, login, Some(owner), name).await
        }
        ["secret", "rm", name] => secret_rm(shared, login, None, name).await,
        ["secret", "rm", "--owner", owner, name] => {
            secret_rm(shared, login, Some(owner), name).await
        }
        ["policy", "show"] => policy_show(shared, login, None).await,
        ["policy", "show", owner] => policy_show(shared, login, Some(owner)).await,
        ["policy", "set"] => policy_set(shared, login, None, stdin).await,
        ["policy", "set", owner] => policy_set(shared, login, Some(owner), stdin).await,
        ["policy", "rm"] => policy_rm(shared, login, None).await,
        ["policy", "rm", owner] => policy_rm(shared, login, Some(owner)).await,
        ["token", "create", name] => token_create(shared, login, name, None).await,
        ["token", "create", name, "--expires", expires] => {
            token_create(shared, login, name, Some(expires)).await
        }
        ["token", "list"] => token_list(shared, login).await,
        ["token", "revoke", id] => token_revoke(shared, login, id).await,
        ["key", "list"] => key_list(shared, login).await,
        ["key", "rm", fingerprint] => key_rm(shared, login, fingerprint).await,
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
    let kinds = catalog::PRESETS;
    format!(
        "usage:\n  chloe whoami\n  chloe ws list\n  chloe ws create NAME [--root persistent|ephemeral] [--workspace SIZE] [--docker SIZE]\n  chloe ws root NAME persistent|ephemeral\n  chloe ws disk NAME [--workspace SIZE] [--docker SIZE]   (grows the disks from the next start)\n  chloe ws policy show NAME\n  chloe ws policy set NAME   (reads the rules as TOML from stdin)\n  chloe ws policy rm NAME\n  chloe ws stop NAME\n  chloe ws rm NAME [--disks]   (--disks also deletes its disks)\n  chloe secret list [--owner OWNER]\n  chloe secret show [--owner OWNER] NAME\n  chloe secret set [--owner OWNER] NAME   (reads the value from stdin)\n  chloe secret set [--owner OWNER] NAME --spec   (reads the definition and values as TOML from stdin)\n  chloe secret rm [--owner OWNER] NAME\n  chloe policy show [OWNER]\n  chloe policy set [OWNER]   (reads TOML from stdin)\n  chloe policy rm [OWNER]\n  chloe token create NAME [--expires 30d]\n  chloe token list\n  chloe token revoke ID\n  chloe key list\n  chloe key rm FINGERPRINT\n  chloe github login|status|logout\nOWNER is an organization (org) or a team (org/team). Without it, secrets and policy are your personal ones.\nSIZE is in GiB (50G or 50).\nbuilt-in secrets: {} (other names need a definition: --spec)\n",
        kinds.join(", ")
    )
}

/// `ws create` と `ws disk` のオプション。`--root` は `allow_root` のときだけ受け付ける。
fn parse_ws_options(
    options: &[&str],
    allow_root: bool,
) -> Result<(Option<RootMode>, DiskSizes), String> {
    let mut root = None;
    let mut disks = DiskSizes::default();
    let mut options = options.iter();
    while let Some(option) = options.next() {
        let mut value = || {
            options
                .next()
                .copied()
                .ok_or_else(|| format!("{option} needs a value"))
        };
        match *option {
            "--root" if allow_root => root = Some(value()?.parse().map_err(|e| format!("{e}"))?),
            "--workspace" => disks.workspace_gib = Some(ops::parse_gib(value()?).map_err(shown)?),
            "--docker" => disks.docker_gib = Some(ops::parse_gib(value()?).map_err(shown)?),
            _ => return Err(format!("unknown option: {option}\n{}", help())),
        }
    }
    Ok((root, disks))
}

/// 操作の失敗をコマンドの表示にする。
fn shown(error: ops::Error) -> String {
    match &error {
        ops::Error::Invalid(m) if m.contains("pass one with --spec") => {
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
            format!(
                "{}\t{}\t{state}\t{}G\t{}G\n",
                ws.name,
                ws.root.as_str(),
                ws.workspace_disk_gib,
                ws.docker_disk_gib
            )
        })
        .collect())
}

async fn ws_create(
    shared: &Shared,
    login: &str,
    name: &str,
    root: Option<RootMode>,
    disks: DiskSizes,
) -> Result<String, String> {
    let ws = ops::create_workspace(shared, login, name, root, disks)
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

async fn ws_disk_show(shared: &Shared, login: &str, name: &str) -> Result<String, String> {
    let ws = ops::workspace_info(shared, login, name)
        .await
        .map_err(shown)?;
    Ok(format!(
        "workspace\t{}G\ndocker\t{}G\n",
        ws.workspace_disk_gib, ws.docker_disk_gib
    ))
}

async fn ws_disk(
    shared: &Shared,
    login: &str,
    name: &str,
    disks: DiskSizes,
) -> Result<String, String> {
    let ws = ops::set_disks(shared, login, name, disks)
        .await
        .map_err(shown)?;
    let config = shared.controller.disks();
    Ok(format!(
        "{} will have a {}G workspace disk and a {}G Docker disk from the next start (run `chloe ws stop {}` to restart now)\n",
        ws.display_name(),
        config.workspace_gib(ws.disks),
        config.docker_gib(ws.disks),
        ws.display_name()
    ))
}

async fn ws_stop(shared: &Shared, login: &str, name: &str) -> Result<String, String> {
    let ws = ops::stop_workspace(shared, login, name)
        .await
        .map_err(shown)?;
    Ok(format!("stopped {}\n", ws.display_name()))
}

async fn ws_rm(shared: &Shared, login: &str, name: &str, disks: bool) -> Result<String, String> {
    let ws = ops::remove_workspace(shared, login, name, disks)
        .await
        .map_err(shown)?;
    Ok(if disks {
        format!("removed {} and its disks\n", ws.display_name())
    } else {
        format!(
            "removed {} (its disks are kept under the runner state directory as {}; pass --disks to delete them too)\n",
            ws.display_name(),
            ws.key()
        )
    })
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
    let owner = owner_or_personal(login, owner).map_err(shown)?;
    let names = ops::list_secrets(shared, login, &owner)
        .await
        .map_err(shown)?;
    Ok(names.iter().map(|n| format!("{n}\n")).collect())
}

async fn secret_set(
    shared: &Shared,
    login: &str,
    owner: Option<&str>,
    name: &str,
    with_spec: bool,
    stdin: &[u8],
) -> Result<String, String> {
    let owner = owner_or_personal(login, owner).map_err(shown)?;
    ops::check_secret_name(name).map_err(shown)?;
    ops::check_secret_manager(shared, login, &owner)
        .await
        .map_err(shown)?;
    let text =
        String::from_utf8(stdin.to_vec()).map_err(|_| "the input must be UTF-8".to_string())?;
    let input = if with_spec {
        toml::from_str::<SecretInput>(&text).map_err(|e| format!("invalid definition: {e}"))?
    } else {
        let value = text.trim_end_matches(['\n', '\r']);
        if value.is_empty() {
            return Err("the value is empty (pass it on stdin)".into());
        }
        SecretInput::value(value)
    };
    ops::set_secret(shared, login, &owner, name, input)
        .await
        .map_err(shown)?;
    Ok(format!(
        "saved {name} for {} (used from the next request; in a running workspace, shells opened from now on get its placeholder)\n",
        owner_arg(&owner)
    ))
}

async fn secret_show(
    shared: &Shared,
    login: &str,
    owner: Option<&str>,
    name: &str,
) -> Result<String, String> {
    let owner = owner_or_personal(login, owner).map_err(shown)?;
    let view = ops::show_secret(shared, login, &owner, name)
        .await
        .map_err(shown)?;
    let spec = toml::to_string(&view.spec).map_err(|e| e.to_string())?;
    let kind = if view.builtin { "built-in" } else { "defined" };
    Ok(format!(
        "# {} for {} ({kind})\n{spec}",
        view.name, view.owner
    ))
}

async fn secret_rm(
    shared: &Shared,
    login: &str,
    owner: Option<&str>,
    name: &str,
) -> Result<String, String> {
    let owner = owner_or_personal(login, owner).map_err(shown)?;
    ops::remove_secret(shared, login, &owner, name)
        .await
        .map_err(shown)?;
    Ok(format!("removed {name} from {}\n", owner_arg(&owner)))
}

async fn ws_policy_show(shared: &Shared, login: &str, name: &str) -> Result<String, String> {
    match ops::show_workspace_policy(shared, login, name)
        .await
        .map_err(shown)?
    {
        (_, Some(policy)) => toml::to_string(&policy).map_err(|e| e.to_string()),
        (ws, None) => Ok(format!("# no rules for {}\n", ws.display_name())),
    }
}

async fn ws_policy_set(
    shared: &Shared,
    login: &str,
    name: &str,
    stdin: &[u8],
) -> Result<String, String> {
    let ws = ops::find_workspace(shared, login, name)
        .await
        .map_err(shown)?;
    let text = std::str::from_utf8(stdin).map_err(|_| "the rules must be UTF-8".to_string())?;
    // 権限のない利用者には、書き方の誤りより先に権限がないことを伝える。
    ops::check_workspace_policy_admin(shared, login, &ws)
        .await
        .map_err(shown)?;
    let policy: WorkspacePolicy =
        toml::from_str(text).map_err(|e| format!("invalid rules: {e}"))?;
    let refreshed = ops::set_workspace_policy(shared, login, &ws, &policy)
        .await
        .map_err(shown)?;
    Ok(format!(
        "saved the rules of {} ({refreshed} running workspace(s) updated)\n",
        ws.display_name()
    ))
}

async fn ws_policy_rm(shared: &Shared, login: &str, name: &str) -> Result<String, String> {
    let ws = ops::find_workspace(shared, login, name)
        .await
        .map_err(shown)?;
    let refreshed = ops::remove_workspace_policy(shared, login, &ws)
        .await
        .map_err(shown)?;
    Ok(format!(
        "removed the rules of {} ({refreshed} running workspace(s) updated)\n",
        ws.display_name()
    ))
}

async fn policy_show(shared: &Shared, login: &str, owner: Option<&str>) -> Result<String, String> {
    let owner = owner_or_personal(login, owner).map_err(shown)?;
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
    owner: Option<&str>,
    stdin: &[u8],
) -> Result<String, String> {
    let owner = owner_or_personal(login, owner).map_err(shown)?;
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

async fn policy_rm(shared: &Shared, login: &str, owner: Option<&str>) -> Result<String, String> {
    let owner = owner_or_personal(login, owner).map_err(shown)?;
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

async fn key_list(shared: &Shared, login: &str) -> Result<String, String> {
    let keys = ops::list_ssh_keys(shared, login).await.map_err(shown)?;
    Ok(keys
        .iter()
        .map(|k| {
            let kind = k.public_key.split_whitespace().next().unwrap_or("");
            let used = k
                .last_used_at
                .map(|at| format!("last used at {at}"))
                .unwrap_or_else(|| "never used".into());
            format!(
                "{}\t{kind}\tadded at {}\t{used}\n",
                k.fingerprint, k.created_at
            )
        })
        .collect())
}

async fn key_rm(shared: &Shared, login: &str, fingerprint: &str) -> Result<String, String> {
    ops::remove_ssh_key(shared, login, fingerprint)
        .await
        .map_err(shown)?;
    Ok(format!("removed {fingerprint}\n"))
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
        assert!(needs_stdin(&["policy".into(), "set".into()]));
        assert!(needs_stdin(&[
            "ws".into(),
            "policy".into(),
            "set".into(),
            "acme/web".into()
        ]));
        assert!(!needs_stdin(&["ws".into(), "policy".into(), "show".into()]));
        assert!(!needs_stdin(&["ws".into(), "list".into()]));
    }

    #[test]
    fn parses_workspace_options() {
        let (root, disks) =
            parse_ws_options(&["--workspace", "50G", "--root", "persistent"], true).unwrap();
        assert_eq!(root, Some(RootMode::Persistent));
        assert_eq!(disks.workspace_gib, Some(50));
        assert_eq!(disks.docker_gib, None);
        let (_, disks) = parse_ws_options(&["--docker", "80GiB"], false).unwrap();
        assert_eq!(disks.docker_gib, Some(80));
        assert!(parse_ws_options(&["--root", "persistent"], false).is_err());
        assert!(parse_ws_options(&["--workspace"], true).is_err());
    }
}
