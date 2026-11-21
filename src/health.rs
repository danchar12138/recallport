//! How tidy the library is: duplicates, names nobody settled, memories about nothing in particular, and
//! correction chains that fork or loop. The server only reports; the AI (or the person) decides what to fix.
use crate::{entities::norm, memory::*, store::Store};
use anyhow::Result;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Clone, Debug, Serialize)]
pub struct Issue {
    pub kind: &'static str,
    pub count: usize,
    /// What it is, in the user's language.
    pub detail: String,
    /// A few memory ids, entity ids or names to start from.
    pub examples: Vec<String>,
    /// What to do about it.
    pub fix: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct Health {
    pub ok: bool,
    pub memories: usize,
    pub entities: usize,
    pub issues: Vec<Issue>,
}

/// A few numbers and names for the start of a session.
#[derive(Clone, Debug, Default)]
pub struct Brief {
    pub memories: usize,
    pub entities: usize,
    pub latest: String,
    pub top: Vec<(String, usize)>,
    pub unresolved: usize,
    pub issues: usize,
}

fn flat(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

impl Store {
    /// Would `from` correcting `to` close a loop, because `to` is already (through earlier corrections) a
    /// correction of `from`?
    pub(crate) async fn correction_loop(&self, from: &str, to: &str) -> Result<bool> {
        let entries = self.all().await?;
        let by_id: HashMap<&str, &Entry> = entries.iter().map(|e| (e.id.as_str(), e)).collect();
        let mut stack = vec![to];
        let mut seen = HashSet::new();
        while let Some(id) = stack.pop() {
            if id == from {
                return Ok(true);
            }
            if !seen.insert(id) {
                continue;
            }
            if let Some(e) = by_id.get(id) {
                stack.extend(
                    e.references
                        .iter()
                        .filter(|r| r.kind == RefKind::Corrects)
                        .map(|r| r.id.as_str()),
                );
            }
        }
        Ok(false)
    }

    pub async fn health(&self) -> Result<Health> {
        let entries = self.all().await?;
        let by_id: HashMap<&str, &Entry> = entries.iter().map(|e| (e.id.as_str(), e)).collect();
        let mut issues = vec![];

        // The same text saved twice.
        let mut same: BTreeMap<String, Vec<&Entry>> = BTreeMap::new();
        for e in entries.iter() {
            same.entry(flat(&e.text)).or_default().push(e);
        }
        let twins: Vec<&Vec<&Entry>> = same.values().filter(|g| g.len() > 1).collect();
        if !twins.is_empty() {
            issues.push(Issue {
                kind: "duplicate_memories",
                count: twins.iter().map(|g| g.len() - 1).sum(),
                detail: format!("{} 组记忆的正文完全相同", twins.len()),
                examples: twins
                    .iter()
                    .take(3)
                    .flat_map(|g| g.iter().take(2).map(|e| e.id.clone()))
                    .collect(),
                fix: "Keep the copy with the most context, links and entities; `forget` the others (standing permission, no need to ask). Do not save the same experience again.",
            });
        }

        let idx = self.idx.read().await;
        // Two entities with the same name and nothing to tell them apart.
        let mut named: BTreeMap<String, Vec<(&str, &str)>> = BTreeMap::new();
        for e in idx.entities.values().filter(|e| e.merged_into.is_none()) {
            named
                .entry(norm(&e.name))
                .or_default()
                .push((e.id.as_str(), e.hint.trim()));
        }
        let mut doubles = vec![];
        for group in named.values().filter(|g| g.len() > 1) {
            let hints: HashSet<String> = group.iter().map(|(_, h)| norm(h)).collect();
            if hints.len() < group.len() {
                doubles.push(group);
            }
        }
        if !doubles.is_empty() {
            issues.push(Issue {
                kind: "duplicate_entities",
                count: doubles.len(),
                detail: format!("{} 个名字对应了不止一个实体，而且没有说明能区分它们", doubles.len()),
                examples: doubles
                    .iter()
                    .take(3)
                    .flat_map(|g| g.iter().take(2).map(|(id, _)| (*id).to_owned()))
                    .collect(),
                fix: "If they are the same thing, `entity` merge one into the other. If not, `entity` update to give each a hint that tells them apart.",
            });
        }

        // Without any entity.
        let mut bare: Vec<&Entry> = entries.iter().filter(|e| e.mentions.is_empty()).collect();
        bare.sort_by(|a, b| b.recorded.cmp(&a.recorded));
        if !bare.is_empty() {
            issues.push(Issue {
                kind: "without_entities",
                count: bare.len(),
                detail: format!("{} 条记忆没有关联任何实体", bare.len()),
                examples: bare.iter().take(5).map(|e| e.id.clone()).collect(),
                fix: "`read` them and `link` the projects, devices, tools or people they are about (search with `entity` first).",
            });
        }
        drop(idx);

        // Names AI submitted and nobody settled.
        let waiting = self.unresolved().await?;
        if !waiting.is_empty() {
            issues.push(Issue {
                kind: "unresolved_names",
                count: waiting.len(),
                detail: format!("{} 个实体名称还没有指向具体实体", waiting.len()),
                examples: waiting.iter().take(6).map(|(n, _)| n.clone()).collect(),
                fix: "Look each up with `entity` search, then `link` {from, entity:{name, id}} (or new:true for a different thing).",
            });
        }

        // Corrections that fork or loop.
        let idx = self.idx.read().await;
        let corrected_by = |id: &str| -> Vec<&str> {
            idx.cites
                .get(id)
                .into_iter()
                .flatten()
                .filter(|(s, r)| r.kind == RefKind::Corrects && idx.live.contains(s))
                .map(|(s, _)| s.as_str())
                .collect()
        };
        let forks: Vec<&str> = entries
            .iter()
            .filter(|e| {
                corrected_by(&e.id)
                    .into_iter()
                    .collect::<HashSet<_>>()
                    .len()
                    > 1
            })
            .map(|e| e.id.as_str())
            .collect();
        if !forks.is_empty() {
            issues.push(Issue {
                kind: "correction_forks",
                count: forks.len(),
                detail: format!("{} 条记忆被两条以上不同的新记忆更正，哪个才是现状没有定论", forks.len()),
                examples: forks.iter().take(5).map(|s| (*s).to_owned()).collect(),
                fix: "`read` them and weigh the trail. Then `remember` the settled state citing the corrections with kind=summarizes.",
            });
        }
        let mut looped: Vec<&str> = vec![];
        for e in entries.iter() {
            // Follow the corrections this memory makes; if they lead back to it, it is in a loop.
            let mut stack: Vec<&str> = e
                .references
                .iter()
                .filter(|r| r.kind == RefKind::Corrects)
                .map(|r| r.id.as_str())
                .collect();
            let mut seen = HashSet::new();
            while let Some(id) = stack.pop() {
                if id == e.id {
                    looped.push(e.id.as_str());
                    break;
                }
                if seen.insert(id)
                    && let Some(next) = by_id.get(id)
                {
                    stack.extend(
                        next.references
                            .iter()
                            .filter(|r| r.kind == RefKind::Corrects)
                            .map(|r| r.id.as_str()),
                    );
                }
            }
        }
        if !looped.is_empty() {
            issues.push(Issue {
                kind: "correction_loops",
                count: looped.len(),
                detail: format!("{} 条记忆处在互相更正的循环里", looped.len()),
                examples: looped.iter().take(5).map(|s| (*s).to_owned()).collect(),
                fix: "Decide which memory is the latest truth, then `link` with remove=true on the correction that points the wrong way.",
            });
        }

        // Attachment files that are gone.
        let missing: Vec<String> = idx
            .files
            .values()
            .filter(|a| match a.storage {
                Storage::Stored => !self.blob(&a.hash).is_file(),
                Storage::Referenced => !std::path::Path::new(&a.path).exists(),
            })
            .map(|a| a.name.clone())
            .collect();
        if !missing.is_empty() {
            issues.push(Issue {
                kind: "missing_files",
                count: missing.len(),
                detail: format!("{} 个附件的文件找不到了", missing.len()),
                examples: missing.iter().take(5).cloned().collect(),
                fix: "Look for the file; if you find it, `link` the attachment again with the new path. If it is really gone, `link` with remove=true to drop that attachment. No need to ask.",
            });
        }
        let entities = idx
            .entities
            .values()
            .filter(|e| e.merged_into.is_none())
            .count();
        drop(idx);

        // Memories without vectors are repaired by the server; only report a lasting gap.
        let status = self.status.read().await;
        if status.skipped > 0 && status.semantic.state != "working" {
            issues.push(Issue {
                kind: "index_gap",
                count: status.skipped,
                detail: format!(
                    "{} 条记忆还没有语义向量，语义搜索可能找不到它们",
                    status.skipped
                ),
                fix: "Nothing to do: the server repairs the index in the background.",
                examples: vec![],
            });
        }
        Ok(Health {
            ok: issues.is_empty(),
            memories: entries.len(),
            entities,
            issues,
        })
    }

    pub async fn brief(&self) -> Result<Brief> {
        let entries = self.all().await?;
        let latest = entries
            .iter()
            .max_by(|a, b| a.recorded.cmp(&b.recorded))
            .map(|e| {
                let day = if e.occurred.is_empty() {
                    e.recorded.get(..10).unwrap_or_default()
                } else {
                    e.occurred.as_str()
                };
                format!("{day} {}", snippet(&e.text, 60))
            })
            .unwrap_or_default();
        let idx = self.idx.read().await;
        let mut top: Vec<(String, usize)> = idx
            .entities
            .values()
            .filter(|e| e.merged_into.is_none())
            .map(|e| (e.name.clone(), idx.memory_ids(&e.id).len()))
            .filter(|(_, n)| *n > 0)
            .collect();
        top.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        top.truncate(8);
        let entities = idx
            .entities
            .values()
            .filter(|e| e.merged_into.is_none())
            .count();
        drop(idx);
        let unresolved = self.unresolved().await.map(|u| u.len()).unwrap_or(0);
        Ok(Brief {
            memories: entries.len(),
            entities,
            latest,
            top,
            unresolved,
            issues: self.health().await.map_or(0, |h| h.issues.len()),
        })
    }
}
