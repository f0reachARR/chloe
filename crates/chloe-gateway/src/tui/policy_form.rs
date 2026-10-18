//! 規則と設定のフォーム。`chloe policy set` に渡す TOML と同じ形 ([`PolicyLayer`]) にする。
//!
//! 規則は Rules タブで、スコープ (個人、Organization、team、ワークスペース) ごとに編集する
//! ([`rules_form`])。Settings タブは、規則のほか (ルートの扱いとトークンの範囲) を編集する
//! ([`form`])。

use chloe_controller::runner::PolicyLayer;
use chloe_policy::PathRuleConfig;
use serde_json::{Map, Value, json};

use super::form::{Field, Form, RowKind};

const INHERIT: &str = "(not set)";
const ALL: &str = "all";
const LISTED: &str = "listed";

/// スコープ自身の規則のフォーム。
pub fn rules_form(scope: &str, rules: &[PathRuleConfig]) -> Form {
    let mut form = Form::new(
        format!("Rules of {scope}"),
        vec![
            Field::note("Scope", scope),
            Field::rows("rules", "Rules", RULE_ROW, vec![]).help(
                "Allow or deny by host, method and path. The upper layers' rules are checked first; \
                 requests no rule matches are blocked.",
            ),
        ],
    );
    if let Ok(json) = serde_json::to_value(rules) {
        fill_rules(&mut form, &json);
    }
    form
}

/// 設定のフォーム。`inherited` は上の層 (team なら Organization) の名前と設定。個人の設定
/// (`personal`) には、トークンの範囲の欄を出さない。
pub fn form(
    owner: &str,
    personal: bool,
    layer: Option<&PolicyLayer>,
    inherited: &[(String, PolicyLayer)],
) -> Form {
    let mut fields = vec![Field::note("Owner", owner)];
    for (name, upper) in inherited {
        fields.push(Field::note(format!("From {name}").as_str(), summary(upper)));
    }
    fields.push(
        Field::choice(
            "root",
            "Root default",
            &[INHERIT, "ephemeral", "persistent"],
            INHERIT,
        )
        .help("Whether new workspaces keep changes to the root file system."),
    );
    if !personal {
        fields.extend([
            Field::choice("repos", "GitHub repos", &[ALL, LISTED], ALL)
                .help("Repositories the installation token of shared workspaces can use."),
            Field::text("repositories", "Repositories", "")
                .shown_when("repos", LISTED)
                .help("Repository names without the organization, separated by commas."),
            Field::rows("permissions", "GitHub perms", PERMISSION_ROW, vec![]).help(
                "Narrow the token's permissions (e.g. contents = read). None means all of the app's.",
            ),
        ]);
    }
    let mut form = Form::new(format!("Settings of {owner}"), fields);
    if let Some(layer) = layer
        && let Ok(json) = serde_json::to_value(layer)
    {
        fill(&mut form, &json);
    }
    form
}

const RULE_ROW: RowKind = RowKind {
    make: || rule_row("", &[], &[], &[], "allow"),
    summary: |row| {
        let name = row.text("name");
        let target = format!("{} {}", row.choice("action"), row.text("hosts"));
        if name.is_empty() {
            target
        } else {
            format!("{name}: {target}")
        }
    },
};

fn rule_row(
    name: &str,
    hosts: &[String],
    methods: &[String],
    paths: &[String],
    action: &str,
) -> Form {
    Form::new(
        "Rule",
        vec![
            Field::text("name", "Name", name).help("Shown in the logs."),
            Field::text("hosts", "Hosts", &hosts.join(", ")),
            Field::text("methods", "Methods", &methods.join(", "))
                .help("e.g. GET, POST. Empty matches every method."),
            Field::text("paths", "Paths", &paths.join(", "))
                .help("e.g. /repos/*. Empty matches every path."),
            Field::choice("action", "Action", &["allow", "deny"], action),
        ],
    )
}

const PERMISSION_ROW: RowKind = RowKind {
    make: || permission_row("", "read"),
    summary: |row| format!("{} = {}", row.text("name"), row.choice("level")),
};

fn permission_row(name: &str, level: &str) -> Form {
    Form::new(
        "Permission",
        vec![
            Field::text("name", "Permission", name).help("e.g. contents, pull_requests, issues"),
            Field::choice("level", "Level", &["read", "write"], level),
        ],
    )
}

/// 上の層の設定を一行にする。
fn summary(layer: &PolicyLayer) -> String {
    let mut parts = Vec::new();
    if let Some(root) = layer.root {
        parts.push(format!("root {}", root.as_str()));
    }
    if layer.github.is_some() {
        parts.push("GitHub scope".into());
    }
    if parts.is_empty() {
        "(nothing)".into()
    } else {
        parts.join("; ")
    }
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// 規則の一覧 (JSON の配列) を、規則の欄の行にする。
fn fill_rules(form: &mut Form, rules: &Value) {
    let rows = rules
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            rule_row(
                r["name"].as_str().unwrap_or(""),
                &strings(&r["hosts"]),
                &strings(&r["methods"]),
                &strings(&r["paths"]),
                r["action"].as_str().unwrap_or("deny"),
            )
        })
        .collect();
    set_rows(form, "rules", rows);
}

