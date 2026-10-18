//! ゲスト内常駐のエージェント。
//!
//! vsock で待ち受け、ホストからの要求ごとにプロセスを起動して、その入出力を中継する。
//! ポート転送の要求では、VM の中から TCP で接続して、バイト列をそのまま中継する。
//! 接続を受けるのはホスト (CID 2) からだけで、ゲスト内のプロセスからの接続は拒否する。

use std::process::Stdio;

use anyhow::{Context, Result, bail};
use chloe_agent_proto::{
    AGENT_PORT, AgentMessage, ClientMessage, ConnectRequest, Connection, ExecRequest, ExitStatus,
    GuestFile, PROTOCOL_VERSION, Receiver, Sender, TtyRequest,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Child;
use tokio_vsock::{VMADDR_CID_ANY, VMADDR_CID_HOST, VsockAddr, VsockListener, VsockStream};
use tracing::{info, warn};

/// 利用者を指定しない要求を実行する利用者。
const DEFAULT_USER: &str = "user";
/// ランナーが渡した環境変数 (プレースホルダーなど)。chloe-load-config が置く。
const RUNNER_ENV: &str = "/run/chloe/env";
/// 起動したときにランナーが渡したファイル ([`GuestFile`] の JSON)。chloe-load-config が置く。
const RUNNER_FILES: &str = "/run/chloe/files.json";
/// 置いたファイルの一覧。ランナーが渡さなくなったファイルを消すのに使う。ルートの層と一緒に
/// 残るよう、/run ではなくここに置く。
const FILES_MANIFEST: &str = "/var/lib/chloe/files";
/// ランナーのファイルを置かない場所。
const RESERVED_DIRS: &[&str] = &["/run/chloe", "/proc", "/sys", "/dev"];
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
/// ポート転送で、VM の中の接続先につながるまで待つ時間。
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // 起動したときのファイルを置く。置けなくても、シェルは使えるようにする。
    match std::fs::read(RUNNER_FILES) {
        Ok(json) => match serde_json::from_slice::<Vec<GuestFile>>(&json) {
            Ok(files) => update_files(&files),
            Err(error) => warn!("invalid {RUNNER_FILES}: {error}"),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => warn!("failed to read {RUNNER_FILES}: {error}"),
    }

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
        Some(ClientMessage::Connect(request)) => return forward(conn, request).await,
        Some(ClientMessage::SetEnv(env)) => {
            match write_runner_env(std::path::Path::new(RUNNER_ENV), &env) {
                Ok(()) => {
                    info!(count = env.len(), "runner environment updated");
                    conn.send(&AgentMessage::EnvUpdated).await?;
                }
                Err(error) => {
                    conn.send(&AgentMessage::Error(format!("{error:#}")))
                        .await?
                }
            }
            return Ok(());
        }
        Some(ClientMessage::SetFiles(files)) => {
            match apply_files(&files, std::path::Path::new(FILES_MANIFEST), lookup_user) {
                Ok(()) => {
                    info!(count = files.len(), "runner files updated");
                    conn.send(&AgentMessage::FilesUpdated).await?;
                }
                Err(error) => {
                    conn.send(&AgentMessage::Error(format!("{error:#}")))
                        .await?
                }
            }
            return Ok(());
        }
        _ => bail!("the first message must be Exec, Activity, Connect, SetEnv or SetFiles"),
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

/// ポート転送。VM の中で `request` の宛先に接続し、ホストとのあいだでバイト列を中継する。
async fn forward(mut conn: Connection<VsockStream>, request: ConnectRequest) -> Result<()> {
    if request.version != PROTOCOL_VERSION {
        let message = format!(
            "unsupported protocol version {} (agent speaks {PROTOCOL_VERSION})",
            request.version
        );
        conn.send(&AgentMessage::Error(message)).await?;
        return Ok(());
    }
    let target = (request.host.as_str(), request.port);
    let mut tcp =
        match tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::TcpStream::connect(target)).await {
            Ok(Ok(tcp)) => tcp,
            Ok(Err(error)) => {
                let message = format!("connect to {}:{}: {error}", request.host, request.port);
                conn.send(&AgentMessage::Error(message)).await?;
                return Ok(());
            }
            Err(_) => {
                let message = format!("connect to {}:{}: timed out", request.host, request.port);
                conn.send(&AgentMessage::Error(message)).await?;
                return Ok(());
            }
        };
    info!(host = %request.host, port = request.port, "forward");
    conn.send(&AgentMessage::Connected).await?;
    let (mut vsock, buffered) = conn.into_raw();
    tcp.write_all(&buffered).await?;
    // どちらかが閉じても、もう片方の向きは最後まで流す。
    let _ = tokio::io::copy_bidirectional(&mut vsock, &mut tcp).await;
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
///
/// 印にエージェントのプロセス ID が書いてあり、そのプロセスがもう動いていなければ、
/// 印を消して作業中とはみなさない (ターンの失敗でフックが呼ばれずに残った印など)。
fn busy_markers(dir: &std::path::Path, ttl: std::time::Duration) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut busy = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let fresh = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age < ttl);
        if !fresh {
            continue;
        }
        let pid = std::fs::read_to_string(entry.path())
            .ok()
            .and_then(|content| content.trim().parse::<u32>().ok());
        if let Some(pid) = pid
            && !std::path::Path::new(&format!("/proc/{pid}")).exists()
        {
            let _ = std::fs::remove_file(entry.path());
            continue;
        }
        busy.push(entry.file_name().to_string_lossy().into_owned());
    }
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

