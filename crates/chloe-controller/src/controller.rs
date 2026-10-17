//! ワークスペースへの接続とランナーの寿命。
//!
//! 接続を開くとランナーを (止まっていれば) 起動し、最後の接続が閉じてから `idle_timeout` が
//! 経つとランナーを止める。ランナーが動いているあいだは、そのランナー専用の問い合わせ用ソケットで
//! 本物の値の問い合わせに答える ([`crate::broker`])。

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chloe_agent_proto::{AgentMessage, ClientMessage, Connection, ExecRequest, PROTOCOL_VERSION};
use chloe_secrets::{Owner, SecretDb, SecretString};
use futures::future::BoxFuture;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::Error;
use crate::catalog::{self, SecretSpec};
use crate::runner::{AgentStream, BasePolicy, RunnerBackend, RunnerSpec, build_spec};
use crate::store::Store;
use crate::workspace::Workspace;

/// VM を止めるときに、VM の中で再起動を頼んでから待つ時間の既定値。
const GRACEFUL_STOP_TIMEOUT: Duration = Duration::from_secs(30);
/// エージェントが作業中のとき、止める前に作業中かを尋ね直す間隔の既定値。
const ACTIVITY_POLL: Duration = Duration::from_secs(60);
/// エージェントが作業中でも、接続がなくなってからこれだけ経ったら止める (既定値)。
const MAX_BUSY_WAIT: Duration = Duration::from_secs(12 * 60 * 60);

/// データベースの外で作るシークレット (GitHub のトークンなど)。問い合わせのたびに尋ねる。
/// `kinds` に挙げた種類は、データベースの同じ種類より優先する。
pub trait DynamicSecrets: Send + Sync + 'static {
    /// ワークスペースで使える種類。
    fn kinds<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, Vec<String>>;
    fn resolve<'a>(
        &'a self,
        workspace: &'a Workspace,
        kind: &'a str,
    ) -> BoxFuture<'a, Result<Option<SecretString>, Error>>;
}

/// ワークスペースのシークレットの出どころをまとめたもの。
#[derive(Clone)]
pub(crate) struct Resolver {
    secrets: Arc<SecretDb>,
    dynamic: Option<Arc<dyn DynamicSecrets>>,
    /// 組み込みの種類の定義。既定は [`catalog::preset`] で、設定で変えられる。
    presets: Arc<BTreeMap<String, SecretSpec>>,
}

/// ワークスペースのシークレットを探す持ち主 (先にあるほうが優先する)。
/// team のワークスペースは、team になければ Organization のシークレットを使う。
pub fn secret_owners(owner: &Owner) -> Vec<Owner> {
    match owner {
        Owner::Team { org, .. } => vec![owner.clone(), Owner::Org(org.clone())],
        _ => vec![owner.clone()],
    }
}

impl Resolver {
    /// ワークスペースで使えるシークレットと、その定義。持ち主 (team なら Organization も) が
    /// データベースに登録したものと、動的に作る種類を合わせたもの。
    ///
    /// 定義は値を復号して確かめたものだけを使う (定義だけを書き換えて別の宛先へ送らせないため)。
    /// 定義がなく組み込みの種類でもない行 (GitHub のユーザートークンなど) は含めない。
    pub async fn specs(&self, workspace: &Workspace) -> Result<Vec<(String, SecretSpec)>, Error> {
        let mut specs = std::collections::BTreeMap::new();
        if let Some(dynamic) = &self.dynamic {
            for kind in dynamic.kinds(workspace).await {
                if let Some(spec) = self.preset(&kind) {
                    specs.insert(kind, spec);
                }
            }
        }
        for owner in secret_owners(&workspace.owner) {
            for name in self.secrets.list(&owner).await? {
                if specs.contains_key(&name) {
                    continue;
                }
                match self.secrets.get_with_spec(&owner, &name).await {
                    Ok(Some((_, stored))) => {
                        if let Some(spec) = self.spec_of(&name, stored.as_deref()) {
                            specs.insert(name, spec);
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        warn!(workspace = %workspace.display_name(), secret = %name, "skipped a secret that cannot be read: {e}");
                    }
                }
            }
        }
        Ok(specs.into_iter().collect())
    }

    /// シークレットの値 (保存した形のまま) と定義。
    pub async fn resolve(
        &self,
        workspace: &Workspace,
        name: &str,
    ) -> Result<Option<(SecretString, SecretSpec)>, Error> {
        if let Some(dynamic) = &self.dynamic
            && let Some(spec) = self.preset(name)
            && dynamic.kinds(workspace).await.iter().any(|k| k == name)
        {
            return Ok(dynamic
                .resolve(workspace, name)
                .await?
                .map(|value| (value, spec)));
        }
        for owner in secret_owners(&workspace.owner) {
            if let Some((value, stored)) = self.secrets.get_with_spec(&owner, name).await? {
                return Ok(self
                    .spec_of(name, stored.as_deref())
                    .map(|spec| (value, spec)));
            }
        }
        Ok(None)
    }

    pub(crate) fn preset(&self, name: &str) -> Option<SecretSpec> {
        self.presets.get(name).cloned()
    }

    /// 保存した定義か、なければ組み込みの種類の定義。
    fn spec_of(&self, name: &str, stored: Option<&str>) -> Option<SecretSpec> {
        match stored {
            Some(text) => match serde_json::from_str(text) {
                Ok(spec) => Some(spec),
                Err(e) => {
                    warn!(secret = %name, "ignored an invalid secret definition: {e}");
                    None
                }
            },
            None => self.preset(name),
        }
    }
}

pub struct Controller<B> {
    inner: Arc<Inner<B>>,
}

impl<B> Clone for Controller<B> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

struct Inner<B> {
    store: Store,
    resolver: Resolver,
    backend: B,
    base: BasePolicy,
    idle_timeout: Duration,
    stop_timeout: Duration,
    activity_poll: Duration,
    max_busy_wait: Duration,
    runners: Mutex<HashMap<i64, Arc<RunnerState>>>,
}

#[derive(Default)]
struct RunnerState {
    /// 起動と停止を一つずつ進めるための錠。動いていれば `Some`。
    lifecycle: tokio::sync::Mutex<Option<Running>>,
    sessions: Mutex<SessionCount>,
}

struct Running {
    /// 同じホストで本物の値の問い合わせに答えるタスク (ほかのホストのランナーでは `None`)。
    broker: Option<JoinHandle<()>>,
}

impl Running {
    fn abort_broker(&self) {
        if let Some(broker) = &self.broker {
            broker.abort();
        }
    }
}

#[derive(Default)]
struct SessionCount {
    open: usize,
    /// 最後の接続が閉じるたびに増やし、古い停止の予約を無効にする。
    generation: u64,
}

/// [`Controller::reconcile`] で直したランナーの識別子。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// 動いていたので引き取ったもの。
    pub adopted: Vec<String>,
    /// 持ち主がいないので止めたもの。
    pub stopped: Vec<String>,
    /// 止まっていたので把握を消したもの。
    pub cleared: Vec<String>,
}

