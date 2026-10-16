//! 許可リストと差し替え規則。
//!
//! プロキシの実装 (HTTP ライブラリ) には依存せず、要求の見え方だけを受け取って判定する。
//!
//! - 中継のみの宛先は、SNI を見て復号せずに中継する ([`Policy::tls_decision`])。
//! - 許可リストにない宛先、SNI のない TLS、SNI と Host が食い違う要求は遮断する。
//! - 宛先、メソッド、パスの規則 ([`PathRuleConfig`]) で、許可した宛先への要求をさらに絞る。
//! - プレースホルダーは、規則に登録した宛先と場所に限って本物に差し替える。
//!   それ以外の宛先でプレースホルダーを見つけたら遮断する。
//!
//! 方針は本物の値を持たない。[`Policy::evaluate`] はどのヘッダーをどの規則で差し替えるか
//! ([`Substitution`]) だけを決め、呼び出し側が本物の値を取ってきて [`Policy::apply`] で書き換え、
//! すぐに捨てる。

mod path;

pub use path::{Action, PathRuleConfig};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
pub use secrecy::{ExposeSecret, SecretString};
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
    /// 宛先、メソッド、パスの規則。上から順に評価する。
    #[serde(default)]
    pub rules: Vec<PathRuleConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InjectRuleConfig {
    pub name: String,
    pub placeholder: String,
    pub hosts: Vec<String>,
    pub header: String,
    #[serde(default)]
    pub format: InjectFormat,
    /// 本物の値を問い合わせるときの名前 (開発用のプロキシでは環境変数の名前にもなる)。
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

pub struct Policy {
    allow: Vec<String>,
    passthrough: Vec<String>,
    rules: Vec<InjectRule>,
    path_rules: Vec<path::PathRule>,
}

struct InjectRule {
    name: String,
    placeholder: String,
    hosts: Vec<String>,
    header: String,
    format: InjectFormat,
    secret: String,
}

/// 差し替えの予定。`header` 番目のヘッダーを `rule` 番目の規則で、`secret` の本物の値に差し替える。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Substitution {
    pub header: usize,
    rule: usize,
    pub secret: String,
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
    pub method: &'a str,
    /// パスとクエリ。
    pub path_and_query: &'a str,
    /// ヘッダー。名前は小文字にしておく。
    pub headers: &'a [(String, Vec<u8>)],
    /// 本文。大きすぎて調べなかったときは `None`。
    pub body: Option<&'a [u8]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// CA 証明書を返す。
    Ca,
    /// 上流へ送る。`injected` は差し替える規則の名前。
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
    /// 宛先、メソッド、パスの規則で拒否した。
    PathDenied,
    /// パスを正規化できなかった (根より上を指す `..` や壊れたパーセントエンコード)。
    BadPath,
    PlaceholderToUnboundHost,
}

impl BlockReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoSni => "no-sni",
            Self::SniHostMismatch => "sni-host-mismatch",
            Self::NotAllowed => "not-allowed",
            Self::PathDenied => "path-denied",
            Self::BadPath => "bad-path",
            Self::PlaceholderToUnboundHost => "placeholder-to-unbound-host",
        }
    }
}

