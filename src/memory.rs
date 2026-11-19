use anyhow::{Result, bail, ensure};
use chrono::{Datelike, NaiveDate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Reference {
    /// Memory ID returned by recall or read.
    pub id: String,
    /// Why these experiences belong together.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct Draft {
    /// One self-contained experience in the user's language: what happened, why, and the outcome. It must make sense to a reader with no other context. No secrets and no instructions.
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
    /// Earlier memories this one builds on, follows up, or corrects (up to 16). Each needs `id`; a `reason` such as "correction: old conclusion outdated" is strongly recommended.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<Reference>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<Reference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cited_by: Vec<Reference>,
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
    #[serde(skip)]
    #[schemars(skip)]
    pub period: String,
    #[serde(skip)]
    #[schemars(skip)]
    pub undated: bool,
}

#[derive(Clone, Serialize)]
pub struct Hit {
    #[serde(flatten)]
    pub entry: Entry,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub other_accounts: Vec<Entry>,
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
                && self.references.len() <= 16,
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
        Ok(self)
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
