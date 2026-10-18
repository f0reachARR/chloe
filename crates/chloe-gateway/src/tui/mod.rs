//! `ssh -t <host> chloe` で開く管理の TUI。
//!
//! ゲートウェイが SSH のチャネルに画面を描き、チャネルのデータをキーとして読む。操作は
//! 管理コマンドや管理 API と同じ [`crate::ops`] で行う。

mod app;
mod form;
mod picker;
mod policy_form;
mod secret_form;
mod term;

use std::io;
use std::sync::Arc;

use chloe_agent_proto::{ClientMessage, WindowSize};
use chloe_controller::workspace::MachineSize;
use chloe_secrets::Owner;
use ratatui::layout::{Rect, Size};
use ratatui::{Terminal, TerminalOptions, Viewport};
use russh::ChannelId;
use russh::server::Handle;
use terminput::{Event, KeyEventKind};
use tokio::sync::mpsc;
use tracing::{info, warn};

pub use self::picker::{Picked, Picker, pick};

use self::app::{Action, App, Outcome};
use self::term::{ENTER, InputParser, LEAVE, Output, SshBackend};
use crate::device_flow;
use crate::github_token::USER_TOKEN_SECRET;
use crate::ops;
use crate::session::Shared;

/// TUI を動かし、利用者が閉じるかチャネルが閉じたら終わる。利用者が閉じたときは `true`
/// を返す。画面の大きさが変わったら `size` に書き戻す。
pub async fn run(
    shared: Arc<Shared>,
    login: String,
    handle: Handle,
    channel: ChannelId,
    size: &mut WindowSize,
    input: &mut mpsc::Receiver<ClientMessage>,
) -> bool {
    let send = |data: Vec<u8>| {
        let handle = handle.clone();
        async move { handle.data(channel, data).await.is_ok() }
    };
    let output = Output::default();
    let mut terminal = match terminal(output.clone(), Size::new(size.cols, size.rows)) {
        Ok(terminal) => terminal,
        Err(error) => {
            warn!("failed to start the TUI: {error}");
            return false;
        }
    };
    if !send(ENTER.to_vec()).await {
        return false;
    }
    let (results, mut outcomes) = mpsc::unbounded_channel();
    let mut app = App::new(&login);
    let start = app.start();
    spawn_all(&shared, &login, &results, start);
    let mut parser = InputParser::default();
    loop {
        if let Err(error) = terminal.draw(|frame| app.render(frame)) {
            warn!("failed to draw the TUI: {error}");
            break;
        }
        if !send(output.take()).await || app.quit {
            break;
        }
        tokio::select! {
            message = input.recv() => match message {
                Some(ClientMessage::Stdin(data)) => {
                    for event in parser.feed(&data) {
                        match event {
                            Event::Key(key) if key.kind != KeyEventKind::Release => {
                                let actions = app.handle_key(key);
                                spawn_all(&shared, &login, &results, actions);
                            }
                            Event::Paste(text) => app.paste(&text),
                            _ => {}
                        }
                    }
                }
                Some(ClientMessage::Resize(new_size)) => {
                    *size = new_size;
                    if resize(&mut terminal, new_size).is_err() {
                        break;
                    }
                }
                Some(ClientMessage::CloseStdin) | None => break,
                Some(_) => {}
            },
            Some(outcome) = outcomes.recv() => {
                let actions = app.apply(outcome);
                spawn_all(&shared, &login, &results, actions);
            }
        }
    }
    let _ = send(output.take()).await;
    let _ = send(LEAVE.to_vec()).await;
    app.quit
}

/// SSH のチャネルに描く、画面全体を使う端末。
fn terminal(output: Output, size: Size) -> io::Result<Terminal<SshBackend>> {
    Terminal::with_options(
        SshBackend::new(output, size),
        TerminalOptions {
            viewport: Viewport::Fixed(Rect::from((ratatui::layout::Position::ORIGIN, size))),
        },
    )
}