/// 開いている接続。drop すると接続を閉じたことになる。
pub struct Session<B: RunnerBackend> {
    controller: Controller<B>,
    workspace_id: i64,
    key: String,
}

impl<B: RunnerBackend> Session<B> {
    /// ワークスペースの VM の guest-agent につなぐ。VM が起動しきるまで `wait` だけ待つ。
    pub async fn connect_agent(&self, wait: Duration) -> Result<AgentStream, Error> {
        self.controller
            .inner
            .backend
            .connect_agent(&self.key, wait)
            .await
    }
}

impl<B: RunnerBackend> Controller<B> {
    pub fn new(
        store: Store,
        secrets: SecretDb,
        backend: B,
        base: BasePolicy,
        idle_timeout: Duration,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                store,
                resolver: Resolver {
                    secrets: Arc::new(secrets),
                    dynamic: None,
                    presets: Arc::new(
                        catalog::PRESETS
                            .iter()
                            .filter_map(|name| Some((name.to_string(), catalog::preset(name)?)))
                            .collect(),
                    ),
                },
                backend,
                base,
                idle_timeout,
                stop_timeout: GRACEFUL_STOP_TIMEOUT,
                activity_poll: ACTIVITY_POLL,
                max_busy_wait: MAX_BUSY_WAIT,
                runners: Mutex::new(HashMap::new()),
            }),
        }
    }

    fn configure(self, f: impl FnOnce(&mut Inner<B>)) -> Self {
        let mut inner = Arc::try_unwrap(self.inner)
            .unwrap_or_else(|_| panic!("the controller must be configured before sharing"));
        f(&mut inner);
        Self {
            inner: Arc::new(inner),
        }
    }

    /// VM の中で再起動を頼んでから、ランナーが止まるのを待つ時間を変える。
    pub fn with_stop_timeout(self, stop_timeout: Duration) -> Self {
        self.configure(|inner| inner.stop_timeout = stop_timeout)
    }

    /// エージェントが作業中のときに尋ね直す間隔と、待つ長さの上限を変える。
    pub fn with_activity_wait(self, poll: Duration, max: Duration) -> Self {
        self.configure(|inner| {
            inner.activity_poll = poll;
            inner.max_busy_wait = max;
        })
    }

    /// データベースの外で作るシークレットの出どころを足す。
    pub fn with_dynamic_secrets(self, dynamic: Arc<dyn DynamicSecrets>) -> Self {
        self.configure(|inner| inner.resolver.dynamic = Some(dynamic))
    }

    /// 組み込みの種類の定義を変える (GitHub のトークンの宛先など)。値は chloe が作るので、
    /// 欄は `value` 一つに限る。
    pub fn with_preset(self, name: &str, spec: SecretSpec) -> Result<Self, Error> {
        if !catalog::PRESETS.contains(&name) {
            return Err(Error::Invalid(format!("{name} is not a built-in secret")));
        }
        spec.validate().map_err(Error::Invalid)?;
        if !spec.is_single() {
            return Err(Error::Invalid(format!(
                "the definition of {name} must have the single field \"value\""
            )));
        }
        Ok(self.configure(|inner| {
            Arc::make_mut(&mut inner.resolver.presets).insert(name.to_string(), spec);
        }))
    }

    /// 組み込みの種類の定義。
    pub fn preset(&self, name: &str) -> Option<SecretSpec> {
        self.inner.resolver.preset(name)
    }

    pub fn store(&self) -> &Store {
        &self.inner.store
    }

    pub fn secrets(&self) -> &SecretDb {
        &self.inner.resolver.secrets
    }

    fn state(&self, workspace_id: i64) -> Arc<RunnerState> {
        self.inner
            .runners
            .lock()
            .unwrap()
            .entry(workspace_id)
            .or_default()
            .clone()
    }

    /// ワークスペースへの接続を開く。ランナーが止まっていれば起動する。
    pub async fn open(&self, workspace: &Workspace) -> Result<Session<B>, Error> {
        let state = self.state(workspace.id);
        state.sessions.lock().unwrap().open += 1;
        match self.ensure_running(workspace, &state).await {
            Ok(()) => Ok(Session {
                controller: self.clone(),
                workspace_id: workspace.id,
                key: workspace.key(),
            }),
            Err(error) => {
                self.close(workspace.id);
                Err(error)
            }
        }
    }

    async fn ensure_running(
        &self,
        workspace: &Workspace,
        state: &RunnerState,
    ) -> Result<(), Error> {
        let mut running = state.lifecycle.lock().await;
        let key = workspace.key();
        if let Some(current) = running.as_ref() {
            if self.inner.backend.is_running(&key).await? {
                return Ok(());
            }
            current.abort_broker();
        }
        let spec = self.build(workspace).await?;
        // ランナーのプロキシが問い合わせられるよう、起動する前に待ち受けを始める。
        let broker = self.serve_local_broker(workspace)?;
        info!(target: "audit", workspace = %workspace.display_name(), key, "starting runner");
        match self.inner.backend.start(spec).await {
            Ok(()) => {
                *running = Some(Running { broker });
                self.inner
                    .store
                    .record_runner(workspace.id, &self.inner.backend.node_of(&key).await?)
                    .await?;
                Ok(())
            }
            Err(error) => {
                if let Some(broker) = broker {
                    broker.abort();
                }
                Err(error)
            }
        }
    }

    /// 同じホストのランナーなら、問い合わせ用ソケットで待ち受けを始める。
    fn serve_local_broker(&self, workspace: &Workspace) -> Result<Option<JoinHandle<()>>, Error> {
        match self.inner.backend.local_broker_socket(&workspace.key()) {
            Some(socket) => Ok(Some(crate::broker::serve(
                &socket,
                workspace.clone(),
                self.inner.resolver.clone(),
            )?)),
            None => Ok(None),
        }
    }

    fn remove_local_broker(&self, key: &str) {
        if let Some(socket) = self.inner.backend.local_broker_socket(key) {
            let _ = std::fs::remove_file(socket);
        }
    }

    /// ランナーのプロキシからの本物の値の問い合わせ (一行の JSON) に答える。ほかのホストの
    /// `chloe-node` が中継してきた問い合わせに使う。そのランナーを動かしているホストからの
    /// 問い合わせかは、呼ぶ側が確かめる。
    pub async fn answer_secret(&self, key: &str, question: &str) -> String {
        let workspace = match key
            .strip_prefix("ws-")
            .and_then(|id| id.parse::<i64>().ok())
        {
            Some(id) => self.inner.store.get(id).await.ok().flatten(),
            None => None,
        };
        match workspace {
            Some(workspace) => {
                crate::broker::answer(question, &workspace, &self.inner.resolver).await
            }
            None => format!("{}\n", serde_json::json!({ "error": "unknown runner" })),
        }
    }

    /// 動いているランナーのワークスペースの一覧。
    pub async fn running(&self) -> Result<Vec<Workspace>, Error> {
        let ids: Vec<i64> = self.inner.runners.lock().unwrap().keys().copied().collect();
        let mut workspaces = Vec::new();
        for id in ids {
            let state = self.state(id);
            if state.lifecycle.lock().await.is_some()
                && let Some(ws) = self.inner.store.get(id).await?
            {
                workspaces.push(ws);
            }
        }
        Ok(workspaces)
    }

    /// 動いているランナーの方針を作り直して送る (使えるシークレットの種類が変わったときなど)。
    /// 動いていなければ何もせず `false` を返す。
    pub async fn refresh(&self, workspace: &Workspace) -> Result<bool, Error> {
        let state = self.state(workspace.id);
        let running = state.lifecycle.lock().await;
        if running.is_none() {
            return Ok(false);
        }
        let spec = self.build(workspace).await?;
        let env = spec.guest_env.clone();
        self.inner.backend.update(spec).await?;
        info!(workspace = %workspace.display_name(), "runner policy refreshed");
        // 新しく使えるようになった種類のプレースホルダーを、このあとに起動するシェルに入れる。
        // すでに動いているシェルの環境は変わらない。
        drop(running);
        if let Err(error) = self.push_env(&workspace.key(), env).await {
            warn!(workspace = %workspace.display_name(), "failed to update the VM environment: {error}");
        }
        Ok(true)
    }

    async fn build(&self, workspace: &Workspace) -> Result<RunnerSpec, Error> {
        let specs = self.inner.resolver.specs(workspace).await?;
        let placeholders = self
            .inner
            .store
            .ensure_placeholders(workspace.id, &specs)
            .await?;
        let layers = self.inner.store.policy_layers(&workspace.owner).await?;
        Ok(build_spec(
            workspace,
            &self.inner.base.with_layers(&layers),
            &placeholders,
            &specs,
        ))
    }

    fn close(&self, workspace_id: i64) {
        let state = self.state(workspace_id);
        {
            let mut sessions = state.sessions.lock().unwrap();
            sessions.open = sessions.open.saturating_sub(1);
            if sessions.open > 0 {
                return;
            }
        }
        self.schedule_idle_stop(workspace_id);
    }

    /// 接続がないまま `idle_timeout` が経ったらランナーを止める予約をする。
    /// 止める前に VM の中のエージェントに作業中かを尋ね、作業中なら `max_busy_wait` までは
    /// `activity_poll` ごとに尋ね直して待つ。そのあいだに接続されたら予約を取り消す。
    fn schedule_idle_stop(&self, workspace_id: i64) {
        let state = self.state(workspace_id);
        let generation = {
            let mut sessions = state.sessions.lock().unwrap();
            if sessions.open > 0 {
                return;
            }
            sessions.generation += 1;
            sessions.generation
        };
        let still_idle = {
            let state = state.clone();
            move || {
                let sessions = state.sessions.lock().unwrap();
                sessions.open == 0 && sessions.generation == generation
            }
        };
        let controller = self.clone();
        tokio::spawn(async move {
            let inner = &controller.inner;
            tokio::time::sleep(inner.idle_timeout).await;
            let busy_since = tokio::time::Instant::now();
            loop {
                if !still_idle() {
                    return;
                }
                let busy = controller.activity(workspace_id, &state).await;
                if busy.is_empty() {
                    break;
                }
                if busy_since.elapsed() >= inner.max_busy_wait {
                    warn!(
                        workspace_id,
                        ?busy,
                        "stopping although an agent still looks busy"
                    );
                    break;
                }
                info!(
                    workspace_id,
                    ?busy,
                    "an agent is working; the runner keeps running"
                );
                tokio::time::sleep(inner.activity_poll).await;
            }
            if !still_idle() {
                return;
            }
            if let Err(error) = controller.stop_runner(workspace_id, &state).await {
                warn!(workspace_id, "failed to stop the runner: {error}");
            }
        });
    }

    /// VM の中で作業中のエージェントの印。尋ねられなければ作業中のものはないとみなす。
    async fn activity(&self, workspace_id: i64, state: &RunnerState) -> Vec<String> {
        if state.lifecycle.lock().await.is_none() {
            return vec![];
        }
        let key = format!("ws-{workspace_id}");
        match tokio::time::timeout(Duration::from_secs(10), self.query_activity(&key)).await {
            Ok(Ok(busy)) => busy,
            Ok(Err(error)) => {
                warn!("failed to ask the VM about agent activity: {error}");
                vec![]
            }
            Err(_) => {
                warn!("the VM did not answer about agent activity in time");
                vec![]
            }
        }
    }

    /// 保存しているランナーの記録と、実際に動いているランナーを突き合わせて直す。
    ///
    /// - 動いているが controller が把握していないランナー (ゲートウェイの再起動の前に起動したもの)
    ///   は引き取り、問い合わせ用ソケットを張り直して、接続がなければ停止を予約する。
    /// - 持ち主のワークスペースがないランナーは止める。
    /// - 止まっているのに把握や記録が残っていれば消す。
    pub async fn reconcile(&self) -> Result<ReconcileReport, Error> {
        let mut report = ReconcileReport::default();
        let actual = self.inner.backend.list().await?;
        for key in &actual {
            let workspace = match key
                .strip_prefix("ws-")
                .and_then(|id| id.parse::<i64>().ok())
            {
                Some(id) => self.inner.store.get(id).await?,
                None => None,
            };
            let Some(workspace) = workspace else {
                warn!(target: "audit", key, "stopping a runner without a workspace");
                self.inner.backend.stop(key).await?;
                self.remove_local_broker(key);
                report.stopped.push(key.clone());
                continue;
            };
            let state = self.state(workspace.id);
            let mut running = state.lifecycle.lock().await;
            // 一覧を取ってから錠を得るまでに、止めたかもしれない。
            if running.is_some() || !self.inner.backend.is_running(key).await? {
                continue;
            }
            let broker = self.serve_local_broker(&workspace)?;
            *running = Some(Running { broker });
            drop(running);
            self.inner
                .store
                .record_runner(workspace.id, &self.inner.backend.node_of(key).await?)
                .await?;
            info!(target: "audit", workspace = %workspace.display_name(), key, "adopted a running runner");
            report.adopted.push(key.clone());
            self.schedule_idle_stop(workspace.id);
        }

        let known: Vec<(i64, Arc<RunnerState>)> = self
            .inner
            .runners
            .lock()
            .unwrap()
            .iter()
            .map(|(id, state)| (*id, state.clone()))
            .collect();
        for (id, state) in known {
            let key = format!("ws-{id}");
            if actual.contains(&key) {
                continue;
            }
            let mut running = state.lifecycle.lock().await;
            // 一覧を取ってから錠を得るまでに、起動したかもしれない (起動は錠を持って進む)。
            // 起動したばかりのランナーの問い合わせ用ソケットを消すと、プロキシが本物の値を
            // 問い合わせられなくなる。
            if self.inner.backend.is_running(&key).await? {
                continue;
            }
            if let Some(current) = running.take() {
                current.abort_broker();
                self.remove_local_broker(&key);
                warn!(target: "audit", key, "a runner stopped without the controller");
                report.cleared.push(key);
            }
        }
        for id in self.inner.store.recorded_runners().await? {
            let key = format!("ws-{id}");
            if !actual.contains(&key) && !self.inner.backend.is_running(&key).await? {
                self.inner.store.remove_runner(id).await?;
            }
        }
        Ok(report)
    }

    /// ランナーを止める。接続が残っていても止める。
    pub async fn stop(&self, workspace: &Workspace) -> Result<(), Error> {
        let state = self.state(workspace.id);
        self.stop_runner(workspace.id, &state).await
    }

    async fn stop_runner(&self, workspace_id: i64, state: &RunnerState) -> Result<(), Error> {
        let mut running = state.lifecycle.lock().await;
        let Some(current) = running.take() else {
            return Ok(());
        };
        let key = format!("ws-{workspace_id}");
        info!(target: "audit", key, "stopping runner");
        // VM の中で再起動を頼むと、ファイルシステムを閉じてから Firecracker が終わる。
        if let Err(error) = self.request_reboot(&key).await {
            warn!(key, "failed to ask the VM to stop: {error}");
        }
        let deadline = tokio::time::Instant::now() + self.inner.stop_timeout;
        while tokio::time::Instant::now() < deadline && self.inner.backend.is_running(&key).await? {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let result = self.inner.backend.stop(&key).await;
        current.abort_broker();
        self.remove_local_broker(&key);
        if result.is_ok() {
            self.inner.store.remove_runner(workspace_id).await?;
        }
        result
    }
}

