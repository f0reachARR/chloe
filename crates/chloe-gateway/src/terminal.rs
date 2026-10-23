//! Web の端末 (`GET /api/v1/workspace-terminal/{name}`)。WebSocket に切り替え、SSH で端末を
//! 割り当ててシェルを開いたときと同じく、guest-agent にログインシェルを起動させて入出力を中継する。
//!
//! バイナリのフレームは、端末の入力と出力そのもの。テキストのフレームは JSON で、画面からは
//! [`ClientFrame`]、ゲートウェイからは [`ServerFrame`] を送る。
//!
//! 開いているあいだは、ワークスペースを止めない。1 分ごとにトークンかセッションを確かめ直し、
//! 使えなくなっていれば閉じる (SSE と違い、つなぎ直させるとシェルが消えるので)。

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use axum::Extension;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use chloe_agent_proto::{
    AgentMessage, ClientMessage, Connection as AgentConnection, ExecRequest, PROTOCOL_VERSION,
    TtyRequest,
};
use chloe_controller::workspace::Workspace;
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use utoipa::{IntoParams, ToSchema};

use crate::api::{Caller, ErrorBody, Via, error};
use crate::session::{AGENT_CONNECT_WAIT, Shared, window_size};

/// シェルに渡す端末の種類 (xterm.js)。
const TERM: &str = "xterm-256color";
/// 開いているあいだ、トークンかセッションを確かめ直す間隔。
const RECHECK_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct SizeQuery {
    /// 端末の最初の幅 (桁)。
    #[serde(default = "default_cols")]
    cols: u16,
    /// 端末の最初の高さ (行)。
    #[serde(default = "default_rows")]
    rows: u16,
}

fn default_cols() -> u16 {
    80
}

fn default_rows() -> u16 {
    24
}

/// 画面からゲートウェイへのテキストのフレーム。
#[derive(Debug, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "lowercase")]
pub(crate) enum ClientFrame {
    /// 端末の大きさが変わった。
    Resize { cols: u16, rows: u16 },
}

/// ゲートウェイから画面へのテキストのフレーム。
#[derive(Debug, Serialize, ToSchema)]
#[serde(tag = "type", rename_all = "lowercase")]
pub(crate) enum ServerFrame {
    /// ワークスペースを用意している (止まっていれば起動する)。
    Starting,
    /// シェルが起動した。
    Started,
    /// シェルが終わった。このあと閉じる。
    Exit { code: i32 },
    /// 開けなかったか、続けられなくなった。このあと閉じる。
    Error { message: String },
}

type Sender = SplitSink<WebSocket, Message>;
type Receiver = SplitStream<WebSocket>;

#[utoipa::path(get, path = "/api/v1/workspace-terminal/{name}", tag = "workspaces", description = "WebSocket に切り替え、ワークスペースのシェルを開く (SSH の `ssh -t` と同じ)。バイナリのフレームは端末の入力と出力、テキストのフレームは JSON で、送るものは `ClientFrame`、受けるものは `ServerFrame`。cookie で開くときは `Origin` が画面と同じでなければ断る。", params(("name" = String, Path, description = "ワークスペース (SSH と同じ書き方。`/` は `%2F` にする)"), SizeQuery), responses((status = 101, description = "WebSocket に切り替えた"), (status = "4XX", description = "誤り", body = ErrorBody), (status = "5XX", description = "ゲートウェイの誤り", body = ErrorBody)))]
pub(crate) async fn open(
    State(shared): State<Arc<Shared>>,
    Extension(caller): Extension<Caller>,
    Path(name): Path<String>,
    Query(size): Query<SizeQuery>,
    upgrade: WebSocketUpgrade,
) -> Response {
    // 入れないワークスペースなら、切り替える前に断る。
    let workspace = match shared.resolve_workspace(&caller.login, Some(&name)).await {
        Ok(workspace) => workspace,
        Err(e) => return error(StatusCode::NOT_FOUND, format!("{e:#}")),
    };
    upgrade.on_upgrade(move |socket| run(shared, caller, workspace, size, socket))
}

async fn run(
    shared: Arc<Shared>,
    caller: Caller,
    workspace: Workspace,
    size: SizeQuery,
    socket: WebSocket,
) {
    let name = workspace.display_name();
    audit_started(&caller, &name);
    let (mut tx, mut rx) = socket.split();
    let result = relay(&shared, &caller, &workspace, size, &mut tx, &mut rx).await;
    let (frame, code) = match result {
        Ok(Some(code)) => (Some(ServerFrame::Exit { code }), Some(code)),
        // 画面が閉じた。
        Ok(None) => (None, None),
        Err(e) => {
            warn!("terminal failed: {e:#}");
            let message = format!("{e:#}");
            (Some(ServerFrame::Error { message }), Some(1))
        }
    };
    if let Some(frame) = frame {
        let _ = send_frame(&mut tx, &frame).await;
        let _ = tx.send(Message::Close(None)).await;
    }
    audit_ended(&caller, &name, code);
}

