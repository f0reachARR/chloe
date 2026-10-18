//! 監査イベント。
//!
//! ランナーのプロキシ (`chloe-egress`) が要求ごとに書く記録 ([`Record`]) の形式と、そのログ
//! (JSON Lines) の末尾を読む処理 ([`tail`]) を持つ。書く側と読む側で同じ型を使う。
//! 読むときは知らない欄を無視し、ない欄を空として読む (古いログには後から足した欄がない)。

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// 読む行数の既定。
pub const DEFAULT_LINES: usize = 200;
/// 読む行数の上限。
pub const MAX_LINES: usize = 2000;
/// ログの末尾から読むバイト数の上限。
pub const MAX_BYTES: u64 = 1 << 20;

/// ログの一行。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    /// UNIX 時刻 (秒)。
    #[serde(default)]
    pub ts: f64,
    #[serde(flatten)]
    pub event: Event,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// 復号せずに中継した TLS の接続。
    Passthrough { host: String },
    /// 復号した (または平文の) HTTP の要求。
    Request(Box<Request>),
}

/// 要求のメタデータ。本文、ヘッダーの値、クエリの値は持たない。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Request {
    pub host: String,
    pub sni: Option<String>,
    pub method: String,
    pub path: String,
    pub query_keys: Vec<String>,
    pub http_version: String,
    pub status: Option<u16>,
    pub request_bytes: Option<u64>,
    pub body_scanned: bool,
    pub error: Option<String>,
    /// `allowed`、`ca`、`blocked:<理由>`。
    pub decision: String,
    /// 差し替えた規則の名前。
    pub injected: Vec<String>,
    /// 遮断した規則の名前。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    /// 上流から受けた応答の本文のバイト数。上流へ送らなかった要求では空。
    pub response_bytes: Option<u64>,
    /// 要求を受けてから応答の本文を流し終えるまでのミリ秒。
    pub duration_ms: Option<u64>,
}

impl Request {
    pub fn blocked(&self) -> bool {
        self.decision.starts_with("blocked")
    }
}

/// ログの末尾から最大 `lines` 行を、古い順に読む。読むのは末尾の [`MAX_BYTES`] までで、
/// 読めない行は飛ばす。ファイルがなければ空を返す。
pub fn tail(path: &Path, lines: usize) -> io::Result<Vec<Record>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let len = file.metadata()?.len();
    let start = len.saturating_sub(MAX_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    file.take(len - start).read_to_end(&mut buf)?;
    let mut text = buf.as_slice();
    // 途中から読んだなら、最初の行は欠けている。
    if start > 0 {
        let skip = text
            .iter()
            .position(|&b| b == b'\n')
            .map_or(text.len(), |i| i + 1);
        text = &text[skip..];
    }
    let mut records: Vec<Record> = text
        .split(|&b| b == b'\n')
        .rev()
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_slice(line).ok())
        .take(lines.min(MAX_LINES))
        .collect();
    records.reverse();
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn reads_old_lines_and_skips_broken_ones() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("requests.jsonl");
        let mut file = File::create(&path).unwrap();
        writeln!(
            file,
            r#"{{"ts":1.5,"event":"passthrough","host":"a.example"}}"#
        )
        .unwrap();
        writeln!(file, "not json").unwrap();
        writeln!(
            file,
            r#"{{"ts":2.0,"event":"request","host":"b.example","sni":null,"method":"GET","path":"/","query_keys":[],"http_version":"HTTP/1.1","status":403,"request_bytes":null,"body_scanned":true,"error":null,"decision":"blocked:host","injected":[],"future":1}}"#
        )
        .unwrap();
        writeln!(file, r#"{{"ts":3.0,"event":"unknown"}}"#).unwrap();

        let records = tail(&path, 10).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(
            records[0].event,
            Event::Passthrough {
                host: "a.example".into()
            }
        );
        let Event::Request(request) = &records[1].event else {
            panic!("expected a request");
        };
        assert!(request.blocked());
        assert_eq!(request.status, Some(403));
        assert_eq!(request.rule, None);
        assert_eq!(request.duration_ms, None);

        let last = tail(&path, 1).unwrap();
        assert_eq!(last, records[1..]);
        assert!(tail(&dir.path().join("missing"), 10).unwrap().is_empty());
    }

    #[test]
    fn reads_only_the_end_of_a_large_log() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("requests.jsonl");
        let mut file = File::create(&path).unwrap();
        let line = r#"{"ts":1.0,"event":"passthrough","host":"a.example"}"#;
        let count = MAX_BYTES as usize / line.len() + 100;
        for _ in 0..count {
            writeln!(file, "{line}").unwrap();
        }
        let records = tail(&path, usize::MAX).unwrap();
        assert_eq!(records.len(), MAX_LINES);
    }

    #[test]
    fn writes_the_same_shape() {
        let record = Record {
            ts: 1.0,
            event: Event::Request(Box::new(Request {
                host: "a.example".into(),
                decision: "allowed".into(),
                ..Default::default()
            })),
        };
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(json["event"], "request");
        assert_eq!(json["host"], "a.example");
        assert!(json["status"].is_null());
        assert!(json.get("rule").is_none());
        assert_eq!(serde_json::from_value::<Record>(json).unwrap(), record);
    }
}
