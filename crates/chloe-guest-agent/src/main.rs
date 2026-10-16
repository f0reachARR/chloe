//! ゲスト内常駐のエージェント。
//!
//! vsock で待ち受け、ホストからの要求ごとにプロセスを起動して、その入出力を中継する。
//! 接続を受けるのはホスト (CID 2) からだけで、ゲスト内のプロセスからの接続は拒否する。

use std::process::Stdio;

use anyhow::{Context, Result, bail};
use chloe_agent_proto::{
    AGENT_PORT, AgentMessage, ClientMessage, Connection, ExecRequest, ExitStatus, PROTOCOL_VERSION,
    Receiver, Sender, TtyRequest,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Child;
use tokio_vsock::{VMADDR_CID_ANY, VMADDR_CID_HOST, VsockAddr, VsockListener, VsockStream};
use tracing::{info, warn};

/// 利用者を指定しない要求を実行する利用者。
const DEFAULT_USER: &str = "user";
/// ランナーが渡した環境変数 (プレースホルダーなど)。chloe-load-config が置く。
const RUNNER_ENV: &str = "/run/chloe/env";
/// OS の信頼ストアを読まないツールのために、chloe の CA を含む信頼ストアを指す。
const CA_BUNDLE: &str = "/etc/ssl/certs/ca-certificates.crt";
/// エージェントの作業中の印 (chloe-activity が置く)。
const ACTIVITY_DIR: &str = "/run/chloe/activity";
/// これより長く更新されていない印は、エージェントが落ちて残ったものとみなす。
const ACTIVITY_TTL: std::time::Duration = std::time::Duration::from_secs(30 * 60);
const CA_ENV: &[&str] = &[
    "SSL_CERT_FILE",
    "REQUESTS_CA_BUNDLE",
    "NODE_EXTRA_CA_CERTS",
    "CODEX_CA_CERTIFICATE",
];
const READ_BUF_LEN: usize = 32 * 1024;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let listener = VsockListener::bind(VsockAddr::new(VMADDR_CID_ANY, AGENT_PORT))
        .context("failed to listen on vsock")?;
    info!(port = AGENT_PORT, "listening");

    loop {
        let (stream, peer) = listener.accept().await?;
        if peer.cid() != VMADDR_CID_HOST {
            warn!(cid = peer.cid(), "rejected connection from a non-host peer");
            continue;
        }
        tokio::spawn(async move {
            if let Err(error) = handle(stream).await {
                warn!("session failed: {error:#}");
            }
        });
    }
}

async fn handle(stream: VsockStream) -> Result<()> {
    let mut conn = Connection::new(stream);
    let request = match conn.recv().await? {
        Some(ClientMessage::Exec(request)) => request,
        Some(ClientMessage::Activity) => {
            let busy = busy_markers(std::path::Path::new(ACTIVITY_DIR), ACTIVITY_TTL);
            conn.send(&AgentMessage::Activity { busy }).await?;
            return Ok(());
        }
        _ => bail!("the first message must be Exec or Activity"),
    };
    let (mut tx, rx) = conn.split();
    if request.version != PROTOCOL_VERSION {
        let message = format!(
            "unsupported protocol version {} (agent speaks {PROTOCOL_VERSION})",
            request.version
        );
        tx.send(&AgentMessage::Error(message)).await?;
        return Ok(());
    }
    info!(command = ?request.command, user = ?request.user, tty = request.tty.is_some(), "exec");

    let result = match request.tty.clone() {
        Some(tty) => run_with_tty(&mut tx, rx, &request, tty).await,
        None => run_with_pipes(&mut tx, rx, &request).await,
    };
    match result {
        Ok(status) => tx.send(&AgentMessage::Exited(status)).await?,
        Err(error) => tx.send(&AgentMessage::Error(format!("{error:#}"))).await?,
    }
    Ok(())
}

/// 実行するプログラムと引数。root 以外の利用者には runuser で切り替える。
fn command_line(request: &ExecRequest) -> (String, Vec<String>) {
    let user = request.user.as_deref().unwrap_or(DEFAULT_USER);
    let command = &request.command;
    match (command.is_empty(), user == "root") {
        (true, true) => ("/bin/bash".into(), vec!["--login".into()]),
        // runuser --login は環境を消すので使わない。ログインシェルで profile を読ませる。
        (true, false) => (
            "/usr/sbin/runuser".into(),
            vec![
                "-u".into(),
                user.into(),
                "--".into(),
                "/bin/bash".into(),
                "--login".into(),
            ],
        ),
        (false, true) => (command[0].clone(), command[1..].to_vec()),
        (false, false) => {
            let mut args = vec!["-u".into(), user.into(), "--".into()];
            args.extend(command.iter().cloned());
            ("/usr/sbin/runuser".into(), args)
        }
    }
}

