//! SSH の接続ごとの処理。
//!
//! セッションチャネル一つにつき guest-agent への接続を一つ開き、シェルかコマンドを実行して
//! 入出力を中継する。`chloe` で始まるコマンドはゲートウェイが処理する ([`crate::commands`])。
//! ポート転送 (`ssh -L`) では、VM の中の宛先への接続を guest-agent に頼んで中継する。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chloe_agent_proto::{
    AgentMessage, ClientMessage, ConnectRequest, Connection as AgentConnection, ExecRequest,
    PROTOCOL_VERSION, TtyRequest, WindowSize,
};
use chloe_controller::workspace::{RootMode, Workspace, WorkspaceRef};
use chloe_controller::{Controller, Error as ControllerError};
use chloe_github::{DeviceCode, GitHubApp, UserToken};
use russh::keys::PublicKey;
use russh::server::{Auth, ChannelOpenHandle, Handle, Msg, Session};
use russh::{Channel, ChannelId, ChannelOpenFailure, Pty, Sig};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::auth::{
    Claim, GitHubKeys, KeyOutcome, SshUser, fingerprint, key_outcome, openssh, parse_user,
};
use crate::backend::Backend;
use crate::commands;
use crate::device_flow;
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
    pub controller: Controller<Backend>,
    pub github: Option<Arc<GitHubApp>>,
}

