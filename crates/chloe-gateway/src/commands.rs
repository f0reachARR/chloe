//! `chloe` で始まる管理コマンド。ゲートウェイが受け取り、VM には渡さない。
//!
//! ```text
//! chloe whoami              (接続した利用者の GitHub のログイン名)
//! chloe ws list
//! chloe ws create NAME [--root persistent|ephemeral] [--workspace 50G] [--docker 80G]
//!                       [--cpus 8] [--memory 16G] [--idle 2h|never]
//! chloe ws root NAME persistent|ephemeral
//! chloe ws disk NAME [--workspace 50G] [--docker 80G]   (大きくするだけ。次の起動から効く)
//! chloe ws machine NAME [--cpus 8] [--memory 16G]   (次の起動から効く)
//! chloe ws idle NAME [30m|2h|never|default]   (最後の接続が閉じてから止めるまで)
//! chloe ws policy show|set|rm NAME        (ワークスペースの規則。set は TOML を標準入力から読む)
//! chloe ws log NAME [--lines 200] [--blocked]   (HTTP の要求のログの末尾)
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
//! chloe inbox [--all]        (VM の中のエージェントからの依頼。--all なら答えたものも)
//! chloe inbox show ID
//! chloe inbox allow ID       (通信の許可を承認し、ワークスペースの規則に足す)
//! chloe inbox deny ID [MESSAGE]
//! chloe inbox answer ID [TEXT]   (TEXT を省くと標準入力から読む。選択肢の番号でもよい)
//! chloe web login            (Web の画面に一度だけログインできる URL と QR コード。5 分で切れる)
//! chloe web sessions         (Web の画面のセッションの一覧)
//! chloe web revoke ID
//! ```
//!
//! ワークスペースの NAME は SSH と同じ書き方 (`name`、`org/name`、`org/team/name`)。
//! OWNER は Organization (`org`) か team (`org/team`)。シークレットと設定は、OWNER を省くと
//! 接続した利用者の個人のものを扱う。

use chloe_audit::{Event, Record};
use chloe_controller::asks::AskBody;
use chloe_controller::catalog::{self, SecretInput};
use chloe_controller::runner::{PolicyLayer, WorkspacePolicy};
use chloe_controller::workspace::{DiskSizes, IdleStop, MachineSize, RootMode};

use crate::ops::{
    self, NewWorkspace, format_bytes, format_idle, format_mib, format_time_secs, owner_arg,
    owner_or_personal,
};
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
        [a, b, _] if a == "inbox" && b == "answer" => true,
        [a, b, c, ..] => a == "ws" && b == "policy" && c == "set",
        _ => false,
    }
}

