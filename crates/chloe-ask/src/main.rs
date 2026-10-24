//! VM の中のエージェントが、人に依頼 (通信の許可と質問) を出して答えを待つ。
//!
//! 依頼は `ask.chloe.internal` に送り、ランナーのプロキシがコントロールプレーンへ届ける。
//! 人は管理の TUI、`chloe inbox`、管理 API で答える。
//!
//! ```text
//! chloe-ask network HOST [--method M]... [--path P]... [--reason R] [--timeout 1h] [--no-wait]
//! chloe-ask question TEXT [--option O]... [--reason R] [--timeout 1h] [--no-wait]
//! chloe-ask wait ID
//! chloe-ask cancel ID
//! chloe-ask port publish PORT [--https]   (VM の中の HTTP(S) のサーバーをブラウザで開けるようにする)
//! chloe-ask port unpublish PORT
//! chloe-ask port list
//! chloe-ask mcp [--wait 55m]      (標準入出力の MCP のサーバー)
//! chloe-ask register              (Claude Code と Codex の利用者の設定に MCP のサーバーを足す)
//! ```
//!
//! 待つコマンドは、承認されるか答えが来たら 0、断られたら 1、期限切れか取り下げか失敗なら 2 で
//! 終わる。質問の答えは標準出力に、ほかの知らせは標準エラー出力に出す。

mod client;
mod mcp;
mod register;

use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::client::{Ask, Client};

/// MCP のツールの呼び出し一回で待つ時間の既定。
pub const DEFAULT_WAIT: Duration = Duration::from_secs(55 * 60);

#[derive(Parser)]
#[command(about = "Ask a person for network access or an answer, through chloe")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Ask a person to allow HTTP(S) to a host that chloe blocks, and wait for the answer.
    Network {
        /// Host name, like api.example.com or *.example.com.
        host: String,
        /// HTTP method to allow (repeatable). All methods without it.
        #[arg(long = "method")]
        methods: Vec<String>,
        /// Path to allow, like /v1/* (repeatable). All paths without it.
        #[arg(long = "path")]
        paths: Vec<String>,
        #[command(flatten)]
        common: Common,
    },
    /// Ask a person a question, and print the answer.
    Question {
        text: String,
        /// A choice (repeatable). The person may still write another answer.
        #[arg(long = "option")]
        options: Vec<String>,
        #[command(flatten)]
        common: Common,
    },
    /// Wait for the answer to an ask.
    Wait { id: i64 },
    /// Withdraw an ask.
    Cancel { id: i64 },
    /// Let workspace members open an HTTP(S) server in this VM from their browser.
    Port {
        #[command(subcommand)]
        command: PortCommand,
    },
    /// Run the MCP server on stdin and stdout.
    Mcp {
        /// How long one tool call waits before returning pending, like 55m.
        #[arg(long, value_parser = parse_duration, default_value = "55m")]
        wait: Duration,
    },
    /// Add the MCP server to the user's Claude Code and Codex settings (once).
    Register,
}

#[derive(Subcommand)]
enum PortCommand {
    /// Publish a port, and print the URL to open it.
    Publish {
        port: u16,
        /// The server speaks HTTPS (its certificate is not checked).
        #[arg(long)]
        https: bool,
    },
    /// Stop publishing a port.
    Unpublish { port: u16 },
    /// List the published ports.
    List,
}

#[derive(clap::Args)]
struct Common {
    /// Why you need it, for the person deciding.
    #[arg(long)]
    reason: Option<String>,
    /// How long the ask stays open, like 30m or 2h (at most 24h).
    #[arg(long, value_parser = parse_duration)]
    timeout: Option<Duration>,
    /// Print the ask's id and exit without waiting.
    #[arg(long)]
    no_wait: bool,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("chloe-ask: {error:#}");
            ExitCode::from(2)
        }
    }
}

async fn run(cli: Cli) -> Result<ExitCode> {
    let client = Client::from_env();
    let (new, common) = match cli.command {
        Command::Network {
            host,
            methods,
            paths,
            common,
        } => (
            serde_json::json!({ "kind": "network", "host": host, "methods": methods, "paths": paths }),
            common,
        ),
        Command::Question {
            text,
            options,
            common,
        } => (
            serde_json::json!({ "kind": "question", "question": text, "options": options }),
            common,
        ),
        Command::Wait { id } => return wait(&client, id).await,
        Command::Cancel { id } => {
            let ask = client.cancel(id).await?;
            eprintln!("ask {} is {}", ask.id, ask.status);
            return Ok(ExitCode::SUCCESS);
        }
        Command::Port { command } => return port(&client, command).await,
        Command::Mcp { wait } => {
            mcp::serve(client, wait).await?;
            return Ok(ExitCode::SUCCESS);
        }
        Command::Register => {
            let home = std::env::var_os("HOME").context("HOME is not set")?;
            let exe = std::env::current_exe()?;
            for tool in register::register(home.as_ref(), &exe.to_string_lossy())? {
                eprintln!("chloe-ask: added the chloe MCP server to {tool}");
            }
            return Ok(ExitCode::SUCCESS);
        }
    };
    let mut new = new;
    new["reason"] = common.reason.into();
    if let Some(timeout) = common.timeout {
        new["timeout_secs"] = timeout.as_secs().into();
    }
    let ask = client.create(&new).await?;
    if common.no_wait {
        println!("{}", ask.id);
        return Ok(ExitCode::SUCCESS);
    }
    eprintln!(
        "chloe-ask: waiting for a person to answer ask {} (chloe inbox, or the Inbox tab of `ssh -t <host> chloe`)",
        ask.id
    );
    wait(&client, ask.id).await
}

