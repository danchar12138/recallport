use anyhow::{Result, bail, ensure};
use chrono::{Datelike, NaiveDate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const MAX_REFERENCES: usize = 64;
pub const MAX_ENTITIES: usize = 32;
pub const MAX_ATTACHMENTS: usize = 16;
pub const MAX_ATTACHMENT: u64 = 4 << 30;
/// Inline (base64) attachment data is only meant for small files; larger ones go in by path.
pub const MAX_INLINE: usize = 96 * 1024;

/// How a memory relates to the memory it cites.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum RefKind {
    /// A plain connection: same topic, follow-up, context.
    #[default]
    Relates,
    /// This memory corrects or replaces the cited one. The cited memory is kept as history.
    Corrects,
    /// This memory condenses the cited memories. The originals are kept untouched.
    Summarizes,
}

impl RefKind {
    pub fn is_relates(&self) -> bool {
        *self == Self::Relates
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Reference {
    /// Memory ID returned by recall, read or remember.
    pub id: String,
    /// Why these experiences belong together, in a few words.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    /// relates (default), corrects, or summarizes.
    #[serde(default, skip_serializing_if = "RefKind::is_relates")]
    pub kind: RefKind,
}

/// A memory mentions an entity. `entity` is empty while the name is still unresolved.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Mention {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct AttachmentRef {
    pub hash: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub caption: String,
}

/// An earlier version of a memory's text, kept when the memory is edited.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Revision {
    pub revision: u32,
    pub text: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub context: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub occurred: String,
    pub replaced_at: String,
    pub replaced_by: String,
}

