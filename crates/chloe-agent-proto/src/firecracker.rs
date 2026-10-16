//! Firecracker の vsock にホスト側からつなぐ。
//!
//! Firecracker はホスト側に Unix ソケットを置く。そこに接続して `CONNECT <port>\n` を書くと、
//! ゲストのそのポートにつながり、`OK <ホスト側のポート>\n` が返る。

use std::path::Path;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

const RETRY_INTERVAL: Duration = Duration::from_millis(200);

/// ゲストの `port` につなぐ。`wait` のあいだは、つながるまで繰り返す。
pub async fn connect(socket: &Path, port: u32, wait: Duration) -> std::io::Result<UnixStream> {
    let deadline = Instant::now() + wait;
    loop {
        match try_connect(socket, port).await {
            Ok(stream) => return Ok(stream),
            Err(error) if Instant::now() >= deadline => {
                return Err(std::io::Error::new(
                    error.kind(),
                    format!(
                        "failed to connect to port {port} via {}: {error}",
                        socket.display()
                    ),
                ));
            }
            Err(_) => tokio::time::sleep(RETRY_INTERVAL).await,
        }
    }
}

async fn try_connect(socket: &Path, port: u32) -> std::io::Result<UnixStream> {
    let mut stream = UnixStream::connect(socket).await?;
    stream
        .write_all(format!("CONNECT {port}\n").as_bytes())
        .await?;
    // 応答の行のあとはゲストのデータになるので、1 バイトずつ読む。
    let mut line = Vec::new();
    loop {
        let byte = stream.read_u8().await?;
        if byte == b'\n' {
            break;
        }
        line.push(byte);
        if line.len() > 64 {
            return Err(std::io::Error::other("unexpected vsock handshake response"));
        }
    }
    if !line.starts_with(b"OK ") {
        return Err(std::io::Error::other(format!(
            "vsock handshake failed: {}",
            String::from_utf8_lossy(&line)
        )));
    }
    Ok(stream)
}