/// `dir` の中の、`ttl` より最近に更新された印の名前。
fn busy_markers(dir: &std::path::Path, ttl: std::time::Duration) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut busy: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age < ttl)
        })
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    busy.sort();
    busy
}

/// 起動するプロセスの環境変数。ランナーの環境変数、CA の変数、要求の環境変数の順に重ねる。
fn process_env(request: &ExecRequest) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = std::fs::read_to_string(RUNNER_ENV)
        .map(|content| parse_env_file(&content))
        .unwrap_or_default();
    env.extend(
        CA_ENV
            .iter()
            .map(|k| (k.to_string(), CA_BUNDLE.to_string())),
    );
    env.extend(request.env.iter().cloned());
    env
}

/// KEY=VALUE の行を読む。空行と # で始まる行は飛ばす。値の引用符は扱わない。
fn parse_env_file(content: &str) -> Vec<(String, String)> {
    content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .filter(|(key, _)| {
            !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn working_dir(request: &ExecRequest) -> String {
    match (&request.cwd, request.user.as_deref()) {
        (Some(cwd), _) => cwd.clone(),
        (None, Some("root")) => "/root".into(),
        (None, _) => "/workspace".into(),
    }
}

async fn run_with_tty(
    tx: &mut Sender<VsockStream>,
    mut rx: Receiver<VsockStream>,
    request: &ExecRequest,
    tty: TtyRequest,
) -> Result<ExitStatus> {
    let (pty, pts) = pty_process::open().context("failed to open a pty")?;
    pty.resize(pty_process::Size::new(tty.size.rows, tty.size.cols))?;
    let (program, args) = command_line(request);
    let mut child = pty_process::Command::new(&program)
        .args(&args)
        .env("TERM", &tty.term)
        .envs(process_env(request))
        .current_dir(working_dir(request))
        .spawn(pts)
        .with_context(|| format!("failed to spawn {program}"))?;
    send_started(tx, &child).await?;

    let (mut output, mut input) = pty.into_split();
    let mut buf = vec![0; READ_BUF_LEN];
    let mut output_open = true;
    let mut status = None;
    loop {
        tokio::select! {
            read = output.read(&mut buf), if output_open => match read {
                // プロセスが終わって端末が閉じると EIO になる。
                Ok(0) | Err(_) => output_open = false,
                Ok(n) => tx.send(&AgentMessage::Stdout(buf[..n].to_vec())).await?,
            },
            message = rx.recv::<ClientMessage>() => match message? {
                Some(ClientMessage::Stdin(data)) => input.write_all(&data).await?,
                Some(ClientMessage::Resize(size)) => {
                    input.resize(pty_process::Size::new(size.rows, size.cols))?;
                }
                Some(ClientMessage::Signal(signal)) => send_signal(&child, signal),
                Some(ClientMessage::CloseStdin) => {}
                Some(ClientMessage::Exec(_) | ClientMessage::Activity) => bail!("unexpected message"),
                // ホストが切断した。端末を閉じてプロセスに SIGHUP を届ける。
                None => {
                    drop(input);
                    let _ = child.start_kill();
                    return Ok(exit_status(child.wait().await?));
                }
            },
            exited = child.wait(), if status.is_none() => status = Some(exited?),
        }
        if let (false, Some(status)) = (output_open, status) {
            return Ok(exit_status(status));
        }
    }
}

async fn run_with_pipes(
    tx: &mut Sender<VsockStream>,
    mut rx: Receiver<VsockStream>,
    request: &ExecRequest,
) -> Result<ExitStatus> {
    let (program, args) = command_line(request);
    let mut child = tokio::process::Command::new(&program)
        .args(&args)
        .envs(process_env(request))
        .current_dir(working_dir(request))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("failed to spawn {program}"))?;
    send_started(tx, &child).await?;

    let mut stdin = child.stdin.take();
    let mut stdout = child.stdout.take().context("missing stdout")?;
    let mut stderr = child.stderr.take().context("missing stderr")?;
    let mut out_buf = vec![0; READ_BUF_LEN];
    let mut err_buf = vec![0; READ_BUF_LEN];
    let (mut stdout_open, mut stderr_open, mut client_open) = (true, true, true);
    let mut status = None;
    loop {
        tokio::select! {
            read = read_some(&mut stdout, &mut out_buf), if stdout_open => match read {
                Some(data) => tx.send(&AgentMessage::Stdout(data)).await?,
                None => stdout_open = false,
            },
            read = read_some(&mut stderr, &mut err_buf), if stderr_open => match read {
                Some(data) => tx.send(&AgentMessage::Stderr(data)).await?,
                None => stderr_open = false,
            },
            message = rx.recv::<ClientMessage>(), if client_open => match message? {
                Some(ClientMessage::Stdin(data)) => {
                    if let Some(stdin) = stdin.as_mut() {
                        // プロセスが標準入力を閉じていても、要求全体は失敗させない。
                        let _ = stdin.write_all(&data).await;
                    }
                }
                Some(ClientMessage::CloseStdin) => stdin = None,
                Some(ClientMessage::Signal(signal)) => send_signal(&child, signal),
                Some(ClientMessage::Resize(_)) => {}
                Some(ClientMessage::Exec(_) | ClientMessage::Activity) => bail!("unexpected message"),
                None => {
                    client_open = false;
                    let _ = child.start_kill();
                }
            },
            exited = child.wait(), if status.is_none() => status = Some(exited?),
        }
        if let (false, false, Some(status)) = (stdout_open, stderr_open, status) {
            return Ok(exit_status(status));
        }
    }
}

async fn read_some(reader: &mut (impl AsyncRead + Unpin), buf: &mut [u8]) -> Option<Vec<u8>> {
    match reader.read(buf).await {
        Ok(0) | Err(_) => None,
        Ok(n) => Some(buf[..n].to_vec()),
    }
}

async fn send_started(tx: &mut Sender<VsockStream>, child: &Child) -> Result<()> {
    let pid = child.id().unwrap_or_default();
    tx.send(&AgentMessage::Started { pid }).await?;
    Ok(())
}

fn send_signal(child: &Child, signal: i32) {
    let (Some(pid), Some(signal)) = (
        child
            .id()
            .and_then(|pid| rustix::process::Pid::from_raw(pid as i32)),
        rustix::process::Signal::from_named_raw(signal),
    ) else {
        return;
    };
    if let Err(error) = rustix::process::kill_process(pid, signal) {
        warn!("failed to send signal: {error}");
    }
}

fn exit_status(status: std::process::ExitStatus) -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    ExitStatus {
        code: status.code(),
        signal: status.signal(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(command: &[&str], user: Option<&str>) -> ExecRequest {
        ExecRequest {
            version: PROTOCOL_VERSION,
            command: command.iter().map(|s| s.to_string()).collect(),
            user: user.map(Into::into),
            env: vec![],
            cwd: None,
            tty: None,
        }
    }

    #[test]
    fn login_shell_for_default_user_keeps_environment() {
        let (program, args) = command_line(&request(&[], None));
        assert_eq!(program, "/usr/sbin/runuser");
        assert_eq!(args, ["-u", "user", "--", "/bin/bash", "--login"]);
    }

    #[test]
    fn reports_only_fresh_markers() {
        let dir = std::env::temp_dir().join(format!("chloe-activity-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("claude-s1"), "").unwrap();
        std::fs::write(dir.join("codex-s2"), "").unwrap();
        let old = std::fs::File::options()
            .write(true)
            .open(dir.join("codex-s2"))
            .unwrap();
        old.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
            .unwrap();
        assert_eq!(
            busy_markers(&dir, std::time::Duration::from_secs(1800)),
            ["claude-s1"]
        );
        assert!(busy_markers(&dir.join("missing"), std::time::Duration::from_secs(1)).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parses_runner_env_file() {
        let env = parse_env_file("A=1\n\n# c\nB=x=y\nbad line\n=v\nC D=1\n");
        assert_eq!(
            env,
            [
                ("A".to_string(), "1".to_string()),
                ("B".to_string(), "x=y".to_string())
            ]
        );
    }

    #[test]
    fn command_as_user_goes_through_runuser() {
        let (program, args) = command_line(&request(&["ls", "-l"], Some("user")));
        assert_eq!(program, "/usr/sbin/runuser");
        assert_eq!(args, ["-u", "user", "--", "ls", "-l"]);
    }

    #[test]
    fn command_as_root_runs_directly() {
        let (program, args) = command_line(&request(&["ls", "-l"], Some("root")));
        assert_eq!(program, "ls");
        assert_eq!(args, ["-l"]);
        assert_eq!(working_dir(&request(&[], Some("root"))), "/root");
    }
}
