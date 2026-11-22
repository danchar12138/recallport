//! Reading a backup made by the 0.x series (`recallport-archive`: memories, entities, references of kind
//! corrects / summarizes) and turning it into tree events, so an old library can come along.
//!
//! A memory becomes a node (its context folded into the text, the entities it mentions kept as tags). The
//! summaries become parents; a memory that corrects another becomes a later version of one node, so the old text
//! lives on in its history; a summary that corrects another takes over its children.
use crate::tree::{Attachment, Event, MAX_CONTENT, Node, Status, Storage};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

pub const FORMAT: &str = "recallport-archive";

#[derive(Deserialize)]
pub struct Archive {
    #[serde(default)]
    memories: Vec<Memory>,
    #[serde(default)]
    entities: Vec<Entity>,
    #[serde(default)]
    attachments: Vec<File>,
}

#[derive(Deserialize)]
struct Memory {
    id: String,
    text: String,
    #[serde(default)]
    context: String,
    #[serde(default)]
    occurred: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    recorded: String,
    #[serde(default)]
    updated: String,
    #[serde(default)]
    references: Vec<Reference>,
    #[serde(default)]
    mentions: Vec<Mention>,
    #[serde(default)]
    attachments: Vec<AttachmentRef>,
    #[serde(default)]
    history: Vec<Revision>,
}

#[derive(Deserialize)]
struct Reference {
    id: String,
    #[serde(default)]
    kind: String,
}

#[derive(Deserialize)]
struct Mention {
    name: String,
    #[serde(default)]
    entity: Option<String>,
}

#[derive(Deserialize)]
struct AttachmentRef {
    hash: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    caption: String,
}

#[derive(Deserialize)]
struct Revision {
    text: String,
    #[serde(default)]
    context: String,
    #[serde(default)]
    occurred: String,
    #[serde(default)]
    replaced_at: String,
    #[serde(default)]
    replaced_by: String,
}

#[derive(Deserialize)]
struct Entity {
    id: String,
    name: String,
    #[serde(default)]
    merged_into: Option<String>,
}

#[derive(Deserialize)]
struct File {
    hash: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    mime: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    storage: String,
    #[serde(default)]
    path: String,
}

fn joined(text: &str, context: &str) -> String {
    if context.is_empty() {
        text.to_owned()
    } else {
        format!("{text}\n{context}")
    }
}

impl Archive {
    /// Hashes of the stored files the memories mention.
    pub fn stored_files(&self) -> HashSet<String> {
        self.attachments
            .iter()
            .filter(|f| f.storage != "referenced")
            .map(|f| f.hash.clone())
            .collect()
    }

    pub fn len(&self) -> usize {
        self.memories.len()
    }