async fn port(client: &Client, command: PortCommand) -> Result<ExitCode> {
    match command {
        PortCommand::Publish { port, https } => {
            let published = client.publish(port, https).await?;
            match &published.url {
                // URL だけを標準出力に出し、スクリプトで使えるようにする。
                Some(url) => println!("{url}"),
                None => eprintln!(
                    "chloe-ask: published port {port}, but chloe has no proxy domain to open it (api.proxy_url)"
                ),
            }
        }
        PortCommand::Unpublish { port } => {
            client.unpublish(port).await?;
            eprintln!("chloe-ask: unpublished port {port}");
        }
        PortCommand::List => {
            for p in client.ports().await? {
                println!(
                    "{}\t{}\t{}",
                    p.port,
                    p.scheme,
                    p.url.as_deref().unwrap_or("-")
                );
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// 答えが来るまで待つ。Ctrl-C で依頼を取り下げて終わる。
async fn wait(client: &Client, id: i64) -> Result<ExitCode> {
    let ask = tokio::select! {
        ask = client.wait(id, None) => ask?,
        _ = tokio::signal::ctrl_c() => {
            let _ = client.cancel(id).await;
            eprintln!("chloe-ask: withdrew ask {id}");
            return Ok(ExitCode::from(130));
        }
    };
    let (text, code) = describe(&ask);
    match (ask.status.as_str(), &ask.answer) {
        // 答えだけを標準出力に出し、スクリプトで使えるようにする。
        ("answered", Some(answer)) => println!("{answer}"),
        _ => eprintln!("chloe-ask: {text}"),
    }
    Ok(ExitCode::from(code))
}

/// エージェントに伝える文と、終了コード。
pub fn describe(ask: &Ask) -> (String, u8) {
    let id = ask.id;
    let by = ask
        .answered_by
        .as_deref()
        .map(|by| format!(" by {by}"))
        .unwrap_or_default();
    match ask.status.as_str() {
        "allowed" => (
            format!(
                "allowed{by}: {} is reachable now; retry the request (ask {id})",
                ask.host.as_deref().unwrap_or("the host")
            ),
            0,
        ),
        "answered" => (
            format!(
                "answered{by} (ask {id}): {}",
                ask.answer.as_deref().unwrap_or("")
            ),
            0,
        ),
        "denied" => {
            let message = ask
                .answer
                .as_deref()
                .map(|m| format!(": {m}"))
                .unwrap_or_default();
            (format!("denied{by} (ask {id}){message}"), 1)
        }
        "pending" => (
            format!(
                "pending: no answer yet (ask {id}). Call wait_for_answer with id {id} to keep waiting."
            ),
            2,
        ),
        "expired" => (format!("nobody answered before ask {id} expired"), 2),
        other => (format!("ask {id} is {other}"), 2),
    }
}

/// `30s`、`10m`、`2h`、`1d`、または秒数。
fn parse_duration(text: &str) -> Result<Duration, String> {
    let text = text.trim();
    let (number, unit) = match text.find(|c: char| !c.is_ascii_digit()) {
        Some(i) => text.split_at(i),
        None => (text, "s"),
    };
    let n: u64 = number
        .parse()
        .map_err(|_| format!("invalid duration {text:?}"))?;
    let secs = match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86400,
        _ => return Err(format!("invalid duration {text:?} (use s, m, h or d)")),
    };
    Ok(Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration("90").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("55m").unwrap(), Duration::from_secs(3300));
        assert_eq!(parse_duration("1d").unwrap(), Duration::from_secs(86400));
        assert!(parse_duration("1w").is_err());
        assert!(parse_duration("m").is_err());
    }

    #[test]
    fn describes_answers() {
        let ask = |status: &str, answer: Option<&str>| Ask {
            id: 3,
            kind: "question".into(),
            status: status.into(),
            host: None,
            answer: answer.map(Into::into),
            answered_by: Some("alice".into()),
        };
        assert_eq!(
            describe(&ask("answered", Some("dev"))),
            ("answered by alice (ask 3): dev".into(), 0)
        );
        assert_eq!(
            describe(&ask("denied", Some("not now"))),
            ("denied by alice (ask 3): not now".into(), 1)
        );
        assert_eq!(describe(&ask("expired", None)).1, 2);
        assert!(
            describe(&ask("pending", None))
                .0
                .contains("wait_for_answer")
        );
    }
}
