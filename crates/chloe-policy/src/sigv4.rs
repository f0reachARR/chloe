//! AWS Signature Version 4 の署名し直し。
//!
//! VM はプレースホルダーのアクセスキーで署名する。プロキシは `Authorization` ヘッダーから
//! 署名の範囲と署名したヘッダーの名前を読み取り、本物のアクセスキーで同じ要求に署名し直す。
//! VM の署名は確かめない (プレースホルダーで作った署名に意味はない)。
//!
//! 扱わないもの：署名付き URL (クエリの `X-Amz-Credential`)、チャンクごとの署名
//! (`x-amz-content-sha256: STREAMING-AWS4-...`)。チャンクに署名しない
//! `STREAMING-UNSIGNED-PAYLOAD-TRAILER` は、ヘッダーの署名だけを作り直せばよいので扱う。

use hmac::{Hmac, Mac as _};
use sha2::{Digest as _, Sha256};

pub(crate) const ALGORITHM: &str = "AWS4-HMAC-SHA256";

/// `Authorization` ヘッダーから読み取った署名の情報。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Authorization {
    pub access_key_id: String,
    /// `<日付>/<リージョン>/<サービス>/aws4_request`
    pub scope: String,
    /// 小文字の名前を `;` でつないだもの。
    pub signed_headers: String,
}

impl Authorization {
    pub fn parse(value: &[u8]) -> Option<Self> {
        let value = std::str::from_utf8(value).ok()?;
        let rest = value.strip_prefix(ALGORITHM)?;
        if !rest.starts_with([' ', '\t']) {
            return None;
        }
        let (mut credential, mut signed_headers) = (None, None);
        for part in rest.split(',') {
            match part.trim().split_once('=') {
                Some(("Credential", v)) => credential = Some(v),
                Some(("SignedHeaders", v)) => signed_headers = Some(v),
                _ => {}
            }
        }
        let (access_key_id, scope) = credential?.split_once('/')?;
        let parts: Vec<&str> = scope.split('/').collect();
        if parts.len() != 4 || parts[3] != "aws4_request" || parts[..3].iter().any(|p| p.is_empty())
        {
            return None;
        }
        let signed_headers = signed_headers?;
        if signed_headers.is_empty() || signed_headers.chars().any(|c| c.is_ascii_uppercase()) {
            return None;
        }
        Some(Self {
            access_key_id: access_key_id.to_string(),
            scope: scope.to_string(),
            signed_headers: signed_headers.to_string(),
        })
    }

    fn service(&self) -> &str {
        self.scope.split('/').nth(2).unwrap_or_default()
    }

    fn date(&self) -> &str {
        self.scope.split('/').next().unwrap_or_default()
    }

    fn region(&self) -> &str {
        self.scope.split('/').nth(1).unwrap_or_default()
    }

    pub fn signed_header_names(&self) -> impl Iterator<Item = &str> {
        self.signed_headers.split(';')
    }
}

/// 署名に使う要求の見え方。
pub(crate) struct SigningRequest<'a> {
    pub method: &'a str,
    pub path_and_query: &'a str,
    /// `host` ヘッダーがない (HTTP/2) ときに使う要求先。ポートを含んでいてもよい。
    pub authority: &'a str,
    pub headers: &'a [(String, Vec<u8>)],
    pub payload_hash: &'a str,
}

/// 本物のアクセスキーで署名し、新しい `Authorization` ヘッダーの値を返す。
/// 署名したヘッダーが要求になければ `None`。
pub(crate) fn sign(
    auth: &Authorization,
    request: &SigningRequest<'_>,
    access_key_id: &str,
    secret_access_key: &str,
) -> Option<String> {
    let amz_date = header_values(request.headers, "x-amz-date")?;
    let canonical = canonical_request(auth, request)?;
    let string_to_sign = format!(
        "{ALGORITHM}\n{amz_date}\n{}\n{}",
        auth.scope,
        hex(&Sha256::digest(canonical.as_bytes()))
    );
    let key = hmac(
        format!("AWS4{secret_access_key}").as_bytes(),
        auth.date().as_bytes(),
    );
    let key = hmac(&key, auth.region().as_bytes());
    let key = hmac(&key, auth.service().as_bytes());
    let key = hmac(&key, b"aws4_request");
    let signature = hex(&hmac(&key, string_to_sign.as_bytes()));
    Some(format!(
        "{ALGORITHM} Credential={access_key_id}/{}, SignedHeaders={}, Signature={signature}",
        auth.scope, auth.signed_headers
    ))
}

pub(crate) fn canonical_request(
    auth: &Authorization,
    request: &SigningRequest<'_>,
) -> Option<String> {
    let (path, query) = match request.path_and_query.split_once('?') {
        Some((path, query)) => (path, query),
        None => (request.path_and_query, ""),
    };
    // S3 は送ったパスをそのまま使い、ほかのサービスは正規化してもう一度エンコードする。
    let uri = if matches!(auth.service(), "s3" | "s3-object-lambda" | "s3express") {
        if path.is_empty() { "/" } else { path }.to_string()
    } else {
        encode(remove_dot_segments(path).as_bytes(), false)
    };
    let mut headers = String::new();
    for name in auth.signed_header_names() {
        let value = match header_values(request.headers, name) {
            Some(value) => value,
            None if name == "host" => request.authority.to_string(),
            None => return None,
        };
        headers.push_str(&format!("{name}:{value}\n"));
    }
    Some(format!(
        "{}\n{uri}\n{}\n{headers}\n{}\n{}",
        request.method,
        canonical_query(query),
        auth.signed_headers,
        request.payload_hash
    ))
}

/// 同じ名前のヘッダーの値を、前後の空白を取り、続く空白を一つにして `,` でつなぐ。
pub(crate) fn header_values(headers: &[(String, Vec<u8>)], name: &str) -> Option<String> {
    let values: Vec<String> = headers
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| {
            String::from_utf8_lossy(v)
                .split_ascii_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    (!values.is_empty()).then(|| values.join(","))
}

fn canonical_query(query: &str) -> String {
    let mut pairs: Vec<(String, String)> = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (
                encode(&percent_decode(key), true),
                encode(&percent_decode(value), true),
            )
        })
        .collect();
    pairs.sort();
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// `.` と `..` と空の区切りを取り除く (botocore の `remove_dot_segments` と同じ)。
fn remove_dot_segments(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    let last = if path.ends_with('/') && !out.is_empty() {
        "/"
    } else {
        ""
    };
    format!("/{}{last}", out.join("/"))
}

/// 英数字と `-_.~` のほかをエンコードする。`slash` が偽なら `/` は残す。
fn encode(value: &[u8], slash: bool) -> String {
    let mut out = String::with_capacity(value.len());
    for &b in value {
        if b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'_' | b'.' | b'~')
            || (b == b'/' && !slash)
        {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn percent_decode(value: &str) -> Vec<u8> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(byte) = bytes
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// チャンクごとに前の署名をつないで署名する方式か (本文を書き換えないと作り直せない)。
pub(crate) fn signs_chunks(payload_hash: &str) -> bool {
    payload_hash.starts_with("STREAMING-AWS4-")
}

pub(crate) fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
