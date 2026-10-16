//! 許可リストと差し替え規則。
//!
//! プロキシの実装 (HTTP ライブラリ) には依存せず、要求の見え方だけを受け取って判定する。
//!
//! - 中継のみの宛先は、SNI を見て復号せずに中継する ([`Policy::tls_decision`])。
//! - 許可リストにない宛先、SNI のない TLS、SNI と Host が食い違う要求は遮断する。
//! - プレースホルダーは、規則に登録した宛先と場所に限って本物に差し替える。
//!   それ以外の宛先でプレースホルダーを見つけたら遮断する。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

/// CA 証明書を返すホスト名。外部には存在しない。
pub const CA_HOST: &str = "ca.chloe.internal";

/// 方針の設定ファイルの形。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PolicyConfig {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub passthrough: Vec<String>,
    #[serde(default)]
    pub inject: Vec<InjectRuleConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InjectRuleConfig {
    pub name: String,
    pub placeholder: String,
    pub hosts: Vec<String>,
    pub header: String,
    #[serde(default)]
    pub format: InjectFormat,
    /// 本物の値を読む環境変数。段階 6 でシークレットの保管先への参照に置き換える。
    pub secret_env: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InjectFormat {
    /// ヘッダーの値の中のプレースホルダーを本物に置き換える。
    #[default]
    Replace,
    /// Basic 認証のパスワードがプレースホルダーのとき、パスワードを本物に置き換える。
    BasicPassword,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to resolve the secret for rule {rule}: {message}")]
    Secret { rule: String, message: String },
}

pub struct Policy {
    allow: Vec<String>,
    passthrough: Vec<String>,
    rules: Vec<InjectRule>,
}

struct InjectRule {
    name: String,
    placeholder: String,
    hosts: Vec<String>,
    header: String,
    format: InjectFormat,
    secret: SecretString,
}

/// TLS の接続を復号するかどうか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsDecision {
    Intercept,
    Passthrough,
}

/// 判定に使う要求の見え方。
pub struct Request<'a> {
    /// Host ヘッダー (なければ要求先) のホスト名。ポートを含んでいてもよい。
    pub host: &'a str,
    /// TLS の SNI。平文の HTTP なら `None`。
    pub sni: Option<&'a str>,
    pub tls: bool,
    /// パスとクエリ。
    pub path_and_query: &'a str,
    /// ヘッダー。名前は小文字にしておく。差し替えた値はここに書き戻す。
    pub headers: &'a mut Vec<(String, Vec<u8>)>,
    /// 本文。大きすぎて調べなかったときは `None`。
    pub body: Option<&'a [u8]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// CA 証明書を返す。
    Ca,
    /// 上流へ送る。`injected` は差し替えた規則の名前。
    Allowed { injected: Vec<String> },
    Blocked {
        reason: BlockReason,
        rule: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockReason {
    NoSni,
    SniHostMismatch,
    NotAllowed,
    PlaceholderToUnboundHost,
}

impl BlockReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoSni => "no-sni",
            Self::SniHostMismatch => "sni-host-mismatch",
            Self::NotAllowed => "not-allowed",
            Self::PlaceholderToUnboundHost => "placeholder-to-unbound-host",
        }
    }
}

impl Policy {
    /// 設定から方針を作る。`resolve` は規則の本物の値を返す。
    pub fn new(
        config: PolicyConfig,
        resolve: impl Fn(&InjectRuleConfig) -> Result<String, String>,
    ) -> Result<Self, Error> {
        let rules = config
            .inject
            .iter()
            .map(|rule| {
                let secret = resolve(rule).map_err(|message| Error::Secret {
                    rule: rule.name.clone(),
                    message,
                })?;
                Ok(InjectRule {
                    name: rule.name.clone(),
                    placeholder: rule.placeholder.clone(),
                    hosts: rule.hosts.clone(),
                    header: rule.header.to_ascii_lowercase(),
                    format: rule.format,
                    secret: secret.into(),
                })
            })
            .collect::<Result<_, Error>>()?;
        let mut allow = config.allow;
        allow.extend(config.passthrough.iter().cloned());
        Ok(Self {
            allow,
            passthrough: config.passthrough,
            rules,
        })
    }

