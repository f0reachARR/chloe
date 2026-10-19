//! `ask.chloe.internal` との HTTP のやり取り。
//!
//! VM の 80 番への通信はランナーのプロキシに届き、プロキシが問い合わせ用ソケットで
//! コントロールプレーンへ送る。プロキシのほかに相手はいないので、HTTP/1.1 を一つの要求ごとに
//! 接続して閉じる、いちばん簡単な形で話す。

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// 依頼を受けるホスト。
pub const ASK_HOST: &str = "ask.chloe.internal";
/// 応答の上限。依頼の中身は数 KiB に収まる。
const MAX_RESPONSE: usize = 256 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// 答えを待つあいだ、状態を見直す間隔。待つほど長くする。
const POLL_FIRST: Duration = Duration::from_secs(2);
const POLL_MAX: Duration = Duration::from_secs(15);

/// コントロールプレーンが返す依頼。
#[derive(Debug, Clone, Deserialize)]
pub struct Ask {
    pub id: i64,
    pub kind: String,
    pub status: String,
    #[serde(default)]
    pub host: Option<String>,
    /// 質問への答えか、断ったときの言葉。
    #[serde(default)]
    pub answer: Option<String>,
    #[serde(default)]
    pub answered_by: Option<String>,
}

impl Ask {
    pub fn pending(&self) -> bool {
        self.status == "pending"
    }
}

#[derive(Clone)]
pub struct Client {
    /// 接続先 (`host:port`)。確かめるときに環境変数 `CHLOE_ASK_ADDR` で変える。
    addr: String,
    /// 状態を見直す最初の間隔。
    poll: Duration,
}

impl Client {
    /// VM の既定の経路のゲートウェイの 80 番につなぐ。ランナーは VM からの 80 番への接続を
    /// 宛先によらずプロキシに渡し、プロキシは `Host` で `ask.chloe.internal` を見分けるので、
    /// 名前を引かなくてよい。musl の名前の解決は、AAAA に NXDOMAIN が返ると A があっても
    /// 失敗するので、引かずに済ませる。経路が読めなければ名前でつなぐ。
    pub fn from_env() -> Self {
        let addr = std::env::var("CHLOE_ASK_ADDR").unwrap_or_else(|_| {
            let route = std::fs::read_to_string("/proc/net/route").unwrap_or_default();
            match default_gateway(&route) {
                Some(ip) => format!("{ip}:80"),
                None => format!("{ASK_HOST}:80"),
            }
        });
        Self {
            addr,
            poll: POLL_FIRST,
        }
    }

    /// 依頼を出す。`ask` は依頼の JSON (`{"kind": "network", "host": ...}` など)。
    pub async fn create(&self, ask: &serde_json::Value) -> Result<Ask> {
        self.call("POST", "/v1/asks", Some(ask)).await
    }

    pub async fn get(&self, id: i64) -> Result<Ask> {
        self.call("GET", &format!("/v1/asks/{id}"), None).await
    }

    pub async fn cancel(&self, id: i64) -> Result<Ask> {
        self.call("DELETE", &format!("/v1/asks/{id}"), None).await
    }

    /// 答えが来るまで状態を見直す。`limit` を過ぎたら、答えを待つままの依頼を返す。
    /// 状態を見るのに失敗しても (プロキシの入れ替えなど)、続けて試す。
    pub async fn wait(&self, id: i64, limit: Option<Duration>) -> Result<Ask> {
        let started = tokio::time::Instant::now();
        let mut interval = self.poll;
        let mut failures = 0;
        loop {
            match self.get(id).await {
                Ok(ask) if !ask.pending() => return Ok(ask),
                Ok(ask) => {
                    failures = 0;
                    if limit.is_some_and(|limit| started.elapsed() + interval > limit) {
                        return Ok(ask);
                    }
                }
                // 依頼が見つからない (ワークスペースを作り直したなど) なら、待っても来ない。
                Err(error) if error.to_string().contains("404") => return Err(error),
                Err(error) => {
                    failures += 1;
                    if failures >= 20 {
                        return Err(error.context("failed to check the ask"));
                    }
                }
            }
            tokio::time::sleep(interval).await;
            interval = (interval * 2).min(POLL_MAX);
        }
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<Ask> {
        let (status, body) = tokio::time::timeout(REQUEST_TIMEOUT, self.send(method, path, body))
            .await
            .with_context(|| format!("{ASK_HOST} did not answer in time"))??;
        if status != 200 {
            let message = serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("error")?.as_str().map(str::to_string))
                .unwrap_or_else(|| String::from_utf8_lossy(&body).into_owned());
            bail!("{status}: {message}");
        }
        serde_json::from_slice(&body).context("invalid answer from chloe")
    }

    async fn send(
        &self,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<(u16, Vec<u8>)> {
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        let mut stream = TcpStream::connect(&self.addr)
            .await
            .with_context(|| format!("connect to {}", self.addr))?;
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {ASK_HOST}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(request.as_bytes()).await?;
        let mut response = Vec::new();
        (&mut stream)
            .take(MAX_RESPONSE as u64 + 1)
            .read_to_end(&mut response)
            .await?;
        if response.len() > MAX_RESPONSE {
            bail!("the answer from chloe is too large");
        }
        parse_response(&response)
    }
}

/// `/proc/net/route` の既定の経路 (宛先 0.0.0.0) のゲートウェイ。値はリトルエンディアンの 16 進。
fn default_gateway(route: &str) -> Option<std::net::Ipv4Addr> {
    route.lines().skip(1).find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.get(1) != Some(&"00000000") {
            return None;
        }
        let gateway = u32::from_str_radix(fields.get(2)?, 16).ok()?;
        (gateway != 0).then(|| std::net::Ipv4Addr::from(gateway.swap_bytes()))
    })
}