pub async fn run(shared: &Shared, login: &str, args: &[String], stdin: &[u8]) -> Output {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["whoami"] => Ok(format!("{login}\n")),
        ["ws", "list"] => ws_list(shared, login).await,
        ["ws", "create", name, options @ ..] => match parse_ws_options(options, CREATE_OPTIONS) {
            Ok(options) => ws_create(shared, login, name, options).await,
            Err(e) => Err(e),
        },
        ["ws", "root", name, mode] => match mode.parse() {
            Ok(mode) => ws_root(shared, login, name, mode).await,
            Err(e) => Err(e.to_string()),
        },
        ["ws", "disk", name] => ws_disk_show(shared, login, name).await,
        ["ws", "disk", name, options @ ..] => match parse_ws_options(options, DISK_OPTIONS) {
            Ok(options) => ws_disk(shared, login, name, options.disks).await,
            Err(e) => Err(e),
        },
        ["ws", "machine", name] => ws_machine_show(shared, login, name).await,
        ["ws", "machine", name, options @ ..] => match parse_ws_options(options, MACHINE_OPTIONS) {
            Ok(options) => ws_machine(shared, login, name, options.machine).await,
            Err(e) => Err(e),
        },
        ["ws", "idle", name] => ws_idle_show(shared, login, name).await,
        ["ws", "idle", name, idle] => match ops::parse_idle_stop(idle) {
            Ok(idle) => ws_idle(shared, login, name, idle).await,
            Err(e) => Err(shown(e)),
        },
        ["ws", "policy", "show", name] => ws_policy_show(shared, login, name).await,
        ["ws", "policy", "set", name] => ws_policy_set(shared, login, name, stdin).await,
        ["ws", "policy", "rm", name] => ws_policy_rm(shared, login, name).await,
        ["ws", "log", name, options @ ..] => match parse_log_options(options) {
            Ok((lines, blocked)) => ws_log(shared, login, name, lines, blocked).await,
            Err(e) => Err(e),
        },
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
        ["inbox"] | ["inbox", "list"] => inbox_list(shared, login, false).await,
        ["inbox", "--all"] | ["inbox", "list", "--all"] => inbox_list(shared, login, true).await,
        ["inbox", "show", id] => inbox_show(shared, login, id).await,
        ["inbox", "allow", id] => inbox_reply(shared, login, id, ops::Reply::Allow).await,
        ["inbox", "deny", id, message @ ..] => {
            let message = (!message.is_empty()).then(|| message.join(" "));
            inbox_reply(shared, login, id, ops::Reply::Deny(message)).await
        }
        ["inbox", "answer", id] => {
            let text = String::from_utf8_lossy(stdin).trim_end().to_string();
            inbox_answer(shared, login, id, text).await
        }
        ["inbox", "answer", id, text @ ..] => inbox_answer(shared, login, id, text.join(" ")).await,
        ["web", "login"] => web_login(shared, login).await,
        ["web", "sessions"] => web_sessions(shared, login).await,
        ["web", "revoke", id] => web_revoke(shared, login, id).await,
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
        "usage:\n  chloe whoami\n  chloe ws list\n  chloe ws create NAME [--root persistent|ephemeral] [--workspace SIZE] [--docker SIZE] [--cpus N] [--memory MEM] [--idle TIME]\n  chloe ws root NAME persistent|ephemeral\n  chloe ws disk NAME [--workspace SIZE] [--docker SIZE]   (grows the disks from the next start)\n  chloe ws machine NAME [--cpus N] [--memory MEM]   (resizes the VM from the next start)\n  chloe ws idle NAME [TIME]   (stops the VM TIME after the last connection closes)\n  chloe ws policy show NAME\n  chloe ws policy set NAME   (reads the rules as TOML from stdin)\n  chloe ws policy rm NAME\n  chloe ws log NAME [--lines N] [--blocked]   (shows the end of the HTTP request log)\n  chloe ws stop NAME\n  chloe ws rm NAME [--disks]   (--disks also deletes its disks)\n  chloe secret list [--owner OWNER]\n  chloe secret show [--owner OWNER] NAME\n  chloe secret set [--owner OWNER] NAME   (reads the value from stdin)\n  chloe secret set [--owner OWNER] NAME --spec   (reads the definition and values as TOML from stdin)\n  chloe secret rm [--owner OWNER] NAME\n  chloe policy show [OWNER]\n  chloe policy set [OWNER]   (reads TOML from stdin)\n  chloe policy rm [OWNER]\n  chloe token create NAME [--expires 30d]\n  chloe token list\n  chloe token revoke ID\n  chloe key list\n  chloe key rm FINGERPRINT\n  chloe github login|status|logout\n  chloe inbox [--all]   (asks from the agents in your workspaces)\n  chloe inbox show ID\n  chloe inbox allow ID   (adds a rule for the host to the workspace)\n  chloe inbox deny ID [MESSAGE]\n  chloe inbox answer ID [TEXT]   (an option number or any text; reads stdin without TEXT)\n  chloe web login   (a one-time URL and QR code to sign in to the web UI)\n  chloe web sessions\n  chloe web revoke ID\nOWNER is an organization (org) or a team (org/team). Without it, secrets and policy are your personal ones.\nSIZE is in GiB (50G or 50). MEM is like 8G or 512M.\nTIME is like 30m, 2h or 1d; never keeps the VM running, default uses the gateway's.\nbuilt-in secrets: {} (other names need a definition: --spec)\n",
        kinds.join(", ")
    )
}

