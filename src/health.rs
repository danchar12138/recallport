//! How tidy the library is: duplicates, names nobody settled, memories about nothing in particular, correction
//! chains that fork or loop, and how far the summary tree has grown. The server only reports; the AI (or the
//! person) decides what to fix.
use crate::{
    clusters::{cosine, partition},
    entities::norm,
    memory::*,
    store::{Hier, Store, hierarchy},
};
use anyhow::Result;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

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
    /// The summary tree layer by layer, from the memories themselves (layer 0) up.
    pub layers: Vec<Layer>,
    pub issues: Vec<Issue>,
}

/// One layer of the summary tree. Replaced (corrected) memories do not count: their correction stands in for them.
#[derive(Clone, Debug, Serialize)]
pub struct Layer {
    pub layer: u32,
    pub count: usize,
    /// How many of them sit under a summary.
    pub covered: usize,
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

/// The tree is finished when this many summaries or fewer are left at the top.
const TOP_MAX: usize = 8;
/// How many memories one new summary should gather.
const GROUP_MIN: usize = 3;
const GROUP_MAX: usize = 12;
/// A summary with this many children is full: it should not grow further.
const KIDS_MAX: usize = 16;
/// How alike (cosine of the mean vectors) a memory and a summary must be to suggest the memory joins it.
const ATTACH_SIM: f32 = 0.5;
/// How many of the newest memories of one layer are looked at for work.
const SUMMARY_SCAN: usize = 800;
/// A summary is stale once this many of its children (or a quarter of them) joined after it was last touched.
const STALE_MIN: usize = 2;

const SUMMARY_GAPS_FIX: &str = "Work one layer at a time, from the bottom (layer 0 is the memories themselves). Each example is a batch of ids. `new <ids>`: `read` a few, then `remember` ONE summary (a title plus the conclusion) with `references` of kind=summarizes to every id in the batch and the entities they share. `extend <summary> <ids>`: they belong under that existing summary: `link` {from: <summary>, to: <id>, kind: summarizes} for each id. If a batch is not really about one thing, split it into several summaries. Then call `health` again: once a layer is covered, the next layer up shows its batches, until at most 8 summaries are left at the top.";

const STALE_FIX: &str = "Each example starts with a summary, then the children that joined after it was last touched. `read` the summary and those children. If it no longer tells the whole story, `remember` a new summary with `references` kind=corrects to the old one and kind=summarizes to the newer children: it keeps all the old children and the old one's place in the tree (the summary above it may then go stale in turn: refresh it the same way when `health` says so). If it is still right, `link` {from: <summary>, to: <child>, kind: summarizes} for one of the newer children to mark it as checked.";

/// Summaries that were written before a good part of their children, and so may no longer say it all.
fn stale_summaries(
    current: &[&Entry],
    by_id: &HashMap<&str, &Entry>,
    hier: &Hier<'_>,
    replaced: &HashSet<&str>,
) -> Option<Issue> {
    let mut stale: Vec<(&Entry, Vec<&str>)> = vec![];
    for s in current {
        let touched = s.recorded.as_str().max(s.updated.as_str());
        let kids: Vec<&str> = hier
            .kids
            .get(s.id.as_str())
            .into_iter()
            .flatten()
            .copied()
            .filter(|k| !replaced.contains(k))
            .collect();
        let newer: Vec<&str> = kids
            .iter()
            .copied()
            .filter(|k| by_id[k].recorded.as_str() > touched)
            .collect();
        if newer.len() >= STALE_MIN.max(kids.len().div_ceil(4)) {
            stale.push((s, newer));
        }
    }
    if stale.is_empty() {
        return None;
    }
    stale.sort_by(|a, b| b.0.recorded.cmp(&a.0.recorded));
    Some(Issue {
        kind: "stale_summaries",
        count: stale.len(),
        detail: format!(
            "{} 条汇总写好之后，又有不少新记忆并了进来，可能已经说不全了",
            stale.len()
        ),
        examples: stale
            .iter()
            .take(4)
            .map(|(s, newer)| format!("{} {}", s.id, newer.join(" ")))
            .collect(),
        fix: STALE_FIX,
    })
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

        // The summary tree: what is not under a summary yet, and which summaries have fallen behind.
        let hier = hierarchy(&entries, &by_id);
        let replaced: HashSet<&str> = entries
            .iter()
            .flat_map(|e| {
                e.references
                    .iter()
                    .filter(|r| r.kind == RefKind::Corrects && by_id.contains_key(r.id.as_str()))
                    .map(|r| r.id.as_str())
            })
            .collect();
        let current: Vec<&Entry> = entries
            .iter()
            .filter(|e| !replaced.contains(e.id.as_str()))
            .collect();
        let mut counts: BTreeMap<u32, (usize, usize)> = BTreeMap::new();
        for e in &current {
            let at = counts.entry(hier.layer(e)).or_default();
            at.0 += 1;
            at.1 += usize::from(hier.parent.contains_key(e.id.as_str()));
        }
        let layers: Vec<Layer> = counts
            .into_iter()
            .map(|(layer, (count, covered))| Layer {
                layer,
                count,
                covered,
            })
            .collect();
        issues.extend(self.summary_gaps(&current, &hier, &replaced).await);
        issues.extend(stale_summaries(&current, &by_id, &hier, &replaced));

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
            layers,
            issues,
        })
    }

    /// Work for the lowest layer that has any. Memories that are not under a summary (while too many are left at
    /// the top, or while they sit beside a taller summary) are either sent to the existing summary they resemble
    /// or grouped for a new one.
    async fn summary_gaps(
        &self,
        current: &[&Entry],
        hier: &Hier<'_>,
        replaced: &HashSet<&str>,
    ) -> Option<Issue> {
        let open: Vec<&Entry> = current
            .iter()
            .copied()
            .filter(|e| !hier.parent.contains_key(e.id.as_str()))
            .collect();
        let tallest = open.iter().map(|e| hier.layer(e)).max()?;
        if open.len() <= TOP_MAX && open.iter().all(|e| hier.layer(e) == tallest) {
            return None;
        }
        let layers: BTreeSet<u32> = open.iter().map(|e| hier.layer(e)).collect();
        for layer in layers {
            let mut nodes: Vec<&Entry> = open
                .iter()
                .copied()
                .filter(|e| hier.layer(e) == layer)
                .collect();
            nodes.sort_by(|a, b| b.recorded.cmp(&a.recorded));
            nodes.truncate(SUMMARY_SCAN);
            let mut homes: Vec<(&Entry, usize)> = current
                .iter()
                .copied()
                .filter(|s| hier.layer(s) == layer + 1)
                .map(|s| {
                    let kids = hier
                        .kids
                        .get(s.id.as_str())
                        .map_or(0, |k| k.iter().filter(|k| !replaced.contains(*k)).count());
                    (s, KIDS_MAX.saturating_sub(kids))
                })
                .filter(|(_, room)| *room > 0)
                .collect();
            let ids: Vec<&str> = nodes
                .iter()
                .chain(homes.iter().map(|(s, _)| s))
                .map(|e| e.id.as_str())
                .collect();
            let vectors: HashMap<String, Vec<f32>> = self
                .mean_vectors(&ids)
                .await
                .unwrap_or_default()
                .into_iter()
                .collect();
            let mut joins: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
            let mut loose: Vec<&str> = vec![];
            for node in &nodes {
                let Some(v) = vectors.get(&node.id) else {
                    continue;
                };
                let best = homes
                    .iter_mut()
                    .filter_map(|(s, room)| {
                        (*room > 0).then_some(())?;
                        Some((cosine(v, vectors.get(&s.id)?), s, room))
                    })
                    .max_by(|a, b| a.0.total_cmp(&b.0));
                match best {
                    Some((sim, s, room)) if sim >= ATTACH_SIM => {
                        *room -= 1;
                        joins
                            .entry(s.id.as_str())
                            .or_default()
                            .push(node.id.as_str());
                    }
                    _ => loose.push(node.id.as_str()),
                }
            }
            let groups = if loose.len() >= GROUP_MIN {
                let rows: Vec<Vec<f32>> = loose.iter().map(|id| vectors[*id].clone()).collect();
                partition(&rows, GROUP_MIN, GROUP_MAX)
            } else {
                vec![]
            };
            if joins.is_empty() && groups.is_empty() {
                continue;
            }
            let joined: usize = joins.values().map(Vec::len).sum();
            let grouped: usize = groups.iter().map(Vec::len).sum();
            let mut examples: Vec<String> = joins
                .iter()
                .map(|(s, ids)| format!("extend {s} {}", ids.join(" ")))
                .collect();
            examples.extend(groups.iter().map(|g| {
                format!(
                    "new {}",
                    g.iter().map(|&i| loose[i]).collect::<Vec<_>>().join(" ")
                )
            }));
            examples.truncate(6);
            return Some(Issue {
                kind: "summary_gaps",
                count: joins.len() + groups.len(),
                detail: format!(
                    "第 {layer} 层{}有 {} 条还没被第 {} 层汇总：{grouped} 条要新写汇总（{} 组），{joined} 条可以并进已有的汇总",
                    if layer == 0 { "（记忆本身）" } else { "" },
                    joined + grouped,
                    layer + 1,
                    groups.len(),
                ),
                examples,
                fix: SUMMARY_GAPS_FIX,
            });
        }
        None
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
