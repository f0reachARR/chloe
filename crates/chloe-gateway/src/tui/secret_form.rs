//! シークレットのフォーム。定義 (環境変数、プレースホルダー、差し替えと署名の規則、VM に置くファイル) と値を
//! 欄で入力し、`chloe secret set --spec` に渡す TOML と同じ形 ([`SecretInput`]) にする。

use chloe_controller::catalog::{SecretInput, SecretSpec};
use serde_json::{Map, Value, json};

use super::form::{Field, Form, RowKind};

/// 雛形 (`deploy/secrets/`)。名前とその TOML。
pub const TEMPLATES: &[(&str, &str)] = &[
    (
        "anthropic_api_key",
        include_str!("../../../../deploy/secrets/anthropic_api_key.toml"),
    ),
    (
        "claude_oauth_token",
        include_str!("../../../../deploy/secrets/claude_oauth_token.toml"),
    ),
    (
        "openai_api_key",
        include_str!("../../../../deploy/secrets/openai_api_key.toml"),
    ),
    ("aws", include_str!("../../../../deploy/secrets/aws.toml")),
    ("apt", include_str!("../../../../deploy/secrets/apt.toml")),
];

const SINGLE: &str = "single value";
const FIELDS: &str = "several fields";

/// 新しいシークレットのフォーム。`template` があれば、その雛形で欄を埋める (値は埋めない)。
pub fn new_form(owner: &str, template: Option<&str>) -> Form {
    let mut form = form(owner, None);
    if let Some((name, text)) = template.and_then(|t| TEMPLATES.iter().find(|(n, _)| *n == t)) {
        form.set_text("name", name);
        if let Ok(value) = toml::from_str::<toml::Value>(text)
            && let Ok(json) = serde_json::to_value(value)
        {
            fill(&mut form, &json);
        }
    }
    form
}

/// 登録したシークレットを編集するフォーム。値は空のまま (空なら今の値を保つ)。
pub fn edit_form(owner: &str, name: &str, spec: &SecretSpec) -> Form {
    let mut form = form(owner, Some(name));
    if let Ok(json) = serde_json::to_value(spec) {
        fill(&mut form, &json);
    }
    form
}

/// 組み込みの種類 (定義を変えられず、値だけを登録する) のフォーム。
pub fn value_form(owner: &str, name: &str) -> Form {
    Form::new(
        format!("Secret {name}"),
        vec![
            Field::note("Owner", owner),
            Field::note(
                "Name",
                format!("{name} (built-in: only the value can be set)"),
            ),
            Field::secret("value", "Value"),
        ],
    )
}

fn form(owner: &str, name: Option<&str>) -> Form {
    let mut fields = vec![Field::note("Owner", owner)];
    fields.push(match name {
        Some(name) => Field::note("Name", name),
        None => Field::text("name", "Name", "").help(
            "Starts with a lowercase letter; lowercase letters, digits and single underscores.",
        ),
    });
    let keep = if name.is_some() {
        " Leave it empty to keep the current value."
    } else {
        ""
    };
    fields.extend([
        Field::choice("kind", "Kind", &[SINGLE, FIELDS], SINGLE).help(
            "Several fields are for credentials made of more than one value (such as AWS keys).",
        ),
        Field::text("env", "Env vars", "")
            .shown_when("kind", SINGLE)
            .help("VM environment variables that get the placeholder, separated by commas."),
        Field::text("placeholder", "Placeholder", "")
            .shown_when("kind", SINGLE)
            .help("Contains {id} once (replaced per workspace). Match the real value's prefix, e.g. sk-{id}. Empty means {id}."),
        Field::secret("value", "Value")
            .shown_when("kind", SINGLE)
            .help(format!("The real value. It never enters the VM.{keep}")),
        Field::rows("fields", "Fields", FIELD_ROW, vec![])
            .shown_when("kind", FIELDS)
            .help(format!("Each field has its own env vars, placeholder and value.{keep}")),
        Field::rows("inject", "Inject rules", INJECT_ROW, vec![]).help(
            "Where the proxy replaces the placeholder with the real value (hosts and header).",
        ),
        Field::rows("sign", "Sign rules", SIGN_ROW, vec![])
            .help("Requests the proxy signs again with the real keys (aws-sigv4)."),
        Field::rows("files", "Files", FILE_ROW, vec![]).help(
            "Files put in the VM with placeholders, for tools that read config files instead of env vars.",
        ),
    ]);
    let title = match name {
        Some(name) => format!("Secret {name}"),
        None => "New secret".to_string(),
    };
    Form::new(title, fields)
}

const FIELD_ROW: RowKind = RowKind {
    make: || field_row("", &[], "", ""),
    summary: |row| row.text("name"),
};