/// 端末の大きさが変わった。描き直すため、画面を消しておく。
fn resize(terminal: &mut Terminal<SshBackend>, size: WindowSize) -> io::Result<()> {
    let size = Size::new(size.cols, size.rows);
    terminal.backend_mut().set_size(size);
    terminal.resize(Rect::from((ratatui::layout::Position::ORIGIN, size)))?;
    terminal.clear()
}

fn spawn_all(
    shared: &Arc<Shared>,
    login: &str,
    results: &mpsc::UnboundedSender<Outcome>,
    actions: Vec<Action>,
) {
    for action in actions {
        let (shared, login, results) = (shared.clone(), login.to_string(), results.clone());
        tokio::spawn(async move {
            if let Some(name) = action.audit_name() {
                info!(target: "audit", via = "tui", login = %login, command = name, "management command");
            }
            let outcome = perform(&shared, &login, action, &results).await;
            let _ = results.send(outcome);
        });
    }
}

/// 操作を行う。途中で知らせることがあれば `progress` に送る。
async fn perform(
    shared: &Shared,
    login: &str,
    action: Action,
    progress: &mpsc::UnboundedSender<Outcome>,
) -> Outcome {
    let result: Result<Outcome, String> = async {
        let done = |message: String, reload: Vec<Action>| Outcome::Done { message, reload };
        let shown = |e: ops::Error| e.to_string();
        Ok(match action {
            Action::LoadWorkspaces => {
                Outcome::Workspaces(ops::list_workspaces(shared, login).await.map_err(shown)?)
            }
            Action::CreateWorkspace { name, options } => {
                let ws = ops::create_workspace(shared, login, &name, options)
                    .await
                    .map_err(shown)?;
                done(
                    format!("created {}", ws.name),
                    vec![Action::LoadWorkspaces, Action::LoadRuleScopes],
                )
            }
            Action::SetRoot { name, root } => {
                let ws = ops::set_root(shared, login, &name, root)
                    .await
                    .map_err(shown)?;
                done(
                    format!(
                        "{} will use the {} root from the next start",
                        ws.display_name(),
                        root.as_str()
                    ),
                    vec![Action::LoadWorkspaces],
                )
            }
            Action::SetDisks { name, disks } => {
                let ws = ops::set_disks(shared, login, &name, disks)
                    .await
                    .map_err(shown)?;
                let config = shared.controller.disks();
                done(
                    format!(
                        "{} will have a {}G workspace disk and a {}G Docker disk from the next start",
                        ws.display_name(),
                        config.workspace_gib(ws.disks),
                        config.docker_gib(ws.disks)
                    ),
                    vec![Action::LoadWorkspaces],
                )
            }
            Action::SetMachine {
                name,
                machine,
                idle_stop,
            } => {
                let mut ws = ops::find_workspace(shared, login, &name)
                    .await
                    .map_err(shown)?;
                if machine != MachineSize::default() {
                    ws = ops::set_machine(shared, login, &name, machine)
                        .await
                        .map_err(shown)?;
                }
                if let Some(idle_stop) = idle_stop {
                    ws = ops::set_idle_stop(shared, login, &name, idle_stop)
                        .await
                        .map_err(shown)?;
                }
                let config = shared.controller.machine();
                let idle = ws
                    .idle_stop
                    .timeout(shared.controller.idle_timeout())
                    .map(|d| d.as_secs());
                done(
                    format!(
                        "{} will have {} vCPUs and {} of memory from the next start, and stops {}",
                        ws.display_name(),
                        config.vcpus(ws.machine),
                        ops::format_mib(config.memory_mib(ws.machine)),
                        match idle {
                            Some(_) => format!("{} after the last connection closes", ops::format_idle(idle)),
                            None => "only when you stop it".into(),
                        }
                    ),
                    vec![Action::LoadWorkspaces],
                )
            }
            Action::StopWorkspace(name) => {
                let ws = ops::stop_workspace(shared, login, &name)
                    .await
                    .map_err(shown)?;
                done(
                    format!("stopped {}", ws.display_name()),
                    vec![Action::LoadWorkspaces],
                )
            }
            Action::LoadRequests(name) => {
                let records = ops::tail_requests(shared, login, &name, None)
                    .await
                    .map_err(shown)?;
                Outcome::Requests { name, records }
            }
            Action::RemoveWorkspace { name, disks } => {
                let ws = ops::remove_workspace(shared, login, &name, disks)
                    .await
                    .map_err(shown)?;
                let kept = if disks { "" } else { " (its disks are kept)" };
                done(
                    format!("removed {}{kept}", ws.display_name()),
                    vec![Action::LoadWorkspaces, Action::LoadRuleScopes],
                )
            }
            Action::LoadOwners => {
                let mut owners = shared
                    .owners_for(login)
                    .await
                    .map_err(|e| format!("{e:#}"))?;
                // 個人のものを先に、Organization のあとにその team を並べる。
                owners.sort_by_key(|o| (!matches!(o, Owner::User(_)), ops::owner_arg(o)));
                Outcome::Owners(owners)
            }
            Action::LoadSecrets(owner) => {
                let mut names = ops::list_secrets(shared, login, &owner)
                    .await
                    .map_err(shown)?;
                // GitHub のユーザートークンは chloe github login で扱う。
                names.retain(|n| n != USER_TOKEN_SECRET);
                Outcome::Secrets { owner, names }
            }
            Action::ShowSecret { owner, name } => {
                let view = ops::show_secret(shared, login, &owner, &name)
                    .await
                    .map_err(shown)?;
                Outcome::Secret { owner, view }
            }
            Action::SaveSecret {
                owner,
                name,
                mut input,
                keep_values,
            } => {
                if keep_values {
                    ops::keep_secret_values(shared, login, &owner, &name, &mut input)
                        .await
                        .map_err(shown)?;
                }
                ops::set_secret(shared, login, &owner, &name, input)
                    .await
                    .map_err(shown)?;
                done(
                    format!("saved {name} for {}", ops::owner_arg(&owner)),
                    vec![Action::LoadSecrets(owner)],
                )
            }
            Action::RemoveSecret { owner, name } => {
                ops::remove_secret(shared, login, &owner, &name)
                    .await
                    .map_err(shown)?;
                done(
                    format!("removed {name} from {}", ops::owner_arg(&owner)),
                    vec![Action::LoadSecrets(owner)],
                )
            }
            Action::ShowPolicy(owner) => {
                let layer = ops::show_policy(shared, login, &owner)
                    .await
                    .map_err(shown)?;
                let mut inherited = Vec::new();
                if let Owner::Team { org, .. } = &owner
                    && let Some(upper) = ops::show_policy(shared, login, &Owner::Org(org.clone()))
                        .await
                        .ok()
                        .flatten()
                {
                    inherited.push((org.clone(), upper));
                }
                Outcome::Policy {
                    owner,
                    layer,
                    inherited,
                }
            }
            Action::SaveSettings { owner, settings } => {
                let refreshed = ops::set_owner_settings(shared, login, &owner, settings)
                    .await
                    .map_err(shown)?;
                done(
                    format!(
                        "saved the settings of {} ({refreshed} running workspace(s) updated)",
                        ops::owner_arg(&owner)
                    ),
                    vec![],
                )
            }
            Action::ResetSettings(owner) => {
                let refreshed = ops::set_owner_settings(shared, login, &owner, Default::default())
                    .await
                    .map_err(shown)?;
                done(
                    format!(
                        "reset the settings of {} ({refreshed} running workspace(s) updated)",
                        ops::owner_arg(&owner)
                    ),
                    vec![],
                )
            }
            Action::LoadRuleScopes => {
                Outcome::RuleScopes(ops::rule_scopes(shared, login).await.map_err(shown)?)
            }
            Action::ShowRules(scope) => {
                Outcome::Rules(ops::show_rules(shared, login, scope).await.map_err(shown)?)
            }
            Action::SaveRules { scope, rules } => {
                let refreshed = ops::set_rules(shared, login, &scope, rules)
                    .await
                    .map_err(shown)?;
                done(
                    format!(
                        "saved the rules of {} ({refreshed} running workspace(s) updated)",
                        scope.label()
                    ),
                    vec![Action::ShowRules(scope)],
                )
            }
            Action::ClearRules(scope) => {
                let refreshed = ops::set_rules(shared, login, &scope, vec![])
                    .await
                    .map_err(shown)?;
                done(
                    format!(
                        "removed the rules of {} ({refreshed} running workspace(s) updated)",
                        scope.label()
                    ),
                    vec![Action::ShowRules(scope)],
                )
            }
            Action::LoadTokens => {
                Outcome::Tokens(ops::list_tokens(shared, login).await.map_err(shown)?)
            }
            Action::CreateToken { name, expires } => {
                let ttl = expires
                    .as_deref()
                    .map(ops::parse_duration)
                    .transpose()
                    .map_err(shown)?;
                let (id, token) = ops::create_token(shared, login, &name, ttl)
                    .await
                    .map_err(shown)?;
                Outcome::TokenCreated { id, name, token }
            }
            Action::RevokeToken(id) => {
                ops::revoke_token(shared, login, id).await.map_err(shown)?;
                done(format!("revoked token {id}"), vec![Action::LoadTokens])
            }
            Action::LoadKeys => {
                Outcome::Keys(ops::list_ssh_keys(shared, login).await.map_err(shown)?)
            }
            Action::RemoveKey(fingerprint) => {
                ops::remove_ssh_key(shared, login, &fingerprint)
                    .await
                    .map_err(shown)?;
                done(format!("removed {fingerprint}"), vec![Action::LoadKeys])
            }
            Action::GitHubStatus => Outcome::GitHub(github_status(shared, login).await?),
            Action::GitHubLogin => {
                let github = shared
                    .github
                    .as_ref()
                    .ok_or("the GitHub App is not configured")?;
                let device = github
                    .start_device_flow()
                    .await
                    .map_err(|e| e.to_string())?;
                let _ = progress.send(Outcome::DeviceCode {
                    uri: device.verification_uri.clone(),
                    code: device.user_code.clone(),
                });
                match device_flow::renew(shared, github, login, &device)
                    .await
                    .map_err(|e| format!("{e:#}"))?
                {
                    Some(refreshed) => done(
                        format!("Signed in. {refreshed} running workspace(s) got the new token."),
                        vec![Action::GitHubStatus],
                    ),
                    None => return Err("the code expired before it was approved".into()),
                }
            }
            Action::GitHubLogout => {
                shared
                    .delete_user_token(login)
                    .await
                    .map_err(|e| format!("{e:#}"))?;
                done(
                    "signed out (the next connection needs the Device Flow)".into(),
                    vec![Action::GitHubStatus],
                )
            }
        })
    }
    .await;
    result.unwrap_or_else(Outcome::Failed)
}

async fn github_status(shared: &Shared, login: &str) -> Result<String, String> {
    if shared.github.is_none() {
        return Ok("The GitHub App is not configured.".into());
    }
    let token = shared
        .load_user_token(login)
        .await
        .map_err(|e| format!("{e:#}"))?;
    Ok(match token {
        Some(token) if token.is_usable() => format!(
            "Signed in to GitHub as {login}. The token is refreshed automatically{}.\n\nPersonal workspaces use it for git and GH_TOKEN.",
            token
                .refresh_expires_at
                .map(|at| format!(" until {}", ops::format_time(at as i64)))
                .unwrap_or_default()
        ),
        Some(_) => "The GitHub token expired. Press l to sign in again.".into(),
        None => "Not signed in. Press l to sign in.".into(),
    })
}
