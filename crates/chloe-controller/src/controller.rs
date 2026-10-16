//! ワークスペースへの接続とランナーの寿命。
//!
//! 接続を開くとランナーを (止まっていれば) 起動し、最後の接続が閉じてから `idle_timeout` が
//! 経つとランナーを止める。ランナーが動いているあいだは、そのランナー専用の問い合わせ用ソケットで
//! 本物の値の問い合わせに答える ([`crate::broker`])。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chloe_agent_proto::{
    AGENT_PORT, AgentMessage, ClientMessage, Connection, ExecRequest, PROTOCOL_VERSION, firecracker,
};
use chloe_secrets::{SecretDb, SecretString};
use futures::future::BoxFuture;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::Error;
use crate::catalog;
use crate::runner::{BasePolicy, RunnerBackend, RunnerSpec, build_spec};
use crate::store::Store;
use crate::workspace::Workspace;

/// VM を止めるときに、VM の中で再起動を頼んでから待つ時間の既定値。
const GRACEFUL_STOP_TIMEOUT: Duration = Duration::from_secs(30);

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
}

impl Resolver {
    /// ワークスペースで使えるシークレットの種類。持ち主がデータベースに登録した種類と、
    /// 動的に作る種類を合わせたもの。
    pub async fn kinds(&self, workspace: &Workspace) -> Result<Vec<String>, Error> {
        let mut kinds: Vec<String> = self
            .secrets
            .list(&workspace.owner)
            .await?
            .into_iter()
            .filter(|name| catalog::kind(name).is_some())
            .collect();
        if let Some(dynamic) = &self.dynamic {
            kinds.extend(dynamic.kinds(workspace).await);
        }
        kinds.sort();
        kinds.dedup();
        Ok(kinds)
    }

    pub async fn resolve(
        &self,
        workspace: &Workspace,
        kind: &str,
    ) -> Result<Option<SecretString>, Error> {
        if catalog::kind(kind).is_none() {
            return Ok(None);
        }
        if let Some(dynamic) = &self.dynamic
            && dynamic.kinds(workspace).await.iter().any(|k| k == kind)
        {
            return dynamic.resolve(workspace, kind).await;
        }
        Ok(self.secrets.get(&workspace.owner, kind).await?)
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
    runners: Mutex<HashMap<i64, Arc<RunnerState>>>,
}

#[derive(Default)]
struct RunnerState {
    /// 起動と停止を一つずつ進めるための錠。動いていれば `Some`。
    lifecycle: tokio::sync::Mutex<Option<Running>>,
    sessions: Mutex<SessionCount>,
}

struct Running {
    vsock: PathBuf,
    /// 本物の値の問い合わせに答えるタスク。
    broker: JoinHandle<()>,
}

#[derive(Default)]
struct SessionCount {
    open: usize,
    /// 最後の接続が閉じるたびに増やし、古い停止の予約を無効にする。
    generation: u64,
}

/// 開いている接続。drop すると接続を閉じたことになる。
pub struct Session<B: RunnerBackend> {
    controller: Controller<B>,
    workspace_id: i64,
    pub vsock: PathBuf,
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
                },
                backend,
                base,
                idle_timeout,
                stop_timeout: GRACEFUL_STOP_TIMEOUT,
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