    /// The events of the library and the text each node's vector was made from, by node id.
    pub fn events(&self) -> Vec<Event> {
        let by_id: HashMap<&str, &Memory> = self.memories.iter().map(|m| (m.id.as_str(), m)).collect();
        // old -> the newest memory that corrects it
        let mut next: HashMap<&str, &Memory> = HashMap::new();
        for m in &self.memories {
            for r in m.references.iter().filter(|r| r.kind == "corrects") {
                if by_id.contains_key(r.id.as_str())
                    && r.id != m.id
                    && next.get(r.id.as_str()).is_none_or(|have| have.recorded < m.recorded)
                {
                    next.insert(r.id.as_str(), m);
                }
            }
        }
        let end_of = |id: &str| -> String {
            let mut at = id;
            for _ in 0..=self.memories.len() {
                match next.get(at) {
                    Some(n) if n.id != id => at = n.id.as_str(),
                    _ => break,
                }
            }
            at.to_owned()
        };
        // who summarizes whom: a memory belongs to the newest summary that cites it
        let mut summary_of: HashMap<&str, &Memory> = HashMap::new();
        for m in &self.memories {
            for r in m.references.iter().filter(|r| r.kind == "summarizes") {
                if by_id.contains_key(r.id.as_str())
                    && r.id != m.id
                    && summary_of.get(r.id.as_str()).is_none_or(|have| have.recorded < m.recorded)
                {
                    summary_of.insert(r.id.as_str(), m);
                }
            }
        }
        let mut chains: HashMap<String, Vec<&Memory>> = HashMap::new();
        for m in &self.memories {
            chains.entry(end_of(&m.id)).or_default().push(m);
        }
        let live: HashSet<String> = chains.keys().cloned().collect();
        let entity_name: HashMap<&str, &Entity> = self.entities.iter().map(|e| (e.id.as_str(), e)).collect();
        let name_of = |m: &Mention| -> String {
            let mut at = m.entity.as_deref().and_then(|id| entity_name.get(id).copied());
            for _ in 0..16 {
                match at.and_then(|e| e.merged_into.as_deref()).and_then(|id| entity_name.get(id).copied()) {
                    Some(next) => at = Some(next),
                    None => break,
                }
            }
            at.map_or_else(|| m.name.clone(), |e| e.name.clone())
        };
        let file_of: HashMap<&str, &File> = self.attachments.iter().map(|f| (f.hash.as_str(), f)).collect();
        // parent of each chain: the newest summarizer of any member
        let mut parent: HashMap<String, String> = HashMap::new();
        for (id, chain) in &chains {
            let up = chain
                .iter()
                .filter_map(|m| summary_of.get(m.id.as_str()))
                .max_by(|a, b| a.recorded.cmp(&b.recorded))
                .map(|s| end_of(&s.id))
                .filter(|p| p != id && live.contains(p));
            if let Some(up) = up {
                parent.insert(id.clone(), up);
            }
        }
        // a summary that ends up (through others) under itself would loop: cut it
        for start in parent.keys().cloned().collect::<Vec<_>>() {
            let mut at = start.clone();
            for _ in 0..=parent.len() {
                match parent.get(&at) {
                    Some(up) if *up == start => {
                        parent.remove(&start);
                        break;
                    }
                    Some(up) => at = up.clone(),
                    None => break,
                }
            }
        }
        let mut out = vec![];
        for (id, mut chain) in chains {
            chain.sort_by(|a, b| a.recorded.cmp(&b.recorded).then_with(|| a.id.cmp(&b.id)));
            let last = by_id[id.as_str()];
            let mut tags: Vec<String> = vec![];
            for m in chain.iter().flat_map(|m| &m.mentions) {
                let name = name_of(m);
                if tags.len() < 12 && !tags.contains(&name) {
                    tags.push(name);
                }
            }
            let attachments: Vec<Attachment> = last
                .attachments
                .iter()
                .map(|r| {
                    let f = file_of.get(r.hash.as_str());
                    Attachment {
                        hash: r.hash.clone(),
                        name: if r.name.is_empty() { f.map_or("file".into(), |f| f.name.clone()) } else { r.name.clone() },
                        caption: r.caption.clone(),
                        mime: f.map_or_else(|| "application/octet-stream".into(), |f| f.mime.clone()),
                        size: f.map_or(0, |f| f.size),
                        storage: if f.is_some_and(|f| f.storage == "referenced") { Storage::Referenced } else { Storage::Stored },
                        path: f.map_or_else(String::new, |f| f.path.clone()),
                    }
                })
                .collect();
            // every earlier text of the node: older memories of the chain, and edits of each memory
            let mut versions: Vec<(String, String, String, String)> = vec![]; // (at, actor, content, occurred)
            for m in &chain {
                let mut revs: Vec<&Revision> = m.history.iter().collect();
                revs.sort_by(|a, b| a.replaced_at.cmp(&b.replaced_at));
                let mut at = m.recorded.clone();
                for r in revs {
                    versions.push((at.clone(), r.replaced_by.clone(), joined(&r.text, &r.context), r.occurred.clone()));
                    at = r.replaced_at.clone();
                }
                let stamp = if m.updated.is_empty() { at } else { m.updated.clone() };
                versions.push((stamp, m.source.clone(), joined(&m.text, &m.context), m.occurred.clone()));
            }
            let created = chain[0].recorded.clone();
            let total = versions.len();
            for (n, (at, actor, content, occurred)) in versions.into_iter().enumerate() {
                let last_version = n + 1 == total;
                out.push(Event {
                    at,
                    actor,
                    op: if n == 0 { "import" } else { "update" }.into(),
                    reason: if n == 0 { String::new() } else { "来自旧版的修改或更正".into() },
                    node: Node {
                        id: id.clone(),
                        content,
                        tags: tags.clone(),
                        parent: parent.get(&id).cloned(),
                        refs: if last_version {
                            last.references
                                .iter()
                                .filter(|r| r.kind.is_empty() || r.kind == "relates")
                                .map(|r| end_of(&r.id))
                                .filter(|r| *r != id && live.contains(r))
                                .collect()
                        } else {
                            vec![]
                        },
                        sources: if last.source.is_empty() { vec![] } else { vec![last.source.clone()] },
                        attachments: if last_version { attachments.clone() } else { vec![] },
                        version: n as u32 + 1,
                        occurred,
                        created: created.clone(),
                        updated: if last.updated.is_empty() { last.recorded.clone() } else { last.updated.clone() },
                        status: Status::Live,
                    },
                });
            }
        }
        // the parts of a node's history must follow each other in time
        out.sort_by(|a, b| a.node.id.cmp(&b.node.id).then_with(|| a.node.version.cmp(&b.node.version)));
        let mut previous: Option<(String, String)> = None;
        for e in &mut out {
            if let Some((id, at)) = &previous
                && *id == e.node.id
                && e.at < *at
            {
                e.at = at.clone();
            }
            previous = Some((e.node.id.clone(), e.at.clone()));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory(id: &str, text: &str, recorded: &str, references: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id, "text": text, "recorded": recorded, "source": "codex",
            "references": serde_json::from_str::<serde_json::Value>(references).unwrap(),
            "mentions": [{"name": "NAS", "entity": "e1"}]
        })
    }

