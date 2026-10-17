//! シークレットの定義と、組み込みの種類。
//!
//! シークレットは一つ以上の欄 (field) を持つ。欄ごとに、VM に渡す環境変数とプレースホルダーの形を決める。
//! 欄が一つのシークレットは、その欄の名前を `value` にする。
//! 差し替えの規則 (inject) は欄の一つを宛先の決まったヘッダーで本物に差し替え、署名の規則 (sign) は
//! 欄のいくつかを鍵にして署名を作り直す。
//!
//! 組み込みの種類 ([`preset`]) は定義を登録しなくても使える。ほかの名前は、登録するときに定義を渡す。

use std::collections::{BTreeMap, BTreeSet};

use chloe_policy::InjectFormat;
use secrecy::zeroize::Zeroize as _;
use serde::{Deserialize, Serialize};

/// 欄が一つのシークレットの欄の名前。
pub const VALUE: &str = "value";

/// プレースホルダーの形のうち、乱数に置き換える部分。
pub const ID: &str = "{id}";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretSpec {
    pub fields: BTreeMap<String, FieldSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inject: Vec<InjectSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sign: Vec<SignSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldSpec {
    /// プレースホルダーを入れる VM の環境変数。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    /// プレースホルダーの形。`{id}` をワークスペースごとの乱数に置き換える
    /// (ツールが手元で形式を調べることがあるので、本物に似せる)。
    pub placeholder: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InjectSpec {
    #[serde(default = "value_field", skip_serializing_if = "is_value_field")]
    pub field: String,
    pub hosts: Vec<String>,
    pub header: String,
    #[serde(default)]
    pub format: InjectFormat,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignSpec {
    pub scheme: SignKind,
    pub hosts: Vec<String>,
}

/// 署名の方式。方式ごとに使う欄の名前が決まっている ([`SignKind::fields`])。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SignKind {
    #[serde(rename = "aws-sigv4")]
    AwsSigV4,
}

impl SignKind {
    /// 欠かせない欄と、あれば使う欄。
    pub fn fields(self) -> (&'static [&'static str], &'static [&'static str]) {
        match self {
            Self::AwsSigV4 => (&["access_key_id", "secret_access_key"], &["session_token"]),
        }
    }
}

fn value_field() -> String {
    VALUE.into()
}

fn is_value_field(field: &str) -> bool {
    field == VALUE
}

impl SecretSpec {
    /// 欄が `value` だけなら、値はそのままの文字列で置く。ほかは欄の名前から値への JSON で置く。
    pub fn is_single(&self) -> bool {
        self.fields.len() == 1 && self.fields.contains_key(VALUE)
    }

    /// 保存した値から欄の値を取り出す。
    pub fn field_value(&self, stored: &str, field: &str) -> Option<String> {
        if self.is_single() {
            return (field == VALUE).then(|| stored.to_string());
        }
        let mut values: BTreeMap<String, String> = serde_json::from_str(stored).ok()?;
        let value = values.remove(field);
        // 使わない欄の値も、捨てる前に消す。
        for other in values.values_mut() {
            other.zeroize();
        }
        value
    }

