//! ホストと guest-agent のあいだのプロトコル。
//!
//! 一つの接続で一つのプロセスを扱う。ホストは接続してすぐに [`ClientMessage::Exec`] を送り、
//! 以後は標準入力と端末の大きさを送る。guest-agent はプロセスの出力を送り、
//! プロセスが終わったら [`AgentMessage::Exited`] を送って接続を閉じる。
//!
//! ポート転送では、ホストは最初に [`ClientMessage::Connect`] を送る。guest-agent が VM の中で
//! TCP で接続して [`AgentMessage::Connected`] を返したら、以後はメッセージではなく生のバイト列を
//! やり取りする ([`Connection::into_raw`])。
//!
//! メッセージは 4 バイトの長さ (ビッグエンディアン) のあとに postcard で符号化した本体を置く。

pub mod firecracker;

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
    /// エージェントが作業中かを尋ねる。[`ClientMessage::Exec`] の代わりに接続して最初に送り、
    /// [`AgentMessage::Activity`] を受け取ったら接続を閉じる。
    Activity,
    /// VM の中から TCP で接続する。[`ClientMessage::Exec`] の代わりに接続して最初に送る。
    Connect(ConnectRequest),
    /// ランナーが渡す環境変数 (プレースホルダーなど) を入れ替える。このあとに起動するプロセスと
    /// ログインシェルに入る。[`ClientMessage::Exec`] の代わりに接続して最初に送り、
    /// [`AgentMessage::EnvUpdated`] を受け取ったら接続を閉じる。
    SetEnv(Vec<(String, String)>),
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
    /// 作業中のエージェントの印 (古いものを除く)。空なら作業中のものはない。
    Activity {
        busy: Vec<String>,
    },
    /// [`ClientMessage::Connect`] の接続ができた。このあとは生のバイト列になる。
    Connected,
    /// [`ClientMessage::SetEnv`] の環境変数を入れ替えた。
    EnvUpdated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectRequest {
    pub version: u32,
    /// VM の中から見た接続先。
    pub host: String,
    pub port: u16,
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

    /// メッセージのやり取りをやめて、下の接続と、読んだがまだメッセージにしていないバイト列を返す。
    /// 返したバイト列は、接続から読むより先に扱う。
    pub fn into_raw(self) -> (S, bytes::BytesMut) {
        let parts = self.framed.into_parts();
        (parts.io, parts.read_buf)
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

    #[tokio::test]
    async fn into_raw_keeps_buffered_bytes() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (a, mut b) = tokio::io::duplex(64 * 1024);
        let mut client = Connection::new(a);
        // 応答のすぐあとに生のバイト列が続き、一度に読まれる。
        let mut agent = Connection::new(&mut b);
        agent.send(&AgentMessage::Connected).await.unwrap();
        drop(agent);
        b.write_all(b"raw").await.unwrap();
        drop(b);
        assert_eq!(
            client.recv::<AgentMessage>().await.unwrap(),
            Some(AgentMessage::Connected)
        );
        let (mut io, buffered) = client.into_raw();
        let mut rest = buffered.to_vec();
        io.read_to_end(&mut rest).await.unwrap();
        assert_eq!(rest, b"raw");
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
