//! ワークスペースへの接続とランナーの寿命。
//!
//! 接続を開くとランナーを (止まっていれば) 起動し、最後の接続が閉じてから `idle_timeout` が
//! 経つとランナーを止める。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chloe_agent_proto::{
    AGENT_PORT, AgentMessage, ClientMessage, Connection, ExecRequest, PROTOCOL_VERSION, firecracker,
};
use chloe_secrets::{Owner, SecretDb};
use tracing::{info, warn};

use crate::Error;
use crate::catalog;
use crate::runner::{BasePolicy, RunnerBackend, build_spec};
use crate::store::Store;
use crate::workspace::Workspace;

/// VM を止めるときに、VM の中で再起動を頼んでから待つ時間の既定値。
const GRACEFUL_STOP_TIMEOUT: Duration = Duration::from_secs(30);

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
    secrets: SecretDb,
    backend: B,
    base: BasePolicy,
    idle_timeout: Duration,
    stop_timeout: Duration,
    runners: Mutex<HashMap<i64, Arc<RunnerState>>>,
}

#[derive(Default)]
struct RunnerState {
    /// 起動と停止を一つずつ進めるための錠。
    lifecycle: tokio::sync::Mutex<Option<PathBuf>>,
    sessions: Mutex<SessionCount>,
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
                secrets,
                backend,
                base,
                idle_timeout,
                stop_timeout: GRACEFUL_STOP_TIMEOUT,
                runners: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// VM の中で再起動を頼んでから、ランナーが止まるのを待つ時間を変える。
    pub fn with_stop_timeout(self, stop_timeout: Duration) -> Self {
        let mut inner = Arc::try_unwrap(self.inner)
            .unwrap_or_else(|_| panic!("with_stop_timeout must be called before sharing"));
        inner.stop_timeout = stop_timeout;
        Self {
            inner: Arc::new(inner),
        }
    }

    pub fn store(&self) -> &Store {
        &self.inner.store
    }

