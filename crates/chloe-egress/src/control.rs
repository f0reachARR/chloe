//! 方針と本物の値を、プロキシを動かしたまま入れ替えるための制御用の Unix ソケット。
//!
//! 接続ごとに JSON を一行受け取り、一行で答える。
//!
//! ```json
//! {"policy": {...PolicyConfig...}}
//! ```
//!
//! 本物の値は送らない (差し替えのたびに問い合わせる。[`crate::secrets`])。ソケットは権限 600 で作り、プロキシと同じ利用者 (ランナーの状態のディレクトリの持ち主) だけが
//! つなげる。

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use chloe_policy::{Policy, PolicyConfig};
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tracing::{info, warn};

/// 要求ごとに読む、入れ替えられる方針。
#[derive(Clone)]
pub struct SharedPolicy(Arc<RwLock<Arc<Policy>>>);

impl SharedPolicy {
    pub fn new(policy: Policy) -> Self {
        Self(Arc::new(RwLock::new(Arc::new(policy))))
    }

    pub fn current(&self) -> Arc<Policy> {
        self.0.read().unwrap().clone()
    }

    fn replace(&self, policy: Policy) {
        *self.0.write().unwrap() = Arc::new(policy);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    policy: PolicyConfig,
}

pub async fn serve(path: &Path, shared: SharedPolicy) -> Result<()> {
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path).with_context(|| format!("bind {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    info!(path = %path.display(), "control socket listening");
    loop {
        let (stream, _) = listener.accept().await?;
        let shared = shared.clone();
        tokio::spawn(async move {
            let (read, mut write) = stream.into_split();
            let mut line = String::new();
            let response = match BufReader::new(read).read_line(&mut line).await {
                Ok(_) => match apply(&line, &shared) {
                    Ok(rules) => {
                        info!(rules, "policy updated");
                        serde_json::json!({ "ok": true })
                    }
                    Err(error) => {
                        warn!("rejected a policy update: {error:#}");
                        serde_json::json!({ "error": format!("{error:#}") })
                    }
                },
                Err(error) => serde_json::json!({ "error": error.to_string() }),
            };
            let _ = write.write_all(format!("{response}\n").as_bytes()).await;
        });
    }
}

/// 更新を当てて、差し替えの規則の数を返す。失敗したら今の方針を変えない。
fn apply(line: &str, shared: &SharedPolicy) -> Result<usize> {
    let update: Update = serde_json::from_str(line).context("invalid update")?;
    let rules = update.policy.inject.len() + update.policy.sign.len();
    shared.replace(Policy::new(update.policy));
    Ok(rules)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chloe_policy::{Decision, Request};

    fn injected(shared: &SharedPolicy, auth: &str) -> Vec<String> {
        let headers = vec![("authorization".to_string(), auth.as_bytes().to_vec())];
        let policy = shared.current();
        let (decision, plan) = policy.evaluate(&Request {
            host: "api.github.com",
            sni: Some("api.github.com"),
            tls: true,
            method: "GET",
            path_and_query: "/user",
            headers: &headers,
            body: None,
        });
        match decision {
            Decision::Allowed { .. } => policy.secrets_needed(&plan),
            other => panic!("unexpected {other:?}"),
        }
    }

    fn update(placeholder: &str) -> String {
        serde_json::json!({
            "policy": {
                "rules": [{ "hosts": ["api.github.com"], "action": "allow" }],
                "inject": [{ "name": "gh", "placeholder": placeholder, "hosts": ["api.github.com"], "header": "authorization", "secret_env": "CHLOE_SECRET_GITHUB_TOKEN" }]
            }
        })
        .to_string()
    }

    #[tokio::test]
    async fn replaces_the_policy_over_the_socket() {
        let dir = std::env::temp_dir().join(format!("chloe-control-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("egress.sock");
        let shared = SharedPolicy::new(Policy::new(PolicyConfig {
            rules: vec![chloe_policy::PathRuleConfig {
                name: None,
                hosts: vec!["api.github.com".into()],
                methods: vec![],
                paths: vec![],
                action: chloe_policy::Action::Allow,
            }],
            ..Default::default()
        }));
        tokio::spawn({
            let socket = socket.clone();
            let shared = shared.clone();
            async move { serve(&socket, shared).await }
        });
        for _ in 0..50 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let send = |line: String| {
            let socket = socket.clone();
            async move {
                let mut stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
                stream
                    .write_all(format!("{line}\n").as_bytes())
                    .await
                    .unwrap();
                let mut response = String::new();
                BufReader::new(stream)
                    .read_line(&mut response)
                    .await
                    .unwrap();
                response
            }
        };

        assert!(injected(&shared, "token ghu_chloeA").is_empty());
        assert!(send(update("ghu_chloeA")).await.contains("\"ok\":true"));
        assert_eq!(
            injected(&shared, "token ghu_chloeA"),
            ["CHLOE_SECRET_GITHUB_TOKEN"]
        );

        // 本物の値を含む更新は受け付けない (値は問い合わせで渡す)。
        let with_secret = serde_json::json!({ "policy": {}, "secrets": { "X": "v" } });
        assert!(send(with_secret.to_string()).await.contains("error"));
        assert_eq!(
            injected(&shared, "token ghu_chloeA"),
            ["CHLOE_SECRET_GITHUB_TOKEN"]
        );
        let mode = std::fs::metadata(&socket).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
