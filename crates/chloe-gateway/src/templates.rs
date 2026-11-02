//! シークレットの雛形 (`deploy/secrets/*.toml`)。管理の TUI と、管理 API の
//! `/api/v1/secret-templates` (Web の画面) が使う。

use chloe_controller::catalog::{SecretInput, SecretSpec};

/// 雛形の名前と、TOML (`chloe secret set --spec` に渡す形。値は例)。
pub const TEMPLATES: &[(&str, &str)] = &[
    (
        "anthropic_api_key",
        include_str!("../../../deploy/secrets/anthropic_api_key.toml"),
    ),
    (
        "claude_oauth_token",
        include_str!("../../../deploy/secrets/claude_oauth_token.toml"),
    ),
    (
        "openai_api_key",
        include_str!("../../../deploy/secrets/openai_api_key.toml"),
    ),
    ("aws", include_str!("../../../deploy/secrets/aws.toml")),
    ("apt", include_str!("../../../deploy/secrets/apt.toml")),
];

/// 雛形の定義 (値を除いたもの)。読めない雛形は、テストで見つける。
pub fn template_specs() -> Vec<(&'static str, SecretSpec)> {
    TEMPLATES
        .iter()
        .filter_map(|(name, text)| Some((*name, parse(text).ok()?)))
        .collect()
}

fn parse(text: &str) -> Result<SecretSpec, String> {
    let input: SecretInput = toml::from_str(text).map_err(|e| e.to_string())?;
    let (spec, _) = input.into_parts()?;
    spec.ok_or_else(|| "the template has no definition".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_template_is_a_valid_definition() {
        for (name, text) in TEMPLATES {
            parse(text).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
        assert_eq!(template_specs().len(), TEMPLATES.len());
    }
}