/// HTTP/1.1 の応答からステータスと本文を取り出す。本文は `Content-Length` か、なければ
/// 接続が閉じるまで。チャンクで送られた本文は扱う。
fn parse_response(response: &[u8]) -> Result<(u16, Vec<u8>)> {
    let split = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .context("incomplete answer from chloe")?;
    let head = std::str::from_utf8(&response[..split]).context("invalid answer from chloe")?;
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .context("invalid status line from chloe")?;
    let mut length = None;
    let mut chunked = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            length = value.parse::<usize>().ok();
        } else if name.eq_ignore_ascii_case("transfer-encoding")
            && value.eq_ignore_ascii_case("chunked")
        {
            chunked = true;
        }
    }
    let rest = &response[split + 4..];
    let body = if chunked {
        dechunk(rest)?
    } else {
        match length {
            Some(n) if n <= rest.len() => rest[..n].to_vec(),
            Some(_) => bail!("incomplete answer from chloe"),
            None => rest.to_vec(),
        }
    };
    Ok((status, body))
}

fn dechunk(mut data: &[u8]) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let end = data
            .windows(2)
            .position(|w| w == b"\r\n")
            .context("incomplete chunk from chloe")?;
        let size = std::str::from_utf8(&data[..end])?
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        let size = usize::from_str_radix(size, 16).context("invalid chunk size")?;
        data = &data[end + 2..];
        if size == 0 {
            return Ok(body);
        }
        if data.len() < size + 2 {
            bail!("incomplete chunk from chloe");
        }
        body.extend_from_slice(&data[..size]);
        data = &data[size + 2..];
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::AsyncBufReadExt;
    use tokio::net::TcpListener;

    #[test]
    fn finds_the_default_gateway() {
        let route = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\n\
            eth0\t0000001D\t00000000\t0001\t0\t0\t0\t00FFFFFF\n\
            eth0\t00000000\t01001DAC\t0003\t0\t0\t0\t00000000\n";
        assert_eq!(
            default_gateway(route),
            Some(std::net::Ipv4Addr::new(172, 29, 0, 1))
        );
        assert_eq!(default_gateway("Iface\tDestination\n"), None);
    }

    #[test]
    fn parses_responses() {
        let (status, body) =
            parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}extra").unwrap();
        assert_eq!((status, body.as_slice()), (200, b"{}".as_slice()));
        let (status, body) = parse_response(
            b"HTTP/1.1 404 Not Found\r\nTransfer-Encoding: chunked\r\n\r\n3\r\n{\"a\r\n3\r\n\":1\r\n1\r\n}\r\n0\r\n\r\n",
        )
        .unwrap();
        assert_eq!((status, body.as_slice()), (404, b"{\"a\":1}".as_slice()));
        assert!(parse_response(b"HTTP/1.1 200 OK\r\n").is_err());
        assert!(parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\n{}").is_err());
    }

    /// プロキシの代わりに、依頼の状態を `states` の順に返すサーバー。受けた要求の行を残す。
    pub(crate) async fn fake_server(
        states: Vec<&'static str>,
    ) -> (Client, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            let mut states = states.into_iter();
            let mut last = "pending";
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let mut reader = tokio::io::BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                log.lock().unwrap().push(line.trim_end().to_string());
                // 残りのヘッダーと本文は読み捨てる。
                let mut header = String::new();
                let mut length = 0;
                loop {
                    header.clear();
                    reader.read_line(&mut header).await.unwrap();
                    if header == "\r\n" {
                        break;
                    }
                    if let Some(v) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).await.unwrap();
                let status = if line.starts_with("DELETE") {
                    "cancelled"
                } else {
                    states.next().unwrap_or(last)
                };
                last = status;
                let json = format!(
                    r#"{{"id":5,"kind":"network","status":"{status}","host":"example.com","answer":"ok"}}"#
                );
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                    json.len()
                );
                reader
                    .get_mut()
                    .write_all(response.as_bytes())
                    .await
                    .unwrap();
            }
        });
        let client = Client {
            addr,
            poll: Duration::from_millis(10),
        };
        (client, seen)
    }

    #[tokio::test]
    async fn waits_until_answered_or_the_limit() {
        let (client, seen) = fake_server(vec!["pending", "pending", "allowed"]).await;
        let ask = client
            .create(&serde_json::json!({"kind": "network", "host": "example.com"}))
            .await
            .unwrap();
        assert!(ask.pending());
        let ask = client.wait(ask.id, None).await.unwrap();
        assert_eq!(ask.status, "allowed");
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "POST /v1/asks HTTP/1.1",
                "GET /v1/asks/5 HTTP/1.1",
                "GET /v1/asks/5 HTTP/1.1"
            ]
        );

        let (client, _) = fake_server(vec![]).await;
        let ask = client
            .wait(5, Some(Duration::from_millis(50)))
            .await
            .unwrap();
        assert!(ask.pending());
        assert_eq!(client.cancel(5).await.unwrap().status, "cancelled");
    }
}
