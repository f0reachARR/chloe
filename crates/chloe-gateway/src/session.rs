//! SSH の接続ごとの処理。
//!
//! セッションチャネル一つにつき guest-agent への接続を一つ開き、シェルかコマンドを実行して
//! 入出力を中継する。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chloe_agent_proto::{
    AGENT_PORT, AgentMessage, ClientMessage, Connection as AgentConnection, ExecRequest,
    PROTOCOL_VERSION, TtyRequest, WindowSize, firecracker,
};
use russh::keys::PublicKey;
use russh::server::{Auth, ChannelOpenHandle, Handle, Msg, Session};
use russh::{Channel, ChannelId, Pty, Sig};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::auth::{GitHubKeys, parse_user};
use crate::config::{Config, Workspace};

/// guest-agent への接続を待つ時間。VM が起動したばかりのときのために少し待つ。
const AGENT_CONNECT_WAIT: Duration = Duration::from_secs(5);
/// ホストから guest-agent へ送るメッセージを溜めておく数。
const INPUT_BUFFER: usize = 64;

pub struct Shared {
    pub config: Config,
    pub keys: GitHubKeys,
}

/// 認証を通った利用者と、入るワークスペース。
#[derive(Clone)]
struct Authenticated {
    login: String,
    workspace_name: String,
    workspace: Workspace,
}

#[derive(Default)]
struct ChannelState {
    tty: Option<TtyRequest>,
    env: Vec<(String, String)>,
    input: Option<mpsc::Sender<ClientMessage>>,
}

pub struct Connection {
    shared: Arc<Shared>,
    peer: Option<std::net::SocketAddr>,
    user: Option<Authenticated>,
    channels: HashMap<ChannelId, ChannelState>,
}

impl Connection {
    pub fn new(shared: Arc<Shared>, peer: Option<std::net::SocketAddr>) -> Self {
        Self {
            shared,
            peer,
            user: None,
            channels: HashMap::new(),
        }
    }

    /// 利用者がワークスペースに入れるなら、その情報を返す。
    async fn authorize(&self, user: &str, key: &PublicKey) -> Result<Option<Authenticated>> {
        let user = parse_user(user)?;
        let workspace_name = user
            .workspace
            .unwrap_or_else(|| self.shared.config.default_workspace.clone());
        let Some(workspace) = self.shared.config.workspaces.get(&workspace_name) else {
            return Ok(None);
        };
        // GitHub に問い合わせる前に、メンバーでない利用者を落とす。
        if !workspace.is_member(&user.login) {
            return Ok(None);
        }
        if !self.shared.keys.is_registered(&user.login, key).await? {
            return Ok(None);
        }
        Ok(Some(Authenticated {
            login: user.login,
            workspace_name,
            workspace: workspace.clone(),
        }))
    }

    async fn check_key(&self, user: &str, key: &PublicKey) -> Option<Authenticated> {
        match self.authorize(user, key).await {
            Ok(result) => result,
            Err(error) => {
                warn!(peer = ?self.peer, user, "authentication failed: {error:#}");
                None
            }
        }
    }

    fn start(
        &mut self,
        channel: ChannelId,
        command: Vec<String>,
        session: &mut Session,
    ) -> Result<(), russh::Error> {
        let (Some(user), Some(state)) = (&self.user, self.channels.get_mut(&channel)) else {
            return session.channel_failure(channel);
        };
        if state.input.is_some() {
            // 一つのチャネルで二度目の shell や exec は受けない。
            return session.channel_failure(channel);
        }
        let request = ExecRequest {
            version: PROTOCOL_VERSION,
            command,
            user: None,
            env: state.env.clone(),
            cwd: None,
            tty: state.tty.clone(),
        };
        info!(
            login = %user.login,
            workspace = %user.workspace_name,
            command = ?request.command,
            tty = request.tty.is_some(),
            "session started"
        );
        let (tx, rx) = mpsc::channel(INPUT_BUFFER);
        state.input = Some(tx);
        tokio::spawn(relay(
            session.handle(),
            channel,
            user.workspace.vsock.clone(),
            request,
            rx,
        ));
        session.channel_success(channel)
    }

    async fn send_input(&mut self, channel: ChannelId, message: ClientMessage) {
        if let Some(input) = self.channels.get(&channel).and_then(|s| s.input.as_ref()) {
            // 中継が終わっていれば送れないが、それはチャネルを閉じる側で扱う。
            let _ = input.send(message).await;
        }
    }
}

impl russh::server::Handler for Connection {
    type Error = russh::Error;