/// ワークスペースを用意し、シェルを起動して、終わるまで中継する。シェルの終了コードを返し、
/// 画面が閉じたなら `None` を返す。
async fn relay(
    shared: &Shared,
    caller: &Caller,
    workspace: &Workspace,
    size: SizeQuery,
    tx: &mut Sender,
    rx: &mut Receiver,
) -> Result<Option<i32>> {
    // VM の起動を待つあいだ、画面が黙らないように知らせる。
    if send_frame(tx, &ServerFrame::Starting).await.is_err() {
        return Ok(None);
    }
    // セッションを持っているあいだは、ランナーが止められない。
    let session = shared.controller.open(workspace).await?;
    let stream = session
        .connect_agent(AGENT_CONNECT_WAIT)
        .await
        .context("the workspace VM is not reachable")?;
    let mut conn = AgentConnection::new(stream);
    conn.send(&ClientMessage::Exec(ExecRequest {
        version: PROTOCOL_VERSION,
        command: vec![],
        user: None,
        env: vec![],
        cwd: None,
        tty: Some(TtyRequest {
            term: TERM.into(),
            size: window_size(size.cols.into(), size.rows.into()),
        }),
    }))
    .await?;
    let (mut agent_tx, mut agent_rx) = conn.split();
    let start = tokio::time::Instant::now() + RECHECK_INTERVAL;
    let mut recheck = tokio::time::interval_at(start, RECHECK_INTERVAL);
    loop {
        tokio::select! {
            message = rx.next() => match message {
                Some(Ok(Message::Binary(data))) => {
                    agent_tx.send(&ClientMessage::Stdin(data.to_vec())).await?;
                }
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<ClientFrame>(&text) {
                    Ok(ClientFrame::Resize { cols, rows }) => {
                        let size = window_size(cols.into(), rows.into());
                        agent_tx.send(&ClientMessage::Resize(size)).await?;
                    }
                    Err(e) => bail!("invalid frame from the browser: {e}"),
                },
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                // 送り口を捨てると guest-agent はシェルを止める (SSH の接続が切れたときと同じ)。
                Some(Ok(Message::Close(_)) | Err(_)) | None => return Ok(None),
            },
            message = agent_rx.recv::<AgentMessage>() => match message? {
                Some(AgentMessage::Started { .. }) => {
                    if send_frame(tx, &ServerFrame::Started).await.is_err() {
                        return Ok(None);
                    }
                }
                Some(AgentMessage::Activity { .. } | AgentMessage::Connected | AgentMessage::EnvUpdated | AgentMessage::FilesUpdated) => {}
                // 端末つきなので、出力はふつう Stdout にまとまって来る。
                Some(AgentMessage::Stdout(data) | AgentMessage::Stderr(data)) => {
                    if tx.send(Message::Binary(data.into())).await.is_err() {
                        return Ok(None);
                    }
                }
                Some(AgentMessage::Exited(status)) => return Ok(Some(status.shell_code())),
                Some(AgentMessage::Error(message)) => bail!("guest-agent: {message}"),
                None => bail!("guest-agent closed the connection"),
            },
            _ = recheck.tick() => {
                if let Err(reason) = caller.recheck(shared).await {
                    bail!("{reason}");
                }
            }
        }
    }
}

async fn send_frame(tx: &mut Sender, frame: &ServerFrame) -> Result<()> {
    let text = serde_json::to_string(frame)?;
    tx.send(Message::Text(text.into())).await?;
    Ok(())
}

fn audit_started(caller: &Caller, workspace: &str) {
    match caller.via {
        Via::Session(session) => {
            info!(target: "audit", via = "web", login = %caller.login, session, workspace, kind = "shell", tty = true, "session started");
        }
        Via::Token(token) => {
            info!(target: "audit", via = "api", login = %caller.login, token, workspace, kind = "shell", tty = true, "session started");
        }
    }
}

fn audit_ended(caller: &Caller, workspace: &str, code: Option<i32>) {
    match caller.via {
        Via::Session(session) => {
            info!(target: "audit", via = "web", login = %caller.login, session, workspace, code, "session ended");
        }
        Via::Token(token) => {
            info!(target: "audit", via = "api", login = %caller.login, token, workspace, code, "session ended");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_writes_frames() {
        assert_eq!(
            serde_json::from_str::<ClientFrame>(r#"{"type":"resize","cols":120,"rows":40}"#)
                .unwrap(),
            ClientFrame::Resize {
                cols: 120,
                rows: 40
            }
        );
        assert!(serde_json::from_str::<ClientFrame>(r#"{"type":"exec"}"#).is_err());
        assert_eq!(
            serde_json::to_string(&ServerFrame::Exit { code: 130 }).unwrap(),
            r#"{"type":"exit","code":130}"#
        );
        assert_eq!(
            serde_json::to_string(&ServerFrame::Starting).unwrap(),
            r#"{"type":"starting"}"#
        );
    }
}
