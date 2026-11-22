//! Turn a library of the current memory store into tree events, so both can be asked the same questions.
//!
//! A memory becomes a layer-0 node (its context folded into the content, the entities it mentions kept as tags);
//! the summaries become the parents; a memory that corrects another becomes a later version of one node, so the old
//! text lives on in the node's history instead of competing in searches.
use super::model::{Event, Node, Status};
use crate::{
    memory::{Entry, RefKind},
    store::hierarchy,
};
use std::collections::{HashMap, HashSet};

pub fn events(entries: &[Entry]) -> Vec<Event> {
    let by_id: HashMap<&str, &Entry> = entries.iter().map(|e| (e.id.as_str(), e)).collect();
    let hier = hierarchy(entries, &by_id);
    // old -> the newest memory that corrects it
    let mut next: HashMap<&str, &Entry> = HashMap::new();
    for e in entries {
        for r in e.references.iter().filter(|r| r.kind == RefKind::Corrects) {
            if by_id.contains_key(r.id.as_str())
                && next
                    .get(r.id.as_str())
                    .is_none_or(|have| have.recorded < e.recorded)
            {
                next.insert(r.id.as_str(), e);
            }
        }
    }
    let end_of = |id: &str| -> String {
        let mut at = id;
        for _ in 0..entries.len() {
            match next.get(at) {
                Some(n) if n.id != id => at = n.id.as_str(),
                _ => break,
            }
        }
        at.to_owned()
    };
    // the corrector of every chain keeps the chain's id; the replaced ones only feed its history
    let mut chains: HashMap<String, Vec<&Entry>> = HashMap::new();
    for e in entries {
        chains.entry(end_of(&e.id)).or_default().push(e);
    }
    let text = |e: &Entry| {
        if e.context.is_empty() {
            e.text.clone()
        } else {
            format!("{}\n{}", e.text, e.context)
        }
    };
    let live: HashSet<String> = chains.keys().cloned().collect();
    let mut out = vec![];
    for (id, mut chain) in chains {
        chain.sort_by(|a, b| a.recorded.cmp(&b.recorded).then_with(|| a.id.cmp(&b.id)));
        let last = by_id[id.as_str()];
        let parent = chain
            .iter()
            .filter_map(|e| hier.parent.get(e.id.as_str()))
            .map(|p| end_of(p))
            .find(|p| p != &id && live.contains(p));
        let mut tags: Vec<String> = vec![];
        for m in chain.iter().flat_map(|e| &e.mentions) {
            if tags.len() < 12 && !tags.contains(&m.name) {
                tags.push(m.name.clone());
            }
        }
        let refs: Vec<String> = last
            .references
            .iter()
            .filter(|r| r.kind == RefKind::Relates)
            .map(|r| end_of(&r.id))
            .filter(|r| r != &id && live.contains(r))
            .collect();
        let at = |e: &Entry| {
            if e.updated.is_empty() {
                e.recorded.clone()
            } else {
                e.updated.clone()
            }
        };
        for (n, e) in chain.iter().enumerate() {
            out.push(Event {
                at: at(e),
                actor: e.source.clone(),
                op: if n == 0 { "import" } else { "update" }.into(),
                reason: if n == 0 {
                    String::new()
                } else {
                    "更正".into()
                },
                node: Node {
                    id: id.clone(),
                    content: text(e),
                    tags: tags.clone(),
                    parent: parent.clone(),
                    refs: refs.clone(),
                    sources: if e.source.is_empty() {
                        vec![]
                    } else {
                        vec![e.source.clone()]
                    },
                    version: n as u32 + 1,
                    occurred: e.occurred.clone(),
                    created: chain[0].recorded.clone(),
                    updated: at(e),
                    status: Status::Live,
                },
            });
        }
    }
    out.sort_by(|a, b| {
        a.at.cmp(&b.at)
            .then_with(|| a.node.version.cmp(&b.node.version))
    });
    out
}