const DISK_OPTIONS: &[&str] = &["--workspace", "--docker"];
const MACHINE_OPTIONS: &[&str] = &["--cpus", "--memory"];
const CREATE_OPTIONS: &[&str] = &[
    "--root",
    "--workspace",
    "--docker",
    "--cpus",
    "--memory",
    "--idle",
];

/// `ws create`、`ws disk` と `ws machine` のオプション。`allowed` にあるものだけを受け付ける。
fn parse_ws_options(options: &[&str], allowed: &[&str]) -> Result<NewWorkspace, String> {
    let mut parsed = NewWorkspace::default();
    let mut options = options.iter();
    while let Some(option) = options.next() {
        if !allowed.contains(option) {
            return Err(format!("unknown option: {option}\n{}", help()));
        }
        let value = options
            .next()
            .copied()
            .ok_or_else(|| format!("{option} needs a value"))?;
        match *option {
            "--root" => parsed.root = Some(value.parse().map_err(|e| format!("{e}"))?),
            "--workspace" => {
                parsed.disks.workspace_gib = Some(ops::parse_gib(value).map_err(shown)?)
            }
            "--docker" => parsed.disks.docker_gib = Some(ops::parse_gib(value).map_err(shown)?),
            "--cpus" => {
                parsed.machine.vcpus = Some(
                    value
                        .parse()
                        .map_err(|_| format!("invalid number of vCPUs: {value}"))?,
                )
            }
            "--memory" => parsed.machine.memory_mib = Some(ops::parse_mib(value).map_err(shown)?),
            "--idle" => parsed.idle_stop = ops::parse_idle_stop(value).map_err(shown)?,
            _ => unreachable!("{option} is allowed but not handled"),
        }
    }
    Ok(parsed)
}

/// `ws log` のオプション。読む行数と、遮断した要求だけにするか。
fn parse_log_options(options: &[&str]) -> Result<(Option<usize>, bool), String> {
    let (mut lines, mut blocked) = (None, false);
    let mut options = options.iter();
    while let Some(option) = options.next() {
        match *option {
            "--blocked" => blocked = true,
            "--lines" => {
                let value = options.next().ok_or("--lines needs a value")?;
                lines = Some(
                    value
                        .parse()
                        .map_err(|_| format!("invalid number of lines: {value}"))?,
                );
            }
            _ => return Err(format!("unknown option: {option}\n{}", help())),
        }
    }
    Ok((lines, blocked))
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
    let workspaces = ops::list_workspaces(shared, login, false)
        .await
        .map_err(shown)?;
    Ok(workspaces
        .iter()
        .map(|ws| {
            let state = if ws.running { "running" } else { "stopped" };
            format!(
                "{}\t{}\t{state}\t{}G\t{}G\t{}cpu\t{}\t{}\n",
                ws.name,
                ws.root.as_str(),
                ws.workspace_disk_gib,
                ws.docker_disk_gib,
                ws.vcpus,
                format_mib(ws.memory_mib),
                format_idle(ws.idle_timeout_secs)
            )
        })
        .collect())
}