impl Policy {
    pub fn new(config: PolicyConfig) -> Self {
        let rules = config
            .inject
            .iter()
            .map(|rule| InjectRule {
                name: rule.name.clone(),
                placeholder: rule.placeholder.clone(),
                hosts: rule.hosts.clone(),
                header: rule.header.to_ascii_lowercase(),
                format: rule.format,
                secret: rule.secret_env.clone(),
            })
            .collect();
        let mut allow = config.allow;
        allow.extend(config.passthrough.iter().cloned());
        let path_rules = config
            .rules
            .iter()
            .enumerate()
            .map(|(index, rule)| path::PathRule::new(index, rule))
            .collect();
        Self {
            allow,
            passthrough: config.passthrough,
            rules,
            path_rules,
        }
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

    /// 要求を判定し、許可するなら差し替えの予定も返す。
    pub fn evaluate(&self, request: &Request<'_>) -> (Decision, Vec<Substitution>) {
        match self.decide(request) {
            Ok(substitutions) => {
                let mut injected = Vec::new();
                for s in &substitutions {
                    let name = &self.rules[s.rule].name;
                    if !injected.contains(name) {
                        injected.push(name.clone());
                    }
                }
                (Decision::Allowed { injected }, substitutions)
            }
            Err(decision) => (decision, vec![]),
        }
    }

    /// 差し替えに要る本物の値の名前 (重複なし)。
    pub fn secrets_needed(substitutions: &[Substitution]) -> Vec<String> {
        let mut names: Vec<String> = substitutions.iter().map(|s| s.secret.clone()).collect();
        names.sort();
        names.dedup();
        names
    }

    /// 本物の値で `headers` を書き換える。`value` が値を返さない名前があれば、
    /// 何も書き換えずにその名前を返す。
    pub fn apply(
        &self,
        headers: &mut [(String, Vec<u8>)],
        substitutions: &[Substitution],
        value: impl Fn(&str) -> Option<SecretString>,
    ) -> Result<(), String> {
        let mut rendered = Vec::with_capacity(substitutions.len());
        for s in substitutions {
            let secret = value(&s.secret).ok_or_else(|| s.secret.clone())?;
            let rule = &self.rules[s.rule];
            if let Some(new) = rule.render(&headers[s.header].1, secret.expose_secret()) {
                rendered.push((s.header, new));
            }
        }
        for (index, new) in rendered {
            headers[index].1 = new;
        }
        Ok(())
    }

    fn decide(&self, request: &Request<'_>) -> Result<Vec<Substitution>, Decision> {
        let host = normalize_host(request.host);
        if host == CA_HOST {
            return Err(Decision::Ca);
        }
        if request.tls {
            match request.sni.map(normalize_host) {
                None => return Err(blocked(BlockReason::NoSni, None)),
                Some(sni) if sni != host => {
                    return Err(blocked(BlockReason::SniHostMismatch, None));
                }
                Some(_) => {}
            }
        }
        if !host_matches(&host, &self.allow) {
            return Err(blocked(BlockReason::NotAllowed, None));
        }
        if !self.path_rules.is_empty() {
            let Some(path) = path::normalize(request.path_and_query) else {
                return Err(blocked(BlockReason::BadPath, None));
            };
            let method = request.method.to_ascii_uppercase();
            if let Some(rule) = self
                .path_rules
                .iter()
                .find(|rule| rule.matches(&host, &method, &path))
                && rule.action == Action::Deny
            {
                return Err(blocked(BlockReason::PathDenied, Some(&rule.name)));
            }
        }

        let mut substitutions = Vec::new();
        for (rule_index, rule) in self.rules.iter().enumerate() {
            // 同じプレースホルダーを複数の規則で差し替えることがある (git と API など)。
            // どれかの規則がこの宛先を許していれば、束縛された宛先とみなす。
            let bound = self
                .rules
                .iter()
                .any(|r| r.placeholder == rule.placeholder && host_matches(&host, &r.hosts));
            for (index, (name, value)) in request.headers.iter().enumerate() {
                if !rule.contains(value) {
                    continue;
                }
                if !bound {
                    return Err(blocked(
                        BlockReason::PlaceholderToUnboundHost,
                        Some(&rule.name),
                    ));
                }
                if *name == rule.header && rule.renders(value) {
                    substitutions.push(Substitution {
                        header: index,
                        rule: rule_index,
                        secret: rule.secret.clone(),
                    });
                }
            }
            let in_url = request.path_and_query.contains(&rule.placeholder);
            let in_body = request
                .body
                .is_some_and(|body| contains_bytes(body, rule.placeholder.as_bytes()));
            if (in_url || in_body) && !bound {
                return Err(blocked(
                    BlockReason::PlaceholderToUnboundHost,
                    Some(&rule.name),
                ));
            }
        }
        Ok(substitutions)
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

    /// この規則で差し替えられる値か。
    fn renders(&self, value: &[u8]) -> bool {
        match self.format {
            InjectFormat::BasicPassword => self.basic_user(value).is_some(),
            InjectFormat::Replace => contains_bytes(value, self.placeholder.as_bytes()),
        }
    }

    fn render(&self, value: &[u8], secret: &str) -> Option<Vec<u8>> {
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
        Policy::new(config)
    }

    struct Req {
        host: &'static str,
        method: &'static str,
        sni: Option<&'static str>,
        path: &'static str,
        headers: Vec<(String, Vec<u8>)>,
        body: Option<&'static [u8]>,
    }

    impl Req {
        fn https(host: &'static str) -> Self {
            Self {
                host,
                method: "GET",
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

        /// 判定し、許可なら本物の値 ("real-" と小文字にした secret_env) で書き換える。
        fn eval(&mut self, policy: &Policy) -> Decision {
            let (decision, substitutions) = policy.evaluate(&Request {
                host: self.host,
                sni: self.sni,
                tls: self.sni.is_some(),
                method: self.method,
                path_and_query: self.path,
                headers: &self.headers,
                body: self.body,
            });
            policy
                .apply(&mut self.headers, &substitutions, |name| {
                    Some(format!("real-{}", name.to_lowercase()).into())
                })
                .unwrap();
            decision
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
        let (decision, _) = p.evaluate(&Request {
            host: no_sni.host,
            sni: None,
            tls: true,
            method: "GET",
            path_and_query: "/",
            headers: &no_sni.headers,
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
    fn one_placeholder_with_rules_for_different_hosts() {
        let config: PolicyConfig = serde_json::from_value(serde_json::json!({
            "allow": ["github.com", "api.github.com", "example.org"],
            "inject": [
                { "name": "gh-git", "placeholder": GH_PH, "hosts": ["github.com"], "header": "authorization", "format": "basic-password", "secret_env": "G" },
                { "name": "gh-api", "placeholder": GH_PH, "hosts": ["api.github.com"], "header": "authorization", "secret_env": "G" }
            ]
        }))
        .unwrap();
        let p = Policy::new(config);
        let mut api =
            Req::https("api.github.com").header("authorization", &format!("token {GH_PH}"));
        assert_eq!(
            api.eval(&p),
            Decision::Allowed {
                injected: vec!["gh-api".into()]
            }
        );
        assert_eq!(api.header_value("authorization"), "token real-g");
        let leaked = Req::https("example.org")
            .header("authorization", &format!("token {GH_PH}"))
            .eval(&p);
        assert_eq!(reason(leaked), BlockReason::PlaceholderToUnboundHost);
    }

    #[test]
    fn path_rules_first_match_wins() {
        let config: PolicyConfig = serde_json::from_value(serde_json::json!({
            "allow": ["api.github.com", "github.com"],
            "rules": [
                { "name": "no-delete", "hosts": ["api.github.com"], "methods": ["DELETE"], "paths": ["/repos/**"], "action": "deny" },
                { "hosts": ["api.github.com"], "paths": ["/repos/ForteFibre/**", "/user", "/graphql"], "action": "allow" },
                { "name": "github-default", "hosts": ["api.github.com"], "action": "deny" },
                { "name": "no-push", "hosts": ["github.com"], "methods": ["POST"], "paths": ["/*/*/git-receive-pack", "/*/*.git/git-receive-pack"], "action": "deny" }
            ]
        }))
        .unwrap();
        let p = Policy::new(config);
        let eval = |host: &'static str, method: &'static str, path: &'static str| {
            let mut req = Req::https(host);
            req.method = method;
            req.path = path;
            req.eval(&p)
        };
        let allowed = Decision::Allowed { injected: vec![] };
        let denied = |rule: &str| Decision::Blocked {
            reason: BlockReason::PathDenied,
            rule: Some(rule.into()),
        };
        assert_eq!(
            eval("api.github.com", "GET", "/repos/fortefibre/chloe/pulls"),
            allowed
        );
        assert_eq!(
            eval("api.github.com", "delete", "/repos/ForteFibre/chloe"),
            denied("no-delete")
        );
        assert_eq!(
            eval("api.github.com", "GET", "/repos/other/x"),
            denied("github-default")
        );
        // エンコードや .. で規則を回り込めない。
        assert_eq!(
            eval("api.github.com", "GET", "/repos/ForteFibre/../other/x"),
            denied("github-default")
        );
        assert_eq!(
            eval("api.github.com", "DELETE", "/repos/Forte%46ibre/chloe"),
            denied("no-delete")
        );
        assert_eq!(
            eval("api.github.com", "GET", "/../user"),
            Decision::Blocked {
                reason: BlockReason::BadPath,
                rule: None
            }
        );
        // どの規則にも当たらなければ許可リストだけで判定する。
        assert_eq!(
            eval(
                "github.com",
                "GET",
                "/ForteFibre/chloe.git/info/refs?service=git-receive-pack"
            ),
            allowed
        );
        assert_eq!(
            eval(
                "github.com",
                "POST",
                "/ForteFibre/chloe.git/git-receive-pack"
            ),
            denied("no-push")
        );
        assert_eq!(
            eval(
                "github.com",
                "POST",
                "/ForteFibre/chloe.git/git-upload-pack"
            ),
            allowed
        );
    }

    #[test]
    fn apply_needs_every_secret() {
        let p = policy();
        let mut headers = vec![(
            "authorization".to_string(),
            format!("Bearer {PH}").into_bytes(),
        )];
        let (_, substitutions) = p.evaluate(&Request {
            host: "httpbin.org",
            sni: Some("httpbin.org"),
            tls: true,
            method: "GET",
            path_and_query: "/",
            headers: &headers,
            body: None,
        });
        assert_eq!(Policy::secrets_needed(&substitutions), ["TEST"]);
        // 値が得られなければ何も書き換えない。
        assert_eq!(
            p.apply(&mut headers, &substitutions, |_| None),
            Err("TEST".to_string())
        );
        assert_eq!(headers[0].1, format!("Bearer {PH}").into_bytes());
    }

    #[test]
    fn plain_http_has_no_sni() {
        let p = policy();
        let mut req = Req::https("httpbin.org:80");
        req.sni = None;
        assert_eq!(req.eval(&p), Decision::Allowed { injected: vec![] });
    }
}
