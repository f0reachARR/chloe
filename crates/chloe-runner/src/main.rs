//! ランナーの操作。
//!
//! 今は、起動している VM の guest-agent に Firecracker の vsock 越しにつなぎ、
//! シェルを開いたりコマンドを実行したりする。

mod terminal;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Result, bail};
use chloe_agent_proto::{
    AgentMessage, ClientMessage, Connection, ExecRequest, PROTOCOL_VERSION, TtyRequest,
};
use clap::{Args, Parser, Subcommand};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::signal::unix::{SignalKind, signal};

#[derive(Parser)]
#[command(about = "chloe のランナーを操作する")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// VM の中でログインシェルを開く。
    Shell {
        #[command(flatten)]
        target: Target,
    },
    /// VM の中でコマンドを実行し、その終了コードで終わる。
    Exec {
        #[command(flatten)]
        target: Target,
        /// 端末を割り当てる。
        #[arg(short = 't', long)]
        tty: bool,
        /// 作業ディレクトリ。
        #[arg(long)]
        cwd: Option<String>,
        /// 環境変数 (KEY=VALUE)。
        #[arg(short = 'e', long = "env", value_parser = parse_env)]
        env: Vec<(String, String)>,
        #[arg(required = true, last = true)]
        command: Vec<String>,
    },
}

#[derive(Args)]
struct Target {
    /// Firecracker の vsock の Unix ソケット。
    #[arg(long, env = "CHLOE_VSOCK")]
    vsock: PathBuf,
    /// 実行する利用者。省略すると VM の既定の利用者になる。
    #[arg(short, long)]
    user: Option<String>,
    /// guest-agent が応答するまで待つ秒数。
    #[arg(long, default_value_t = 0)]
    wait: u64,
}

fn parse_env(s: &str) -> Result<(String, String), String> {
    s.split_once('=')
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .ok_or_else(|| format!("expected KEY=VALUE: {s}"))
}

fn main() {
    let cli = Cli::parse();
    let runtime = tokio::runtime::Runtime::new().expect("failed to start the runtime");
    let code = match runtime.block_on(run(cli)) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("chloe-runner: {error:#}");
            1
        }
    };
    // 標準入力を読んでいるスレッドの終わりを待たずに抜ける。
    std::process::exit(code);
}

async fn run(cli: Cli) -> Result<i32> {
    match cli.command {
        Command::Shell { target } => exec(target, vec![], None, vec![], true).await,
        Command::Exec {
            target,
            tty,
            cwd,
            env,
            command,
        } => exec(target, command, cwd, env, tty).await,
    }
}

async fn exec(
    target: Target,
    command: Vec<String>,
    cwd: Option<String>,
    env: Vec<(String, String)>,
    tty: bool,
) -> Result<i32> {
    let stream = chloe_agent_proto::firecracker::connect(
        &target.vsock,
        chloe_agent_proto::AGENT_PORT,
        Duration::from_secs(target.wait),
    )
    .await?;

    let tty = if tty {
        let term = std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".into());
        Some(TtyRequest {
            term,
            size: terminal::size(),
        })
    } else {
        None
    };
    let raw_mode = match &tty {
        Some(_) => terminal::RawMode::enable()?,
        None => None,
    };

    let mut conn = Connection::new(stream);
    conn.send(&ClientMessage::Exec(ExecRequest {
        version: PROTOCOL_VERSION,
        command,
        user: target.user,
        env,
        cwd,
        tty: tty.clone(),
    }))
    .await?;
    let (mut tx, mut rx) = conn.split();

    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut stderr = tokio::io::stderr();
    let mut buf = vec![0; 32 * 1024];
    let mut stdin_open = true;
    let mut winch = signal(SignalKind::window_change())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;

    let code = loop {
        tokio::select! {
            read = stdin.read(&mut buf), if stdin_open => match read {
                Ok(0) | Err(_) => {
                    stdin_open = false;
                    tx.send(&ClientMessage::CloseStdin).await?;
                }
                Ok(n) => tx.send(&ClientMessage::Stdin(buf[..n].to_vec())).await?,
            },
            message = rx.recv::<AgentMessage>() => match message? {
                Some(AgentMessage::Started { .. }) | Some(AgentMessage::Activity { .. }) => {}
                Some(AgentMessage::Stdout(data)) => {
                    stdout.write_all(&data).await?;
                    stdout.flush().await?;
                }
                Some(AgentMessage::Stderr(data)) => {
                    stderr.write_all(&data).await?;
                    stderr.flush().await?;
                }
                Some(AgentMessage::Exited(status)) => break status.shell_code(),
                Some(AgentMessage::Error(message)) => {
                    drop(raw_mode);
                    bail!("guest-agent: {message}");
                }
                None => {
                    drop(raw_mode);
                    bail!("guest-agent closed the connection");
                }
            },
            _ = winch.recv(), if tty.is_some() => {
                tx.send(&ClientMessage::Resize(terminal::size())).await?;
            }
            // 端末つきのときは Ctrl-C が端末の入力として届くので、シグナルは来ない。
            _ = interrupt.recv() => tx.send(&ClientMessage::Signal(libc_signal::SIGINT)).await?,
            _ = terminate.recv() => tx.send(&ClientMessage::Signal(libc_signal::SIGTERM)).await?,
        }
    };
    drop(raw_mode);
    Ok(code)
}

/// ゲスト (Linux x86_64) のシグナル番号。
mod libc_signal {
    pub const SIGINT: i32 = 2;
    pub const SIGTERM: i32 = 15;
}