impl Shared {
    /// 入るワークスペースを決める。指定がなければ個人の default を (なければ作って) 使う。
    async fn resolve_workspace(&self, login: &str, spec: Option<&str>) -> Result<Workspace> {
        let target = WorkspaceRef::parse(spec.unwrap_or(DEFAULT_WORKSPACE), login)?;
        if !self.can_access(login, &target.owner).await? {
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

/// 公開鍵の確認の結果。
enum KeyCheck {
    Accept(Authenticated),
    /// 鍵は通すが、Device Flow を済ませないと入れない。
    DeviceFlow,
    Reject,
}

/// keyboard-interactive で進めている Device Flow。
enum DeviceState {
    /// 承認を待っている。`expected` は SSH ユーザー名の GitHub のログイン名 (誰でもよい
    /// 名前なら `None`)。
    Waiting {
        expected: Option<String>,
        workspace: Option<String>,
        device: DeviceCode,
    },
    /// 失敗を見せた。次の応答で断る。
    Failed,
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
    /// Device Flow に進む前に通した公開鍵。Device Flow を済ませたら、この鍵を利用者に結び付ける。
    offered_key: Option<PublicKey>,
    /// keyboard-interactive で進めている Device Flow。
    device: Option<DeviceState>,
    /// ポート転送をした接続では、転送中の TCP の接続がなくても、SSH の接続が切れるまで
    /// ランナーを止めない (`ssh -N -L` で開いたまま、ときどき使うため)。
    forwarding: ForwardingHold,
}

type ForwardingHold = Arc<std::sync::Mutex<Option<chloe_controller::Session<Backend>>>>;

impl Connection {
    pub fn new(shared: Arc<Shared>, peer: Option<std::net::SocketAddr>) -> Self {
        Self {
            shared,
            peer,
            user: None,
            channels: HashMap::new(),
            offered_key: None,
            device: None,
            forwarding: Arc::default(),
        }
    }

    /// ログイン名とワークスペースだけで判断できる確認。利用者がこのサーバーを使えて、
    /// 指定したワークスペースに入れるか。
    async fn admit(&self, login: &str, workspace: Option<&str>) -> Result<Option<Authenticated>> {
        if !self.shared.is_allowed(login).await? {
            return Ok(None);
        }
        if let Some(spec) = workspace {
            let target = WorkspaceRef::parse(spec, login)?;
            if !self.shared.can_access(login, &target.owner).await?
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
        Ok(Some(Authenticated {
            login: login.to_ascii_lowercase(),
            workspace: workspace.map(str::to_string),
        }))
    }

    /// SSH ユーザー名の名前が、GitHub のログイン名か、誰でもよい名前 (予約したユーザー名か
    /// GitHub に存在しない名前) か。
    async fn claim(&self, user: &SshUser) -> Result<Claim> {
        if user.name.eq_ignore_ascii_case(&self.shared.config.ssh_user) {
            return Ok(Claim::Anyone);
        }
        Ok(if self.shared.keys.exists(&user.name).await? {
            Claim::Login(user.name.to_ascii_lowercase())
        } else {
            Claim::Anyone
        })
    }

    async fn authorize(&self, user: &str, key: &PublicKey) -> Result<KeyCheck> {
        let user = parse_user(user)?;
        let claim = self.claim(&user).await?;
        let store = self.shared.controller.store();
        let fingerprint = fingerprint(key);
        let owner = match &claim {
            Claim::Login(login) => {
                // 入れない利用者には、Device Flow を始めさせない。
                if self
                    .admit(login, user.workspace.as_deref())
                    .await?
                    .is_none()
                {
                    return Ok(KeyCheck::Reject);
                }
                let registered = self.shared.keys.is_registered(login, key).await?
                    || store.ssh_key_login(&fingerprint).await?.as_ref() == Some(login);
                registered.then(|| login.clone())
            }
            Claim::Anyone => store.ssh_key_login(&fingerprint).await?,
        };
        let device_flow = self.shared.github.is_some();
        let token_usable = match &owner {
            Some(login) if device_flow => self.shared.has_usable_token(login).await?,
            _ => false,
        };
        Ok(
            match key_outcome(&claim, owner.as_deref(), device_flow, token_usable) {
                KeyOutcome::Reject => KeyCheck::Reject,
                KeyOutcome::DeviceFlow => KeyCheck::DeviceFlow,
                KeyOutcome::Accept => {
                    let login = owner.unwrap_or_default();
                    match self.admit(&login, user.workspace.as_deref()).await? {
                        Some(admitted) => KeyCheck::Accept(admitted),
                        None => KeyCheck::Reject,
                    }
                }
            },
        )
    }

    async fn check_key(&self, user: &str, key: &PublicKey) -> KeyCheck {
        match self.authorize(user, key).await {
            Ok(result) => result,
            Err(error) => {
                warn!(target: "audit", peer = %peer_name(self.peer), user, "authentication failed: {error:#}");
                KeyCheck::Reject
            }
        }
    }

    /// keyboard-interactive の最初の呼び出し。Device Flow を始めてコードを見せる。
    async fn start_device_flow(&mut self, github: &GitHubApp, user: &str) -> Result<Option<Auth>> {
        let user = parse_user(user)?;
        let expected = match self.claim(&user).await? {
            Claim::Login(login) => {
                if self
                    .admit(&login, user.workspace.as_deref())
                    .await?
                    .is_none()
                {
                    return Ok(None);
                }
                Some(login)
            }
            Claim::Anyone => None,
        };
        let device = github.start_device_flow().await?;
        let mut instructions = match &expected {
            Some(login) => format!("Sign in to GitHub as {login}:\n"),
            None => "Sign in to GitHub:\n".to_string(),
        };
        instructions.push_str(&format!(
            "  1. Open {}\n  2. Enter the code {}\n",
            device.verification_uri, device.user_code
        ));
        if expected.is_none()
            && let Some(key) = &self.offered_key
        {
            instructions.push_str(&format!(
                "This SSH key ({}) will sign you in from now on.\n",
                fingerprint(key)
            ));
        }
        self.device = Some(DeviceState::Waiting {
            expected,
            workspace: user.workspace,
            device,
        });
        Ok(Some(device_prompt(instructions)))
    }

    /// Device Flow が承認されたあと、トークンの持ち主を確かめて保存する。誰でもよい名前で
    /// 公開鍵を通していれば、その鍵を持ち主に結び付ける。
    async fn finish_device_login(
        &self,
        expected: Option<String>,
        workspace: Option<String>,
        owner: String,
        token: UserToken,
    ) -> Result<Authenticated> {
        let owner = owner.to_ascii_lowercase();
        if let Some(expected) = &expected
            && *expected != owner
        {
            bail!(
                "signed in to GitHub as {owner}, not {expected}. To sign in with any GitHub account, connect as {}@",
                self.shared.config.ssh_user
            );
        }
        let Some(admitted) = self.admit(&owner, workspace.as_deref()).await? else {
            bail!(
                "{owner} cannot use this server{}",
                workspace
                    .map(|w| format!(" or the workspace {w}"))
                    .unwrap_or_default()
            );
        };
        self.shared.store_user_token(&owner, &token).await?;
        // ログアウトのあとも動いていたワークスペースに、トークンをまた渡す。
        self.shared
            .refresh_running(Some(&crate::access::personal(&owner)))
            .await;
        if expected.is_none()
            && let Some(key) = &self.offered_key
        {
            let fingerprint = fingerprint(key);
            let bound = self
                .shared
                .controller
                .store()
                .bind_ssh_key(&owner, &fingerprint, &openssh(key))
                .await?;
            if bound == owner {
                info!(target: "audit", peer = %peer_name(self.peer), login = %owner, %fingerprint, "SSH key bound");
            } else {
                warn!(target: "audit", peer = %peer_name(self.peer), login = %owner, %fingerprint, bound_to = %bound, "the SSH key is bound to another user; not rebound");
            }
        }
        Ok(admitted)
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
        // 監査には、利用者のコマンドの引数 (トークンを含むことがある) を残さない。
        info!(
            target: "audit",
            login = %user.login,
            workspace = user.workspace.as_deref().unwrap_or(DEFAULT_WORKSPACE),
            kind = if request.command.is_empty() { "shell" } else { "exec" },
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
            info!(target: "audit", via = "ssh", login = %user.login, command = ?args, "management command");
            if args == ["github", "login"] {
                let code = github_login_command(&shared, &user.login, &handle, channel).await;
                let _ = handle.exit_status_request(channel, code).await;
                let _ = handle.eof(channel).await;
                let _ = handle.close(channel).await;
                return;
            }
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
        // 鍵の署名を求める前に、通せる鍵かを確かめる。違えば次の鍵を試させる。
        Ok(match self.check_key(user, public_key).await {
            KeyCheck::Accept(_) | KeyCheck::DeviceFlow => Auth::Accept,
            KeyCheck::Reject => Auth::reject(),
        })
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        public_key: &PublicKey,
    ) -> Result<Auth, Self::Error> {
        match self.check_key(user, public_key).await {
            KeyCheck::Accept(authenticated) => {
                info!(target: "audit", peer = %peer_name(self.peer), login = %authenticated.login, workspace = authenticated.workspace.as_deref().unwrap_or(DEFAULT_WORKSPACE), method = "publickey", "authenticated");
                let _ = self
                    .shared
                    .controller
                    .store()
                    .touch_ssh_key(&fingerprint(public_key))
                    .await;
                self.user = Some(authenticated);
                Ok(Auth::Accept)
            }
            // 鍵は通したことにして (部分的な成功)、Device Flow だけを続けさせる。
            KeyCheck::DeviceFlow => {
                info!(target: "audit", peer = %peer_name(self.peer), user, method = "publickey", fingerprint = %fingerprint(public_key), "device flow required");
                self.offered_key = Some(public_key.clone());
                Ok(Auth::Reject {
                    proceed_with_methods: Some(russh::MethodSet::from(
                        &[russh::MethodKind::KeyboardInteractive][..],
                    )),
                    partial_success: true,
                })
            }
            KeyCheck::Reject => {
                info!(target: "audit", peer = %peer_name(self.peer), user, method = "publickey", "rejected");
                Ok(Auth::reject())
            }
        }
    }

    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        user: &str,
        _submethods: &str,
        response: Option<russh::server::Response<'a>>,
    ) -> Result<Auth, Self::Error> {
        let Some(github) = self.shared.github.clone() else {
            return Ok(Auth::reject());
        };
        let outcome = match (response, self.device.take()) {
            // 最初の呼び出し。Device Flow を始めてコードを見せる。
            (None, _) => match self.start_device_flow(&github, user).await {
                Ok(Some(prompt)) => return Ok(prompt),
                Ok(None) => Ok(None),
                Err(error) => Err(error),
            },
            // 利用者が Enter を押した。承認されるまで少し待つ。
            (
                Some(_),
                Some(DeviceState::Waiting {
                    expected,
                    workspace,
                    device,
                }),
            ) => {
                let deadline = tokio::time::Instant::now() + KI_POLL_WAIT;
                match device_flow::wait_for_token(&github, &device, deadline).await {
                    Ok(Some((owner, token))) => {
                        match self
                            .finish_device_login(expected, workspace, owner, token)
                            .await
                        {
                            Ok(admitted) => Ok(Some(admitted)),
                            Err(error) => {
                                warn!(target: "audit", peer = %peer_name(self.peer), user, method = "device_flow", "rejected: {error:#}");
                                self.device = Some(DeviceState::Failed);
                                return Ok(failure_prompt(&error));
                            }
                        }
                    }
                    Ok(None) => {
                        self.device = Some(DeviceState::Waiting {
                            expected,
                            workspace,
                            device,
                        });
                        return Ok(device_prompt(
                            "Not authorized yet. Approve the code on GitHub, then press Enter.\n"
                                .into(),
                        ));
                    }
                    Err(error) => Err(error),
                }
            }
            (Some(_), _) => Ok(None),
        };
        match outcome {
            Ok(Some(authenticated)) => {
                let method = if self.offered_key.is_some() {
                    "publickey+device_flow"
                } else {
                    "device_flow"
                };
                info!(target: "audit", peer = %peer_name(self.peer), login = %authenticated.login, workspace = authenticated.workspace.as_deref().unwrap_or(DEFAULT_WORKSPACE), method, "authenticated");
                self.user = Some(authenticated);
                Ok(Auth::Accept)
            }
            Ok(None) => {
                info!(target: "audit", peer = %peer_name(self.peer), user, method = "device_flow", "rejected");
                Ok(Auth::reject())
            }
            Err(error) => {
                warn!(target: "audit", peer = %peer_name(self.peer), user, method = "device_flow", "rejected: {error:#}");
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

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host_to_connect: &str,
        port_to_connect: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let (Some(user), Ok(port)) = (self.user.clone(), u16::try_from(port_to_connect)) else {
            reply.reject(ChannelOpenFailure::ConnectFailed).await;
            return Ok(());
        };
        info!(
            target: "audit",
            login = %user.login,
            workspace = user.workspace.as_deref().unwrap_or(DEFAULT_WORKSPACE),
            host = host_to_connect,
            port,
            "port forwarding"
        );
        let request = ConnectRequest {
            version: PROTOCOL_VERSION,
            host: host_to_connect.to_string(),
            port,
        };
        // ランナーの起動を待つことがあるので、接続の処理を止めないよう別のタスクで進める。
        tokio::spawn(forward(
            self.shared.clone(),
            user,
            self.forwarding.clone(),
            channel,
            reply,
            request,
        ));
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
        let stream = session
            .connect_agent(AGENT_CONNECT_WAIT)
            .await
            .context("the workspace VM is not reachable")?;
        relay_inner(&handle, channel, stream, request, input).await
    }
    .await;
    let code = match result {
        Ok(Some(code)) => code,
        // 利用者がチャネルを閉じた。
        Ok(None) => {
            info!(target: "audit", login = %user.login, workspace = user.workspace.as_deref().unwrap_or(DEFAULT_WORKSPACE), "session ended");
            return;
        }
        Err(error) => {
            warn!("session failed: {error:#}");
            let message = format!("chloe: {error:#}\r\n");
            let _ = handle.extended_data(channel, 1, message.into_bytes()).await;
            1
        }
    };
    info!(target: "audit", login = %user.login, workspace = user.workspace.as_deref().unwrap_or(DEFAULT_WORKSPACE), code, "session ended");
    let _ = handle.exit_status_request(channel, code).await;
    let _ = handle.eof(channel).await;
    let _ = handle.close(channel).await;
}

async fn relay_inner(
    handle: &Handle,
    channel: ChannelId,
    stream: chloe_controller::runner::AgentStream,
    request: ExecRequest,
    mut input: mpsc::Receiver<ClientMessage>,
) -> Result<Option<u32>> {
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
                Some(AgentMessage::Started { .. } | AgentMessage::Activity { .. } | AgentMessage::Connected | AgentMessage::EnvUpdated) => {}
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

/// ポート転送。guest-agent に VM の中の宛先へ接続させ、つながったらチャネルを受け入れて
/// バイト列を中継する。つながらなければチャネルを断る。
async fn forward(
    shared: Arc<Shared>,
    user: Authenticated,
    hold: ForwardingHold,
    channel: Channel<Msg>,
    reply: ChannelOpenHandle,
    request: ConnectRequest,
) {
    let target = format!("{}:{}", request.host, request.port);
    let connected = async {
        let workspace = shared
            .resolve_workspace(&user.login, user.workspace.as_deref())
            .await?;
        // 中継しているあいだは、ランナーが止められない。
        let session = shared.controller.open(&workspace).await?;
        if hold.lock().unwrap().is_none() {
            let held = shared.controller.open(&workspace).await?;
            hold.lock().unwrap().get_or_insert(held);
        }
        let stream = session
            .connect_agent(AGENT_CONNECT_WAIT)
            .await
            .context("the workspace VM is not reachable")?;
        let mut conn = AgentConnection::new(stream);
        conn.send(&ClientMessage::Connect(request)).await?;
        match conn.recv::<AgentMessage>().await? {
            Some(AgentMessage::Connected) => Ok((session, conn.into_raw())),
            Some(AgentMessage::Error(message)) => bail!("guest-agent: {message}"),
            _ => bail!("guest-agent did not accept the connection"),
        }
    }
    .await;
    let (session, (mut vsock, buffered)) = match connected {
        Ok(connected) => connected,
        Err(error) => {
            warn!(target: "audit", login = %user.login, workspace = user.workspace.as_deref().unwrap_or(DEFAULT_WORKSPACE), %target, "port forwarding failed: {error:#}");
            reply.reject(ChannelOpenFailure::ConnectFailed).await;
            return;
        }
    };
    reply.accept().await;
    let mut stream = channel.into_stream();
    if tokio::io::AsyncWriteExt::write_all(&mut stream, &buffered)
        .await
        .is_ok()
    {
        let _ = tokio::io::copy_bidirectional(&mut stream, &mut vsock).await;
    }
    drop(session);
}

/// keyboard-interactive で一度に承認を待つ時間。
const KI_POLL_WAIT: Duration = Duration::from_secs(30);

fn device_prompt(instructions: String) -> Auth {
    Auth::Partial {
        name: "chloe".into(),
        instructions: instructions.into(),
        prompts: vec![(
            "Press Enter after you have approved the code: ".into(),
            true,
        )]
        .into(),
    }
}

/// 失敗の理由を見せて、Enter で閉じさせる。
fn failure_prompt(error: &anyhow::Error) -> Auth {
    Auth::Partial {
        name: "chloe".into(),
        instructions: format!("chloe: {error:#}\n").into(),
        prompts: vec![("Press Enter to close: ".into(), true)].into(),
    }
}

/// `chloe github login`：Device Flow で GitHub のユーザートークンを得て保存する。
async fn github_login_command(
    shared: &Shared,
    login: &str,
    handle: &Handle,
    channel: ChannelId,
) -> u32 {
    let say = |text: String| async move {
        let _ = handle.data(channel, text.into_bytes()).await;
    };
    let Some(github) = &shared.github else {
        say("chloe: the GitHub App is not configured\n".into()).await;
        return 1;
    };
    let device = match github.start_device_flow().await {
        Ok(device) => device,
        Err(error) => {
            say(format!("chloe: {error}\n")).await;
            return 1;
        }
    };
    say(format!(
        "Sign in to GitHub as {login}:\n  1. Open {}\n  2. Enter the code {}\nWaiting for approval...\n",
        device.verification_uri, device.user_code
    ))
    .await;
    match device_flow::renew(shared, github, login, &device).await {
        Ok(Some(refreshed)) => {
            say(format!(
                "Signed in. {refreshed} running workspace(s) can use GitHub now; shells opened from now on get GH_TOKEN.\n"
            ))
            .await;
            0
        }
        Ok(None) => {
            say("chloe: the code expired before it was approved\n".into()).await;
            1
        }
        Err(error) => {
            say(format!("chloe: {error:#}\n")).await;
            1
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

/// ログに書く接続元。
fn peer_name(peer: Option<std::net::SocketAddr>) -> String {
    peer.map(|p| p.to_string())
        .unwrap_or_else(|| "unknown".into())
}