    /// 欄ごとの値を保存する形にする。欄が定義と合わなければ失敗にする。
    pub fn encode_values(&self, values: &BTreeMap<String, String>) -> Result<String, String> {
        let given: BTreeSet<&String> = values.keys().collect();
        let defined: BTreeSet<&String> = self.fields.keys().collect();
        if given != defined {
            return Err(format!(
                "give a value for each field: {}",
                self.fields.keys().cloned().collect::<Vec<_>>().join(", ")
            ));
        }
        if let Some((field, _)) = values.iter().find(|(_, v)| v.is_empty()) {
            return Err(format!("the value of {field} is empty"));
        }
        if self.is_single() {
            Ok(values[VALUE].clone())
        } else {
            serde_json::to_string(values).map_err(|e| e.to_string())
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.fields.is_empty() {
            return Err("define at least one field".into());
        }
        for (field, spec) in &self.fields {
            check_name(field).map_err(|e| format!("field {field}: {e}"))?;
            check_template(&spec.placeholder).map_err(|e| format!("field {field}: {e}"))?;
            for env in &spec.env {
                check_env(env).map_err(|e| format!("field {field}: {e}"))?;
            }
        }
        let mut envs = BTreeSet::new();
        for env in self.fields.values().flat_map(|f| &f.env) {
            if !envs.insert(env) {
                return Err(format!("environment variable {env} is given twice"));
            }
        }
        if self.inject.is_empty() && self.sign.is_empty() {
            return Err("define at least one inject or sign rule".into());
        }
        for rule in &self.inject {
            if !self.fields.contains_key(&rule.field) {
                return Err(format!("inject: no such field: {}", rule.field));
            }
            check_hosts(&rule.hosts)?;
            check_header(&rule.header)?;
        }
        for rule in &self.sign {
            check_hosts(&rule.hosts)?;
            let (required, _) = rule.scheme.fields();
            for field in required {
                if !self.fields.contains_key(*field) {
                    return Err(format!(
                        "sign: {} needs the field {field}",
                        rule.scheme.name()
                    ));
                }
            }
        }
        Ok(())
    }
}

impl SignKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::AwsSigV4 => "aws-sigv4",
        }
    }
}

/// 登録するときに渡す、定義と値。
///
/// 欄が一つなら `env`、`placeholder`、`value` を直接書き、欄がいくつもあれば `fields` に書く。
/// 定義の項目 (環境変数、プレースホルダー、規則) を何も書かなければ、値だけを変える。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretInput {
    #[serde(default)]
    pub env: Vec<String>,
    pub placeholder: Option<String>,
    pub value: Option<String>,
    #[serde(default)]
    pub fields: BTreeMap<String, FieldInput>,
    #[serde(default)]
    pub inject: Vec<InjectSpec>,
    #[serde(default)]
    pub sign: Vec<SignSpec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldInput {
    #[serde(default)]
    pub env: Vec<String>,
    pub placeholder: Option<String>,
    pub value: String,
}

impl SecretInput {
    /// 値だけのもの。
    pub fn value(value: impl Into<String>) -> Self {
        Self {
            value: Some(value.into()),
            ..Default::default()
        }
    }

    /// 定義 (値だけを変えるなら `None`) と、欄ごとの値に分ける。
    /// プレースホルダーの形を書かなければ `{id}` にする。
    pub fn into_parts(self) -> Result<(Option<SecretSpec>, BTreeMap<String, String>), String> {
        let fields: Vec<(String, Vec<String>, Option<String>, String)> = if self.fields.is_empty() {
            let value = self.value.ok_or("give the value")?;
            vec![(VALUE.into(), self.env, self.placeholder, value)]
        } else {
            if !self.env.is_empty() || self.placeholder.is_some() || self.value.is_some() {
                return Err(
                    "with fields, write env, placeholder and value inside each field".into(),
                );
            }
            self.fields
                .into_iter()
                .map(|(name, f)| (name, f.env, f.placeholder, f.value))
                .collect()
        };
        let defines = !self.inject.is_empty()
            || !self.sign.is_empty()
            || fields
                .iter()
                .any(|(_, env, placeholder, _)| !env.is_empty() || placeholder.is_some());
        let mut values = BTreeMap::new();
        let mut spec_fields = BTreeMap::new();
        for (name, env, placeholder, value) in fields {
            values.insert(name.clone(), value);
            spec_fields.insert(
                name,
                FieldSpec {
                    env,
                    placeholder: placeholder.unwrap_or_else(|| ID.into()),
                },
            );
        }
        let spec = defines.then_some(SecretSpec {
            fields: spec_fields,
            inject: self.inject,
            sign: self.sign,
        });
        Ok((spec, values))
    }
}

