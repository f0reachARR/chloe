//! SSH の接続ごとの処理。
//!
//! セッションチャネル一つにつき guest-agent への接続を一つ開き、シェルかコマンドを実行して
//! 入出力を中継する。`chloe` で始まるコマンドはゲートウェイが処理する ([`crate::commands`])。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chloe_agent_proto::{
    AGENT_PORT, AgentMessage, ClientMessage, Connection as AgentConnection, ExecRequest,
    PROTOCOL_VERSION, TtyRequest, WindowSize, firecracker,
};
use chloe_controller::docker::DockerBackend;
use chloe_controller::workspace::{RootMode, Workspace, WorkspaceRef};
use chloe_controller::{Controller, Error as ControllerError};
use chloe_secrets::Owner;
use russh::keys::PublicKey;
use russh::server::{Auth, ChannelOpenHandle, Handle, Msg, Session};
use russh::{Channel, ChannelId, Pty, Sig};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::auth::{GitHubKeys, parse_user};
use crate::commands;
use crate::config::Config;

/// ワークスペースを指定しなかったときに入る、個人のワークスペースの名前。
const DEFAULT_WORKSPACE: &str = "default";
/// ランナーを起動してから guest-agent が応答するまで待つ時間。
const AGENT_CONNECT_WAIT: Duration = Duration::from_secs(90);
/// ホストから guest-agent へ送るメッセージを溜めておく数。
const INPUT_BUFFER: usize = 64;
/// 管理コマンドの標準入力の上限。
const MAX_COMMAND_STDIN: usize = 64 * 1024;

pub struct Shared {
    pub config: Config,
    pub keys: GitHubKeys,
    pub controller: Controller<DockerBackend>,
}

impl Shared {
    /// 利用者が入れるワークスペースの持ち主 (本人と、所属する team や Organization)。
    pub fn owners_for(&self, login: &str) -> Vec<Owner> {
        let mut owners = vec![Owner::User(login.to_ascii_lowercase())];
        for (owner, members) in &self.config.memberships {
            if members.iter().any(|m| m.eq_ignore_ascii_case(login))
                && let Ok(owner) = owner.parse()
            {
                owners.push(owner);
            }
        }
        owners
    }

    pub fn can_access(&self, login: &str, owner: &Owner) -> bool {
        self.owners_for(login).contains(owner)
    }

    /// 入るワークスペースを決める。指定がなければ個人の default を (なければ作って) 使う。
    async fn resolve_workspace(&self, login: &str, spec: Option<&str>) -> Result<Workspace> {
        let target = WorkspaceRef::parse(spec.unwrap_or(DEFAULT_WORKSPACE), login)?;
        if !self.can_access(login, &target.owner) {
            bail!("no such workspace: {target}");
        }
        let store = self.controller.store();
        if let Some(ws) = store.find(&target).await? {
            return Ok(ws);
        }
        if spec.is_some() {
            bail!("no such workspace: {target} (create it with `chloe ws create {target}`)");
        }
        match store.create(&target, RootMode::Ephemeral, login).await {
            Ok(ws) => Ok(ws),
            // 同時に接続した別の接続が先に作った。
            Err(ControllerError::AlreadyExists(_)) => store
                .find(&target)
                .await?
                .context("the default workspace disappeared"),
            Err(e) => Err(e.into()),
        }
    }
}

/// 認証を通った利用者と、SSH ユーザー名で指定されたワークスペース。
#[derive(Clone)]
struct Authenticated {
    login: String,
    workspace: Option<String>,
}

#[derive(Default)]
struct ChannelState {
    tty: Option<TtyRequest>,
    env: Vec<(String, String)>,
    input: Option<mpsc::Sender<ClientMessage>>,
    /// 標準入力を最後まで読んでから実行する管理コマンド。
    pending_command: Option<(Vec<String>, Vec<u8>)>,
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