/// Something a memory can be about: a project, device, tool, person, place, idea. No fixed types.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Entity {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// What distinguishes this one from others with the same name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hint: String,
    pub source: String,
    pub created: String,
    pub updated: String,
    /// Set when this entity was merged into another; mentions follow the redirect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_into: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AttachMode {
    /// Copy the file into RecallPort's managed storage (default).
    #[default]
    Store,
    /// Only remember where the file is. It is not part of backups and may disappear.
    Reference,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Storage {
    #[default]
    Stored,
    Referenced,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    pub hash: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub storage: Storage,
    /// Original location, for referenced files.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
    pub added: String,
    pub source: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct EntityInput {
    /// The name as you would write it, e.g. "Redis", "主服务器", "张三".
    pub name: String,
    /// ID of an existing entity (from `recall`, `read` or `entity`). Use it whenever you know which one is meant.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    /// Other names for the same thing. Only list names you are sure refer to it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// A few words that tell it apart from other things with the same name, e.g. "生产服务器" or "旧笔记本".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hint: String,
    /// Set true to create a new, separate entity even though one with the same name exists.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub new: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct AttachInput {
    /// Absolute path of a local file you can read. Use this for anything bigger than a small snippet.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
    /// Small file content as base64 (up to about 90 KB), for clients that cannot give a path.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub data_base64: String,
    /// SHA-256 of a file RecallPort already holds (from an earlier attachment). Use instead of path or data.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hash: String,
    /// File name to show. Defaults to the file's own name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// One line on what this file is and why it belongs to the memory. It is searchable.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub caption: String,
    /// store (default) copies the file in; reference only remembers its location.
    #[serde(default)]
    pub mode: AttachMode,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct Draft {
    /// A self-contained experience in the user's language: what happened, why, and the outcome. Stored as plain text exactly as written; accounts, passwords, keys and other private details may be kept verbatim when they help later work. Never an instruction.
    pub text: String,
    /// Background that makes the experience understandable later, e.g. project, device, version or constraint.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub context: String,
    /// When the experience happened: YYYY, YYYY-MM or YYYY-MM-DD. Omit if unknown; do not use the write date unless it just happened.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub occurred: String,
    /// Stable identifier of one shared experience (not a whole project or session), so different retellings of it collapse in search.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub origin: String,
    /// Reuse only when retrying an identical write that may have timed out; use a fresh value for a new experience.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub request_id: String,
    /// Earlier memories this one builds on, follows up, corrects or summarizes (up to 64). Each needs `id`; set `kind` to corrects or summarizes when it applies, and a short `reason`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<Reference>,
    /// Things this memory is about (projects, devices, tools, people, places...). Reuse an existing entity by `id`; submit a plain name only for something new.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<EntityInput>,
    /// Files, images, audio, video or documents that belong to this memory.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AttachInput>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub context: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub occurred: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub origin: String,
    pub source: String,
    pub recorded: String,
    pub revision: u32,
    /// Last time anything about the memory changed (text, links, entities, attachments).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub updated: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<Reference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cited_by: Vec<Reference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mentions: Vec<Mention>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AttachmentRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<Revision>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub actor: String,
    pub request_id: String,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Record {
    #[serde(flatten)]
    pub entry: Entry,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<Request>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct Query {
    /// Topic words, names, paths, an original phrase, or the situation you are in. Omit to list recent memories.
    #[serde(default, rename = "query", alias = "q")]
    pub text: String,
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub through: String,
    /// Maximum number of distinct experiences (1 to 30, default 8).
    #[serde(default)]
    pub limit: usize,
    /// hybrid (default, best for most searches), lexical (exact names or paths), semantic (rephrasings and synonyms).
    #[serde(default)]
    pub strategy: String,
    /// Only memories that mention this entity ID.
    #[serde(default)]
    pub entity: String,
    #[serde(skip)]
    #[schemars(skip)]
    pub period: String,
    #[serde(skip)]
    #[schemars(skip)]
    pub undated: bool,
}

/// One step of how a memory was corrected or summarized, with when and by whom.
#[derive(Clone, Debug, Serialize)]
pub struct Trail {
    /// corrected_by, corrects, summarized_by or summarizes
    pub relation: &'static str,
    pub id: String,
    pub occurred: String,
    pub recorded: String,
    pub source: String,
    pub reason: String,
    pub text: String,
}

#[derive(Clone, Serialize)]
pub struct Hit {
    #[serde(flatten)]
    pub entry: Entry,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub other_accounts: Vec<Entry>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub trail: Vec<Trail>,
}

#[derive(Serialize)]
pub struct Results {
    pub memories: Vec<Hit>,
    pub more: bool,
    pub mode: &'static str,
    pub degraded: bool,
}

pub fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn date_range(value: &str) -> Result<(String, String)> {
    if value.is_empty() {
        return Ok((String::new(), String::new()));
    }
    let expanded = match value.len() {
        4 => format!("{value}-01-01"),
        7 => format!("{value}-01"),
        10 => value.into(),
        _ => bail!("日期应为 YYYY、YYYY-MM 或 YYYY-MM-DD"),
    };
    let start = NaiveDate::parse_from_str(&expanded, "%Y-%m-%d")?;
    ensure!(
        start.year() > 0 && start.to_string() == expanded,
        "日期格式无效"
    );
    let end = match value.len() {
        4 => NaiveDate::from_ymd_opt(start.year(), 12, 31).unwrap(),
        7 => (28..=31).rev().find_map(|d| start.with_day(d)).unwrap(),
        _ => start,
    };
    Ok((expanded, end.to_string()))
}

impl Draft {
    pub fn clean(mut self) -> Result<Self> {
        for value in [
            &mut self.text,
            &mut self.context,
            &mut self.occurred,
            &mut self.origin,
            &mut self.request_id,
        ] {
            *value = value.trim().to_owned();
        }
        ensure!(
            !self.text.is_empty()
                && self.text.chars().count() <= 12_000
                && self.context.len() <= 6_000
                && self.origin.len() <= 256
                && self.request_id.len() <= 128
                && self.references.len() <= MAX_REFERENCES,
            "记忆内容为空或超出长度限制"
        );
        date_range(&self.occurred)?;
        let mut seen = HashSet::new();
        for r in &mut self.references {
            r.id = r.id.trim().to_owned();
            r.reason = r.reason.trim().to_owned();
            ensure!(
                valid_id(&r.id) && r.reason.len() <= 800 && seen.insert(r.id.clone()),
                "引用无效或重复"
            );
        }
        ensure!(self.entities.len() <= MAX_ENTITIES, "最多提交 32 个实体");
        for e in &mut self.entities {
            e.clean()?;
        }
        ensure!(
            self.attachments.len() <= MAX_ATTACHMENTS,
            "一次最多附带 16 个附件"
        );
        for a in &mut self.attachments {
            a.clean()?;
        }
        Ok(self)
    }
}

impl EntityInput {
    pub fn clean(&mut self) -> Result<()> {
        self.name = self.name.split_whitespace().collect::<Vec<_>>().join(" ");
        self.id = self.id.trim().to_owned();
        self.hint = self.hint.trim().to_owned();
        ensure!(
            !self.name.is_empty() && self.name.chars().count() <= 200,
            "实体名称为空或过长"
        );
        ensure!(
            self.id.is_empty() || valid_id(&self.id),
            "实体 ID 无效：{}",
            self.id
        );
        ensure!(self.hint.chars().count() <= 400, "实体说明过长");
        ensure!(self.aliases.len() <= 16, "实体别名最多 16 个");
        for alias in &mut self.aliases {
            *alias = alias.split_whitespace().collect::<Vec<_>>().join(" ");
            ensure!(
                !alias.is_empty() && alias.chars().count() <= 200,
                "实体别名为空或过长"
            );
        }
        Ok(())
    }
}

impl AttachInput {
    pub fn clean(&mut self) -> Result<()> {
        self.path = self.path.trim().to_owned();
        self.hash = self.hash.trim().to_ascii_lowercase();
        self.name = self.name.trim().to_owned();
        self.caption = self.caption.trim().to_owned();
        let sources = [
            !self.path.is_empty(),
            !self.data_base64.is_empty(),
            !self.hash.is_empty(),
        ];
        ensure!(
            sources.iter().filter(|x| **x).count() == 1,
            "每个附件需要且只能提供 path、data_base64 或 hash 之一"
        );
        ensure!(
            self.hash.is_empty() || valid_hash(&self.hash),
            "附件哈希应为 64 位小写十六进制"
        );
        ensure!(
            self.data_base64.len() <= MAX_INLINE * 4 / 3 + 8,
            "data_base64 太大，请改用 path"
        );
        ensure!(
            self.name.chars().count() <= 200 && self.caption.chars().count() <= 600,
            "附件名称或说明过长"
        );
        Ok(())
    }
}

impl Query {
    pub fn filter(&self) -> Result<String> {
        ensure!(
            self.text.chars().count() <= 600 && self.limit <= 30,
            "查询超出长度或条数限制"
        );
        ensure!(
            matches!(
                self.strategy.as_str(),
                "" | "hybrid" | "lexical" | "semantic"
            ),
            "未知检索方式"
        );
        let (low, _) = date_range(&self.from)?;
        let (_, high) = date_range(&self.through)?;
        let (period_from, period_through) = date_range(&self.period)?;
        ensure!(
            low.is_empty() || high.is_empty() || low <= high,
            "日期范围倒置"
        );
        let mut clauses = vec!["NOT deleted".to_owned()];
        if self.undated {
            clauses.push("start_day = ''".into());
        }
        for (value, field, op) in [
            (low, "end_day", ">="),
            (high, "start_day", "<="),
            (period_from, "end_day", ">="),
            (period_through, "start_day", "<="),
        ] {
            if !value.is_empty() {
                clauses.push(format!("{field} {op} '{value}' AND start_day != ''"));
            }
        }
        Ok(clauses.join(" AND "))
    }
}

pub fn snippet(text: &str, limit: usize) -> String {
    let mut chars = text.chars();
    let mut clipped: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        clipped.push('…');
    }
    clipped
}