    async fn auth_publickey_offered(
        &mut self,
        user: &str,
        public_key: &PublicKey,
    ) -> Result<Auth, Self::Error> {
        // 鍵の署名を求める前に、登録された鍵かを確かめる。違えば次の鍵を試させる。
        Ok(match self.check_key(user, public_key).await {
            Some(_) => Auth::Accept,
            None => Auth::reject(),
        })
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        public_key: &PublicKey,
    ) -> Result<Auth, Self::Error> {
        match self.check_key(user, public_key).await {
            Some(authenticated) => {
                info!(peer = ?self.peer, login = %authenticated.login, workspace = %authenticated.workspace_name, "authenticated");
                self.user = Some(authenticated);
                Ok(Auth::Accept)
            }
            None => {
                info!(peer = ?self.peer, user, "rejected");
                Ok(Auth::reject())
            }
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.insert(channel.id(), ChannelState::default());
        reply.accept().await;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        term: &str,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let Some(state) = self.channels.get_mut(&channel) else {
            return session.channel_failure(channel);
        };
        state.tty = Some(TtyRequest {
            term: term.to_string(),
            size: window_size(col_width, row_height),
        });
        session.channel_success(channel)
    }

    async fn env_request(
        &mut self,
        channel: ChannelId,
        variable_name: &str,
        variable_value: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        // OpenSSH の既定と同じく、ロケールの変数だけを受け取る。
        let accepted = variable_name == "LANG" || variable_name.starts_with("LC_");
        match self.channels.get_mut(&channel) {
            Some(state) if accepted => {
                state
                    .env
                    .push((variable_name.to_string(), variable_value.to_string()));
                session.channel_success(channel)
            }
            _ => session.channel_failure(channel),
        }
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.start(channel, vec![], session)
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        // OpenSSH と同じく、コマンドは利用者のシェルに渡す。
        let command = String::from_utf8_lossy(data).into_owned();
        self.start(
            channel,
            vec!["/bin/bash".into(), "-c".into(), command],
            session,
        )
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.send_input(channel, ClientMessage::Stdin(data.to_vec()))
            .await;
        Ok(())
    }

    async fn channel_eof(
        &mut self,
        channel: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.send_input(channel, ClientMessage::CloseStdin).await;
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        channel: ChannelId,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let size = window_size(col_width, row_height);
        self.send_input(channel, ClientMessage::Resize(size)).await;
        Ok(())
    }

    async fn signal(
        &mut self,
        channel: ChannelId,
        signal: Sig,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if let Some(number) = signal_number(&signal) {
            self.send_input(channel, ClientMessage::Signal(number))
                .await;
        }
        Ok(())
    }

    async fn channel_close(
        &mut self,
        channel: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        // 送り口を捨てると中継が終わり、guest-agent はプロセスを止める。
        self.channels.remove(&channel);
        Ok(())
    }
}

/// guest-agent とのあいだで入出力を中継し、終わったらチャネルを閉じる。
async fn relay(
    handle: Handle,
    channel: ChannelId,
    vsock: PathBuf,
    request: ExecRequest,
    input: mpsc::Receiver<ClientMessage>,
) {
    let result = relay_inner(&handle, channel, &vsock, request, input).await;
    let code = match result {
        Ok(Some(code)) => code,
        // 利用者がチャネルを閉じた。
        Ok(None) => return,
        Err(error) => {
            warn!("session failed: {error:#}");
            let message = format!("chloe: {error:#}\r\n");
            let _ = handle.extended_data(channel, 1, message.into_bytes()).await;
            1
        }
    };
    let _ = handle.exit_status_request(channel, code).await;
    let _ = handle.eof(channel).await;
    let _ = handle.close(channel).await;
}

async fn relay_inner(
    handle: &Handle,
    channel: ChannelId,
    vsock: &std::path::Path,
    request: ExecRequest,
    mut input: mpsc::Receiver<ClientMessage>,
) -> Result<Option<u32>> {
    let stream = firecracker::connect(vsock, AGENT_PORT, AGENT_CONNECT_WAIT)
        .await
        .context("the workspace VM is not reachable")?;
    let mut conn = AgentConnection::new(stream);
    conn.send(&ClientMessage::Exec(request)).await?;
    let (mut tx, mut rx) = conn.split();
    loop {
        tokio::select! {
            message = input.recv() => match message {
                Some(message) => tx.send(&message).await?,
                None => return Ok(None),
            },
            message = rx.recv::<AgentMessage>() => match message? {
                Some(AgentMessage::Started { .. }) => {}
                Some(AgentMessage::Stdout(data)) => {
                    if handle.data(channel, data).await.is_err() {
                        return Ok(None);
                    }
                }
                Some(AgentMessage::Stderr(data)) => {
                    if handle.extended_data(channel, 1, data).await.is_err() {
                        return Ok(None);
                    }
                }
                Some(AgentMessage::Exited(status)) => {
                    return Ok(Some(status.shell_code() as u32));
                }
                Some(AgentMessage::Error(message)) => bail!("guest-agent: {message}"),
                None => bail!("guest-agent closed the connection"),
            },
        }
    }
}

fn window_size(cols: u32, rows: u32) -> WindowSize {
    WindowSize {
        rows: u16::try_from(rows).unwrap_or(u16::MAX).max(1),
        cols: u16::try_from(cols).unwrap_or(u16::MAX).max(1),
    }
}

/// SSH のシグナル名をゲスト (Linux x86_64) のシグナル番号に直す。
fn signal_number(signal: &Sig) -> Option<i32> {
    Some(match signal {
        Sig::HUP => 1,
        Sig::INT => 2,
        Sig::QUIT => 3,
        Sig::ILL => 4,
        Sig::ABRT => 6,
        Sig::FPE => 8,
        Sig::KILL => 9,
        Sig::USR1 => 10,
        Sig::SEGV => 11,
        Sig::PIPE => 13,
        Sig::ALRM => 14,
        Sig::TERM => 15,
        Sig::Custom(_) => return None,
    })
}