    async fn authorize(&self, user: &str, key: &PublicKey) -> Result<Option<Authenticated>> {
        let user = parse_user(user)?;
        if !self.shared.config.is_allowed(&user.login) {
            return Ok(None);
        }
        // ワークスペースを指定していれば、GitHub に問い合わせる前に存在と所属を確かめる。
        if let Some(spec) = &user.workspace {
            let target = WorkspaceRef::parse(spec, &user.login)?;
            if !self.shared.can_access(&user.login, &target.owner)
                || self
                    .shared
                    .controller
                    .store()
                    .find(&target)
                    .await?
                    .is_none()
            {
                return Ok(None);
            }
        }
        if !self.shared.keys.is_registered(&user.login, key).await? {
            return Ok(None);
        }
        Ok(Some(Authenticated {
            login: user.login.to_ascii_lowercase(),
            workspace: user.workspace,
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
        if state.input.is_some() || state.pending_command.is_some() {
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
            workspace = ?user.workspace,
            command = ?request.command,
            tty = request.tty.is_some(),
            "session started"
        );
        let (tx, rx) = mpsc::channel(INPUT_BUFFER);
        state.input = Some(tx);
        tokio::spawn(relay(
            self.shared.clone(),
            user.clone(),
            session.handle(),
            channel,
            request,
            rx,
        ));
        session.channel_success(channel)
    }

    fn run_command(
        &self,
        channel: ChannelId,
        args: Vec<String>,
        stdin: Vec<u8>,
        session: &Session,
    ) {
        let (Some(user), handle) = (self.user.clone(), session.handle()) else {
            return;
        };
        let shared = self.shared.clone();
        tokio::spawn(async move {
            info!(login = %user.login, command = ?args, "management command");
            let output = commands::run(&shared, &user.login, &args, &stdin).await;
            let _ = handle.data(channel, output.stdout.into_bytes()).await;
            let _ = handle
                .extended_data(channel, 1, output.stderr.into_bytes())
                .await;
            let _ = handle.exit_status_request(channel, output.code).await;
            let _ = handle.eof(channel).await;
            let _ = handle.close(channel).await;
        });
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
                info!(peer = ?self.peer, login = %authenticated.login, workspace = ?authenticated.workspace, "authenticated");
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
        let command = String::from_utf8_lossy(data).into_owned();
        if let Some(args) = commands::parse(&command) {
            let Some(state) = self.channels.get_mut(&channel) else {
                return session.channel_failure(channel);
            };
            session.channel_success(channel)?;
            if commands::needs_stdin(&args) {
                state.pending_command = Some((args, Vec::new()));
            } else {
                self.run_command(channel, args, Vec::new(), session);
            }
            return Ok(());
        }
        // OpenSSH と同じく、コマンドは利用者のシェルに渡す。
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
        if let Some((_, stdin)) = self
            .channels
            .get_mut(&channel)
            .and_then(|s| s.pending_command.as_mut())
        {
            if stdin.len() + data.len() <= MAX_COMMAND_STDIN {
                stdin.extend_from_slice(data);
            }
            return Ok(());
        }
        self.send_input(channel, ClientMessage::Stdin(data.to_vec()))
            .await;
        Ok(())
    }

    async fn channel_eof(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if let Some((args, stdin)) = self
            .channels
            .get_mut(&channel)
            .and_then(|s| s.pending_command.take())
        {
            self.run_command(channel, args, stdin, session);
            return Ok(());
        }
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

/// ワークスペースのランナーを用意し、guest-agent とのあいだで入出力を中継して、
/// 終わったらチャネルを閉じる。
async fn relay(
    shared: Arc<Shared>,
    user: Authenticated,
    handle: Handle,
    channel: ChannelId,
    request: ExecRequest,
    input: mpsc::Receiver<ClientMessage>,
) {
    let result = async {
        let workspace = shared
            .resolve_workspace(&user.login, user.workspace.as_deref())
            .await?;
        // セッションを持っているあいだは、ランナーが止められない。
        let session = shared.controller.open(&workspace).await?;
        relay_inner(&handle, channel, &session.vsock, request, input).await
    }
    .await;
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
