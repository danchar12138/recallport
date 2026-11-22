//! Connect RecallPort to AI clients by editing only their `recallport` entry.
//! Never overwrites a same-named entry that points elsewhere without an explicit choice, backs the
//! file up before changing it, and never touches the library.
use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use toml_edit::{DocumentMut, Item, Table, value};

const ENTRY: &str = "recallport";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Client {
    Codex,
    Claude,
}

impl Client {
    pub const ALL: [Client; 2] = [Client::Codex, Client::Claude];

    pub fn parse(id: &str) -> Option<Client> {
        Self::ALL.into_iter().find(|c| c.id() == id)
    }
    pub fn id(self) -> &'static str {
        match self {
            Client::Codex => "codex",
            Client::Claude => "claude",
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Client::Codex => "Codex",
            Client::Claude => "Claude Code",
        }
    }
    /// What the client calls itself in the MCP handshake.
    pub fn handshake_name(self) -> &'static str {
        self.id()
    }
    pub fn scope(self) -> &'static str {
        "用户级（对你的所有项目生效）"
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

pub fn config_path(c: Client) -> Option<PathBuf> {
    match c {
        Client::Codex => Some(
            std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .or_else(|| home().map(|h| h.join(".codex")))?
                .join("config.toml"),
        ),
        Client::Claude => Some(match std::env::var_os("CLAUDE_CONFIG_DIR") {
            Some(dir) => PathBuf::from(dir).join(".claude.json"),
            None => home()?.join(".claude.json"),
        }),
    }
}

fn installed(c: Client, path: &Path) -> bool {
    match c {
        Client::Codex => path.parent().is_some_and(Path::is_dir),
        Client::Claude => path.is_file() || home().is_some_and(|h| h.join(".claude").is_dir()),
    }
}

#[derive(Debug, Default)]
struct Existing {
    url: Option<String>,
    other: bool,
}