    pub fn tls_decision(&self, sni: Option<&str>) -> TlsDecision {
        match sni {
            Some(sni) if host_matches(sni, &self.passthrough) => TlsDecision::Passthrough,
            _ => TlsDecision::Intercept,
        }
    }

    /// 宛先が許可リストにあるか。上流へ接続する前の判定に使う。
    pub fn is_allowed(&self, host: &str) -> bool {
        host_matches(host, &self.allow)
    }

    /// 要求を判定し、許可するなら差し替えを `request.headers` に書き戻す。
    pub fn evaluate(&self, request: &mut Request<'_>) -> Decision {
        let host = normalize_host(request.host);
        if host == CA_HOST {
            return Decision::Ca;
        }
        if request.tls {
            match request.sni.map(normalize_host) {
                None => return blocked(BlockReason::NoSni, None),
                Some(sni) if sni != host => return blocked(BlockReason::SniHostMismatch, None),
                Some(_) => {}
            }
        }
        if !host_matches(&host, &self.allow) {
            return blocked(BlockReason::NotAllowed, None);
        }

        // 先にすべての規則を確かめてから差し替える。途中で遮断したときに、
        // 一部だけ差し替えたヘッダーが残らないようにする。
        let mut edits = Vec::new();
        for rule in &self.rules {
            let bound = host_matches(&host, &rule.hosts);
            for (index, (name, value)) in request.headers.iter().enumerate() {
                if !rule.contains(value) {
                    continue;
                }
                if !bound {
                    return blocked(BlockReason::PlaceholderToUnboundHost, Some(&rule.name));
                }
                if *name == rule.header
                    && let Some(rendered) = rule.render(value)
                {
                    edits.push((index, rendered, rule.name.clone()));
                }
            }
            let in_url = request.path_and_query.contains(&rule.placeholder);
            let in_body = request
                .body
                .is_some_and(|body| contains_bytes(body, rule.placeholder.as_bytes()));
            if (in_url || in_body) && !bound {
                return blocked(BlockReason::PlaceholderToUnboundHost, Some(&rule.name));
            }
        }

        let mut injected = Vec::new();
        for (index, value, rule) in edits {
            request.headers[index].1 = value;
            if !injected.contains(&rule) {
                injected.push(rule);
            }
        }
        Decision::Allowed { injected }
    }
}

impl InjectRule {
    /// Basic 認証のパスワードがプレースホルダーなら、そのユーザー名を返す。
    fn basic_user(&self, value: &[u8]) -> Option<String> {
        let value = std::str::from_utf8(value).ok()?;
        let (scheme, encoded) = value.split_once(' ')?;
        if !scheme.eq_ignore_ascii_case("basic") {
            return None;
        }
        let decoded = String::from_utf8(BASE64.decode(encoded.trim()).ok()?).ok()?;
        let (user, password) = decoded.split_once(':')?;
        (password == self.placeholder).then(|| user.to_string())
    }

    fn contains(&self, value: &[u8]) -> bool {
        contains_bytes(value, self.placeholder.as_bytes())
            || (self.format == InjectFormat::BasicPassword && self.basic_user(value).is_some())
    }

    fn render(&self, value: &[u8]) -> Option<Vec<u8>> {
        let secret = self.secret.expose_secret();
        match self.format {
            InjectFormat::BasicPassword => {
                let user = self.basic_user(value)?;
                let token = BASE64.encode(format!("{user}:{secret}"));
                Some(format!("Basic {token}").into_bytes())
            }
            InjectFormat::Replace => {
                let value = std::str::from_utf8(value).ok()?;
                value
                    .contains(&self.placeholder)
                    .then(|| value.replace(&self.placeholder, secret).into_bytes())
            }
        }
    }
}

fn blocked(reason: BlockReason, rule: Option<&str>) -> Decision {
    Decision::Blocked {
        reason,
        rule: rule.map(Into::into),
    }
}

/// ホスト名を小文字にし、ポートと末尾のドットを取る。
pub fn normalize_host(host: &str) -> String {
    let host = match host.rsplit_once(':') {
        // IPv6 のアドレスはポートと区別がつかないので、角括弧つきのときだけポートを取る。
        Some((name, port))
            if port.chars().all(|c| c.is_ascii_digit())
                && (!name.contains(':') || name.ends_with(']')) =>
        {
            name
        }
        _ => host,
    };
    host.trim_end_matches('.').to_ascii_lowercase()
}