/// ランナーの環境変数のファイルを書き換える。ログインシェルがこのファイルをシェルとして読むので、
/// 名前と値はシェルで特別な意味を持たない文字だけに限る。途中で読まれても壊れないよう、
/// 一時的なファイルに書いてから置き換える。
fn write_runner_env(path: &std::path::Path, env: &[(String, String)]) -> Result<()> {
    let name_ok = |k: &str| {
        !k.is_empty()
            && !k.starts_with(|c: char| c.is_ascii_digit())
            && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    let value_ok = |v: &str| {
        v.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-:/+=@".contains(c))
    };
    let mut content = String::new();
    for (key, value) in env {
        if !name_ok(key) || !value_ok(value) {
            bail!("refused an unsafe environment variable: {key}");
        }
        content.push_str(&format!("{key}={value}\n"));
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, content).context("write the runner environment")?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644))?;
    std::fs::rename(&tmp, path).context("replace the runner environment")?;
    Ok(())
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

/// 起動したときに、ランナーのファイルを置く。失敗はログに残すだけにする。
fn update_files(files: &[GuestFile]) {
    match apply_files(files, std::path::Path::new(FILES_MANIFEST), lookup_user) {
        Ok(()) => info!(count = files.len(), "runner files placed"),
        Err(error) => warn!("failed to place the runner files: {error:#}"),
    }
}

/// ランナーのファイルを置き、前に置いて今回にないファイルを消す。置いたファイルの一覧を
/// `manifest` に残す。パスか持ち主がおかしいファイルが一つでもあれば、何も変えない。
fn apply_files(
    files: &[GuestFile],
    manifest: &std::path::Path,
    lookup: impl Fn(&str) -> Option<(u32, u32)>,
) -> Result<()> {
    let mut owners = Vec::with_capacity(files.len());
    for file in files {
        check_file_path(&file.path)?;
        if file.mode > 0o777 {
            bail!("invalid mode {:o} for {}", file.mode, file.path);
        }
        let owner = lookup(&file.owner)
            .with_context(|| format!("no such user {} for {}", file.owner, file.path))?;
        owners.push(owner);
    }
    let previous = std::fs::read_to_string(manifest).unwrap_or_default();
    let mut failed = Vec::new();
    for (file, (uid, gid)) in files.iter().zip(owners) {
        if let Err(error) = write_file(file, uid, gid) {
            warn!(path = %file.path, "failed to place a runner file: {error:#}");
            failed.push(file.path.as_str());
        }
    }
    for old in previous.lines().filter(|line| !line.is_empty()) {
        if files.iter().any(|file| file.path == old) {
            continue;
        }
        match std::fs::remove_file(old) {
            Ok(()) => info!(path = old, "removed a runner file"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => warn!(path = old, "failed to remove a runner file: {error}"),
        }
    }
    // 置けなかったファイルも一覧に入れ、あとで渡さなくなったら消す。
    let list: String = files
        .iter()
        .map(|file| format!("{}\n", file.path))
        .collect();
    if let Some(dir) = manifest.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = manifest.with_extension("tmp");
    std::fs::write(&tmp, list).context("write the list of runner files")?;
    std::fs::rename(&tmp, manifest).context("replace the list of runner files")?;
    if !failed.is_empty() {
        bail!("failed to place {}", failed.join(", "));
    }
    Ok(())
}

/// 絶対パスで、`.` と `..` を含まず、ランナーのファイルを置かない場所の外にあるか。
fn check_file_path(path: &str) -> Result<()> {
    use std::path::{Component, Path};
    let p = Path::new(path);
    let mut components = p.components();
    let valid = components.next() == Some(Component::RootDir)
        && p.file_name().is_some()
        && components.all(|c| matches!(c, Component::Normal(_)))
        && !path.chars().any(char::is_control);
    if !valid {
        bail!("invalid path {path:?}");
    }
    if RESERVED_DIRS.iter().any(|dir| p.starts_with(dir)) {
        bail!("path {path} is reserved");
    }
    Ok(())
}

/// 一時的なファイルに書いてから置き換える。足りないディレクトリは作り、持ち主をそろえる。
fn write_file(file: &GuestFile, uid: u32, gid: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let path = std::path::Path::new(&file.path);
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        bail!("invalid path {}", file.path);
    };
    let missing: Vec<&std::path::Path> = dir.ancestors().take_while(|d| !d.exists()).collect();
    for new in missing.into_iter().rev() {
        std::fs::create_dir(new).with_context(|| format!("create {}", new.display()))?;
        std::os::unix::fs::chown(new, Some(uid), Some(gid))?;
    }
    let tmp = dir.join(format!(".{}.chloe-tmp", name.to_string_lossy()));
    std::fs::write(&tmp, &file.content).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(file.mode))?;
    std::os::unix::fs::chown(&tmp, Some(uid), Some(gid))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replace {}", file.path))?;
    Ok(())
}