    /// データベースの外で作るシークレットの出どころを足す。
    pub fn with_dynamic_secrets(self, dynamic: Arc<dyn DynamicSecrets>) -> Self {
        self.configure(|inner| inner.resolver.dynamic = Some(dynamic))
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
            Ok(vsock) => Ok(Session {
                controller: self.clone(),
                workspace_id: workspace.id,
                vsock,
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
    ) -> Result<PathBuf, Error> {
        let mut running = state.lifecycle.lock().await;
        let key = workspace.key();
        if let Some(current) = running.as_ref() {
            if self.inner.backend.is_running(&key).await? {
                return Ok(current.vsock.clone());
            }
            current.broker.abort();
        }
        let spec = self.build(workspace).await?;
        // ランナーのプロキシが問い合わせられるよう、起動する前に待ち受けを始める。
        let broker = crate::broker::serve(
            &self.inner.backend.broker_socket(&key),
            workspace.clone(),
            self.inner.resolver.clone(),
        )?;
        info!(workspace = %workspace.display_name(), key, "starting runner");
        match self.inner.backend.start(spec).await {
            Ok(vsock) => {
                *running = Some(Running {
                    vsock: vsock.clone(),
                    broker,
                });
                Ok(vsock)
            }
            Err(error) => {
                broker.abort();
                Err(error)
            }
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
        self.inner.backend.update(spec).await?;
        info!(workspace = %workspace.display_name(), "runner policy refreshed");
        Ok(true)
    }

    async fn build(&self, workspace: &Workspace) -> Result<RunnerSpec, Error> {
        let placeholders = self.inner.store.placeholders(workspace.id).await?;
        let kinds = self.inner.resolver.kinds(workspace).await?;
        Ok(build_spec(
            workspace,
            &self.inner.base,
            &placeholders,
            &kinds,
        ))
    }

    fn close(&self, workspace_id: i64) {
        let state = self.state(workspace_id);
        let generation = {
            let mut sessions = state.sessions.lock().unwrap();
            sessions.open = sessions.open.saturating_sub(1);
            if sessions.open > 0 {
                return;
            }
            sessions.generation += 1;
            sessions.generation
        };
        let controller = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(controller.inner.idle_timeout).await;
            {
                let sessions = state.sessions.lock().unwrap();
                if sessions.open > 0 || sessions.generation != generation {
                    return;
                }
            }
            if let Err(error) = controller.stop_runner(workspace_id, &state).await {
                warn!(workspace_id, "failed to stop the runner: {error}");
            }
        });
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
        info!(key, "stopping runner");
        // VM の中で再起動を頼むと、ファイルシステムを閉じてから Firecracker が終わる。
        if let Err(error) = request_reboot(&current.vsock, self.inner.stop_timeout).await {
            warn!(key, "failed to ask the VM to stop: {error}");
        }
        let deadline = tokio::time::Instant::now() + self.inner.stop_timeout;
        while tokio::time::Instant::now() < deadline && self.inner.backend.is_running(&key).await? {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let result = self.inner.backend.stop(&key).await;
        current.broker.abort();
        let _ = std::fs::remove_file(self.inner.backend.broker_socket(&key));
        result
    }
}

impl<B: RunnerBackend> Drop for Session<B> {
    fn drop(&mut self) {
        self.controller.close(self.workspace_id);
    }
}

async fn request_reboot(vsock: &std::path::Path, wait: Duration) -> Result<(), Error> {
    let stream = firecracker::connect(vsock, AGENT_PORT, wait.min(Duration::from_secs(2))).await?;
    let mut conn = Connection::new(stream);
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
    // 再起動が始まると接続が切れるので、終了の知らせは待たなくてよい。
    let _ = tokio::time::timeout(Duration::from_secs(5), conn.recv::<AgentMessage>()).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{RootMode, WorkspaceRef};
    use chloe_secrets::{MasterKey, Owner};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    struct FakeBackend {
        dir: tempfile::TempDir,
        starts: AtomicUsize,
        stops: AtomicUsize,
        updates: AtomicUsize,
        /// 動いているランナーの識別子と、その方針の差し替えの規則の数。
        running: Mutex<HashMap<String, usize>>,
    }

    impl RunnerBackend for Arc<FakeBackend> {
        async fn start(&self, spec: RunnerSpec) -> Result<PathBuf, Error> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            self.running
                .lock()
                .unwrap()
                .insert(spec.key.clone(), spec.policy.inject.len());
            Ok(self.dir.path().join(format!("{}.vsock", spec.key)))
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
            self.running
                .lock()
                .unwrap()
                .insert(spec.key.clone(), spec.policy.inject.len());
            Ok(())
        }

        fn broker_socket(&self, key: &str) -> PathBuf {
            self.dir.path().join(format!("{key}.broker.sock"))
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

    async fn controller(idle: Duration) -> (Controller<Arc<FakeBackend>>, Arc<FakeBackend>) {
        let pool = crate::store::tests::pool().await;
        let secrets = SecretDb::new(pool.clone(), MasterKey::from_bytes("t", &[3; 32]).unwrap());
        let backend = Arc::new(FakeBackend {
            dir: tempfile::tempdir().unwrap(),
            starts: AtomicUsize::new(0),
            stops: AtomicUsize::new(0),
            updates: AtomicUsize::new(0),
            running: Mutex::new(HashMap::new()),
        });
        // 偽の実装は VM を持たないので、再起動を頼めない。待つ時間を短くしておく。
        let controller = Controller::new(
            Store::new(pool),
            secrets,
            backend.clone(),
            BasePolicy::default(),
            idle,
        )
        .with_stop_timeout(Duration::from_millis(50))
        .with_dynamic_secrets(Arc::new(FakeGitHub));
        (controller, backend)
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

    #[tokio::test]
    async fn starts_once_and_stops_after_idle() {
        let ms = Duration::from_millis;
        let (controller, backend) = controller(ms(300)).await;
        let ws = controller
            .store()
            .create(
                &WorkspaceRef::parse("api", "alice").unwrap(),
                RootMode::Ephemeral,
                "alice",
            )
            .await
            .unwrap();
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
        assert!(!backend.is_running(&ws.key()).await.unwrap());
        // 止めたら問い合わせ用ソケットも片づく。
        assert!(!backend.broker_socket(&ws.key()).exists());

        let _again = controller.open(&ws).await.unwrap();
        assert_eq!(backend.starts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn broker_answers_only_the_workspace_secrets() {
        let (controller, backend) = controller(Duration::from_secs(60)).await;
        let alice = Owner::User("alice".into());
        let bob = Owner::User("bob".into());
        controller
            .secrets()
            .set(&alice, "anthropic_api_key", &"alice-key".to_string().into())
            .await
            .unwrap();
        controller
            .secrets()
            .set(&bob, "anthropic_api_key", &"bob-key".to_string().into())
            .await
            .unwrap();
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
            .set(
                &alice,
                "anthropic_api_key",
                &"alice-key-2".to_string().into(),
            )
            .await
            .unwrap();
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
    }
}