    pub fn secrets(&self) -> &SecretDb {
        &self.inner.secrets
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
    ///
    /// `extra` は、データベースのシークレットに加えて (同じ名前なら置き換えて) ランナーに渡す
    /// シークレット。GitHub のトークンのように、接続のたびに発行し直すものに使う。
    /// ランナーがすでに動いていれば使わない。
    pub async fn open(
        &self,
        workspace: &Workspace,
        extra: Vec<(String, chloe_secrets::SecretString)>,
    ) -> Result<Session<B>, Error> {
        let state = self.state(workspace.id);
        {
            let mut sessions = state.sessions.lock().unwrap();
            sessions.open += 1;
        }
        let result = self.ensure_running(workspace, &state, extra).await;
        match result {
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
        extra: Vec<(String, chloe_secrets::SecretString)>,
    ) -> Result<PathBuf, Error> {
        let mut running = state.lifecycle.lock().await;
        let key = workspace.key();
        if let Some(vsock) = running.as_ref()
            && self.inner.backend.is_running(&key).await?
        {
            return Ok(vsock.clone());
        }
        let placeholders = self.inner.store.placeholders(workspace.id).await?;
        let mut secrets = self.owner_secrets(&workspace.owner).await?;
        for (name, value) in extra {
            secrets.retain(|(existing, _)| *existing != name);
            secrets.push((name, value));
        }
        let spec = build_spec(workspace, &self.inner.base, &placeholders, secrets);
        info!(workspace = %workspace.display_name(), key, "starting runner");
        let vsock = self.inner.backend.start(spec).await?;
        *running = Some(vsock.clone());
        Ok(vsock)
    }

    async fn owner_secrets(
        &self,
        owner: &Owner,
    ) -> Result<Vec<(String, chloe_secrets::SecretString)>, Error> {
        let mut secrets = Vec::new();
        for name in self.inner.secrets.list(owner).await? {
            if catalog::kind(&name).is_none() {
                continue;
            }
            if let Some(value) = self.inner.secrets.get(owner, &name).await? {
                secrets.push((name, value));
            }
        }
        Ok(secrets)
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
        let Some(vsock) = running.take() else {
            return Ok(());
        };
        let key = format!("ws-{workspace_id}");
        info!(key, "stopping runner");
        // VM の中で再起動を頼むと、ファイルシステムを閉じてから Firecracker が終わる。
        if let Err(error) = request_reboot(&vsock, self.inner.stop_timeout).await {
            warn!(key, "failed to ask the VM to stop: {error}");
        }
        let deadline = tokio::time::Instant::now() + self.inner.stop_timeout;
        while tokio::time::Instant::now() < deadline && self.inner.backend.is_running(&key).await? {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        self.inner.backend.stop(&key).await
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
    use crate::runner::RunnerSpec;
    use crate::workspace::{RootMode, WorkspaceRef};
    use chloe_secrets::{ExposeSecret, MasterKey};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct FakeBackend {
        starts: AtomicUsize,
        stops: AtomicUsize,
        running: Mutex<HashMap<String, Vec<(String, String)>>>,
    }

    impl RunnerBackend for Arc<FakeBackend> {
        async fn start(&self, spec: RunnerSpec) -> Result<PathBuf, Error> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            let secrets = spec
                .secrets
                .iter()
                .map(|(k, v)| (k.clone(), v.expose_secret().to_string()))
                .collect();
            self.running
                .lock()
                .unwrap()
                .insert(spec.key.clone(), secrets);
            Ok(PathBuf::from(format!("/nonexistent/{}.sock", spec.key)))
        }

        async fn stop(&self, key: &str) -> Result<(), Error> {
            self.stops.fetch_add(1, Ordering::SeqCst);
            self.running.lock().unwrap().remove(key);
            Ok(())
        }

        async fn is_running(&self, key: &str) -> Result<bool, Error> {
            Ok(self.running.lock().unwrap().contains_key(key))
        }
    }

    async fn controller(idle: Duration) -> (Controller<Arc<FakeBackend>>, Arc<FakeBackend>) {
        // 偽の実装は VM を持たないので、再起動を頼めない。待つ時間を短くしておく。
        let pool = crate::store::tests::pool().await;
        let secrets = SecretDb::new(pool.clone(), MasterKey::from_bytes("t", &[3; 32]).unwrap());
        let backend = Arc::new(FakeBackend::default());
        let controller = Controller::new(
            Store::new(pool),
            secrets,
            backend.clone(),
            BasePolicy::default(),
            idle,
        )
        .with_stop_timeout(Duration::from_millis(50));
        (controller, backend)
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
        let first = controller.open(&ws, vec![]).await.unwrap();
        let second = controller.open(&ws, vec![]).await.unwrap();
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
        let third = controller.open(&ws, vec![]).await.unwrap();
        tokio::time::sleep(ms(600)).await;
        assert_eq!(backend.stops.load(Ordering::SeqCst), 0);
        assert_eq!(backend.starts.load(Ordering::SeqCst), 1);

        drop(third);
        tokio::time::sleep(ms(1500)).await;
        assert_eq!(backend.stops.load(Ordering::SeqCst), 1);
        assert!(!backend.is_running(&ws.key()).await.unwrap());

        let _again = controller.open(&ws, vec![]).await.unwrap();
        assert_eq!(backend.starts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn passes_only_the_owners_secrets() {
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
        let _a = controller.open(&ws_a, vec![]).await.unwrap();
        let _b = controller
            .open(
                &ws_b,
                vec![("github_token".into(), "ghs_installation".to_string().into())],
            )
            .await
            .unwrap();
        let running = backend.running.lock().unwrap();
        assert_eq!(
            running[&ws_a.key()],
            [(
                "CHLOE_SECRET_ANTHROPIC_API_KEY".to_string(),
                "alice-key".to_string()
            )]
        );
        assert_eq!(running[&ws_b.key()][0].1, "bob-key");
        assert_eq!(
            running[&ws_b.key()][1],
            (
                "CHLOE_SECRET_GITHUB_TOKEN".to_string(),
                "ghs_installation".to_string()
            )
        );
    }
}
