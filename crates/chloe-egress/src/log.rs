//! 要求ごとのメタデータを JSON Lines で記録する。本文とヘッダーの値は記録しない。

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

pub struct Logger {
    file: Option<Mutex<File>>,
}

#[derive(Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event<'a> {
    Passthrough {
        host: &'a str,
    },
    Request {
        host: &'a str,
        sni: Option<&'a str>,
        method: &'a str,
        path: &'a str,
        query_keys: Vec<&'a str>,
        http_version: &'a str,
        status: Option<u16>,
        request_bytes: Option<u64>,
        body_scanned: bool,
        error: Option<String>,
        decision: &'a str,
        injected: &'a [String],
        #[serde(skip_serializing_if = "Option::is_none")]
        rule: Option<&'a str>,
    },
}

#[derive(Serialize)]
struct Line<'a> {
    ts: f64,
    #[serde(flatten)]
    event: Event<'a>,
}

impl Logger {
    pub fn open(path: Option<&Path>) -> std::io::Result<Self> {
        let file = match path {
            Some(path) => Some(Mutex::new(
                File::options().create(true).append(true).open(path)?,
            )),
            None => None,
        };
        Ok(Self { file })
    }

    pub fn log(&self, event: Event<'_>) {
        let Some(file) = &self.file else { return };
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or_default();
        let Ok(mut line) = serde_json::to_vec(&Line { ts, event }) else {
            return;
        };
        line.push(b'\n');
        if let Ok(mut file) = file.lock() {
            let _ = file.write_all(&line);
        }
    }
}

/// クエリのキーだけを取り出す。値にトークンが入っていることがあるので記録しない。
pub fn query_keys(query: Option<&str>) -> Vec<&str> {
    query
        .unwrap_or_default()
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| pair.split_once('=').map_or(pair, |(key, _)| key))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_keys_drop_values() {
        assert_eq!(query_keys(Some("t=secret&a&b=1")), ["t", "a", "b"]);
        assert!(query_keys(None).is_empty());
    }
}
