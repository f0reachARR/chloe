//! ランナーのプロキシからの本物の値の問い合わせに答える。
//!
//! ランナーごとに専用の Unix ソケット (権限 600) で待ち受ける。どのランナーからの問い合わせかは
//! ソケットで決まるので、そのワークスペースで使える種類のシークレットだけを答える。
//! 答えるたびに、ワークスペースとシークレットの種類を記録する (値は記録しない)。
//!
//! やり取りは接続ごとに JSON を一行ずつ。段階 9 で HTTPS (相互 TLS) に置き換えやすい形にしてある。
//!
//! ```json
//! {"secret": "CHLOE_SECRET_GITHUB_TOKEN"}
//! {"value": "..."}  または  {"error": "..."}
//! ```

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use chloe_secrets::ExposeSecret;
use secrecy::zeroize::Zeroize as _;
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::Error;
use crate::catalog;
use crate::controller::Resolver;
use crate::workspace::Workspace;

#[derive(Deserialize)]
struct Question {
    secret: String,
}

/// `socket` で待ち受けを始め、問い合わせに答えるタスクを返す。タスクを止めると待ち受けも終わる。
pub(crate) fn serve(
    socket: &Path,
    workspace: Workspace,
    resolver: Resolver,
) -> Result<JoinHandle<()>, Error> {
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket)?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    Ok(tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let workspace = workspace.clone();
            let resolver = resolver.clone();
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut line = String::new();
                if BufReader::new(read).read_line(&mut line).await.is_err() {
                    return;
                }
                let mut answer = answer(&line, &workspace, &resolver).await;
                let _ = write.write_all(answer.as_bytes()).await;
                answer.zeroize();
            });
        }
    }))
}

async fn answer(line: &str, workspace: &Workspace, resolver: &Resolver) -> String {
    let error = |message: &str| format!("{}\n", serde_json::json!({ "error": message }));
    let Ok(question) = serde_json::from_str::<Question>(line) else {
        return error("invalid question");
    };
    // 問い合わせは環境変数の名前 (CHLOE_SECRET_<種類>) で来る。
    let Some(kind) = catalog::KINDS
        .iter()
        .find(|k| catalog::secret_env(k.name) == question.secret)
    else {
        return error("unknown secret");
    };
    match resolver.resolve(workspace, kind.name).await {
        Ok(Some(value)) => {
            info!(workspace = %workspace.display_name(), secret = kind.name, "secret served");
            let mut encoded = serde_json::to_string(value.expose_secret()).unwrap_or_default();
            let answer = format!("{{\"value\":{encoded}}}\n");
            encoded.zeroize();
            answer
        }
        Ok(None) => {
            warn!(workspace = %workspace.display_name(), secret = kind.name, "secret not available");
            error("not available for this workspace")
        }
        Err(e) => {
            warn!(workspace = %workspace.display_name(), secret = kind.name, "failed to resolve a secret: {e}");
            error("failed to resolve the secret")
        }
    }
}
