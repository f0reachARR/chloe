//! 許可リストと差し替え規則。
//!
//! プロキシの実装 (HTTP ライブラリ) には依存せず、要求の見え方だけを受け取って判定する。
//!
//! - 宛先、メソッド、パスの規則 ([`PathRuleConfig`]) を上から順に評価し、最初に一致した規則で
//!   許すか拒否するかを決める。どの規則にも一致しなければ遮断する。
//! - 中継のみの規則の宛先は、SNI を見て復号せずに中継する ([`Policy::tls_decision`])。
//! - SNI のない TLS、SNI と Host が食い違う要求は遮断する。
//! - プレースホルダーは、規則に登録した宛先と場所に限って本物に差し替える。
//!   それ以外の宛先でプレースホルダーを見つけたら遮断する。
//! - 署名の規則 ([`SignRuleConfig`]) では、プレースホルダーの鍵で作った署名を本物の鍵で作り直す。
//!
//! 方針は本物の値を持たない。[`Policy::evaluate`] はどのヘッダーをどの規則で書き換えるか
//! ([`Plan`]) だけを決め、呼び出し側が本物の値を取ってきて [`Policy::apply`] で書き換え、
//! すぐに捨てる。

mod path;
mod sigv4;

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
    pub inject: Vec<InjectRuleConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sign: Vec<SignRuleConfig>,
    /// 宛先、メソッド、パスの規則。上から順に評価し、どれにも一致しなければ遮断する。
    #[serde(default)]
    pub rules: Vec<PathRuleConfig>,
}

/// 規則が成り立っているかを確かめる。
///
/// 中継のみの規則は復号しないので、メソッドとパスを書けない。また、ほかの規則と宛先が重なると
/// 接続の時点 (SNI) で中継に決まり、もう一方の規則が効かないので、重なりも誤りにする。
pub fn check_rules(rules: &[PathRuleConfig]) -> Result<(), String> {
    let label = |rule: &PathRuleConfig| match &rule.name {
        Some(name) => format!("rule {name:?}"),
        None => format!("the rule for {}", rule.hosts.join(", ")),
    };
    for rule in rules {
        if rule.hosts.is_empty() {
            return Err(format!("{} has no hosts", label(rule)));
        }
        if rule.action == Action::Passthrough
            && (!rule.methods.is_empty() || !rule.paths.is_empty())
        {
            return Err(format!(
                "{} is passthrough and cannot have methods or paths",
                label(rule)
            ));
        }
    }
    for (i, a) in rules.iter().enumerate() {
        if a.action != Action::Passthrough {
            continue;
        }
        for (j, b) in rules.iter().enumerate() {
            if i == j {
                continue;
            }
            for host in &a.hosts {
                if let Some(other) = b.hosts.iter().find(|other| hosts_overlap(host, other)) {
                    return Err(format!(
                        "{} (passthrough {host}) overlaps {} ({other})",
                        label(a),
                        label(b)
                    ));
                }
            }
        }
    }
    Ok(())
}

/// つないでよい宛先 (許すか中継のみの規則の宛先)。VM の DNS で引ける名前にも使う。
pub fn reachable_hosts(rules: &[PathRuleConfig]) -> Vec<String> {
    let mut hosts: Vec<String> = Vec::new();
    for rule in rules.iter().filter(|r| r.action != Action::Deny) {
        for host in &rule.hosts {
            if !hosts.contains(host) {
                hosts.push(host.clone());
            }
        }
    }
    hosts
}

/// 二つの宛先の書き方に、どちらにも一致するホストがあるか。
pub fn hosts_overlap(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
        || host_matches(a, &[b.to_string()])
        || host_matches(b, &[a.to_string()])
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
    /// `basic-password` で、Basic 認証のユーザー名も差し替えるときのプレースホルダーと名前。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<Credential>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InjectFormat {
    /// ヘッダーの値の中のプレースホルダーを本物に置き換える。
    #[default]
    Replace,
    /// Basic 認証のパスワードがプレースホルダーのとき、パスワードを本物に置き換える。
    /// `username` があれば、ユーザー名がそのプレースホルダーのときにユーザー名も置き換える。
    BasicPassword,
}

/// 署名の規則。`hosts` への要求にある、プレースホルダーの鍵で作った署名を作り直す。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignRuleConfig {
    pub name: String,
    pub hosts: Vec<String>,
    #[serde(flatten)]
    pub scheme: SignScheme,
}

