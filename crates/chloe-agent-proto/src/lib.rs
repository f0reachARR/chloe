//! ホストと guest-agent のあいだのプロトコル。
//!
//! 一つの接続で一つのプロセスを扱う。ホストは接続してすぐに [`ClientMessage::Exec`] を送り、
//! 以後は標準入力と端末の大きさを送る。guest-agent はプロセスの出力を送り、
//! プロセスが終わったら [`AgentMessage::Exited`] を送って接続を閉じる。
//!
//! メッセージは 4 バイトの長さ (ビッグエンディアン) のあとに postcard で符号化した本体を置く。

use bytes::Bytes;
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

/// guest-agent が待ち受ける vsock のポート。
pub const AGENT_PORT: u32 = 7000;

/// プロトコルの版。互換性のない変更をしたら上げる。
pub const PROTOCOL_VERSION: u32 = 1;

/// 1 メッセージの上限。標準入出力はこれより小さく分けて送る。
pub const MAX_FRAME_LEN: usize = 1 << 20;

/// ホストから guest-agent へのメッセージ。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientMessage {
    /// 接続して最初に送る。
    Exec(ExecRequest),
    Stdin(Vec<u8>),
    /// 標準入力を閉じる。端末つきのときは何もしない。
    CloseStdin,
    Resize(WindowSize),
    /// プロセスにシグナルを送る。
    Signal(i32),
}

/// guest-agent からホストへのメッセージ。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentMessage {
    Started {
        pid: u32,
    },
    /// 端末つきのときは、端末の出力もここに入る。
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    Exited(ExitStatus),
    /// 要求を処理できなかった。このあと接続を閉じる。
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecRequest {
    pub version: u32,
    /// 実行するコマンド。空なら利用者のログインシェルを起動する。
    pub command: Vec<String>,
    /// 実行する利用者。`None` なら既定の利用者。
    pub user: Option<String>,
    pub env: Vec<(String, String)>,
    pub cwd: Option<String>,
    /// 端末を割り当てるときに指定する。
    pub tty: Option<TtyRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TtyRequest {
    pub term: String,
    pub size: WindowSize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowSize {
    pub rows: u16,
    pub cols: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitStatus {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

impl ExitStatus {
    /// シェルと同じ規則で終了コードに直す (シグナルで終わったら 128 + シグナル番号)。
    pub fn shell_code(&self) -> i32 {
        match (self.code, self.signal) {
            (Some(code), _) => code,
            (None, Some(signal)) => 128 + signal,
            (None, None) => 1,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("invalid message: {0}")]
    Decode(#[from] postcard::Error),
}

/// 長さつきのメッセージを送受信する接続。
pub struct Connection<S> {
    framed: Framed<S, LengthDelimitedCodec>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Connection<S> {
    pub fn new(stream: S) -> Self {
        let codec = LengthDelimitedCodec::builder()
            .max_frame_length(MAX_FRAME_LEN)
            .new_codec();
        Self {
            framed: Framed::new(stream, codec),
        }
    }

    pub async fn send<T: Serialize>(&mut self, message: &T) -> Result<(), Error> {
        let bytes = postcard::to_stdvec(message)?;
        self.framed.send(Bytes::from(bytes)).await?;
        Ok(())
    }

    /// 次のメッセージを受け取る。相手が接続を閉じたら `None` を返す。
    pub async fn recv<T: DeserializeOwned>(&mut self) -> Result<Option<T>, Error> {
        decode(self.framed.next().await)
    }

    /// 送信側と受信側に分ける。送信と受信を同時に待つときに使う。
    pub fn split(self) -> (Sender<S>, Receiver<S>) {
        let (sink, stream) = self.framed.split();
        (Sender { sink }, Receiver { stream })
    }
}

type Frames<S> = Framed<S, LengthDelimitedCodec>;

/// [`Connection::split`] で分けた送信側。
pub struct Sender<S> {
    sink: SplitSink<Frames<S>, Bytes>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Sender<S> {
    pub async fn send<T: Serialize>(&mut self, message: &T) -> Result<(), Error> {
        let bytes = postcard::to_stdvec(message)?;
        self.sink.send(Bytes::from(bytes)).await?;
        Ok(())
    }
}

/// [`Connection::split`] で分けた受信側。
pub struct Receiver<S> {
    stream: SplitStream<Frames<S>>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Receiver<S> {
    /// 次のメッセージを受け取る。相手が接続を閉じたら `None` を返す。
    /// 取り消しても途中のメッセージは失われない ([`tokio::select!`] で使える)。
    pub async fn recv<T: DeserializeOwned>(&mut self) -> Result<Option<T>, Error> {
        decode(self.stream.next().await)
    }
}

fn decode<T: DeserializeOwned>(
    frame: Option<std::io::Result<bytes::BytesMut>>,
) -> Result<Option<T>, Error> {
    match frame {
        Some(frame) => Ok(Some(postcard::from_bytes(&frame?)?)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn roundtrip() {
        let (a, b) = tokio::io::duplex(64 * 1024);
        let mut client = Connection::new(a);
        let mut agent = Connection::new(b);

        let exec = ClientMessage::Exec(ExecRequest {
            version: PROTOCOL_VERSION,
            command: vec!["echo".into(), "hi".into()],
            user: None,
            env: vec![("A".into(), "1".into())],
            cwd: Some("/workspace".into()),
            tty: Some(TtyRequest {
                term: "xterm".into(),
                size: WindowSize { rows: 24, cols: 80 },
            }),
        });
        client.send(&exec).await.unwrap();
        assert_eq!(agent.recv::<ClientMessage>().await.unwrap(), Some(exec));

        let out = AgentMessage::Stdout(vec![0, 1, 2, 255]);
        agent.send(&out).await.unwrap();
        assert_eq!(client.recv::<AgentMessage>().await.unwrap(), Some(out));

        drop(agent);
        assert_eq!(client.recv::<AgentMessage>().await.unwrap(), None);
    }

    #[test]
    fn shell_code() {
        let by_signal = ExitStatus {
            code: None,
            signal: Some(9),
        };
        assert_eq!(by_signal.shell_code(), 137);
    }
}