async fn ws_create(
    shared: &Shared,
    login: &str,
    name: &str,
    options: NewWorkspace,
) -> Result<String, String> {
    let ws = ops::create_workspace(shared, login, name, options)
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

async fn ws_machine_show(shared: &Shared, login: &str, name: &str) -> Result<String, String> {
    let ws = ops::workspace_info(shared, login, name)
        .await
        .map_err(shown)?;
    Ok(format!(
        "cpus\t{}\nmemory\t{}\n",
        ws.vcpus,
        format_mib(ws.memory_mib)
    ))
}

async fn ws_machine(
    shared: &Shared,
    login: &str,
    name: &str,
    machine: MachineSize,
) -> Result<String, String> {
    let ws = ops::set_machine(shared, login, name, machine)
        .await
        .map_err(shown)?;
    let config = shared.controller.machine();
    Ok(format!(
        "{} will have {} vCPUs and {} of memory from the next start (run `chloe ws stop {}` to restart now)\n",
        ws.display_name(),
        config.vcpus(ws.machine),
        format_mib(config.memory_mib(ws.machine)),
        ws.display_name()
    ))
}

async fn ws_idle_show(shared: &Shared, login: &str, name: &str) -> Result<String, String> {
    let ws = ops::workspace_info(shared, login, name)
        .await
        .map_err(shown)?;
    Ok(format!("{}\n", format_idle(ws.idle_timeout_secs)))
}

async fn ws_idle(
    shared: &Shared,
    login: &str,
    name: &str,
    idle: IdleStop,
) -> Result<String, String> {
    let ws = ops::set_idle_stop(shared, login, name, idle)
        .await
        .map_err(shown)?;
    let timeout = idle
        .timeout(shared.controller.idle_timeout())
        .map(|d| d.as_secs());
    Ok(match timeout {
        Some(_) => format!(
            "{} will stop {} after the last connection closes\n",
            ws.display_name(),
            format_idle(timeout)
        ),
        None => format!(
            "{} will keep running until you run `chloe ws stop {}`\n",
            ws.display_name(),
            ws.display_name()
        ),
    })
}

async fn ws_log(
    shared: &Shared,
    login: &str,
    name: &str,
    lines: Option<usize>,
    blocked: bool,
) -> Result<String, String> {
    let records = ops::tail_requests(shared, login, name, lines)
        .await
        .map_err(shown)?;
    Ok(records
        .iter()
        .filter(|r| !blocked || matches!(&r.event, Event::Request(req) if req.blocked()))
        .map(request_line)
        .collect())
}

/// 依頼の一覧。ID、ワークスペース、種類、状態、出た時刻、短い説明をタブで区切る。
async fn inbox_list(shared: &Shared, login: &str, all: bool) -> Result<String, String> {
    let asks = ops::list_asks(shared, login, all).await.map_err(shown)?;
    Ok(asks
        .iter()
        .map(|info| {
            let ask = &info.ask;
            format!(
                "{}\t{}\t{}\t{}\t{}\t{}\n",
                ask.id,
                info.workspace,
                ask.body.kind(),
                ask.status,
                format_time_secs(ask.created_at),
                ask.body.summary()
            )
        })
        .collect())
}

fn parse_ask_id(id: &str) -> Result<i64, String> {
    id.parse().map_err(|_| format!("invalid ask id: {id}"))
}

async fn inbox_show(shared: &Shared, login: &str, id: &str) -> Result<String, String> {
    let info = ops::show_ask(shared, login, parse_ask_id(id)?)
        .await
        .map_err(shown)?;
    Ok(ask_detail(&info))
}

/// 依頼の欄をすべて、一行に一つずつ出す。
fn ask_detail(info: &ops::AskInfo) -> String {
    let ask = &info.ask;
    let mut out = format!(
        "id: {}\nworkspace: {}\nkind: {}\nstatus: {}\ncreated: {}\nexpires: {}\n",
        ask.id,
        info.workspace,
        ask.body.kind(),
        ask.status,
        format_time_secs(ask.created_at),
        format_time_secs(ask.expires_at)
    );
    match &ask.body {
        AskBody::Network {
            host,
            methods,
            paths,
        } => {
            out.push_str(&format!("host: {host}\n"));
            if !methods.is_empty() {
                out.push_str(&format!("methods: {}\n", methods.join(" ")));
            }
            if !paths.is_empty() {
                out.push_str(&format!("paths: {}\n", paths.join(" ")));
            }
        }
        AskBody::Question { question, options } => {
            out.push_str(&format!("question: {}\n", indent(question)));
            for (i, option) in options.iter().enumerate() {
                out.push_str(&format!("option {}: {option}\n", i + 1));
            }
        }
    }
    if let Some(reason) = &ask.reason {
        out.push_str(&format!("reason: {}\n", indent(reason)));
    }
    if let Some(answer) = &ask.answer {
        out.push_str(&format!("answer: {}\n", indent(answer)));
    }
    if let Some(by) = &ask.answered_by {
        out.push_str(&format!("answered by: {by}\n"));
    }
    out
}

/// 複数行の値の二行目からを字下げする。
fn indent(text: &str) -> String {
    text.trim_end().replace('\n', "\n  ")
}

async fn inbox_reply(
    shared: &Shared,
    login: &str,
    id: &str,
    reply: ops::Reply,
) -> Result<String, String> {
    let info = ops::answer_ask(shared, login, parse_ask_id(id)?, reply)
        .await
        .map_err(shown)?;
    Ok(format!("ask {} is {}\n", info.ask.id, info.ask.status))
}

/// 質問に答える。選択肢があって `text` がその番号なら、その選択肢を答えにする。
async fn inbox_answer(
    shared: &Shared,
    login: &str,
    id: &str,
    text: String,
) -> Result<String, String> {
    let id = parse_ask_id(id)?;
    let info = ops::show_ask(shared, login, id).await.map_err(shown)?;
    let text = match (&info.ask.body, text.trim().parse::<usize>()) {
        (AskBody::Question { options, .. }, Ok(n)) if (1..=options.len()).contains(&n) => {
            options[n - 1].clone()
        }
        _ => text,
    };
    inbox_reply(shared, login, &id.to_string(), ops::Reply::Answer(text)).await
}

/// ログの一件を、タブで区切った一行にする。時刻、判定、ステータス、メソッド、ホスト、パス、
/// 応答の大きさ、かかった時間、差し替えた規則か遮断した規則、誤り。ないものは `-`。
fn request_line(record: &Record) -> String {
    let time = format_time_secs(record.ts as i64);
    let req = match &record.event {
        Event::Passthrough { host } => {
            return format!("{time}\tpassthrough\t-\t-\t{host}\t-\t-\t-\t-\t-\n");
        }
        Event::Request(req) => req,
    };
    let or_dash = |value: Option<String>| value.unwrap_or_else(|| "-".into());
    let rules = if req.injected.is_empty() {
        or_dash(req.rule.clone())
    } else {
        req.injected.join(",")
    };
    let mut path = req.path.clone();
    if !req.query_keys.is_empty() {
        path.push('?');
        path.push_str(&req.query_keys.join("&"));
    }
    format!(
        "{time}\t{}\t{}\t{}\t{}\t{path}\t{}\t{}\t{rules}\t{}\n",
        req.decision,
        or_dash(req.status.map(|s| s.to_string())),
        req.method,
        req.host,
        or_dash(req.response_bytes.map(format_bytes)),
        or_dash(req.duration_ms.map(|ms| format!("{ms}ms"))),
        or_dash(req.error.clone()),
    )
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

async fn web_login(shared: &Shared, login: &str) -> Result<String, String> {
    let url = ops::create_login_link(shared, login).await.map_err(shown)?;
    let qr = qrcode::QrCode::new(url.as_bytes())
        .map(|code| {
            // 暗い背景の端末でも読めるよう、明るい側を黒地のブロックで描く。
            code.render::<qrcode::render::unicode::Dense1x2>()
                .dark_color(qrcode::render::unicode::Dense1x2::Light)
                .light_color(qrcode::render::unicode::Dense1x2::Dark)
                .build()
        })
        .unwrap_or_default();
    Ok(format!(
        "{qr}\nOpen this URL within 5 minutes to sign in to the web UI (it works once):\n{url}\n"
    ))
}

async fn web_sessions(shared: &Shared, login: &str) -> Result<String, String> {
    let sessions = ops::list_web_sessions(shared, login).await.map_err(shown)?;
    Ok(sessions
        .iter()
        .map(|s| {
            let used = s
                .last_used_at
                .map(|at| format!("last used {}", format_time_secs(at)))
                .unwrap_or_else(|| "never used".into());
            format!(
                "{}\tsigned in {}\texpires {}\t{used}\n",
                s.id,
                format_time_secs(s.created_at),
                format_time_secs(s.expires_at)
            )
        })
        .collect())
}

async fn web_revoke(shared: &Shared, login: &str, id: &str) -> Result<String, String> {
    let id: i64 = id
        .parse()
        .map_err(|_| format!("invalid session id: {id}"))?;
    ops::revoke_web_session(shared, login, id)
        .await
        .map_err(shown)?;
    Ok(format!("signed out session {id}\n"))
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
    fn parses_log_options() {
        assert_eq!(parse_log_options(&[]).unwrap(), (None, false));
        assert_eq!(
            parse_log_options(&["--blocked", "--lines", "50"]).unwrap(),
            (Some(50), true)
        );
        assert!(parse_log_options(&["--lines"]).is_err());
        assert!(parse_log_options(&["--lines", "x"]).is_err());
        assert!(parse_log_options(&["--all"]).is_err());
    }

    #[test]
    fn formats_request_lines() {
        let record = Record {
            ts: 1_792_152_059.5,
            event: Event::Request(Box::new(chloe_audit::Request {
                host: "api.example.com".into(),
                method: "POST".into(),
                path: "/v1/messages".into(),
                query_keys: vec!["beta".into()],
                status: Some(200),
                decision: "allowed".into(),
                injected: vec!["anthropic_api_key-0".into()],
                response_bytes: Some(2048),
                duration_ms: Some(350),
                ..Default::default()
            })),
        };
        assert_eq!(
            request_line(&record),
            "2026-10-16 12:00:59\tallowed\t200\tPOST\tapi.example.com\t/v1/messages?beta\t2.0K\t350ms\tanthropic_api_key-0\t-\n"
        );
        let record = Record {
            ts: 0.0,
            event: Event::Passthrough {
                host: "a.example".into(),
            },
        };
        assert!(request_line(&record).contains("\tpassthrough\t"));
    }

    #[test]
    fn parses_workspace_options() {
        let options = parse_ws_options(
            &["--workspace", "50G", "--root", "persistent"],
            CREATE_OPTIONS,
        )
        .unwrap();
        assert_eq!(options.root, Some(RootMode::Persistent));
        assert_eq!(options.disks.workspace_gib, Some(50));
        assert_eq!(options.disks.docker_gib, None);
        let options = parse_ws_options(&["--docker", "80GiB"], DISK_OPTIONS).unwrap();
        assert_eq!(options.disks.docker_gib, Some(80));
        assert!(parse_ws_options(&["--root", "persistent"], DISK_OPTIONS).is_err());
        assert!(parse_ws_options(&["--workspace"], CREATE_OPTIONS).is_err());

        let options = parse_ws_options(
            &["--cpus", "8", "--memory", "16G", "--idle", "never"],
            CREATE_OPTIONS,
        )
        .unwrap();
        assert_eq!(
            options.machine,
            MachineSize {
                vcpus: Some(8),
                memory_mib: Some(16384)
            }
        );
        assert_eq!(options.idle_stop, IdleStop::Never);
        assert_eq!(options.disks, DiskSizes::default());
        let options = parse_ws_options(&["--memory", "512M"], MACHINE_OPTIONS).unwrap();
        assert_eq!(options.machine.memory_mib, Some(512));
        assert!(parse_ws_options(&["--cpus", "x"], MACHINE_OPTIONS).is_err());
        assert!(parse_ws_options(&["--idle", "2h"], MACHINE_OPTIONS).is_err());
        assert!(parse_ws_options(&["--cpus", "2"], DISK_OPTIONS).is_err());
    }
}