fn same(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

fn read_entry(c: Client, path: &Path) -> Result<Option<Existing>> {
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(path).with_context(|| format!("无法读取 {}", path.display()))?;
    match c {
        Client::Codex => {
            let doc: DocumentMut = text.parse().context("配置文件不是有效的 TOML")?;
            Ok(doc
                .get("mcp_servers")
                .and_then(|s| s.get(ENTRY))
                .map(|e| Existing {
                    url: e.get("url").and_then(|u| u.as_str()).map(str::to_owned),
                    other: e.get("command").is_some(),
                }))
        }
        Client::Claude => {
            let v: Value = serde_json::from_str(&text).context("配置文件不是有效的 JSON")?;
            Ok(v.get("mcpServers")
                .and_then(|s| s.get(ENTRY))
                .map(|e| Existing {
                    url: e.get("url").and_then(|u| u.as_str()).map(str::to_owned),
                    other: e.get("command").is_some(),
                }))
        }
    }
}

pub fn preview(c: Client, url: &str) -> String {
    match c {
        Client::Codex => format!("[mcp_servers.{ENTRY}]\nurl = \"{url}\""),
        Client::Claude => format!(
            "\"mcpServers\": {{\n  \"{ENTRY}\": {{ \"type\": \"http\", \"url\": \"{url}\" }}\n}}"
        ),
    }
}

#[derive(Serialize)]
pub struct Status {
    pub id: &'static str,
    pub name: &'static str,
    pub scope: &'static str,
    pub path: String,
    pub installed: bool,
    /// none, connected, conflict or error
    pub state: &'static str,
    /// What the existing same-named entry points to.
    pub existing: String,
    pub error: String,
    pub preview: String,
    pub command: String,
    /// Whether the usage rules block is in the client's global instruction file.
    pub rules: bool,
    pub rules_path: String,
}

pub fn status(c: Client, url: &str) -> Status {
    let path = config_path(c);
    let shown = path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let mut out = Status {
        id: c.id(),
        name: c.name(),
        scope: c.scope(),
        installed: path.as_deref().is_some_and(|p| installed(c, p)),
        path: shown,
        state: "none",
        existing: String::new(),
        error: String::new(),
        preview: preview(c, url),
        rules: rules_present(c),
        rules_path: rules_path(c)
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        command: match c {
            Client::Claude => format!("claude mcp add --transport http --scope user {ENTRY} {url}"),
            Client::Codex => String::new(),
        },
    };
    match path.as_deref().map(|p| read_entry(c, p)) {
        Some(Ok(Some(e))) => {
            if e.url.as_deref().is_some_and(|u| same(u, url)) && !e.other {
                out.state = "connected";
            } else {
                out.state = "conflict";
                out.existing = e.url.unwrap_or_else(|| "（不是 HTTP 地址的配置）".into());
            }
        }
        Some(Ok(None)) | None => {}
        Some(Err(e)) => {
            out.state = "error";
            out.error = format!("{e:#}");
        }
    }
    out
}

pub enum Outcome {
    Connected { backup: Option<String> },
    Already,
    Conflict { existing: String },
}

/// The usage rules the models are asked to follow, kept in the client's global instruction file
/// between two markers so they can be updated or removed without touching anything else in it.
pub const RULES: &str = include_str!("rules.md");
const BEGIN: &str = "<!-- recallport:begin -->";
const END: &str = "<!-- recallport:end -->";

pub fn rules_path(c: Client) -> Option<PathBuf> {
    match c {
        Client::Codex => Some(
            std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .or_else(|| home().map(|h| h.join(".codex")))?
                .join("AGENTS.md"),
        ),
        Client::Claude => Some(
            std::env::var_os("CLAUDE_CONFIG_DIR")
                .map(PathBuf::from)
                .or_else(|| home().map(|h| h.join(".claude")))?
                .join("CLAUDE.md"),
        ),
    }
}

fn block() -> String {
    format!(
        "{BEGIN}
{}
{END}
",
        RULES.trim_end()
    )
}

pub fn rules_present(c: Client) -> bool {
    rules_path(c)
        .and_then(|p| fs::read_to_string(p).ok())
        .is_some_and(|t| t.contains(BEGIN) && t.contains(END))
}

/// What `write_rules` did.
pub struct RulesWritten {
    /// False when the block was already exactly what it should be.
    pub changed: bool,
    pub backup: Option<String>,
}

/// Add or refresh the rules block. Everything outside the markers is kept; the file is backed up
/// first (`backup` is its copy, when there was a file to copy).
pub fn write_rules(c: Client) -> Result<RulesWritten> {
    let path = rules_path(c).context("找不到用户目录")?;
    let text = if path.is_file() {
        fs::read_to_string(&path)?
    } else {
        String::new()
    };
    let updated = match (text.find(BEGIN), text.find(END)) {
        (Some(a), Some(b)) if a < b => {
            let tail = b + END.len();
            let tail = if text[tail..].starts_with('\n') {
                tail + 1
            } else {
                tail
            };
            format!("{}{}{}", &text[..a], block(), &text[tail..])
        }
        _ if text.trim().is_empty() => block(),
        _ => format!(
            "{}

{}",
            text.trim_end(),
            block()
        ),
    };
    if updated == text {
        return Ok(RulesWritten {
            changed: false,
            backup: None,
        });
    }
    let saved = backup(&path)?;
    write_atomic(&path, &updated)?;
    Ok(RulesWritten {
        changed: true,
        backup: saved,
    })
}

/// Remove only the rules block. Returns whether there was one.
pub fn remove_rules(c: Client) -> Result<bool> {
    let path = rules_path(c).context("找不到用户目录")?;
    let Ok(text) = fs::read_to_string(&path) else {
        return Ok(false);
    };
    let (Some(a), Some(b)) = (text.find(BEGIN), text.find(END)) else {
        return Ok(false);
    };
    if a >= b {
        return Ok(false);
    }
    let tail = b + END.len();
    let tail = if text[tail..].starts_with('\n') {
        tail + 1
    } else {
        tail
    };
    let mut updated = format!("{}{}", text[..a].trim_end(), &text[tail..]);
    if !updated.trim().is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    backup(&path)?;
    if updated.trim().is_empty() {
        fs::remove_file(&path)?;
    } else {
        write_atomic(&path, &updated)?;
    }
    Ok(true)
}

fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let dir = path.parent().context("配置路径无效")?;
    fs::create_dir_all(dir)?;
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    temp.write_all(text.as_bytes())?;
    temp.as_file().sync_all()?;
    temp.persist(path)?;
    Ok(())
}