/// `*.example.com` は example.com のサブドメインに一致し、example.com そのものには一致しない。
pub fn host_matches(host: &str, patterns: &[String]) -> bool {
    let host = normalize_host(host);
    patterns.iter().any(|pattern| {
        let pattern = pattern.to_ascii_lowercase();
        match pattern.strip_prefix("*.") {
            Some(suffix) => host.ends_with(&format!(".{suffix}")),
            None => host == pattern,
        }
    })
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PH: &str = "chloe-ph-test-4f1d2c9a7b";
    const GH_PH: &str = "ghp_chloe0123456789";

    fn policy() -> Policy {
        let config: PolicyConfig = serde_json::from_value(serde_json::json!({
            "allow": ["httpbin.org", "example.org", "github.com", "*.githubusercontent.com"],
            "passthrough": ["registry-1.docker.io", "*.docker.com"],
            "inject": [
                {
                    "name": "test",
                    "placeholder": PH,
                    "hosts": ["httpbin.org"],
                    "header": "Authorization",
                    "secret_env": "TEST"
                },
                {
                    "name": "github",
                    "placeholder": GH_PH,
                    "hosts": ["github.com"],
                    "header": "authorization",
                    "format": "basic-password",
                    "secret_env": "GITHUB"
                }
            ]
        }))
        .unwrap();
        Policy::new(config, |rule| {
            Ok(format!("real-{}", rule.secret_env.to_lowercase()))
        })
        .unwrap()
    }

    struct Req {
        host: &'static str,
        sni: Option<&'static str>,
        path: &'static str,
        headers: Vec<(String, Vec<u8>)>,
        body: Option<&'static [u8]>,
    }

    impl Req {
        fn https(host: &'static str) -> Self {
            Self {
                host,
                sni: Some(host),
                path: "/",
                headers: vec![],
                body: None,
            }
        }

        fn header(mut self, name: &str, value: &str) -> Self {
            self.headers.push((name.into(), value.as_bytes().to_vec()));
            self
        }

        fn eval(&mut self, policy: &Policy) -> Decision {
            policy.evaluate(&mut Request {
                host: self.host,
                sni: self.sni,
                tls: self.sni.is_some(),
                path_and_query: self.path,
                headers: &mut self.headers,
                body: self.body,
            })
        }

        fn header_value(&self, name: &str) -> String {
            let (_, value) = self.headers.iter().find(|(n, _)| n == name).unwrap();
            String::from_utf8(value.clone()).unwrap()
        }
    }

    fn reason(decision: Decision) -> BlockReason {
        match decision {
            Decision::Blocked { reason, .. } => reason,
            other => panic!("expected blocked, got {other:?}"),
        }
    }

    #[test]
    fn host_patterns() {
        let patterns = vec!["example.com".into(), "*.docker.com".into()];
        assert!(host_matches("Example.COM.", &patterns));
        assert!(host_matches("example.com:443", &patterns));
        assert!(host_matches("production.cloudfront.docker.com", &patterns));
        assert!(!host_matches("docker.com", &patterns));
        assert!(!host_matches("evil-docker.com", &patterns));
        assert!(!host_matches("example.com.evil.net", &patterns));
    }

    #[test]
    fn passthrough_by_sni() {
        let p = policy();
        assert_eq!(
            p.tls_decision(Some("registry-1.docker.io")),
            TlsDecision::Passthrough
        );
        assert_eq!(p.tls_decision(Some("httpbin.org")), TlsDecision::Intercept);
        assert_eq!(p.tls_decision(None), TlsDecision::Intercept);
        assert!(p.is_allowed("auth.cloudflare.docker.com"));
    }

    #[test]
    fn ca_host() {
        let mut req = Req::https(CA_HOST);
        req.sni = None;
        assert_eq!(req.eval(&policy()), Decision::Ca);
    }

    #[test]
    fn allowed_and_not_allowed() {
        let p = policy();
        assert_eq!(
            Req::https("httpbin.org").eval(&p),
            Decision::Allowed { injected: vec![] }
        );
        assert_eq!(
            reason(Req::https("example.com").eval(&p)),
            BlockReason::NotAllowed
        );
    }

    #[test]
    fn tls_requires_matching_sni() {
        let p = policy();
        let mut no_sni = Req::https("httpbin.org");
        no_sni.sni = None;
        // sni が None だと平文扱いになるので、TLS を明示して判定する。
        let decision = p.evaluate(&mut Request {
            host: no_sni.host,
            sni: None,
            tls: true,
            path_and_query: "/",
            headers: &mut no_sni.headers,
            body: None,
        });
        assert_eq!(reason(decision), BlockReason::NoSni);

        let mut fronted = Req::https("example.com");
        fronted.sni = Some("httpbin.org");
        assert_eq!(reason(fronted.eval(&p)), BlockReason::SniHostMismatch);
    }

    #[test]
    fn injects_on_bound_host() {
        let p = policy();
        let mut req = Req::https("httpbin.org").header("authorization", &format!("Bearer {PH}"));
        assert_eq!(
            req.eval(&p),
            Decision::Allowed {
                injected: vec!["test".into()]
            }
        );
        assert_eq!(req.header_value("authorization"), "Bearer real-test");
    }

    #[test]
    fn leaves_other_headers_on_bound_host() {
        let p = policy();
        let mut req = Req::https("httpbin.org").header("x-token", PH);
        assert_eq!(req.eval(&p), Decision::Allowed { injected: vec![] });
        assert_eq!(req.header_value("x-token"), PH);
    }

    #[test]
    fn blocks_placeholder_to_unbound_host() {
        let p = policy();
        let header = Req::https("example.org").header("x-token", PH).eval(&p);
        assert_eq!(reason(header), BlockReason::PlaceholderToUnboundHost);

        let mut query = Req::https("example.org");
        query.path = "/?t=chloe-ph-test-4f1d2c9a7b";
        assert_eq!(
            reason(query.eval(&p)),
            BlockReason::PlaceholderToUnboundHost
        );

        let mut body = Req::https("example.org");
        body.body = Some(b"t=chloe-ph-test-4f1d2c9a7b");
        assert_eq!(reason(body.eval(&p)), BlockReason::PlaceholderToUnboundHost);
    }

    #[test]
    fn blocking_leaves_no_partial_injection() {
        let p = policy();
        let mut req = Req::https("github.com")
            .header(
                "authorization",
                &format!("Basic {}", BASE64.encode(format!("x-access-token:{GH_PH}"))),
            )
            .header("x-leak", PH);
        assert_eq!(reason(req.eval(&p)), BlockReason::PlaceholderToUnboundHost);
        assert!(req.header_value("authorization").starts_with("Basic "));
        assert!(!req.header_value("authorization").contains("real"));
    }

    #[test]
    fn injects_basic_password() {
        let p = policy();
        let basic = format!("Basic {}", BASE64.encode(format!("x-access-token:{GH_PH}")));
        let mut req = Req::https("github.com").header("authorization", &basic);
        assert_eq!(
            req.eval(&p),
            Decision::Allowed {
                injected: vec!["github".into()]
            }
        );
        let expected = format!("Basic {}", BASE64.encode("x-access-token:real-github"));
        assert_eq!(req.header_value("authorization"), expected);

        // Basic 認証で包んだプレースホルダーも、登録していない宛先へは送らせない。
        let leaked = Req::https("httpbin.org")
            .header("authorization", &basic)
            .eval(&p);
        assert_eq!(reason(leaked), BlockReason::PlaceholderToUnboundHost);
    }

    #[test]
    fn plain_http_has_no_sni() {
        let p = policy();
        let mut req = Req::https("httpbin.org:80");
        req.sni = None;
        assert_eq!(req.eval(&p), Decision::Allowed { injected: vec![] });
    }

    #[test]
    fn secret_resolution_errors_name_the_rule() {
        let config: PolicyConfig = serde_json::from_value(serde_json::json!({
            "inject": [{ "name": "r", "placeholder": "p", "hosts": [], "header": "h", "secret_env": "X" }]
        }))
        .unwrap();
        let error = Policy::new(config, |_| Err("unset".into())).err().unwrap();
        assert!(error.to_string().contains("rule r"));
    }
}
