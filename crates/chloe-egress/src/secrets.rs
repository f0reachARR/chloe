//! 差し替えに使う本物の値の取り出し。
//!
//! 本物の値はプロキシに持たせない。差し替えのたびにコントロールプレーンの問い合わせ用ソケットへ
//! 尋ね、ヘッダーを書き換えたらすぐ捨てる ([`SecretString`] は捨てるときに中身を消す)。
//!
//! 問い合わせは接続ごとに JSON を一行送り、一行で答えを受け取る。
//!
//! ```json
//! {"secret": "CHLOE_SECRET_GITHUB_TOKEN"}
//! {"value": "..."}  または  {"error": "..."}
//! ```
//!
//! 問い合わせ用ソケットを指定しなければ、開発用にプロキシの環境変数から読む。

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chloe_policy::SecretString;
use secrecy::zeroize::Zeroize as _;
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const BROKER_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub enum SecretSource {
    /// コントロールプレーンの問い合わせ用ソケット。
    Broker(PathBuf),
    /// プロキシの環境変数 (開発用)。
    Env,
}

#[derive(Deserialize)]
struct Answer {
    value: Option<String>,
    error: Option<String>,
}

impl SecretSource {
    /// 名前ごとに本物の値を取り出す。一つでも取り出せなければ失敗にする。
    pub async fn fetch(&self, names: &[String]) -> Result<HashMap<String, SecretString>> {
        let mut values = HashMap::with_capacity(names.len());
        for name in names {
            let value = match self {
                Self::Env => std::env::var(name)
                    .map(Into::into)
                    .with_context(|| format!("{name} is not set"))?,
                Self::Broker(socket) => tokio::time::timeout(BROKER_TIMEOUT, ask(socket, name))
                    .await
                    .context("the secret broker did not answer in time")??,
            };
            values.insert(name.clone(), value);
        }
        Ok(values)
    }
}

impl SecretSource {
    /// 問い合わせを一行送り、答えの一行をそのまま返す (エージェントからの依頼に使う)。
    pub async fn relay(&self, question: &serde_json::Value) -> Result<String> {
        let Self::Broker(socket) = self else {
            bail!("no secret broker to send asks to");
        };
        tokio::time::timeout(BROKER_TIMEOUT, async {
            let mut stream = tokio::net::UnixStream::connect(socket)
                .await
                .with_context(|| format!("connect to the secret broker at {}", socket.display()))?;
            stream.write_all(format!("{question}\n").as_bytes()).await?;
            let mut line = String::new();
            BufReader::new(stream).read_line(&mut line).await?;
            Ok(line)
        })
        .await
        .context("the secret broker did not answer in time")?
    }
}

async fn ask(socket: &PathBuf, name: &str) -> Result<SecretString> {
    let mut stream = tokio::net::UnixStream::connect(socket)
        .await
        .with_context(|| format!("connect to the secret broker at {}", socket.display()))?;
    let request = serde_json::json!({ "secret": name }).to_string();
    stream.write_all(format!("{request}\n").as_bytes()).await?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).await?;
    let answer: Answer =
        serde_json::from_str(&line).context("invalid answer from the secret broker")?;
    // 受け取った行は、値を取り出したらすぐに消す。
    line.zeroize();
    match (answer.value, answer.error) {
        (Some(value), _) => Ok(value.into()),
        (None, Some(error)) => bail!("the secret broker refused {name}: {error}"),
        (None, None) => bail!("the secret broker returned nothing for {name}"),
    }
}