/// Copy the file aside before the first change, so nothing is ever lost.
fn backup(path: &Path) -> Result<Option<String>> {
    if !path.is_file() {
        return Ok(None);
    }
    let name = format!(
        "{}.recallport-{}.bak",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("config"),
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    );
    let target = path.with_file_name(name);
    fs::copy(path, &target)?;
    Ok(Some(target.display().to_string()))
}

pub fn connect(c: Client, url: &str, replace: bool) -> Result<Outcome> {
    let path = config_path(c).context("找不到用户目录")?;
    if let Some(e) = read_entry(c, &path)? {
        if e.url.as_deref().is_some_and(|u| same(u, url)) && !e.other {
            return Ok(Outcome::Already);
        }
        if !replace {
            return Ok(Outcome::Conflict {
                existing: e.url.unwrap_or_else(|| "（不是 HTTP 地址的配置）".into()),
            });
        }
    }
    let text = if path.is_file() {
        fs::read_to_string(&path)?
    } else {
        String::new()
    };
    let updated = match c {
        Client::Codex => {
            let mut doc: DocumentMut = text
                .parse()
                .context("配置文件不是有效的 TOML，未做任何修改")?;
            let servers = doc
                .entry("mcp_servers")
                .or_insert_with(|| {
                    let mut t = Table::new();
                    t.set_implicit(true);
                    Item::Table(t)
                })
                .as_table_mut()
                .context("mcp_servers 的格式不受支持，未做任何修改")?;
            let mut entry = Table::new();
            entry["url"] = value(url);
            servers.insert(ENTRY, Item::Table(entry));
            doc.to_string()
        }
        Client::Claude => {
            let mut v: Value = if text.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(&text).context("配置文件不是有效的 JSON，未做任何修改")?
            };
            let object = v
                .as_object_mut()
                .context("配置文件的顶层不是对象，未做任何修改")?;
            let servers = object.entry("mcpServers").or_insert_with(|| json!({}));
            servers
                .as_object_mut()
                .context("mcpServers 的格式不受支持，未做任何修改")?
                .insert(ENTRY.into(), json!({"type": "http", "url": url}));
            serde_json::to_string_pretty(&v)?
        }
    };
    let saved = backup(&path)?;
    write_atomic(&path, &updated)?;
    // Read it back: what we wrote must parse and point where we meant.
    match read_entry(c, &path)? {
        Some(e) if e.url.as_deref().is_some_and(|u| same(u, url)) => {
            Ok(Outcome::Connected { backup: saved })
        }
        _ => bail!("写入后校验失败，请检查 {}", path.display()),
    }
}