fn field_row(name: &str, env: &[String], placeholder: &str, value: &str) -> Form {
    let mut form = Form::new(
        "Field",
        vec![
            Field::text("name", "Name", name).help("e.g. access_key_id"),
            Field::text("env", "Env vars", &env.join(", ")),
            Field::text("placeholder", "Placeholder", placeholder),
            Field::secret("value", "Value"),
        ],
    );
    form.set_text("value", value);
    form
}

const INJECT_ROW: RowKind = RowKind {
    make: || inject_row("", &[], "", "replace", ""),
    summary: |row| format!("{} → {}", row.text("hosts"), row.text("header")),
};

fn inject_row(field: &str, hosts: &[String], header: &str, format: &str, username: &str) -> Form {
    Form::new(
        "Inject rule",
        vec![
            Field::text("hosts", "Hosts", &hosts.join(", "))
                .help("Destinations, separated by commas. *.example.com matches subdomains."),
            Field::text("header", "Header", header).help("e.g. authorization or x-api-key"),
            Field::choice("format", "Format", &["replace", "basic-password"], format)
                .help("basic-password replaces the password of Basic authentication."),
            Field::text("field", "Field", field)
                .help("With several fields, the field to put in. Empty means value."),
            Field::text("username_field", "Username field", username).help(
                "With basic-password, the field to put in as the user name. Empty keeps the user name sent.",
            ),
        ],
    )
}

const SIGN_ROW: RowKind = RowKind {
    make: || sign_row("aws-sigv4", &[]),
    summary: |row| format!("{} {}", row.choice("scheme"), row.text("hosts")),
};

fn sign_row(scheme: &str, hosts: &[String]) -> Form {
    Form::new(
        "Sign rule",
        vec![
            Field::choice("scheme", "Scheme", &["aws-sigv4"], scheme).help(
                "aws-sigv4 uses the fields access_key_id, secret_access_key (and session_token).",
            ),
            Field::text("hosts", "Hosts", &hosts.join(", ")),
        ],
    )
}

const FILE_ROW: RowKind = RowKind {
    make: || file_row("", "", "root", ""),
    summary: |row| row.text("path"),
};

fn file_row(path: &str, content: &str, owner: &str, mode: &str) -> Form {
    Form::new(
        "File",
        vec![
            Field::text("path", "Path", path).help("Absolute path in the VM."),
            Field::text("content", "Content", &escape_lines(content)).help(
                "{value} (or {field name}) becomes the placeholder. Write a line break as \\n.",
            ),
            Field::choice("owner", "Owner", &["root", "user"], owner),
            Field::text("mode", "Mode", mode).help("Octal, e.g. 0600. Empty means 0644."),
        ],
    )
}

/// 一行の欄で編集できるよう、改行を `\n` に、`\` を `\\` にする。
fn escape_lines(text: &str) -> String {
    text.replace('\\', "\\\\").replace('\n', "\\n")
}

/// [`escape_lines`] を戻す。
fn unescape_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match (c, chars.clone().next()) {
            ('\\', Some('n')) => {
                out.push('\n');
                chars.next();
            }
            ('\\', Some('\\')) => {
                out.push('\\');
                chars.next();
            }
            _ => out.push(c),
        }
    }
    out
}

/// 定義か雛形の JSON で欄を埋める。値は埋めない。
fn fill(form: &mut Form, json: &Value) {
    let strings = |v: &Value| -> Vec<String> {
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let text = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).unwrap_or("").to_string();
    // 欄が value だけなら、欄が一つの書き方にする。
    let fields = json.get("fields").and_then(Value::as_object);
    let single = match fields {
        Some(fields) if fields.len() == 1 && fields.contains_key("value") => fields.get("value"),
        Some(_) => None,
        None => Some(json),
    };
    match single {
        Some(field) => {
            form.set_choice("kind", SINGLE);
            form.set_text("env", &strings(&field["env"]).join(", "));
            let placeholder = text(field, "placeholder");
            form.set_text(
                "placeholder",
                if placeholder == "{id}" {
                    ""
                } else {
                    &placeholder
                },
            );
        }
        None => {
            form.set_choice("kind", FIELDS);
            let rows = fields
                .into_iter()
                .flatten()
                .map(|(name, f)| field_row(name, &strings(&f["env"]), &text(f, "placeholder"), ""))
                .collect();
            set_rows(form, "fields", rows);
        }
    }
    let inject = json["inject"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            let format = match text(r, "format").as_str() {
                "" => "replace".to_string(),
                f => f.to_string(),
            };
            inject_row(
                &text(r, "field"),
                &strings(&r["hosts"]),
                &text(r, "header"),
                &format,
                &text(r, "username_field"),
            )
        })
        .collect();
    set_rows(form, "inject", inject);
    let sign = json["sign"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| sign_row(&text(r, "scheme"), &strings(&r["hosts"])))
        .collect();
    set_rows(form, "sign", sign);
    let files = json["files"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            let owner = match text(r, "owner").as_str() {
                "" => "root".to_string(),
                o => o.to_string(),
            };
            file_row(
                &text(r, "path"),
                &text(r, "content"),
                &owner,
                &text(r, "mode"),
            )
        })
        .collect();
    set_rows(form, "files", files);
}