impl<B: RunnerBackend> Drop for Session<B> {
    fn drop(&mut self) {
        self.controller.close(self.workspace_id);
    }
}

impl<B: RunnerBackend> Controller<B> {
    async fn connect(&self, key: &str, wait: Duration) -> Result<Connection<AgentStream>, Error> {
        Ok(Connection::new(
            self.inner.backend.connect_agent(key, wait).await?,
        ))
    }

    async fn push_env(&self, key: &str, env: Vec<(String, String)>) -> Result<(), Error> {
        let mut conn = self.connect(key, Duration::from_secs(2)).await?;
        conn.send(&ClientMessage::SetEnv(env))
            .await
            .map_err(|e| Error::Runner(e.to_string()))?;
        let answer = tokio::time::timeout(Duration::from_secs(5), conn.recv::<AgentMessage>())
            .await
            .map_err(|_| Error::Runner("the VM did not answer in time".into()))?;
        match answer {
            Ok(Some(AgentMessage::EnvUpdated)) => Ok(()),
            Ok(Some(AgentMessage::Error(message))) => Err(Error::Runner(message)),
            Ok(other) => Err(Error::Runner(format!("unexpected answer: {other:?}"))),
            Err(e) => Err(Error::Runner(e.to_string())),
        }
    }

    async fn query_activity(&self, key: &str) -> Result<Vec<String>, Error> {
        let mut conn = self.connect(key, Duration::from_secs(2)).await?;
        conn.send(&ClientMessage::Activity)
            .await
            .map_err(|e| Error::Runner(e.to_string()))?;
        match conn.recv::<AgentMessage>().await {
            Ok(Some(AgentMessage::Activity { busy })) => Ok(busy),
            Ok(other) => Err(Error::Runner(format!("unexpected answer: {other:?}"))),
            Err(e) => Err(Error::Runner(e.to_string())),
        }
    }

