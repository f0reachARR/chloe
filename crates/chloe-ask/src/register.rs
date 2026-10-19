//! Claude Code と Codex の利用者の設定に、chloe の MCP のサーバーを足す。
//!
//! ログインシェルの profile から呼ぶ。足したことを `~/.config/chloe/registered` に残し、
//! 利用者が設定から外したら足し直さない。設定にすでに `chloe` があれば、書き換えない。

use std::path::Path;

use anyhow::{Context, Result};

/// 呼び出し一回を待つ時間の上限 (秒)。サーバーは [`crate::DEFAULT_WAIT`] だけ待つので、
/// それより長くする。
const TOOL_TIMEOUT_SECS: u64 = 3600;
const NAME: &str = "chloe";
const MARKER: &str = ".config/chloe/registered";

/// `home` の設定に足し、足したものの名前を返す。
pub fn register(home: &Path, exe: &str) -> Result<Vec<&'static str>> {
    let marker = home.join(MARKER);
    let done = std::fs::read_to_string(&marker).unwrap_or_default();
    let mut added = Vec::new();
    for (tool, add) in [
        ("claude", add_claude as fn(&Path, &str) -> Result<bool>),
        ("codex", add_codex),
    ] {
        if done.lines().any(|line| line == tool) {
            continue;
        }
        if add(home, exe).with_context(|| format!("register the MCP server for {tool}"))? {
            added.push(tool);
        }
        let mut lines = std::fs::read_to_string(&marker).unwrap_or_default();
        lines.push_str(&format!("{tool}\n"));
        if let Some(dir) = marker.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&marker, lines)?;
    }
    Ok(added)
}

/// Claude Code の `~/.claude.json` の `mcpServers` に足す。足したら `true`。
fn add_claude(home: &Path, exe: &str) -> Result<bool> {
    let path = home.join(".claude.json");
    let mut config: serde_json::Value = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).context("read ~/.claude.json")?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(error) => return Err(error.into()),
    };
    let Some(root) = config.as_object_mut() else {
        anyhow::bail!("~/.claude.json is not a JSON object");
    };
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));
    let Some(servers) = servers.as_object_mut() else {
        anyhow::bail!("mcpServers in ~/.claude.json is not an object");
    };
    if servers.contains_key(NAME) {
        return Ok(false);
    }
    servers.insert(
        NAME.into(),
        serde_json::json!({
            "type": "stdio",
            "command": exe,
            "args": ["mcp"],
            "env": {},
            "timeout": TOOL_TIMEOUT_SECS * 1000,
        }),
    );
    let text = serde_json::to_string_pretty(&config)?;
    replace(&path, &text)?;
    Ok(true)
}

/// Codex の `~/.codex/config.toml` の終わりに `[mcp_servers.chloe]` を足す。足したら `true`。
/// 利用者が書いた設定の形を崩さないよう、読み直して書くのではなく、終わりに書き足す。
fn add_codex(home: &Path, exe: &str) -> Result<bool> {
    let path = home.join(".codex/config.toml");
    let current = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error.into()),
    };
    let header = format!("[mcp_servers.{NAME}]");
    if current.lines().any(|line| line.trim() == header) {
        return Ok(false);
    }
    let mut text = current;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(&format!(
        "{header}\ncommand = {exe:?}\nargs = [\"mcp\"]\ntool_timeout_sec = {TOOL_TIMEOUT_SECS}\n"
    ));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    replace(&path, &text)?;
    Ok(true)
}

/// 一時的なファイルに書いてから置き換える。元のファイルのパーミッションは保つ。
fn replace(path: &Path, text: &str) -> Result<()> {
    let tmp = path.with_extension("chloe-tmp");
    std::fs::write(&tmp, text)?;
    if let Ok(meta) = std::fs::metadata(path) {
        std::fs::set_permissions(&tmp, meta.permissions())?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_once_and_respects_removal() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        std::fs::write(
            home.join(".claude.json"),
            r#"{"numStartups": 3, "mcpServers": {"other": {"command": "x"}}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::write(home.join(".codex/config.toml"), "model = \"o3\"").unwrap();

        let exe = "/usr/local/bin/chloe-ask";
        assert_eq!(register(home, exe).unwrap(), ["claude", "codex"]);
        let claude: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(home.join(".claude.json")).unwrap())
                .unwrap();
        assert_eq!(claude["numStartups"], 3);
        assert_eq!(claude["mcpServers"]["other"]["command"], "x");
        assert_eq!(claude["mcpServers"]["chloe"]["command"], exe);
        assert_eq!(claude["mcpServers"]["chloe"]["timeout"], 3_600_000);
        let codex = std::fs::read_to_string(home.join(".codex/config.toml")).unwrap();
        assert_eq!(
            codex,
            "model = \"o3\"\n\n[mcp_servers.chloe]\ncommand = \"/usr/local/bin/chloe-ask\"\nargs = [\"mcp\"]\ntool_timeout_sec = 3600\n"
        );
        toml_parses(&codex);

        // 利用者が外したら、足し直さない。
        std::fs::write(home.join(".claude.json"), "{}").unwrap();
        assert!(register(home, exe).unwrap().is_empty());
        assert_eq!(
            std::fs::read_to_string(home.join(".claude.json")).unwrap(),
            "{}"
        );
    }

    #[test]
    fn creates_missing_configs_and_keeps_existing_entries() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::write(
            home.join(".codex/config.toml"),
            "[mcp_servers.chloe]\ncommand = \"mine\"\n",
        )
        .unwrap();
        // Codex には自分で書いたものがあるので、Claude Code にだけ足す。
        assert_eq!(register(home, "/x").unwrap(), ["claude"]);
        assert!(home.join(".claude.json").exists());
        assert_eq!(
            std::fs::read_to_string(home.join(".codex/config.toml")).unwrap(),
            "[mcp_servers.chloe]\ncommand = \"mine\"\n"
        );
        assert_eq!(
            std::fs::read_to_string(home.join(MARKER)).unwrap(),
            "claude\ncodex\n"
        );
    }

    /// 書き足した TOML が、表として読めるか (キーと値の形だけを確かめる)。
    fn toml_parses(text: &str) {
        for line in text
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('['))
        {
            let (key, value) = line.split_once(" = ").unwrap();
            assert!(!key.is_empty() && !value.is_empty(), "{line}");
        }
    }
}