/// 署名の方式と、方式が使う鍵。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scheme", rename_all = "kebab-case")]
pub enum SignScheme {
    /// AWS Signature Version 4。セッショントークンがあれば `x-amz-security-token` も差し替える。
    #[serde(rename = "aws-sigv4")]
    AwsSigV4 {
        access_key_id: Credential,
        secret_access_key: Credential,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_token: Option<Credential>,
    },
}

impl SignScheme {
    fn credentials(&self) -> Vec<&Credential> {
        match self {
            Self::AwsSigV4 {
                access_key_id,
                secret_access_key,
                session_token,
            } => [
                Some(access_key_id),
                Some(secret_access_key),
                session_token.as_ref(),
            ]
            .into_iter()
            .flatten()
            .collect(),
        }
    }
}

/// 署名に使う鍵の一つ。VM に渡したプレースホルダーと、本物の値を問い合わせるときの名前。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credential {
    pub placeholder: String,
    pub secret_env: String,
}

pub struct Policy {
    allow: Vec<String>,
    passthrough: Vec<String>,
    rules: Vec<InjectRule>,
    sign: Vec<SignRuleConfig>,
    /// プレースホルダーごとの、送ってよい宛先。
    bindings: Vec<Binding>,
    path_rules: Vec<path::PathRule>,
}

struct Binding {
    placeholder: String,
    hosts: Vec<String>,
    rule: String,
    /// Basic 認証のユーザー名とパスワードとしても探す。
    basic: bool,
}

struct InjectRule {
    name: String,
    placeholder: String,
    hosts: Vec<String>,
    header: String,
    format: InjectFormat,
    secret: String,
    username: Option<Credential>,
}

/// 書き換えの予定。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    substitutions: Vec<Substitution>,
    signings: Vec<Signing>,
}

/// `header` 番目のヘッダーを `rule` 番目の規則で、`secret` (と Basic 認証のユーザー名なら
/// `username`) の本物の値に差し替える。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Substitution {
    header: usize,
    rule: usize,
    secret: Option<String>,
    username: Option<String>,
}

/// `header` 番目のヘッダー (`Authorization`) を、`rule` 番目の署名の規則で作り直す。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Signing {
    header: usize,
    rule: usize,
    auth: sigv4::Authorization,
    /// `x-amz-security-token` にセッショントークンのプレースホルダーがある。
    session_token: bool,
    method: String,
    path_and_query: String,
    authority: String,
    payload_hash: String,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.substitutions.is_empty() && self.signings.is_empty()
    }
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
    /// 署名の規則の宛先に、作り直せない署名 (署名付き URL、チャンクごとの署名、
    /// 本文のハッシュが分からないものなど) を送ろうとした。
    Unsignable,
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
            Self::Unsignable => "unsignable",
        }
    }
}