    async fn request_reboot(&self, key: &str) -> Result<(), Error> {
        let wait = self.inner.stop_timeout.min(Duration::from_secs(2));
        let mut conn = self.connect(key, wait).await?;
        conn.send(&ClientMessage::Exec(ExecRequest {
            version: PROTOCOL_VERSION,
            command: vec!["systemctl".into(), "reboot".into()],
            user: Some("root".into()),
            env: vec![],
            cwd: None,
            tty: None,
        }))
        .await
        .map_err(|e| Error::Runner(e.to_string()))?;
        // 接続を閉じると guest-agent はプロセスを止めるので、systemctl が再起動を頼み終わる
        // (終わるか、再起動で接続が切れる) まで待つ。`Started` だけで閉じると、再起動が始まらない。
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match conn.recv::<AgentMessage>().await {
                    Ok(Some(AgentMessage::Exited(_) | AgentMessage::Error(_)))
                    | Ok(None)
                    | Err(_) => {
                        break;
                    }
                    Ok(Some(_)) => {}
                }
            }
        })
        .await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{RootMode, WorkspaceRef};
    use chloe_agent_proto::ExitStatus;
    use chloe_secrets::{MasterKey, Owner};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    /// 偽の guest-agent が最後に受け取った環境変数。
    type PushedEnv = Arc<Mutex<Option<Vec<(String, String)>>>>;

    struct FakeBackend {
        dir: tempfile::TempDir,
        starts: AtomicUsize,
        stops: AtomicUsize,
        updates: AtomicUsize,
        /// 動いているランナーの識別子と、その方針の差し替えの規則の数。
        running: Mutex<HashMap<String, usize>>,
        /// 偽の guest-agent が答える、作業中のエージェントの印。
        busy: Arc<Mutex<Vec<String>>>,
        /// 偽の guest-agent が再起動を最後まで受け付けた回数。
        reboots: Arc<AtomicUsize>,
        /// 偽の guest-agent が最後に受け取った環境変数。
        env: PushedEnv,
        /// ランナーごとの、最後に渡した方針の許可リスト。
        allow: Mutex<HashMap<String, Vec<String>>>,
        /// あれば、`list` はこの (古い) 一覧を返す。一覧を取ったあとに起動した場合をまねる。
        stale_list: Mutex<Option<Vec<String>>>,
    }

    /// guest-agent の応答をまねる。
    ///
    /// 本物と同じく、コマンドの要求には先に `Started` を返し、終わる前に接続が閉じられたら
    /// プロセスを止めたことにする (再起動の要求なら、再起動は始まらない)。
    async fn fake_agent(
        path: PathBuf,
        busy: Arc<Mutex<Vec<String>>>,
        reboots: Arc<AtomicUsize>,
        env: PushedEnv,
    ) {
        let _ = std::fs::remove_file(&path);
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let busy = busy.clone();
                let reboots = reboots.clone();
                let env = env.clone();
                tokio::spawn(async move {
                    let mut conn = chloe_agent_proto::Connection::new(stream);
                    let _ = match conn.recv::<ClientMessage>().await {
                        Ok(Some(ClientMessage::Activity)) => {
                            let busy = busy.lock().unwrap().clone();
                            conn.send(&AgentMessage::Activity { busy }).await
                        }
                        Ok(Some(ClientMessage::SetEnv(vars))) => {
                            *env.lock().unwrap() = Some(vars);
                            conn.send(&AgentMessage::EnvUpdated).await
                        }
                        Ok(Some(ClientMessage::Exec(request))) => {
                            conn.send(&AgentMessage::Started { pid: 1 }).await.unwrap();
                            // コマンドが動いているあいだに相手が閉じたら、プロセスは止められる。
                            let closed = tokio::time::timeout(
                                Duration::from_millis(20),
                                conn.recv::<ClientMessage>(),
                            )
                            .await;
                            if matches!(closed, Ok(Ok(None) | Err(_))) {
                                return;
                            }
                            if request.command == ["systemctl", "reboot"] {
                                reboots.fetch_add(1, Ordering::SeqCst);
                            }
                            let status = ExitStatus {
                                code: Some(0),
                                signal: None,
                            };
                            conn.send(&AgentMessage::Exited(status)).await
                        }
                        _ => Ok(()),
                    };
                });
            }
        });
    }

    impl FakeBackend {
        fn broker_socket(&self, key: &str) -> PathBuf {
            self.dir.path().join(format!("{key}.broker.sock"))
        }

        fn vsock_path(&self, key: &str) -> PathBuf {
            self.dir.path().join(format!("{key}.vsock"))
        }
    }

    impl RunnerBackend for Arc<FakeBackend> {
        async fn start(&self, spec: RunnerSpec) -> Result<(), Error> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            self.allow
                .lock()
                .unwrap()
                .insert(spec.key.clone(), spec.policy.allow.clone());
            fake_agent(
                self.vsock_path(&spec.key),
                self.busy.clone(),
                self.reboots.clone(),
                self.env.clone(),
            )
            .await;
            self.running.lock().unwrap().insert(
                spec.key.clone(),
                spec.policy.inject.len() + spec.policy.sign.len(),
            );
            Ok(())
        }

        async fn stop(&self, key: &str) -> Result<(), Error> {
            self.stops.fetch_add(1, Ordering::SeqCst);
            self.running.lock().unwrap().remove(key);
            Ok(())
        }

        async fn is_running(&self, key: &str) -> Result<bool, Error> {
            Ok(self.running.lock().unwrap().contains_key(key))
        }

        async fn update(&self, spec: RunnerSpec) -> Result<(), Error> {
            self.updates.fetch_add(1, Ordering::SeqCst);
            self.allow
                .lock()
                .unwrap()
                .insert(spec.key.clone(), spec.policy.allow.clone());
            self.running.lock().unwrap().insert(
                spec.key.clone(),
                spec.policy.inject.len() + spec.policy.sign.len(),
            );
            Ok(())
        }

        async fn connect_agent(&self, key: &str, _wait: Duration) -> Result<AgentStream, Error> {
            let stream = tokio::net::UnixStream::connect(self.vsock_path(key)).await?;
            Ok(Box::new(stream))
        }

        fn local_broker_socket(&self, key: &str) -> Option<PathBuf> {
            Some(self.broker_socket(key))
        }

        async fn list(&self) -> Result<Vec<String>, Error> {
            if let Some(stale) = self.stale_list.lock().unwrap().clone() {
                return Ok(stale);
            }
            Ok(self.running.lock().unwrap().keys().cloned().collect())
        }
    }

    /// GitHub のトークンのふり。alice の個人のワークスペースにだけ答える。
    struct FakeGitHub;

    impl DynamicSecrets for FakeGitHub {
        fn kinds<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, Vec<String>> {
            Box::pin(async move {
                match &workspace.owner {
                    Owner::User(login) if login == "alice" => vec!["github_token".into()],
                    _ => vec![],
                }
            })
        }

        fn resolve<'a>(
            &'a self,
            _workspace: &'a Workspace,
            _kind: &'a str,
        ) -> BoxFuture<'a, Result<Option<SecretString>, Error>> {
            Box::pin(async { Ok(Some("ghu_fresh".to_string().into())) })
        }
    }

    fn backend() -> Arc<FakeBackend> {
        Arc::new(FakeBackend {
            dir: tempfile::tempdir().unwrap(),
            starts: AtomicUsize::new(0),
            stops: AtomicUsize::new(0),
            updates: AtomicUsize::new(0),
            running: Mutex::new(HashMap::new()),
            busy: Arc::new(Mutex::new(vec![])),
            reboots: Arc::new(AtomicUsize::new(0)),
            env: Arc::new(Mutex::new(None)),
            allow: Mutex::new(HashMap::new()),
            stale_list: Mutex::new(None),
        })
    }

    fn controller_on(
        pool: sqlx::SqlitePool,
        backend: Arc<FakeBackend>,
        idle: Duration,
    ) -> Controller<Arc<FakeBackend>> {
        let secrets = SecretDb::new(pool.clone(), MasterKey::from_bytes("t", &[3; 32]).unwrap());
        Controller::new(
            Store::new(pool),
            secrets,
            backend,
            BasePolicy::default(),
            idle,
        )
        .with_stop_timeout(Duration::from_millis(50))
        .with_activity_wait(Duration::from_millis(100), Duration::from_secs(60))
        .with_dynamic_secrets(Arc::new(FakeGitHub))
    }

    async fn controller(idle: Duration) -> (Controller<Arc<FakeBackend>>, Arc<FakeBackend>) {
        let backend = backend();
        let pool = crate::store::tests::pool().await;
        (controller_on(pool, backend.clone(), idle), backend)
    }

    async fn ask(socket: &std::path::Path, secret: &str) -> String {
        let mut stream = tokio::net::UnixStream::connect(socket).await.unwrap();
        stream
            .write_all(format!("{}\n", serde_json::json!({ "secret": secret })).as_bytes())
            .await
            .unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).await.unwrap();
        line
    }

    async fn workspace(
        controller: &Controller<Arc<FakeBackend>>,
        name: &str,
        login: &str,
    ) -> Workspace {
        controller
            .store()
            .create(
                &WorkspaceRef::parse(name, login).unwrap(),
                RootMode::Ephemeral,
                login,
            )
            .await
            .unwrap()
    }

    /// 例の定義 (deploy/secrets) で登録する。ゲートウェイの登録と同じ形で置く。
    trait SetExample {
        async fn set_example(&self, owner: &Owner, name: &str, value: &str);
    }

    impl SetExample for SecretDb {
        async fn set_example(&self, owner: &Owner, name: &str, value: &str) {
            let (spec, values) = catalog::example(name, value).into_parts().unwrap();
            let spec = spec.unwrap();
            let stored = spec.encode_values(&values).unwrap();
            self.set_with_spec(
                owner,
                name,
                &stored.into(),
                Some(&serde_json::to_string(&spec).unwrap()),
            )
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    async fn waits_while_an_agent_is_working() {
        let ms = Duration::from_millis;
        let (controller, backend) = controller(ms(200)).await;
        let ws = workspace(&controller, "api", "alice").await;
        *backend.busy.lock().unwrap() = vec!["claude-s1".into()];
        drop(controller.open(&ws).await.unwrap());
        tokio::time::sleep(ms(900)).await;
        assert_eq!(
            backend.stops.load(Ordering::SeqCst),
            0,
            "the agent is still working"
        );

        backend.busy.lock().unwrap().clear();
        tokio::time::sleep(ms(600)).await;
        assert_eq!(backend.stops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn stops_a_busy_runner_after_the_limit() {
        let ms = Duration::from_millis;
        let backend = backend();
        let pool = crate::store::tests::pool().await;
        let controller =
            controller_on(pool, backend.clone(), ms(100)).with_activity_wait(ms(50), ms(300));
        let ws = workspace(&controller, "api", "alice").await;
        *backend.busy.lock().unwrap() = vec!["codex-s1".into()];
        drop(controller.open(&ws).await.unwrap());
        tokio::time::sleep(ms(900)).await;
        assert_eq!(backend.stops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn reconcile_adopts_stops_orphans_and_clears() {
        let ms = Duration::from_millis;
        let backend = backend();
        let pool = crate::store::tests::pool().await;
        let before = controller_on(pool.clone(), backend.clone(), Duration::from_secs(60));
        let alice = Owner::User("alice".into());
        before
            .secrets()
            .set_example(&alice, "anthropic_api_key", "alice-key")
            .await;
        let ws = workspace(&before, "api", "alice").await;
        let other = workspace(&before, "other", "alice").await;
        let session = before.open(&ws).await.unwrap();
        // ゲートウェイの再起動: 前の controller は消え、ランナーは動き続ける。
        std::mem::forget(session);
        drop(before);
        backend.running.lock().unwrap().insert("ws-999".into(), 0);
        Store::new(pool.clone())
            .record_runner(other.id, "local")
            .await
            .unwrap();

        let after = controller_on(pool.clone(), backend.clone(), ms(200));
        let report = after.reconcile().await.unwrap();
        assert_eq!(report.adopted, [ws.key()]);
        assert_eq!(report.stopped, ["ws-999"]);
        assert!(report.cleared.is_empty());
        assert_eq!(after.store().recorded_runners().await.unwrap(), [ws.id]);
        // 引き取ったランナーの問い合わせに、新しい controller が答える。
        assert!(
            ask(
                &backend.broker_socket(&ws.key()),
                "CHLOE_SECRET_ANTHROPIC_API_KEY"
            )
            .await
            .contains("alice-key")
        );
        assert_eq!(after.running().await.unwrap(), std::slice::from_ref(&ws));
        // 二度目の reconcile では何も変わらない。
        assert_eq!(after.reconcile().await.unwrap(), ReconcileReport::default());

        // controller の知らないうちにランナーが止まった。
        backend.running.lock().unwrap().remove(&ws.key());
        let report = after.reconcile().await.unwrap();
        assert_eq!(report.cleared, [ws.key()]);
        assert!(after.store().recorded_runners().await.unwrap().is_empty());
        assert!(!backend.broker_socket(&ws.key()).exists());
    }

    #[tokio::test]
    async fn reconcile_keeps_a_runner_started_after_listing() {
        let (controller, backend) = controller(Duration::from_secs(60)).await;
        let ws = workspace(&controller, "api", "alice").await;
        let _session = controller.open(&ws).await.unwrap();
        // 一覧を取ったときにはまだ動いていなかった。
        *backend.stale_list.lock().unwrap() = Some(vec![]);
        let report = controller.reconcile().await.unwrap();
        assert!(report.cleared.is_empty(), "{report:?}");
        assert!(backend.broker_socket(&ws.key()).exists());
        assert_eq!(
            controller.store().recorded_runners().await.unwrap(),
            [ws.id]
        );
        // 一覧にあっても、もう止まっていれば引き取らない。
        let other = workspace(&controller, "other", "alice").await;
        *backend.stale_list.lock().unwrap() = Some(vec![other.key()]);
        let report = controller.reconcile().await.unwrap();
        assert!(report.adopted.is_empty(), "{report:?}");
    }

    #[tokio::test]
    async fn adopted_runner_stops_when_idle() {
        let ms = Duration::from_millis;
        let backend = backend();
        let pool = crate::store::tests::pool().await;
        let before = controller_on(pool.clone(), backend.clone(), Duration::from_secs(60));
        let ws = workspace(&before, "api", "alice").await;
        std::mem::forget(before.open(&ws).await.unwrap());
        drop(before);

        let after = controller_on(pool, backend.clone(), ms(200));
        after.reconcile().await.unwrap();
        tokio::time::sleep(ms(700)).await;
        assert_eq!(backend.stops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn starts_once_and_stops_after_idle() {
        let ms = Duration::from_millis;
        let (controller, backend) = controller(ms(300)).await;
        let ws = workspace(&controller, "api", "alice").await;
        let first = controller.open(&ws).await.unwrap();
        let second = controller.open(&ws).await.unwrap();
        assert_eq!(backend.starts.load(Ordering::SeqCst), 1);
        drop(first);
        tokio::time::sleep(ms(600)).await;
        assert_eq!(
            backend.stops.load(Ordering::SeqCst),
            0,
            "a session is still open"
        );

        drop(second);
        tokio::time::sleep(ms(100)).await;
        // 止まる前にまた接続すると、停止の予約は無効になる。
        let third = controller.open(&ws).await.unwrap();
        tokio::time::sleep(ms(600)).await;
        assert_eq!(backend.stops.load(Ordering::SeqCst), 0);
        assert_eq!(backend.starts.load(Ordering::SeqCst), 1);

        drop(third);
        tokio::time::sleep(ms(1500)).await;
        assert_eq!(backend.stops.load(Ordering::SeqCst), 1);
        // VM には再起動を最後まで頼んでいる (ファイルシステムを閉じてから止まる)。
        assert_eq!(backend.reboots.load(Ordering::SeqCst), 1);
        assert!(!backend.is_running(&ws.key()).await.unwrap());
        // 止めたら問い合わせ用ソケットも片づく。
        assert!(!backend.broker_socket(&ws.key()).exists());

        let _again = controller.open(&ws).await.unwrap();
        assert_eq!(backend.starts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn team_workspaces_fall_back_to_organization_secrets() {
        let (controller, backend) = controller(Duration::from_secs(60)).await;
        let org = Owner::Org("acme".into());
        let team = Owner::Team {
            org: "acme".into(),
            team: "dev".into(),
        };
        let secrets = controller.secrets();
        secrets
            .set_example(&org, "anthropic_api_key", "org-key")
            .await;
        secrets
            .set_example(&org, "openai_api_key", "org-openai")
            .await;
        secrets
            .set_example(&team, "openai_api_key", "team-openai")
            .await;
        let ws = workspace(&controller, "acme/dev/tools", "alice").await;
        let other_team = workspace(&controller, "acme/ops/tools", "alice").await;
        let _a = controller.open(&ws).await.unwrap();
        let _b = controller.open(&other_team).await.unwrap();
        let socket = backend.broker_socket(&ws.key());
        assert!(
            ask(&socket, "CHLOE_SECRET_ANTHROPIC_API_KEY")
                .await
                .contains("org-key")
        );
        assert!(
            ask(&socket, "CHLOE_SECRET_OPENAI_API_KEY")
                .await
                .contains("team-openai"),
            "the team secret wins"
        );
        let other = backend.broker_socket(&other_team.key());
        assert!(
            ask(&other, "CHLOE_SECRET_OPENAI_API_KEY")
                .await
                .contains("org-openai"),
            "another team does not see this team's secret"
        );
    }

    #[tokio::test]
    async fn organization_policy_reaches_its_runners() {
        let (controller, backend) = controller(Duration::from_secs(60)).await;
        let org = Owner::Org("acme".into());
        let layer = |host: &str| crate::runner::PolicyLayer {
            allow: vec![host.into()],
            ..Default::default()
        };
        let store = controller.store();
        store
            .set_policy(&org, &layer("org.example"), "alice")
            .await
            .unwrap();
        let team_ws = workspace(&controller, "acme/dev/tools", "alice").await;
        let personal = workspace(&controller, "mine", "alice").await;
        let _a = controller.open(&team_ws).await.unwrap();
        let _b = controller.open(&personal).await.unwrap();
        let allow = |ws: &Workspace| backend.allow.lock().unwrap()[&ws.key()].clone();
        assert!(allow(&team_ws).contains(&"org.example".to_string()));
        assert!(!allow(&personal).contains(&"org.example".to_string()));

        // 変えた設定は、動いているランナーを作り直すと入る。
        store
            .set_policy(&org, &layer("org2.example"), "alice")
            .await
            .unwrap();
        assert!(controller.refresh(&team_ws).await.unwrap());
        assert!(allow(&team_ws).contains(&"org2.example".to_string()));
        assert!(!allow(&team_ws).contains(&"org.example".to_string()));
    }

    #[tokio::test]
    async fn broker_answers_only_the_workspace_secrets() {
        let (controller, backend) = controller(Duration::from_secs(60)).await;
        let alice = Owner::User("alice".into());
        let bob = Owner::User("bob".into());
        controller
            .secrets()
            .set_example(&alice, "anthropic_api_key", "alice-key")
            .await;
        controller
            .secrets()
            .set_example(&bob, "anthropic_api_key", "bob-key")
            .await;
        let ws_a = controller
            .store()
            .create(
                &WorkspaceRef::parse("w", "alice").unwrap(),
                RootMode::Ephemeral,
                "alice",
            )
            .await
            .unwrap();
        let ws_b = controller
            .store()
            .create(
                &WorkspaceRef::parse("w", "bob").unwrap(),
                RootMode::Ephemeral,
                "bob",
            )
            .await
            .unwrap();
        let _a = controller.open(&ws_a).await.unwrap();
        let _b = controller.open(&ws_b).await.unwrap();
        // alice は anthropic と github (動的) の規則、bob は anthropic の規則だけを持つ。
        assert_eq!(backend.running.lock().unwrap()[&ws_a.key()], 3);
        assert_eq!(backend.running.lock().unwrap()[&ws_b.key()], 1);

        let socket_a = backend.broker_socket(&ws_a.key());
        let socket_b = backend.broker_socket(&ws_b.key());
        assert!(
            ask(&socket_a, "CHLOE_SECRET_ANTHROPIC_API_KEY")
                .await
                .contains("alice-key")
        );
        assert!(
            ask(&socket_b, "CHLOE_SECRET_ANTHROPIC_API_KEY")
                .await
                .contains("bob-key")
        );
        assert!(
            ask(&socket_a, "CHLOE_SECRET_GITHUB_TOKEN")
                .await
                .contains("ghu_fresh")
        );
        assert!(
            ask(&socket_b, "CHLOE_SECRET_GITHUB_TOKEN")
                .await
                .contains("error")
        );
        assert!(ask(&socket_a, "SOMETHING_ELSE").await.contains("error"));

        // 値を変えると、次の問い合わせから新しい値になる (送り込みは要らない)。
        controller
            .secrets()
            .set_example(&alice, "anthropic_api_key", "alice-key-2")
            .await;
        assert!(
            ask(&socket_a, "CHLOE_SECRET_ANTHROPIC_API_KEY")
                .await
                .contains("alice-key-2")
        );
        controller
            .secrets()
            .delete(&alice, "anthropic_api_key")
            .await
            .unwrap();
        assert!(
            ask(&socket_a, "CHLOE_SECRET_ANTHROPIC_API_KEY")
                .await
                .contains("error")
        );

        // 種類が変わったときは方針を作り直して送る。
        assert!(controller.refresh(&ws_a).await.unwrap());
        assert_eq!(backend.running.lock().unwrap()[&ws_a.key()], 2);
        assert_eq!(backend.updates.load(Ordering::SeqCst), 1);
        // VM の環境変数も、使える種類のプレースホルダーだけに入れ替える。
        let env = backend.env.lock().unwrap().clone().unwrap();
        let names: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        assert!(names.contains(&"GH_TOKEN"), "{names:?}");
        assert!(!names.contains(&"ANTHROPIC_API_KEY"), "{names:?}");
    }

    #[tokio::test]
    async fn serves_registered_definitions_by_field() {
        let backend = backend();
        let pool = crate::store::tests::pool().await;
        let controller = controller_on(pool.clone(), backend.clone(), Duration::from_secs(60));
        let alice = Owner::User("alice".into());
        let register = |name: &'static str, toml_text: &'static str| {
            let controller = controller.clone();
            let alice = alice.clone();
            async move {
                let input: catalog::SecretInput = toml::from_str(toml_text).unwrap();
                let (spec, values) = input.into_parts().unwrap();
                let spec = spec.unwrap();
                spec.validate().unwrap();
                let value = spec.encode_values(&values).unwrap();
                controller
                    .secrets()
                    .set_with_spec(
                        &alice,
                        name,
                        &value.into(),
                        Some(&serde_json::to_string(&spec).unwrap()),
                    )
                    .await
                    .unwrap();
            }
        };
        register(
            "aws",
            r#"
            [fields.access_key_id]
            env = ["AWS_ACCESS_KEY_ID"]
            placeholder = "AKIA{id}"
            value = "AKIDEXAMPLE"
            [fields.secret_access_key]
            env = ["AWS_SECRET_ACCESS_KEY"]
            value = "aws-secret"
            [[sign]]
            scheme = "aws-sigv4"
            hosts = ["*.amazonaws.com"]
            "#,
        )
        .await;
        register(
            "hf_token",
            r#"
            env = ["HF_TOKEN"]
            placeholder = "hf_{id}"
            value = "hf-real"
            [[inject]]
            hosts = ["huggingface.co"]
            header = "authorization"
            "#,
        )
        .await;
        // 定義がなく組み込みの種類でもない行は使わない。
        controller
            .secrets()
            .set(&alice, "github_user_token", &"{}".to_string().into())
            .await
            .unwrap();

        let ws = workspace(&controller, "w", "alice").await;
        let _c = controller.open(&ws).await.unwrap();
        // github (動的) の二つ、aws の署名、hf_token の差し替え。
        assert_eq!(backend.running.lock().unwrap()[&ws.key()], 4);
        let allow = backend.allow.lock().unwrap()[&ws.key()].clone();
        assert!(allow.contains(&"*.amazonaws.com".to_string()), "{allow:?}");
        assert!(allow.contains(&"huggingface.co".to_string()), "{allow:?}");

        let socket = backend.broker_socket(&ws.key());
        assert!(
            ask(&socket, "CHLOE_SECRET_AWS__ACCESS_KEY_ID")
                .await
                .contains("AKIDEXAMPLE")
        );
        assert!(
            ask(&socket, "CHLOE_SECRET_AWS__SECRET_ACCESS_KEY")
                .await
                .contains("aws-secret")
        );
        assert!(ask(&socket, "CHLOE_SECRET_AWS").await.contains("error"));
        assert!(
            ask(&socket, "CHLOE_SECRET_HF_TOKEN")
                .await
                .contains("hf-real")
        );
        assert!(
            ask(&socket, "CHLOE_SECRET_GITHUB_USER_TOKEN")
                .await
                .contains("error")
        );

        // 定義だけを書き換えた行は、方針にも問い合わせにも使わない。
        sqlx::query(
            "UPDATE secrets SET spec = replace(spec, 'huggingface.co', 'evil.example') WHERE name = 'hf_token'",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert!(controller.refresh(&ws).await.unwrap());
        assert_eq!(backend.running.lock().unwrap()[&ws.key()], 3);
        let allow = backend.allow.lock().unwrap()[&ws.key()].clone();
        assert!(!allow.contains(&"evil.example".to_string()), "{allow:?}");
        let env = backend.env.lock().unwrap().clone().unwrap();
        let env: HashMap<_, _> = env.into_iter().collect();
        assert!(env["AWS_ACCESS_KEY_ID"].starts_with("AKIAchloe"));
        assert!(env["AWS_SECRET_ACCESS_KEY"].starts_with("chloe"));
        assert!(!env.contains_key("HF_TOKEN"));
        assert!(
            ask(&socket, "CHLOE_SECRET_HF_TOKEN")
                .await
                .contains("error")
        );
    }

    #[tokio::test]
    async fn built_in_definitions_can_be_changed() {
        let backend = backend();
        let pool = crate::store::tests::pool().await;
        // GitHub Enterprise Server に向ける。
        let ghes: catalog::DefinitionInput = toml::from_str(
            r#"
            env = ["GH_TOKEN"]
            placeholder = "ghs_{id}"
            [[inject]]
            hosts = ["ghe.example.com"]
            header = "authorization"
            format = "basic-password"
            "#,
        )
        .unwrap();
        let controller = controller_on(pool.clone(), backend.clone(), Duration::from_secs(60))
            .with_preset("github_token", ghes.into_spec().unwrap())
            .unwrap();
        let ws = workspace(&controller, "w", "alice").await;
        let _c = controller.open(&ws).await.unwrap();
        // alice には FakeGitHub がトークンを作る。
        assert_eq!(backend.running.lock().unwrap()[&ws.key()], 1);
        let allow = backend.allow.lock().unwrap()[&ws.key()].clone();
        assert_eq!(allow, ["ghe.example.com"]);
        let socket = backend.broker_socket(&ws.key());
        assert!(
            ask(&socket, "CHLOE_SECRET_GITHUB_TOKEN")
                .await
                .contains("ghu_fresh")
        );

        // 値を chloe が作るので、欄がいくつもある定義や、組み込みでない名前は変えられない。
        let aws = catalog::example("aws", "v")
            .into_parts()
            .unwrap()
            .0
            .unwrap();
        let other = controller_on(pool.clone(), backend.clone(), Duration::from_secs(60));
        assert!(other.with_preset("github_token", aws.clone()).is_err());
        let other = controller_on(pool, backend, Duration::from_secs(60));
        assert!(other.with_preset("aws", aws).is_err());
    }
}
