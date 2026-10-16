//! 方針と本物の値を、プロキシを動かしたまま入れ替えるための制御用の Unix ソケット。
//!
//! 接続ごとに JSON を一行受け取り、一行で答える。
//!
//! ```json
//! {"policy": {...PolicyConfig...}, "secrets": {"CHLOE_SECRET_GITHUB_TOKEN": "..."}}
//! ```
//!
//! 差し替えの規則の `secret_env` は、まず `secrets` から、なければプロキシの環境変数から引く。
//! ソケットは権限 600 で作り、プロキシと同じ利用者 (ランナーの状態のディレクトリの持ち主) だけが
//! つなげる。

use std::collections::BTreeMap;
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

/// 方針の設定と本物の値から方針を作る。値はまず `secrets` から、なければ環境変数から引く。
pub fn build_policy(config: PolicyConfig, secrets: &BTreeMap<String, String>) -> Result<Policy> {
    Ok(Policy::new(config, |rule| {
        secrets
            .get(&rule.secret_env)
            .cloned()
            .or_else(|| std::env::var(&rule.secret_env).ok())
            .ok_or_else(|| format!("{} is not set", rule.secret_env))
    })?)
}

#[derive(Deserialize)]
struct Update {
    policy: PolicyConfig,
    #[serde(default)]
    secrets: BTreeMap<String, String>,
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
    let rules = update.policy.inject.len();
    let policy = build_policy(update.policy, &update.secrets)?;
    shared.replace(policy);
    Ok(rules)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chloe_policy::{Decision, Request};

    fn evaluate(shared: &SharedPolicy, auth: &str) -> (Decision, String) {
        let mut headers = vec![("authorization".to_string(), auth.as_bytes().to_vec())];
        let decision = shared.current().evaluate(&mut Request {
            host: "api.github.com",
            sni: Some("api.github.com"),
            tls: true,
            method: "GET",
            path_and_query: "/user",
            headers: &mut headers,
            body: None,
        });
        (decision, String::from_utf8(headers[0].1.clone()).unwrap())
    }

    fn update(secret: &str) -> String {
        serde_json::json!({
            "policy": {
                "allow": ["api.github.com"],
                "inject": [{ "name": "gh", "placeholder": "ghu_chloeX", "hosts": ["api.github.com"], "header": "authorization", "secret_env": "CHLOE_SECRET_GITHUB_TOKEN" }]
            },
            "secrets": { "CHLOE_SECRET_GITHUB_TOKEN": secret }
        })
        .to_string()
    }

    #[tokio::test]
    async fn replaces_secrets_over_the_socket() {
        let dir = std::env::temp_dir().join(format!("chloe-control-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("egress.sock");
        let shared =
            SharedPolicy::new(build_policy(PolicyConfig::default(), &BTreeMap::new()).unwrap());
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

        assert!(send(update("ghu_first")).await.contains("\"ok\":true"));
        assert_eq!(evaluate(&shared, "token ghu_chloeX").1, "token ghu_first");
        assert!(send(update("ghu_second")).await.contains("\"ok\":true"));
        assert_eq!(evaluate(&shared, "token ghu_chloeX").1, "token ghu_second");

        // 壊れた更新は拒否し、今の方針を変えない。
        let bad = serde_json::json!({ "policy": { "inject": [{ "name": "x", "placeholder": "p", "hosts": [], "header": "h", "secret_env": "CHLOE_TEST_UNSET_VAR" }] } });
        assert!(send(bad.to_string()).await.contains("error"));
        assert_eq!(evaluate(&shared, "token ghu_chloeX").1, "token ghu_second");
        let mode = std::fs::metadata(&socket).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
