//! 宛先、メソッド、パスで要求を許すか拒否するかを決める規則。
//!
//! 規則は上から順に評価し、最初に一致した規則の `action` で決める。どれにも一致しなければ、
//! 規則では決めない (許可リストだけで判定する)。
//!
//! パスはグロブで書く。`*` は `/` を含まない任意の文字列に、一つの階層まるごとの `**` は
//! 0 個以上の階層に一致する。照合の前にパスを正規化し (パーセントエンコードを戻し、`//`、
//! `.`、`..` を解決する)、大文字と小文字は区別しない (GitHub の所有者とリポジトリの名前は
//! 大文字と小文字を区別しない)。クエリは照合に含めない。
//! 中継のみの宛先は復号しないので、規則は効かない。

use serde::{Deserialize, Serialize};

use crate::host_matches;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathRuleConfig {
    /// ログに残す名前。
    #[serde(default)]
    pub name: Option<String>,
    pub hosts: Vec<String>,
    /// 空ならすべてのメソッドに一致する。
    #[serde(default)]
    pub methods: Vec<String>,
    /// 空ならすべてのパスに一致する。
    #[serde(default)]
    pub paths: Vec<String>,
    pub action: Action,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Allow,
    Deny,
}

pub(crate) struct PathRule {
    pub name: String,
    hosts: Vec<String>,
    methods: Vec<String>,
    paths: Vec<Vec<String>>,
    pub action: Action,
}

impl PathRule {
    pub fn new(index: usize, config: &PathRuleConfig) -> Self {
        Self {
            name: config
                .name
                .clone()
                .unwrap_or_else(|| format!("rule-{index}")),
            hosts: config.hosts.clone(),
            methods: config
                .methods
                .iter()
                .map(|m| m.to_ascii_uppercase())
                .collect(),
            paths: config
                .paths
                .iter()
                .map(|p| segments(&p.to_ascii_lowercase()))
                .collect(),
            action: config.action,
        }
    }

    /// `path` は正規化して小文字にした階層の列 ([`normalize`])。
    pub fn matches(&self, host: &str, method: &str, path: &[String]) -> bool {
        host_matches(host, &self.hosts)
            && (self.methods.is_empty() || self.methods.iter().any(|m| m == method))
            && (self.paths.is_empty() || self.paths.iter().any(|p| glob(p, path)))
    }
}

/// パス (クエリを含んでいてもよい) を正規化し、小文字にした階層の列にする。
/// `..` で根より上に出ようとしたら `None` を返す。
pub(crate) fn normalize(path_and_query: &str) -> Option<Vec<String>> {
    let path = path_and_query.split(['?', '#']).next().unwrap_or_default();
    let mut out: Vec<String> = Vec::new();
    for segment in path.split('/') {
        let segment = percent_decode(segment)?.to_ascii_lowercase();
        match segment.as_str() {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            _ => out.push(segment),
        }
    }
    Some(out)
}

fn segments(pattern: &str) -> Vec<String> {
    pattern
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// `/` を除くパーセントエンコードを戻す。`%2F` は階層の区切りと区別するため戻さない。
fn percent_decode(segment: &str) -> Option<String> {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            let byte = u8::from_str_radix(hex, 16).ok()?;
            if byte == b'/' {
                out.extend_from_slice(b"%2f");
            } else {
                out.push(byte);
            }
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// 階層の列どうしのグロブの照合。
fn glob(pattern: &[String], path: &[String]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((first, rest)) if first == "**" => {
            (0..=path.len()).any(|skip| glob(rest, &path[skip..]))
        }
        Some((first, rest)) => match path.split_first() {
            Some((segment, path_rest)) => segment_matches(first, segment) && glob(rest, path_rest),
            None => false,
        },
    }
}

/// 一つの階層の照合。`*` は任意の文字列に一致する。
fn segment_matches(pattern: &str, segment: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == segment;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !segment.starts_with(first) || !segment[first.len()..].ends_with(last) {
        return false;
    }
    let mut rest = &segment[first.len()..segment.len() - last.len()];
    for part in &parts[1..parts.len() - 1] {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(p: &str) -> Vec<String> {
        normalize(p).unwrap()
    }

    fn matches(pattern: &str, p: &str) -> bool {
        glob(&segments(&pattern.to_ascii_lowercase()), &path(p))
    }

    #[test]
    fn normalizes_paths() {
        assert_eq!(path("/repos//a/./b/../c?x=1"), ["repos", "a", "c"]);
        assert_eq!(path("/Repos/Forte%46ibre"), ["repos", "fortefibre"]);
        assert_eq!(path("/a%2Fb"), ["a%2fb"]);
        assert!(normalize("/../etc").is_none());
        assert!(normalize("/%zz").is_none());
    }

    #[test]
    fn glob_matching() {
        assert!(matches("/repos/*/*", "/repos/a/b"));
        assert!(!matches("/repos/*/*", "/repos/a/b/c"));
        assert!(matches(
            "/repos/ForteFibre/**",
            "/repos/fortefibre/chloe/pulls/1"
        ));
        assert!(matches("/repos/ForteFibre/**", "/repos/ForteFibre"));
        assert!(!matches("/repos/ForteFibre/**", "/repos/other/chloe"));
        assert!(matches(
            "/*/*.git/git-receive-pack",
            "/ForteFibre/chloe.git/git-receive-pack"
        ));
        assert!(!matches(
            "/*/*.git/git-receive-pack",
            "/ForteFibre/chloe/git-receive-pack"
        ));
        assert!(matches(
            "/**/info/refs",
            "/a/b.git/info/refs?service=git-upload-pack"
        ));
        assert!(matches("/a*b*c", "/aXXbYYc"));
        assert!(!matches("/a*b*c", "/aXXcYYb"));
        assert!(matches("/", "/"));
        assert!(matches("/**", "/anything/at/all"));
    }

    #[test]
    fn rules_match_host_method_and_path() {
        let rule = PathRule::new(
            0,
            &PathRuleConfig {
                name: None,
                hosts: vec!["api.github.com".into()],
                methods: vec!["delete".into()],
                paths: vec!["/repos/*/*".into()],
                action: Action::Deny,
            },
        );
        assert_eq!(rule.name, "rule-0");
        assert!(rule.matches("api.github.com", "DELETE", &path("/repos/a/b")));
        assert!(!rule.matches("api.github.com", "GET", &path("/repos/a/b")));
        assert!(!rule.matches("github.com", "DELETE", &path("/repos/a/b")));
        assert!(!rule.matches("api.github.com", "DELETE", &path("/user")));
    }
}