/// Remove only the `recallport` entry. Returns whether there was one.
pub fn disconnect(c: Client) -> Result<(bool, Option<String>)> {
    let path = config_path(c).context("找不到用户目录")?;
    if read_entry(c, &path)?.is_none() {
        return Ok((false, None));
    }
    let text = fs::read_to_string(&path)?;
    let updated = match c {
        Client::Codex => {
            let mut doc: DocumentMut = text
                .parse()
                .context("配置文件不是有效的 TOML，未做任何修改")?;
            if let Some(t) = doc.get_mut("mcp_servers").and_then(|s| s.as_table_mut()) {
                t.remove(ENTRY);
            }
            doc.to_string()
        }
        Client::Claude => {
            let mut v: Value =
                serde_json::from_str(&text).context("配置文件不是有效的 JSON，未做任何修改")?;
            if let Some(s) = v.get_mut("mcpServers").and_then(|s| s.as_object_mut()) {
                s.shift_remove(ENTRY);
            }
            serde_json::to_string_pretty(&v)?
        }
    };
    let saved = backup(&path)?;
    write_atomic(&path, &updated)?;
    Ok((true, saved))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_entry_is_added_replaced_and_removed_without_touching_the_rest() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let file = dir.path().join("config.toml");
        fs::write(
            &file,
            "# my notes\nmodel = \"x\"\n\n[mcp_servers]\n\n[mcp_servers.time]\ncommand = \"uvx\"\nargs = [\"mcp-server-time\"]\n",
        )?;
        unsafe { std::env::set_var("CODEX_HOME", dir.path()) };
        let url = "http://127.0.0.1:3765/mcp";
        assert_eq!(status(Client::Codex, url).state, "none");
        // Rules: appended after the user's own text, refreshed in place, removed without a trace.
        let agents = dir.path().join("AGENTS.md");
        fs::write(
            &agents,
            "# My rules
Be brief.
",
        )?;
        assert!(!rules_present(Client::Codex));
        assert!(write_rules(Client::Codex)?.changed);
        let with = fs::read_to_string(&agents)?;
        assert!(
            with.starts_with(
                "# My rules
Be brief."
            ) && with.contains(RULES.trim_end())
        );
        assert!(rules_present(Client::Codex));
        assert!(
            !write_rules(Client::Codex)?.changed,
            "unchanged: no rewrite"
        );
        assert_eq!(fs::read_to_string(&agents)?, with);
        assert!(remove_rules(Client::Codex)?);
        assert_eq!(
            fs::read_to_string(&agents)?,
            "# My rules
Be brief.
"
        );
        assert!(!remove_rules(Client::Codex)?);
        assert!(matches!(
            connect(Client::Codex, url, false)?,
            Outcome::Connected { backup: Some(_) }
        ));
        let after = fs::read_to_string(&file)?;
        assert!(
            after.contains("# my notes")
                && after.contains("mcp-server-time")
                && after.contains(url)
        );
        assert_eq!(status(Client::Codex, url).state, "connected");
        assert!(matches!(
            connect(Client::Codex, url, false)?,
            Outcome::Already
        ));
        // A same-named entry pointing elsewhere is never silently overwritten.
        let other = "http://127.0.0.1:3999/mcp";
        assert_eq!(status(Client::Codex, other).state, "conflict");
        assert!(matches!(
            connect(Client::Codex, other, false)?,
            Outcome::Conflict { .. }
        ));
        assert!(fs::read_to_string(&file)?.contains(url));
        assert!(matches!(
            connect(Client::Codex, other, true)?,
            Outcome::Connected { .. }
        ));
        // Disconnect removes only ours.
        assert!(disconnect(Client::Codex)?.0);
        let end = fs::read_to_string(&file)?;
        assert!(
            !end.contains("recallport")
                && end.contains("mcp-server-time")
                && end.contains("# my notes")
        );
        assert!(!disconnect(Client::Codex)?.0);
        // A broken file is refused, not rewritten.
        fs::write(&file, "this = [is not toml")?;
        assert!(connect(Client::Codex, url, false).is_err());
        assert_eq!(fs::read_to_string(&file)?, "this = [is not toml");
        Ok(())
    }

    #[test]
    fn claude_entry_keeps_other_servers_and_unknown_fields() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let file = dir.path().join(".claude.json");
        fs::write(
            &file,
            r#"{"numStartups": 7, "mcpServers": {"fetch": {"command": "uvx"}}, "projects": {"a": {}}}"#,
        )?;
        unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", dir.path()) };
        let url = "http://127.0.0.1:3765/mcp";
        assert!(matches!(
            connect(Client::Claude, url, false)?,
            Outcome::Connected { .. }
        ));
        let v: Value = serde_json::from_str(&fs::read_to_string(&file)?)?;
        assert_eq!(v["numStartups"], 7);
        assert_eq!(v["mcpServers"]["fetch"]["command"], "uvx");
        assert_eq!(v["mcpServers"][ENTRY]["url"], url);
        assert_eq!(status(Client::Claude, url).state, "connected");
        assert!(disconnect(Client::Claude)?.0);
        let v: Value = serde_json::from_str(&fs::read_to_string(&file)?)?;
        assert!(v["mcpServers"].get(ENTRY).is_none() && v["mcpServers"].get("fetch").is_some());
        Ok(())
    }
}
