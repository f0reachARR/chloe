//! ランナーのプロキシからの本物の値の問い合わせを、コントロールプレーンへ中継する。
//!
//! ランナーごとの Unix ソケット (権限 600) で、一行の JSON の問い合わせを受け、
//! `POST /internal/v1/broker` にランナーの識別子を添えて送り、答えの行をそのまま返す。
//! コントロールプレーンは、このホストのクライアント証明書で、このホストに置いたランナーかを
//! 確かめる。答え (本物の値を含む) はメモリに置くだけで、ログには書かない。

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use anyhow::Result;
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tracing::warn;

use crate::api::NodeState;

#[derive(Serialize)]
struct Relayed<'a> {
    key: &'a str,
    question: &'a str,
}

/// `key` のランナーの問い合わせ用ソケットで待ち受けを始める。すでに待ち受けていれば作り直す。
pub fn start(state: &Arc<NodeState>, key: &str) -> Result<()> {
    let socket = state.backend.broker_socket(key);
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    let task = {
        let state = state.clone();
        let key = key.to_string();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let state = state.clone();
                let key = key.clone();
                tokio::spawn(async move {
                    let (read, mut write) = stream.into_split();
                    let mut line = String::new();
                    if BufReader::new(read).read_line(&mut line).await.is_err() {
                        return;
                    }
                    let answer = ask(&state, &key, line.trim_end()).await;
                    let _ = write.write_all(answer.as_bytes()).await;
                });
            }
        })
    };
    if let Some(old) = state.relays.lock().unwrap().insert(key.to_string(), task) {
        old.abort();
    }
    Ok(())
}

/// `key` のランナーの中継をやめ、ソケットを消す。
pub fn stop(state: &NodeState, key: &str) {
    if let Some(task) = state.relays.lock().unwrap().remove(key) {
        task.abort();
    }
    let _ = std::fs::remove_file(state.backend.broker_socket(key));
}

async fn ask(state: &NodeState, key: &str, question: &str) -> String {
    let result = async {
        let response = state
            .control_plane
            .post(format!("{}/internal/v1/broker", state.control_plane_url))
            .json(&Relayed { key, question })
            .send()
            .await?
            .error_for_status()?;
        response.text().await
    }
    .await;
    match result {
        Ok(answer) if answer.ends_with('\n') => answer,
        Ok(answer) => format!("{answer}\n"),
        Err(error) => {
            warn!(key, "failed to relay a secret question: {error}");
            format!(
                "{}\n",
                serde_json::json!({ "error": "the control plane is not reachable" })
            )
        }
    }
}