impl Policy {
    pub fn new(config: PolicyConfig) -> Self {
        let rules: Vec<InjectRule> = config
            .inject
            .iter()
            .map(|rule| InjectRule {
                name: rule.name.clone(),
                placeholder: rule.placeholder.clone(),
                hosts: rule.hosts.clone(),
                header: rule.header.to_ascii_lowercase(),
                format: rule.format,
                secret: rule.secret_env.clone(),
                username: rule.username.clone(),
            })
            .collect();
        let mut bindings: Vec<Binding> = Vec::new();
        for rule in &rules {
            let basic = rule.format == InjectFormat::BasicPassword;
            let placeholders = std::iter::once(&rule.placeholder)
                .chain(rule.username.as_ref().map(|u| &u.placeholder));
            for placeholder in placeholders {
                bindings.push(Binding {
                    placeholder: placeholder.clone(),
                    hosts: rule.hosts.clone(),
                    rule: rule.name.clone(),
                    basic,
                });
            }
        }
        for rule in &config.sign {
            for credential in rule.scheme.credentials() {
                bindings.push(Binding {
                    placeholder: credential.placeholder.clone(),
                    hosts: rule.hosts.clone(),
                    rule: rule.name.clone(),
                    basic: false,
                });
            }
        }
        let allow = reachable_hosts(&config.rules);
        let passthrough = config
            .rules
            .iter()
            .filter(|rule| rule.action == Action::Passthrough)
            .flat_map(|rule| rule.hosts.iter().cloned())
            .collect();
        let path_rules = config
            .rules
            .iter()
            .enumerate()
            .map(|(index, rule)| path::PathRule::new(index, rule))
            .collect();
        Self {
            allow,
            passthrough,
            rules,
            sign: config.sign,
            bindings,
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

    /// 要求を判定し、許可するなら書き換えの予定も返す。
    pub fn evaluate(&self, request: &Request<'_>) -> (Decision, Plan) {
        match self.decide(request) {
            Ok(plan) => {
                let mut injected = Vec::new();
                let names = plan
                    .substitutions
                    .iter()
                    .map(|s| &self.rules[s.rule].name)
                    .chain(plan.signings.iter().map(|s| &self.sign[s.rule].name));
                for name in names {
                    if !injected.contains(name) {
                        injected.push(name.clone());
                    }
                }
                (Decision::Allowed { injected }, plan)
            }
            Err(decision) => (decision, Plan::default()),
        }
    }

    /// 書き換えに要る本物の値の名前 (重複なし)。
    pub fn secrets_needed(&self, plan: &Plan) -> Vec<String> {
        let mut names: Vec<String> = plan
            .substitutions
            .iter()
            .flat_map(|s| s.secret.iter().chain(&s.username).cloned())
            .collect();
        for s in &plan.signings {
            let SignScheme::AwsSigV4 {
                access_key_id,
                secret_access_key,
                session_token,
            } = &self.sign[s.rule].scheme;
            names.push(access_key_id.secret_env.clone());
            names.push(secret_access_key.secret_env.clone());
            if let Some(token) = session_token.as_ref().filter(|_| s.session_token) {
                names.push(token.secret_env.clone());
            }
        }
        names.sort();
        names.dedup();
        names
    }

    /// 本物の値で `headers` を書き換える。`value` が値を返さない名前があれば、
    /// 何も書き換えずにその名前を返す。
    pub fn apply(
        &self,
        headers: &mut [(String, Vec<u8>)],
        plan: &Plan,
        value: impl Fn(&str) -> Option<SecretString>,
    ) -> Result<(), String> {
        let get = |name: &str| value(name).ok_or_else(|| name.to_string());
        let mut work = headers.to_vec();
        for s in &plan.substitutions {
            let secret = s.secret.as_deref().map(get).transpose()?;
            let username = s.username.as_deref().map(get).transpose()?;
            let rule = &self.rules[s.rule];
            if let Some(new) = rule.render(
                &work[s.header].1,
                secret.as_ref().map(|v| v.expose_secret()),
                username.as_ref().map(|v| v.expose_secret()),
            ) {
                work[s.header].1 = new;
            }
        }
        // 署名は差し替えたあとのヘッダーで作る (セッショントークンも署名に含まれる)。
        for s in &plan.signings {
            let rule = &self.sign[s.rule];
            let SignScheme::AwsSigV4 {
                access_key_id,
                secret_access_key,
                session_token,
            } = &rule.scheme;
            if let Some(token) = session_token.as_ref().filter(|_| s.session_token) {
                let real = get(&token.secret_env)?;
                for (name, value) in work.iter_mut() {
                    if name == "x-amz-security-token"
                        && let Ok(text) = std::str::from_utf8(value)
                    {
                        *value = text
                            .replace(&token.placeholder, real.expose_secret())
                            .into_bytes();
                    }
                }
            }
            let key_id = get(&access_key_id.secret_env)?;
            let secret = get(&secret_access_key.secret_env)?;
            let request = sigv4::SigningRequest {
                method: &s.method,
                path_and_query: &s.path_and_query,
                authority: &s.authority,
                headers: &work,
                payload_hash: &s.payload_hash,
            };
            let signed = sigv4::sign(
                &s.auth,
                &request,
                key_id.expose_secret(),
                secret.expose_secret(),
            )
            .ok_or_else(|| rule.name.clone())?;
            work[s.header].1 = signed.into_bytes();
        }
        headers.clone_from_slice(&work);
        Ok(())
    }

    fn decide(&self, request: &Request<'_>) -> Result<Plan, Decision> {
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
        let Some(path) = path::normalize(request.path_and_query) else {
            return Err(blocked(BlockReason::BadPath, None));
        };
        let method = request.method.to_ascii_uppercase();
        // 中継のみの規則に一致するのは、ここでは平文の HTTP だけ (TLS は復号せずに中継している)。
        match self
            .path_rules
            .iter()
            .find(|rule| rule.matches(&host, &method, &path))
        {
            None => return Err(blocked(BlockReason::NotAllowed, None)),
            Some(rule) if rule.action == Action::Deny => {
                return Err(blocked(BlockReason::PathDenied, Some(&rule.name)));
            }
            Some(_) => {}
        }

        // 同じプレースホルダーを複数の規則で差し替えることがある (git と API など)。
        // どれかの規則がこの宛先を許していれば、束縛された宛先とみなす。
        for binding in &self.bindings {
            let in_headers = request.headers.iter().any(|(_, value)| {
                contains_bytes(value, binding.placeholder.as_bytes())
                    || (binding.basic
                        && basic_credentials(value).is_some_and(|(u, p)| {
                            u == binding.placeholder || p == binding.placeholder
                        }))
            });
            let in_url = request.path_and_query.contains(&binding.placeholder);
            let in_body = request
                .body
                .is_some_and(|body| contains_bytes(body, binding.placeholder.as_bytes()));
            let bound = self
                .bindings
                .iter()
                .any(|b| b.placeholder == binding.placeholder && host_matches(&host, &b.hosts));
            if (in_headers || in_url || in_body) && !bound {
                return Err(blocked(
                    BlockReason::PlaceholderToUnboundHost,
                    Some(&binding.rule),
                ));
            }
        }

        let mut plan = Plan::default();
        for (rule_index, rule) in self.rules.iter().enumerate() {
            if !host_matches(&host, &rule.hosts) {
                continue;
            }
            for (index, (name, value)) in request.headers.iter().enumerate() {
                if *name != rule.header {
                    continue;
                }
                if let Some((username, password)) = rule.matches(value) {
                    plan.substitutions.push(Substitution {
                        header: index,
                        rule: rule_index,
                        secret: password.then(|| rule.secret.clone()),
                        username: rule
                            .username
                            .as_ref()
                            .filter(|_| username)
                            .map(|u| u.secret_env.clone()),
                    });
                }
            }
        }
        for (rule_index, rule) in self.sign.iter().enumerate() {
            if !host_matches(&host, &rule.hosts) {
                continue;
            }
            let signing = self.plan_signing(rule_index, request)?;
            plan.signings.extend(signing);
        }
        Ok(plan)
    }

    /// 署名の規則の宛先への要求で、プレースホルダーの鍵で作った署名を探す。
    fn plan_signing(
        &self,
        rule_index: usize,
        request: &Request<'_>,
    ) -> Result<Option<Signing>, Decision> {
        let rule = &self.sign[rule_index];
        let SignScheme::AwsSigV4 {
            access_key_id,
            session_token,
            ..
        } = &rule.scheme;
        let unsignable = || blocked(BlockReason::Unsignable, Some(&rule.name));
        let mut found = None;
        for (index, (name, value)) in request.headers.iter().enumerate() {
            if name != "authorization" {
                continue;
            }
            if let Some(auth) = sigv4::Authorization::parse(value)
                && auth.access_key_id == access_key_id.placeholder
            {
                if found.is_some() {
                    return Err(unsignable());
                }
                found = Some((index, auth));
            }
        }
        let Some((header, auth)) = found else {
            // 鍵のプレースホルダーがあるのに作り直せる署名がない (署名付き URL など)。
            let anywhere = request.path_and_query.contains(&access_key_id.placeholder)
                || request
                    .headers
                    .iter()
                    .any(|(_, v)| contains_bytes(v, access_key_id.placeholder.as_bytes()));
            return if anywhere {
                Err(unsignable())
            } else {
                Ok(None)
            };
        };
        // 署名したヘッダーが揃っていなければ、作り直しても通らない。
        let has = |name: &str| request.headers.iter().any(|(n, _)| n == name);
        if !has("x-amz-date") || auth.signed_header_names().any(|n| n != "host" && !has(n)) {
            return Err(unsignable());
        }
        let payload_hash = match sigv4::header_values(request.headers, "x-amz-content-sha256") {
            Some(hash) if sigv4::signs_chunks(&hash) => return Err(unsignable()),
            Some(hash) => hash,
            None => match request.body {
                Some(body) => sigv4::sha256_hex(body),
                None => return Err(unsignable()),
            },
        };
        let session_token = session_token.as_ref().is_some_and(|token| {
            request.headers.iter().any(|(n, v)| {
                n == "x-amz-security-token" && contains_bytes(v, token.placeholder.as_bytes())
            })
        });
        Ok(Some(Signing {
            header,
            rule: rule_index,
            auth,
            session_token,
            method: request.method.to_string(),
            path_and_query: request.path_and_query.to_string(),
            authority: request.host.to_string(),
            payload_hash,
        }))
    }
}

/// Basic 認証の値なら、ユーザー名とパスワードを返す。
fn basic_credentials(value: &[u8]) -> Option<(String, String)> {
    let value = std::str::from_utf8(value).ok()?;
    let (scheme, encoded) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let decoded = String::from_utf8(BASE64.decode(encoded.trim()).ok()?).ok()?;
    let (user, password) = decoded.split_once(':')?;
    Some((user.to_string(), password.to_string()))
}

impl InjectRule {
    fn is_username(&self, user: &str) -> bool {
        self.username
            .as_ref()
            .is_some_and(|u| u.placeholder == user)
    }

    /// この規則で差し替えられる値なら、ユーザー名とパスワード (`replace` では値) のそれぞれを
    /// 差し替えるかを返す。
    fn matches(&self, value: &[u8]) -> Option<(bool, bool)> {
        let (username, password) = match self.format {
            InjectFormat::BasicPassword => {
                let (user, password) = basic_credentials(value)?;
                (self.is_username(&user), password == self.placeholder)
            }
            InjectFormat::Replace => (false, contains_bytes(value, self.placeholder.as_bytes())),
        };
        (username || password).then_some((username, password))
    }

    /// `secret` (パスワードか値) と `username` の、与えられたほうを差し替える。
    fn render(
        &self,
        value: &[u8],
        secret: Option<&str>,
        username: Option<&str>,
    ) -> Option<Vec<u8>> {
        match self.format {
            InjectFormat::BasicPassword => {
                let (mut user, mut password) = basic_credentials(value)?;
                let mut changed = false;
                if let Some(real) = username.filter(|_| self.is_username(&user)) {
                    user = real.to_string();
                    changed = true;
                }
                if let Some(real) = secret.filter(|_| password == self.placeholder) {
                    password = real.to_string();
                    changed = true;
                }
                let token = BASE64.encode(format!("{user}:{password}"));
                changed.then(|| format!("Basic {token}").into_bytes())
            }
            InjectFormat::Replace => {
                let secret = secret?;
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
            "rules": [
                { "hosts": ["httpbin.org", "example.org", "github.com", "*.githubusercontent.com"], "action": "allow" },
                { "hosts": ["registry-1.docker.io", "*.docker.com"], "action": "passthrough" }
            ],
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
    fn passthrough_hosts_allow_plain_http() {
        let mut req = Req::https("registry-1.docker.io:80");
        req.sni = None;
        assert_eq!(req.eval(&policy()), Decision::Allowed { injected: vec![] });
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
    fn injects_basic_username() {
        const USER_PH: &str = "user-chloe0123";
        let config: PolicyConfig = serde_json::from_value(serde_json::json!({
            "rules": [{ "hosts": ["apt.example.com", "example.org"], "action": "allow" }],
            "inject": [{
                "name": "apt",
                "placeholder": PH,
                "hosts": ["apt.example.com"],
                "header": "authorization",
                "format": "basic-password",
                "secret_env": "PASS",
                "username": { "placeholder": USER_PH, "secret_env": "USER" }
            }]
        }))
        .unwrap();
        let p = Policy::new(config);
        let basic = |user: &str, password: &str| {
            format!("Basic {}", BASE64.encode(format!("{user}:{password}")))
        };
        let eval = |host: &'static str, value: &str| {
            let mut req = Req::https(host).header("authorization", value);
            let decision = req.eval(&p);
            (decision, req.header_value("authorization"))
        };
        let injected = Decision::Allowed {
            injected: vec!["apt".into()],
        };

        // ユーザー名とパスワードのどちらも差し替える。
        let headers = vec![("authorization".to_string(), basic(USER_PH, PH).into_bytes())];
        let (_, plan) = p.evaluate(&Request {
            host: "apt.example.com",
            sni: Some("apt.example.com"),
            tls: true,
            method: "GET",
            path_and_query: "/",
            headers: &headers,
            body: None,
        });
        assert_eq!(p.secrets_needed(&plan), ["PASS", "USER"]);
        assert_eq!(
            eval("apt.example.com", &basic(USER_PH, PH)),
            (injected.clone(), basic("real-user", "real-pass"))
        );
        // プレースホルダーでない側は、送られたまま残す。
        assert_eq!(
            eval("apt.example.com", &basic("deploy", PH)),
            (injected.clone(), basic("deploy", "real-pass"))
        );
        assert_eq!(
            eval("apt.example.com", &basic(USER_PH, "x")),
            (injected, basic("real-user", "x"))
        );
        // ユーザー名のプレースホルダーも、登録していない宛先へは送らせない。
        let (leaked, _) = eval("example.org", &basic(USER_PH, "x"));
        assert_eq!(reason(leaked), BlockReason::PlaceholderToUnboundHost);
    }

    #[test]
    fn one_placeholder_with_rules_for_different_hosts() {
        let config: PolicyConfig = serde_json::from_value(serde_json::json!({
            "rules": [{ "hosts": ["github.com", "api.github.com", "example.org"], "action": "allow" }],
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
            "rules": [
                { "name": "no-delete", "hosts": ["api.github.com"], "methods": ["DELETE"], "paths": ["/repos/**"], "action": "deny" },
                { "hosts": ["api.github.com"], "paths": ["/repos/ForteFibre/**", "/user", "/graphql"], "action": "allow" },
                { "name": "github-default", "hosts": ["api.github.com"], "action": "deny" },
                { "name": "no-push", "hosts": ["github.com"], "methods": ["POST"], "paths": ["/*/*/git-receive-pack", "/*/*.git/git-receive-pack"], "action": "deny" },
                { "hosts": ["github.com"], "action": "allow" }
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
    fn unmatched_requests_are_blocked() {
        let config: PolicyConfig = serde_json::from_value(serde_json::json!({
            "rules": [
                { "hosts": ["api.github.com"], "paths": ["/repos/acme/**"], "action": "allow" },
                { "hosts": ["*.example.com"], "methods": ["GET"], "action": "allow" }
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
        assert_eq!(eval("api.github.com", "GET", "/repos/acme/x"), allowed);
        // 規則で許した場所のほかは、同じ宛先でも通さない。
        assert_eq!(
            reason(eval("api.github.com", "GET", "/user")),
            BlockReason::NotAllowed
        );
        assert_eq!(eval("a.example.com", "GET", "/"), allowed);
        assert_eq!(
            reason(eval("a.example.com", "POST", "/")),
            BlockReason::NotAllowed
        );
        assert_eq!(
            reason(eval("example.com", "GET", "/")),
            BlockReason::NotAllowed
        );
        assert!(p.is_allowed("api.github.com"));
        assert!(!p.is_allowed("github.com"));
    }

    fn rules(value: serde_json::Value) -> Vec<PathRuleConfig> {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn passthrough_must_not_overlap_other_rules() {
        let ok = rules(serde_json::json!([
            { "hosts": ["*.docker.com"], "action": "passthrough" },
            { "hosts": ["docker.com", "github.com"], "action": "allow" }
        ]));
        check_rules(&ok).unwrap();
        for other in [
            "*.docker.com",
            "hub.docker.com",
            "*.cloudfront.docker.com",
            "HUB.docker.com",
        ] {
            let overlapping = rules(serde_json::json!([
                { "hosts": ["*.docker.com"], "action": "passthrough" },
                { "hosts": [other], "paths": ["/v2/**"], "action": "deny" }
            ]));
            assert!(check_rules(&overlapping).is_err(), "{other}");
        }
        // 中継のみの宛先どうしも重ねない。
        let twice = rules(serde_json::json!([
            { "hosts": ["*.docker.com"], "action": "passthrough" },
            { "hosts": ["auth.docker.com"], "action": "passthrough" }
        ]));
        assert!(check_rules(&twice).is_err());
        let with_path = rules(serde_json::json!([
            { "hosts": ["registry-1.docker.io"], "paths": ["/v2/**"], "action": "passthrough" }
        ]));
        assert!(check_rules(&with_path).is_err());
        let no_hosts = rules(serde_json::json!([{ "hosts": [], "action": "allow" }]));
        assert!(check_rules(&no_hosts).is_err());
    }

    /// 開発用のランナーの方針が読めて、規則が成り立っていること。
    #[test]
    fn dev_policy_is_valid() {
        let config: PolicyConfig =
            serde_json::from_str(include_str!("../../../images/guest/dev/policy.dev.json"))
                .unwrap();
        check_rules(&config.rules).unwrap();
    }

    #[test]
    fn reachable_hosts_skip_deny_rules() {
        let config = rules(serde_json::json!([
            { "hosts": ["api.github.com"], "methods": ["DELETE"], "action": "deny" },
            { "hosts": ["deny.example"], "action": "deny" },
            { "hosts": ["api.github.com", "github.com"], "action": "allow" },
            { "hosts": ["*.docker.com"], "action": "passthrough" }
        ]));
        assert_eq!(
            reachable_hosts(&config),
            ["api.github.com", "github.com", "*.docker.com"]
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
        assert_eq!(p.secrets_needed(&substitutions), ["TEST"]);
        // 値が得られなければ何も書き換えない。
        assert_eq!(
            p.apply(&mut headers, &substitutions, |_| None),
            Err("TEST".to_string())
        );
        assert_eq!(headers[0].1, format!("Bearer {PH}").into_bytes());
    }

    const SIGV4: &str = include_str!("../testdata/sigv4.json");

    fn sigv4_policy() -> (Policy, serde_json::Value) {
        let vectors: serde_json::Value = serde_json::from_str(SIGV4).unwrap();
        let ph = &vectors["placeholder"];
        let config: PolicyConfig = serde_json::from_value(serde_json::json!({
            "rules": [{ "hosts": ["*.amazonaws.com", "example.org"], "action": "allow" }],
            "sign": [{
                "name": "aws",
                "hosts": ["*.amazonaws.com"],
                "scheme": "aws-sigv4",
                "access_key_id": { "placeholder": ph["access_key_id"], "secret_env": "AKID" },
                "secret_access_key": { "placeholder": ph["secret_access_key"], "secret_env": "SECRET" },
                "session_token": { "placeholder": ph["session_token"], "secret_env": "TOKEN" }
            }]
        }))
        .unwrap();
        (Policy::new(config), vectors)
    }

    fn aws_request<'a>(
        case: &'a serde_json::Value,
        headers: &'a [(String, Vec<u8>)],
        body: Option<&'a [u8]>,
    ) -> Request<'a> {
        let host = case["authority"].as_str().unwrap();
        Request {
            host,
            sni: Some(host),
            tls: true,
            method: case["method"].as_str().unwrap(),
            path_and_query: case["path_and_query"].as_str().unwrap(),
            headers,
            body,
        }
    }

    fn case_headers(case: &serde_json::Value) -> Vec<(String, Vec<u8>)> {
        case["headers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| {
                (
                    h[0].as_str().unwrap().to_string(),
                    h[1].as_str().unwrap().as_bytes().to_vec(),
                )
            })
            .collect()
    }

    /// botocore が本物の鍵で作った署名と同じ署名に作り直す (testdata/sigv4.py)。
    #[test]
    fn resigns_aws_sigv4_like_botocore() {
        let (p, vectors) = sigv4_policy();
        let real = &vectors["real"];
        for case in vectors["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let mut headers = case_headers(case);
            let body = case["body"].as_str().unwrap().as_bytes();
            let (decision, plan) = p.evaluate(&aws_request(case, &headers, Some(body)));
            assert_eq!(
                decision,
                Decision::Allowed {
                    injected: vec!["aws".into()]
                },
                "{name}"
            );
            let mut needed = vec!["AKID", "SECRET"];
            if !case["expected_security_token"].is_null() {
                needed.push("TOKEN");
            }
            assert_eq!(p.secrets_needed(&plan), needed, "{name}");
            p.apply(&mut headers, &plan, |secret| {
                let field = match secret {
                    "AKID" => "access_key_id",
                    "SECRET" => "secret_access_key",
                    _ => "session_token",
                };
                Some(real[field].as_str().unwrap().to_string().into())
            })
            .unwrap();
            let header = |n: &str| {
                headers
                    .iter()
                    .find(|(h, _)| h == n)
                    .map(|(_, v)| String::from_utf8(v.clone()).unwrap())
            };
            assert_eq!(
                header("authorization").unwrap(),
                case["expected_authorization"].as_str().unwrap(),
                "{name}"
            );
            assert_eq!(
                header("x-amz-security-token").as_deref(),
                case["expected_security_token"].as_str(),
                "{name}"
            );
        }
    }

    #[test]
    fn blocks_what_cannot_be_resigned() {
        let (p, vectors) = sigv4_policy();
        let cases = vectors["cases"].as_array().unwrap();
        let ph = vectors["placeholder"]["access_key_id"].as_str().unwrap();
        let vanilla = &cases[0];
        let lambda = &cases[2];
        let decide = |case: &serde_json::Value, headers: &[(String, Vec<u8>)], body| {
            let (decision, plan) = p.evaluate(&aws_request(case, headers, body));
            assert!(plan.is_empty());
            decision
        };

        // 本文のハッシュが分からない (本文を調べられず、x-amz-content-sha256 もない)。
        let headers = case_headers(lambda);
        assert_eq!(
            reason(decide(lambda, &headers, None)),
            BlockReason::Unsignable
        );

        // S3 のチャンクごとの署名。
        let mut headers = case_headers(vanilla);
        headers.push((
            "x-amz-content-sha256".into(),
            b"STREAMING-AWS4-HMAC-SHA256-PAYLOAD".to_vec(),
        ));
        assert_eq!(
            reason(decide(vanilla, &headers, Some(b""))),
            BlockReason::Unsignable
        );

        // 署名したヘッダーが要求にない。
        let headers: Vec<_> = case_headers(vanilla)
            .into_iter()
            .filter(|(n, _)| n != "x-amz-date")
            .collect();
        assert_eq!(
            reason(decide(vanilla, &headers, Some(b""))),
            BlockReason::Unsignable
        );

        // 署名付き URL。
        let presigned = serde_json::json!({
            "authority": "bucket.s3.amazonaws.com",
            "method": "GET",
            "path_and_query": format!("/k?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential={ph}%2F20150830%2Fus-east-1%2Fs3%2Faws4_request"),
        });
        assert_eq!(
            reason(decide(&presigned, &[], Some(b""))),
            BlockReason::Unsignable
        );

        // 規則の宛先でなければ、どの鍵のプレースホルダーも送らせない。
        let mut elsewhere = vanilla.clone();
        elsewhere["authority"] = "example.org".into();
        let headers = case_headers(vanilla);
        assert_eq!(
            reason(decide(&elsewhere, &headers, Some(b""))),
            BlockReason::PlaceholderToUnboundHost
        );
        let secret = vectors["placeholder"]["secret_access_key"]
            .as_str()
            .unwrap();
        assert_eq!(
            reason(decide(&elsewhere, &[], Some(secret.as_bytes()))),
            BlockReason::PlaceholderToUnboundHost
        );

        // プレースホルダーの鍵を使わない署名には触らない。
        let headers = vec![(
            "authorization".to_string(),
            b"AWS4-HMAC-SHA256 Credential=AKIDOTHER/20150830/us-east-1/s3/aws4_request, SignedHeaders=host, Signature=00".to_vec(),
        )];
        assert_eq!(
            decide(vanilla, &headers, Some(b"")),
            Decision::Allowed { injected: vec![] }
        );
    }

    #[test]
    fn sign_rule_roundtrips_as_json() {
        let rule = SignRuleConfig {
            name: "aws".into(),
            hosts: vec!["*.amazonaws.com".into()],
            scheme: SignScheme::AwsSigV4 {
                access_key_id: Credential {
                    placeholder: "AKIAx".into(),
                    secret_env: "A".into(),
                },
                secret_access_key: Credential {
                    placeholder: "s".into(),
                    secret_env: "S".into(),
                },
                session_token: None,
            },
        };
        let json = serde_json::to_value(&rule).unwrap();
        assert_eq!(json["scheme"], "aws-sigv4");
        assert!(json.get("session_token").is_none());
        assert_eq!(
            serde_json::from_value::<SignRuleConfig>(json).unwrap(),
            rule
        );
    }

    #[test]
    fn plain_http_has_no_sni() {
        let p = policy();
        let mut req = Req::https("httpbin.org:80");
        req.sni = None;
        assert_eq!(req.eval(&p), Decision::Allowed { injected: vec![] });
    }
}