fn fill(form: &mut Form, json: &Value) {
    if let Some(root) = json["root"].as_str() {
        form.set_choice("root", root);
    }
    let github = &json["github"];
    if let Some(repositories) = github.get("repositories") {
        form.set_choice("repos", LISTED);
        form.set_text("repositories", &strings(repositories).join(", "));
    }
    let permissions = github
        .get("permissions")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(name, level)| permission_row(name, level.as_str().unwrap_or("read")))
        .collect();
    set_rows(form, "permissions", permissions);
}

fn set_rows(form: &mut Form, key: &str, rows: Vec<Form>) {
    if let Some(index) = form.fields.iter().position(|f| f.key == key) {
        form.put_rows(index, rows);
    }
}

/// 規則のフォームの規則。
pub fn to_rules(form: &Form) -> Result<Vec<PathRuleConfig>, String> {
    let mut rules = Vec::new();
    for row in form.rows("rules") {
        let hosts = row.list("hosts");
        if hosts.is_empty() {
            return Err("every rule needs hosts".into());
        }
        let mut rule = Map::new();
        let name = row.text("name");
        if !name.is_empty() {
            rule.insert("name".into(), json!(name));
        }
        rule.insert("hosts".into(), json!(hosts));
        rule.insert(
            "methods".into(),
            json!(
                row.list("methods")
                    .iter()
                    .map(|m| m.to_ascii_uppercase())
                    .collect::<Vec<_>>()
            ),
        );
        rule.insert("paths".into(), json!(row.list("paths")));
        rule.insert("action".into(), json!(row.choice("action")));
        rules.push(Value::Object(rule));
    }
    serde_json::from_value(Value::Array(rules)).map_err(|e| format!("invalid rules: {e}"))
}

/// 設定のフォームの設定。規則は持たない (保存するときに、今の規則を残す)。
pub fn to_layer(form: &Form) -> Result<PolicyLayer, String> {
    let mut json = Map::new();
    let root = form.choice("root");
    if root != INHERIT {
        json.insert("root".into(), json!(root));
    }
    let mut github = Map::new();
    if form.choice("repos") == LISTED {
        let repositories = form.list("repositories");
        if repositories.is_empty() {
            return Err("list the repositories, or choose all".into());
        }
        github.insert("repositories".into(), json!(repositories));
    }
    let mut permissions = Map::new();
    for row in form.rows("permissions") {
        let name = row.text("name");
        if name.is_empty() {
            return Err("every GitHub permission needs a name".into());
        }
        permissions.insert(name, json!(row.choice("level")));
    }
    if !permissions.is_empty() {
        github.insert("permissions".into(), Value::Object(permissions));
    }
    if !github.is_empty() {
        json.insert("github".into(), Value::Object(github));
    }
    serde_json::from_value(Value::Object(json)).map_err(|e| format!("invalid settings: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_settings() {
        let layer: PolicyLayer = toml::from_str(
            r#"
            root = "persistent"
            [github]
            repositories = ["tools"]
            permissions = { contents = "read" }
            "#,
        )
        .unwrap();
        let form = form("acme", false, Some(&layer), &[]);
        assert_eq!(form.choice("repos"), LISTED);
        assert_eq!(to_layer(&form).unwrap(), layer);
        assert_eq!(
            to_layer(&super::form("acme", false, None, &[])).unwrap(),
            PolicyLayer::default()
        );
        // 個人の設定には、トークンの範囲の欄がない。
        let personal = super::form("alice", true, None, &[]);
        assert!(!personal.fields.iter().any(|f| f.key == "repos"));
        assert_eq!(to_layer(&personal).unwrap(), PolicyLayer::default());
    }

    #[test]
    fn round_trips_rules() {
        let layer: PolicyLayer = toml::from_str(
            r#"
            [[rules]]
            hosts = ["example.org", "pypi.org"]
            action = "allow"
            [[rules]]
            name = "no-push"
            hosts = ["github.com"]
            methods = ["POST"]
            paths = ["/*/git-receive-pack"]
            action = "deny"
            "#,
        )
        .unwrap();
        let form = rules_form("acme", &layer.rules);
        assert_eq!(to_rules(&form).unwrap(), layer.rules);
        assert!(to_rules(&rules_form("acme", &[])).unwrap().is_empty());
    }

    #[test]
    fn shows_inherited_settings() {
        let org = PolicyLayer {
            root: Some(chloe_controller::workspace::RootMode::Persistent),
            ..Default::default()
        };
        let form = form("acme/dev", false, None, &[("acme".into(), org)]);
        assert!(form.fields.iter().any(|f| f.label == "From acme"));
    }

    #[test]
    fn rejects_incomplete_rows() {
        let mut form = form("acme", false, None, &[]);
        form.set_choice("repos", LISTED);
        assert!(to_layer(&form).is_err());
        let mut rules = rules_form("acme", &[]);
        set_rows(&mut rules, "rules", vec![(RULE_ROW.make)()]);
        assert!(to_rules(&rules).is_err());
    }
}
