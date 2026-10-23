//! ランナーのプロキシからの本物の値の問い合わせに答える。
//!
//! ランナーごとに専用の Unix ソケット (権限 600) で待ち受ける。どのランナーからの問い合わせかは
//! ソケットで決まるので、そのワークスペースで使えるシークレットだけを答える。
//! 答えるたびに、ワークスペースとシークレットの名前と欄を記録する (値は記録しない)。
//!
//! VM の中のエージェントからの依頼 ([`crate::asks`]) も同じソケットで受ける。依頼は、出す、
//! 状態を見る、取り下げるの短い問い合わせで、答えを待つのは VM の中の側が状態を見直して行う。
//! 見たり取り下げたりできるのは、そのワークスペースの依頼だけである。
//!
//! やり取りは接続ごとに JSON を一行ずつ。段階 9 で HTTPS (相互 TLS) に置き換えやすい形にしてある。
//!
//! ```json
//! {"secret": "CHLOE_SECRET_GITHUB_TOKEN"}
//! {"value": "..."}  または  {"error": "..."}
//!
//! {"ask": {"create": {"kind": "network", "host": "example.com", "reason": "..."}}}
//! {"ask": {"get": 12}}
//! {"ask": {"cancel": 12}}
//! {"ask": {"id": 12, "kind": "network", "status": "pending", ...}}
//!     または  {"error": "...", "code": "invalid" | "not_found"}
//! ```
//!
//! 公開したポート ([`crate::ports`]) も同じソケットで受ける。扱えるのは、そのワークスペースの
//! ポートだけである。
//!
//! ```json
//! {"port": {"publish": {"port": 5173, "scheme": "http"}}}
//! {"port": {"port": 5173, "scheme": "http", "url": "https://5173-ws-12.preview.example.com/", ...}}
//!
//! {"port": "list"}
//! {"ports": [{"port": 5173, ...}]}
//!
//! {"port": {"unpublish": 5173}}
//! {"unpublished": true}
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
use crate::asks::{Asks, NewAsk, Resolution};
use crate::catalog;
use crate::controller::Resolver;
use crate::ports::{NewPort, Ports};
use crate::workspace::Workspace;

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Question {
    Secret(String),
    Ask(AskOp),
    Port(PortOp),
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum AskOp {
    Create(NewAsk),
    Get(i64),
    Cancel(i64),
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum PortOp {
    Publish(NewPort),
    List,
    Unpublish(u16),
}

/// `socket` で待ち受けを始め、問い合わせに答えるタスクを返す。タスクを止めると待ち受けも終わる。
pub(crate) fn serve(
    socket: &Path,
    workspace: Workspace,
    resolver: Resolver,
    asks: Asks,
    ports: Ports,
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
            let asks = asks.clone();
            let ports = ports.clone();
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut line = String::new();
                if BufReader::new(read).read_line(&mut line).await.is_err() {
                    return;
                }
                let mut answer = answer(&line, &workspace, &resolver, &asks, &ports).await;
                let _ = write.write_all(answer.as_bytes()).await;
                answer.zeroize();
            });
        }
    }))
}

pub(crate) async fn answer(
    line: &str,
    workspace: &Workspace,
    resolver: &Resolver,
    asks: &Asks,
    ports: &Ports,
) -> String {
    match serde_json::from_str::<Question>(line) {
        Ok(Question::Secret(secret)) => answer_secret(&secret, workspace, resolver).await,
        Ok(Question::Ask(op)) => answer_ask(op, workspace, asks).await,
        Ok(Question::Port(op)) => answer_port(op, workspace, ports).await,
        Err(_) => error_line("invalid question"),
    }
}

fn error_line(message: &str) -> String {
    format!("{}\n", serde_json::json!({ "error": message }))
}

/// 依頼の問い合わせに答える。ほかのワークスペースの依頼は、ないものとして扱う。
async fn answer_ask(op: AskOp, workspace: &Workspace, asks: &Asks) -> String {
    let refuse = |code: &str, message: &str| {
        format!(
            "{}\n",
            serde_json::json!({ "error": message, "code": code })
        )
    };
    let own = |id: i64| async move {
        match asks.get(id).await {
            Ok(Some(ask)) if ask.workspace_id == workspace.id => Ok(ask),
            Ok(_) => Err(refuse("not_found", "no such ask")),
            Err(error) => Err(refuse("internal", &error.to_string())),
        }
    };
    let result = match op {
        AskOp::Create(new) => asks
            .create(workspace, new)
            .await
            .map_err(|error| match error {
                Error::Invalid(message) => refuse("invalid", &message),
                error => refuse("internal", &error.to_string()),
            }),
        AskOp::Get(id) => own(id).await,
        AskOp::Cancel(id) => match own(id).await {
            Ok(_) => asks
                .resolve(id, Resolution::Cancel, "agent")
                .await
                .map_err(|error| refuse("invalid", &error.to_string())),
            Err(refused) => Err(refused),
        },
    };
    match result {
        Ok(ask) => format!("{}\n", serde_json::json!({ "ask": ask })),
        Err(refused) => refused,
    }
}

/// 公開したポートの問い合わせに答える。
async fn answer_port(op: PortOp, workspace: &Workspace, ports: &Ports) -> String {
    let answer = match op {
        PortOp::Publish(new) => ports
            .publish(workspace, new)
            .await
            .map(|port| serde_json::json!({ "port": port })),
        PortOp::List => ports
            .list(workspace.id)
            .await
            .map(|list| serde_json::json!({ "ports": list })),
        PortOp::Unpublish(port) => ports
            .unpublish(workspace, port, "agent")
            .await
            .map(|removed| serde_json::json!({ "unpublished": removed })),
    };
    match answer {
        Ok(answer) => format!("{answer}\n"),
        Err(Error::Invalid(message)) => format!(
            "{}\n",
            serde_json::json!({ "error": message, "code": "invalid" })
        ),
        Err(error) => format!(
            "{}\n",
            serde_json::json!({ "error": error.to_string(), "code": "internal" })
        ),
    }
}

async fn answer_secret(secret: &str, workspace: &Workspace, resolver: &Resolver) -> String {
    let error = error_line;
    // 問い合わせは環境変数の名前 (CHLOE_SECRET_<名前>[__<欄>]) で来る。
    let Some((name, field)) = catalog::parse_secret_ref(secret) else {
        return error("unknown secret");
    };
    match resolver.resolve(workspace, &name).await {
        Ok(Some((stored, spec))) => {
            let Some(mut value) = spec.field_value(stored.expose_secret(), &field) else {
                warn!(target: "audit", workspace = %workspace.display_name(), secret = %name, field = %field, "no such field");
                return error("not available for this workspace");
            };
            info!(target: "audit", workspace = %workspace.display_name(), secret = %name, field = %field, "secret served");
            let mut encoded = serde_json::to_string(&value).unwrap_or_default();
            value.zeroize();
            let answer = format!("{{\"value\":{encoded}}}\n");
            encoded.zeroize();
            answer
        }
        Ok(None) => {
            warn!(target: "audit", workspace = %workspace.display_name(), secret = %name, field = %field, "secret not available");
            error("not available for this workspace")
        }
        Err(e) => {
            warn!(target: "audit", workspace = %workspace.display_name(), secret = %name, field = %field, "failed to resolve a secret: {e}");
            error("failed to resolve the secret")
        }
    }
}