/// 利用者の UID と GID を /etc/passwd から引く。
fn lookup_user(name: &str) -> Option<(u32, u32)> {
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    passwd.lines().find_map(|line| {
        let mut parts = line.split(':');
        (parts.next()? == name).then_some(())?;
        let uid = parts.nth(1)?.parse().ok()?;
        let gid = parts.next()?.parse().ok()?;
        Some((uid, gid))
    })
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
                Some(ClientMessage::Exec(_) | ClientMessage::Activity | ClientMessage::Connect(_) | ClientMessage::SetEnv(_) | ClientMessage::SetFiles(_)) => bail!("unexpected message"),
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
                Some(ClientMessage::Exec(_) | ClientMessage::Activity | ClientMessage::Connect(_) | ClientMessage::SetEnv(_) | ClientMessage::SetFiles(_)) => bail!("unexpected message"),
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
    fn writes_only_safe_runner_env() {
        let dir = std::env::temp_dir().join(format!("chloe-env-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("env");
        let env = vec![
            ("GH_TOKEN".to_string(), "ghu_chloeABC123".to_string()),
            (
                "ANTHROPIC_API_KEY".to_string(),
                "sk-ant-api03-chloe_x-y".to_string(),
            ),
        ];
        write_runner_env(&path, &env).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(parse_env_file(&content), env);
        // シェルとして読まれるので、特別な意味を持つ文字は受け付けず、前の内容を残す。
        for bad in [
            ("A", "$(id)"),
            ("A", "x y"),
            ("A B", "x"),
            ("1A", "x"),
            ("A", "x\nB=y"),
        ] {
            let bad = vec![(bad.0.to_string(), bad.1.to_string())];
            assert!(write_runner_env(&path, &bad).is_err());
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reports_only_fresh_markers() {
        let dir = std::env::temp_dir().join(format!("chloe-activity-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("claude-s1"), format!("{}\n", std::process::id())).unwrap();
        std::fs::write(dir.join("codex-s2"), "").unwrap();
        // 終わったプロセスの印は無視して消す。
        std::fs::write(dir.join("codex-dead"), "999999999\n").unwrap();
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
        assert!(!dir.join("codex-dead").exists());
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
    fn places_and_removes_runner_files() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("state/files");
        let me = || {
            let meta = std::fs::metadata(dir.path()).unwrap();
            Some((meta.uid(), meta.gid()))
        };
        let lookup = |name: &str| (name == "user").then(me).flatten();
        let file = |name: &str, content: &str| GuestFile {
            path: dir.path().join(name).to_string_lossy().into_owned(),
            content: content.into(),
            mode: 0o600,
            owner: "user".into(),
        };
        let apt = file(
            "etc/apt/auth.conf.d/chloe.conf",
            "machine a login u password p\n",
        );
        let netrc = file("home/.netrc", "machine b\n");
        apply_files(&[apt.clone(), netrc.clone()], &manifest, lookup).unwrap();
        assert_eq!(std::fs::read_to_string(&apt.path).unwrap(), apt.content);
        let mode = std::fs::metadata(&apt.path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);

        // 渡さなくなったファイルは消し、渡したものは書き換える。
        let apt2 = GuestFile {
            content: "changed\n".into(),
            ..apt.clone()
        };
        apply_files(std::slice::from_ref(&apt2), &manifest, lookup).unwrap();
        assert_eq!(std::fs::read_to_string(&apt.path).unwrap(), "changed\n");
        assert!(!std::path::Path::new(&netrc.path).exists());

        // おかしいものが一つでもあれば何も変えない。
        for bad in [
            GuestFile {
                path: "relative/x".into(),
                ..apt.clone()
            },
            GuestFile {
                path: format!("{}/../x", dir.path().display()),
                ..apt.clone()
            },
            GuestFile {
                path: "/run/chloe/env".into(),
                ..apt.clone()
            },
            GuestFile {
                mode: 0o4755,
                ..apt.clone()
            },
            GuestFile {
                owner: "nobody-here".into(),
                ..apt.clone()
            },
        ] {
            assert!(
                apply_files(std::slice::from_ref(&bad), &manifest, lookup).is_err(),
                "{bad:?}"
            );
        }
        assert_eq!(std::fs::read_to_string(&apt.path).unwrap(), "changed\n");

        apply_files(&[], &manifest, lookup).unwrap();
        assert!(!std::path::Path::new(&apt.path).exists());
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