fn set_rows(form: &mut Form, key: &str, rows: Vec<Form>) {
    if let Some(index) = form.fields.iter().position(|f| f.key == key) {
        form.put_rows(index, rows);
    }
}

/// フォームの名前 (新しいシークレットのとき)。
pub fn name(form: &Form) -> String {
    form.text("name")
}

/// 組み込みの種類のフォームから、値だけの入力にする。
pub fn value_input(form: &Form) -> Result<SecretInput, String> {
    let value = form.text("value");
    if value.is_empty() {
        return Err("enter the value".into());
    }
    Ok(SecretInput::value(value))
}

/// フォームを、定義と値の入力にする。値を空にした欄は、値のないままにする (呼び出し側が
/// 今の値で埋める)。
pub fn to_input(form: &Form) -> Result<SecretInput, String> {
    let mut json = Map::new();
    let optional = |text: String| (!text.is_empty()).then_some(text);
    if form.choice("kind") == SINGLE {
        json.insert("env".into(), json!(form.list("env")));
        if let Some(placeholder) = optional(form.text("placeholder")) {
            json.insert("placeholder".into(), json!(placeholder));
        }
        if let Some(value) = optional(form.text("value")) {
            json.insert("value".into(), json!(value));
        }
    } else {
        let mut fields = Map::new();
        for row in form.rows("fields") {
            let name = row.text("name");
            if name.is_empty() {
                return Err("every field needs a name".into());
            }
            let mut field = Map::new();
            field.insert("env".into(), json!(row.list("env")));
            if let Some(placeholder) = optional(row.text("placeholder")) {
                field.insert("placeholder".into(), json!(placeholder));
            }
            field.insert("value".into(), json!(row.text("value")));
            fields.insert(name, Value::Object(field));
        }
        if fields.is_empty() {
            return Err("add at least one field".into());
        }
        json.insert("fields".into(), Value::Object(fields));
    }
    let inject: Vec<Value> = form
        .rows("inject")
        .iter()
        .map(|row| {
            let mut rule = Map::new();
            if let Some(field) = optional(row.text("field")) {
                rule.insert("field".into(), json!(field));
            }
            rule.insert("hosts".into(), json!(row.list("hosts")));
            rule.insert("header".into(), json!(row.text("header")));
            rule.insert("format".into(), json!(row.choice("format")));
            if let Some(username) = optional(row.text("username_field")) {
                rule.insert("username_field".into(), json!(username));
            }
            Value::Object(rule)
        })
        .collect();
    json.insert("inject".into(), Value::Array(inject));
    let sign: Vec<Value> = form
        .rows("sign")
        .iter()
        .map(|row| json!({"scheme": row.choice("scheme"), "hosts": row.list("hosts")}))
        .collect();
    json.insert("sign".into(), Value::Array(sign));
    let files: Vec<Value> = form
        .rows("files")
        .iter()
        .map(|row| {
            let mut file = Map::new();
            file.insert("path".into(), json!(row.text("path")));
            file.insert(
                "content".into(),
                json!(unescape_lines(&row.text("content"))),
            );
            file.insert("owner".into(), json!(row.choice("owner")));
            if let Some(mode) = optional(row.text("mode")) {
                file.insert("mode".into(), json!(mode));
            }
            Value::Object(file)
        })
        .collect();
    json.insert("files".into(), Value::Array(files));
    serde_json::from_value(Value::Object(json)).map_err(|e| format!("invalid definition: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_fill_the_form_without_values() {
        for (name, _) in TEMPLATES {
            let form = new_form("alice", Some(name));
            assert_eq!(form.text("name"), *name);
            assert_eq!(form.text("value"), "");
        }
        let form = new_form("alice", Some("openai_api_key"));
        assert_eq!(form.text("env"), "OPENAI_API_KEY, CODEX_API_KEY");
        assert_eq!(form.text("placeholder"), "sk-proj-{id}");
        assert_eq!(form.rows("inject").len(), 1);
        let aws = new_form("alice", Some("aws"));
        assert_eq!(aws.choice("kind"), FIELDS);
        assert_eq!(aws.rows("fields").len(), 2);
        assert_eq!(aws.rows("sign")[0].text("hosts"), "*.amazonaws.com");
    }

    #[test]
    fn the_form_becomes_a_definition() {
        let mut form = new_form("alice", Some("anthropic_api_key"));
        form.set_text("value", "sk-ant-real");
        let (spec, values) = to_input(&form).unwrap().into_parts().unwrap();
        let spec = spec.unwrap();
        assert_eq!(values["value"], "sk-ant-real");
        assert_eq!(spec.fields["value"].env, ["ANTHROPIC_API_KEY"]);
        assert_eq!(spec.fields["value"].placeholder, "sk-ant-api03-{id}");
        assert_eq!(spec.inject[0].header, "x-api-key");

        let mut aws = new_form("alice", Some("aws"));
        let mut rows = aws.rows("fields").to_vec();
        rows[0].set_text("value", "AKIAREAL");
        rows[1].set_text("value", "secret");
        set_rows(&mut aws, "fields", rows);
        let (spec, values) = to_input(&aws).unwrap().into_parts().unwrap();
        assert_eq!(values["access_key_id"], "AKIAREAL");
        assert_eq!(spec.unwrap().sign.len(), 1);
    }

    #[test]
    fn editing_round_trips_the_definition() {
        let mut form = new_form("alice", Some("aws"));
        let mut rows = form.rows("fields").to_vec();
        rows[0].set_text("value", "a");
        rows[1].set_text("value", "b");
        set_rows(&mut form, "fields", rows);
        let spec = to_input(&form).unwrap().into_parts().unwrap().0.unwrap();
        let edit = edit_form("alice", "aws", &spec);
        // 値を書かずに保存すると、値のない入力になる。
        let input = to_input(&edit).unwrap();
        assert!(input.fields.values().all(|f| f.value.is_empty()));
        let mut filled = input;
        for field in filled.fields.values_mut() {
            field.value = "x".into();
        }
        assert_eq!(filled.into_parts().unwrap().0.unwrap(), spec);

        let mut single = new_form("alice", Some("openai_api_key"));
        single.set_text("value", "v");
        let spec = to_input(&single).unwrap().into_parts().unwrap().0.unwrap();
        let edit = edit_form("alice", "openai_api_key", &spec);
        assert_eq!(edit.choice("kind"), SINGLE);
        assert_eq!(edit.text("env"), "OPENAI_API_KEY, CODEX_API_KEY");
        assert!(to_input(&edit).unwrap().value.is_none());

        // VM に置くファイルも残る。改行と \ は一行の欄で書けるようにして戻す。
        let mut apt = new_form("alice", Some("apt"));
        apt.set_text("value", "v");
        let mut files = apt.rows("files").to_vec();
        assert_eq!(
            files[0].text("content"),
            "machine apt.example.com login deploy password {value}\\n"
        );
        files.push(file_row("/home/user/x", "a\\b\nc", "user", "0600"));
        set_rows(&mut apt, "files", files);
        let spec = to_input(&apt).unwrap().into_parts().unwrap().0.unwrap();
        assert_eq!(spec.files[1].content, "a\\b\nc");
        let mut edit = edit_form("alice", "apt", &spec);
        edit.set_text("value", "v");
        assert_eq!(
            to_input(&edit).unwrap().into_parts().unwrap().0.unwrap(),
            spec
        );

        // Basic 認証のユーザー名として差し替える欄も残る。
        let mut basic = new_form("alice", None);
        basic.set_text("name", "repo");
        basic.set_choice("kind", FIELDS);
        set_rows(
            &mut basic,
            "fields",
            vec![
                field_row("username", &[], "", "u"),
                field_row("password", &[], "", "p"),
            ],
        );
        set_rows(
            &mut basic,
            "inject",
            vec![inject_row(
                "password",
                &["repo.example.com".into()],
                "authorization",
                "basic-password",
                "username",
            )],
        );
        let spec = to_input(&basic).unwrap().into_parts().unwrap().0.unwrap();
        assert_eq!(spec.inject[0].username_field.as_deref(), Some("username"));
        let mut filled = to_input(&edit_form("alice", "repo", &spec)).unwrap();
        for field in filled.fields.values_mut() {
            field.value = "x".into();
        }
        assert_eq!(filled.into_parts().unwrap().0.unwrap(), spec);
    }

    #[test]
    fn reports_missing_parts() {
        let mut form = new_form("alice", None);
        form.set_choice("kind", FIELDS);
        assert!(to_input(&form).is_err());
        assert!(value_input(&value_form("alice", "github_token")).is_err());
    }
}