/// シークレットと欄の名前。英小文字で始まり、英小文字、数字、`_` からなる。
/// `__` を含まず `_` で終わらないので、[`secret_ref`] から名前と欄を一通りに読み戻せる。
pub fn check_name(name: &str) -> Result<(), String> {
    let valid = name.len() <= 64
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !name.contains("__")
        && !name.ends_with('_');
    if valid {
        Ok(())
    } else {
        Err(format!(
            "invalid name {name:?} (use a-z, 0-9 and single underscores, starting with a letter)"
        ))
    }
}

fn check_template(template: &str) -> Result<(), String> {
    let literal = template.replacen(ID, "", 1);
    let valid = template.matches(ID).count() == 1
        && template.len() <= 128
        && literal
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.~+/=:".contains(c));
    if valid {
        Ok(())
    } else {
        Err(format!(
            "invalid placeholder {template:?} (include {ID} once; other characters: A-Z a-z 0-9 - _ . ~ + / = :)"
        ))
    }
}

fn check_env(env: &str) -> Result<(), String> {
    let valid = env.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && env.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && env.len() <= 128;
    if !valid {
        return Err(format!("invalid environment variable {env:?}"));
    }
    if env.starts_with("CHLOE_") {
        return Err(format!("environment variable {env} is reserved"));
    }
    Ok(())
}

fn check_hosts(hosts: &[String]) -> Result<(), String> {
    if hosts.is_empty() {
        return Err("give at least one host for each rule".into());
    }
    for host in hosts {
        let name = host.strip_prefix("*.").unwrap_or(host);
        let valid = !name.is_empty()
            && name.len() <= 253
            && name.split('.').all(|label| {
                !label.is_empty() && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            });
        if !valid {
            return Err(format!("invalid host {host:?}"));
        }
        if name.eq_ignore_ascii_case(chloe_policy::CA_HOST) {
            return Err(format!("host {host} is reserved"));
        }
    }
    Ok(())
}

fn check_header(header: &str) -> Result<(), String> {
    let valid = !header.is_empty()
        && header
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c));
    if valid {
        Ok(())
    } else {
        Err(format!("invalid header name {header:?}"))
    }
}

/// 組み込みの種類の名前。
pub const PRESETS: &[&str] = &[
    "anthropic_api_key",
    "claude_oauth_token",
    "openai_api_key",
    "github_token",
];

/// 組み込みの種類の定義。
pub fn preset(name: &str) -> Option<SecretSpec> {
    let single = |env: &[&str], placeholder: &str, inject: Vec<InjectSpec>| SecretSpec {
        fields: BTreeMap::from([(
            VALUE.to_string(),
            FieldSpec {
                env: env.iter().map(|e| e.to_string()).collect(),
                placeholder: placeholder.into(),
            },
        )]),
        inject,
        sign: vec![],
    };
    let rule = |hosts: &[&str], header: &str, format| InjectSpec {
        field: VALUE.into(),
        hosts: hosts.iter().map(|h| h.to_string()).collect(),
        header: header.into(),
        format,
    };
    let spec = match name {
        "anthropic_api_key" => single(
            &["ANTHROPIC_API_KEY"],
            "sk-ant-api03-{id}",
            vec![rule(
                &["api.anthropic.com"],
                "x-api-key",
                InjectFormat::Replace,
            )],
        ),
        "claude_oauth_token" => single(
            &["CLAUDE_CODE_OAUTH_TOKEN"],
            "sk-ant-oat01-{id}",
            vec![rule(
                &["api.anthropic.com"],
                "authorization",
                InjectFormat::Replace,
            )],
        ),
        // codex exec は CODEX_API_KEY を読む。
        "openai_api_key" => single(
            &["OPENAI_API_KEY", "CODEX_API_KEY"],
            "sk-proj-{id}",
            vec![rule(
                &["api.openai.com"],
                "authorization",
                InjectFormat::Replace,
            )],
        ),
        // GitHub App のユーザートークンの接頭辞。
        "github_token" => single(
            &["GH_TOKEN", "GITHUB_TOKEN"],
            "ghu_{id}",
            vec![
                rule(
                    &["github.com"],
                    "authorization",
                    InjectFormat::BasicPassword,
                ),
                rule(
                    &["api.github.com", "uploads.github.com"],
                    "authorization",
                    InjectFormat::Replace,
                ),
            ],
        ),
        _ => return None,
    };
    Some(spec)
}