    #[test]
    fn corrections_become_versions_and_summaries_become_parents() {
        let a = "a".repeat(32);
        let b = "b".repeat(32);
        let s = "c".repeat(32);
        let s2 = "d".repeat(32);
        let archive: Archive = serde_json::from_value(serde_json::json!({
            "memories": [
                memory(&a, "NAS 可用 12TB", "2024-01-01T00:00:00Z", "[]"),
                memory(&b, "NAS 可用 11.6TB", "2024-02-01T00:00:00Z", &format!(r#"[{{"id":"{a}","kind":"corrects"}}]"#)),
                memory(&s, "存储的小结", "2024-03-01T00:00:00Z", &format!(r#"[{{"id":"{a}","kind":"summarizes"}}]"#)),
                memory(&s2, "更新的小结", "2024-04-01T00:00:00Z", &format!(r#"[{{"id":"{s}","kind":"corrects"}}]"#)),
            ],
            "entities": [{"id": "e1", "name": "家里的 NAS"}]
        }))
        .unwrap();
        let events = archive.events();
        // the corrected memory is one node (the correction's id) with both texts in its history
        let mine: Vec<&Event> = events.iter().filter(|e| e.node.id == b).collect();
        assert_eq!(mine.len(), 2);
        assert_eq!(mine[0].node.content, "NAS 可用 12TB");
        assert_eq!(mine[1].node.content, "NAS 可用 11.6TB");
        assert_eq!(mine[1].node.tags, vec!["家里的 NAS".to_owned()]);
        assert!(events.iter().all(|e| e.node.id != a && e.node.id != s));
        // the first summary corrected by the second: the memory sits under the second one
        assert_eq!(mine[1].node.parent.as_deref(), Some(s2.as_str()));
    }
}