/// ランナーのプロキシが本物の値を問い合わせるときの名前 (開発用のプロキシでは環境変数の名前にもなる)。
/// 欄が `value` なら `CHLOE_SECRET_<名前>`、ほかは `CHLOE_SECRET_<名前>__<欄>`。
pub fn secret_ref(name: &str, field: &str) -> String {
    let name = name.to_ascii_uppercase();
    if field == VALUE {
        format!("CHLOE_SECRET_{name}")
    } else {
        format!("CHLOE_SECRET_{name}__{}", field.to_ascii_uppercase())
    }
}

/// [`secret_ref`] の名前を、シークレットと欄の名前に読み戻す。
pub fn parse_secret_ref(reference: &str) -> Option<(String, String)> {
    let rest = reference.strip_prefix("CHLOE_SECRET_")?;
    let (name, field) = rest.split_once("__").unwrap_or((rest, "VALUE"));
    let (name, field) = (name.to_ascii_lowercase(), field.to_ascii_lowercase());
    check_name(&name).ok()?;
    check_name(&field).ok()?;
    (secret_ref(&name, &field) == reference).then_some((name, field))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aws() -> SecretInput {
        toml::from_str(
            r#"
            [fields.access_key_id]
            env = ["AWS_ACCESS_KEY_ID"]
            placeholder = "AKIA{id}"
            value = "AKIDEXAMPLE"
            [fields.secret_access_key]
            env = ["AWS_SECRET_ACCESS_KEY"]
            value = "secret"
            [[sign]]
            scheme = "aws-sigv4"
            hosts = ["*.amazonaws.com"]
            "#,
        )
        .unwrap()
    }

    #[test]
    fn presets_are_valid() {
        for name in PRESETS {
            check_name(name).unwrap();
            preset(name).unwrap().validate().unwrap();
        }
        assert!(preset("unknown").is_none());
    }

    #[test]
    fn refs_roundtrip() {
        assert_eq!(
            secret_ref("anthropic_api_key", VALUE),
            "CHLOE_SECRET_ANTHROPIC_API_KEY"
        );
        assert_eq!(
            secret_ref("aws", "secret_access_key"),
            "CHLOE_SECRET_AWS__SECRET_ACCESS_KEY"
        );
        for (name, field) in [("a", VALUE), ("aws_prod", "access_key_id"), ("x1", "y")] {
            assert_eq!(
                parse_secret_ref(&secret_ref(name, field)),
                Some((name.to_string(), field.to_string()))
            );
        }
        for bad in [
            "CHLOE_SECRET_",
            "CHLOE_SECRET_A___B",
            "OTHER",
            "CHLOE_SECRET_a",
        ] {
            assert_eq!(parse_secret_ref(bad), None, "{bad}");
        }
    }

    #[test]
    fn names() {
        for good in ["hf_token", "a", "aws2"] {
            check_name(good).unwrap();
        }
        for bad in ["", "Hf", "1a", "a__b", "a_", "a-b", "_a"] {
            assert!(check_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn single_field_input() {
        let input: SecretInput = toml::from_str(
            r#"
            env = ["HF_TOKEN"]
            placeholder = "hf_{id}"
            value = "hf_real"
            [[inject]]
            hosts = ["huggingface.co"]
            header = "Authorization"
            "#,
        )
        .unwrap();
        let (spec, values) = input.into_parts().unwrap();
        let spec = spec.unwrap();
        spec.validate().unwrap();
        assert!(spec.is_single());
        assert_eq!(spec.inject[0].field, VALUE);
        let stored = spec.encode_values(&values).unwrap();
        assert_eq!(stored, "hf_real");
        assert_eq!(spec.field_value(&stored, VALUE).as_deref(), Some("hf_real"));

        // 値だけなら定義は変えない。
        let (spec, values) = SecretInput::value("new").into_parts().unwrap();
        assert!(spec.is_none());
        assert_eq!(values[VALUE], "new");
    }

    #[test]
    fn multi_field_input() {
        let (spec, values) = aws().into_parts().unwrap();
        let spec = spec.unwrap();
        spec.validate().unwrap();
        assert!(!spec.is_single());
        assert_eq!(spec.fields["secret_access_key"].placeholder, ID);
        let stored = spec.encode_values(&values).unwrap();
        assert_eq!(
            spec.field_value(&stored, "access_key_id").as_deref(),
            Some("AKIDEXAMPLE")
        );
        assert_eq!(spec.field_value(&stored, "missing"), None);

        let mut short = values.clone();
        short.remove("secret_access_key");
        assert!(spec.encode_values(&short).is_err());

        // 署名の方式が使う欄が欠けている。
        let mut broken = spec.clone();
        broken.fields.remove("secret_access_key");
        assert!(broken.validate().is_err());

        // 定義は JSON で置いて読み戻せる。
        let json = serde_json::to_string(&spec).unwrap();
        assert_eq!(serde_json::from_str::<SecretSpec>(&json).unwrap(), spec);
    }

    #[test]
    fn rejects_invalid_specs() {
        let base = || {
            let input: SecretInput = toml::from_str(
                r#"
                env = ["HF_TOKEN"]
                placeholder = "hf_{id}"
                value = "v"
                [[inject]]
                hosts = ["huggingface.co"]
                header = "authorization"
                "#,
            )
            .unwrap();
            input.into_parts().unwrap().0.unwrap()
        };
        type Change = Box<dyn Fn(&mut SecretSpec)>;
        let cases: Vec<(&str, Change)> = vec![
            (
                "no {id}",
                Box::new(|s| s.fields.get_mut(VALUE).unwrap().placeholder = "hf_".into()),
            ),
            (
                "two {id}",
                Box::new(|s| s.fields.get_mut(VALUE).unwrap().placeholder = "{id}{id}".into()),
            ),
            (
                "space",
                Box::new(|s| s.fields.get_mut(VALUE).unwrap().placeholder = "a {id}".into()),
            ),
            (
                "env",
                Box::new(|s| s.fields.get_mut(VALUE).unwrap().env = vec!["1X".into()]),
            ),
            (
                "reserved env",
                Box::new(|s| s.fields.get_mut(VALUE).unwrap().env = vec!["CHLOE_X".into()]),
            ),
            ("no rules", Box::new(|s| s.inject.clear())),
            ("no hosts", Box::new(|s| s.inject[0].hosts.clear())),
            (
                "bad host",
                Box::new(|s| s.inject[0].hosts = vec!["a.*.com".into()]),
            ),
            (
                "ca host",
                Box::new(|s| s.inject[0].hosts = vec![chloe_policy::CA_HOST.into()]),
            ),
            (
                "bad header",
                Box::new(|s| s.inject[0].header = "x y".into()),
            ),
            (
                "unknown field",
                Box::new(|s| s.inject[0].field = "other".into()),
            ),
        ];
        base().validate().unwrap();
        for (name, change) in cases {
            let mut spec = base();
            change(&mut spec);
            assert!(spec.validate().is_err(), "{name}");
        }
        // 欄と直接の値を混ぜられない。
        let mut mixed = aws();
        mixed.value = Some("x".into());
        assert!(mixed.into_parts().is_err());
    }
}
