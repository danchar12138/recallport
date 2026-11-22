//! What the AI has left to do on the tree: nodes that sit under nothing yet, and summaries that have fallen behind
//! the nodes under them. The server never writes content; it only says where the work is, lowest layer first.
use super::{Status, Tree};
use crate::clusters::{cosine, partition};
use std::collections::{BTreeMap, BTreeSet};

/// The tree is finished when this many roots or fewer are left at the top.
const TOP_MAX: usize = 8;
const GROUP_MIN: usize = 3;
const GROUP_MAX: usize = 12;
/// A summary with this many children is full.
const KIDS_MAX: usize = 16;
const ATTACH_SIM: f32 = 0.5;
/// How many of the newest nodes of one layer are looked at for work (grouping is quadratic).
const SCAN: usize = 800;
/// A summary is stale once this many of its children (or a quarter of them) changed after it was last checked.
const STALE_MIN: usize = 2;

/// The lowest layer with work: (layer, [(summary to extend, nodes that fit under it)], [groups for a new summary]).
pub type Gaps = (u32, Vec<(String, Vec<String>)>, Vec<Vec<String>>);

#[derive(Debug, Default)]
pub struct Maintenance {
    /// (layer, nodes in it, how many of them sit under a parent), from layer 0 up.
    pub layers: Vec<(u32, usize, usize)>,
    /// Summaries behind their children, with those children.
    pub stale: Vec<(String, Vec<String>)>,
    /// The lowest layer with work: (layer, [(summary to extend, nodes that fit under it)], [groups for a new summary]).
    pub gaps: Option<Gaps>,
}

impl Tree {
    pub fn maintenance(&self) -> Maintenance {
        let live: Vec<&str> = self
            .nodes
            .values()
            .filter(|n| n.status == Status::Live)
            .map(|n| n.id.as_str())
            .collect();
        let all = self.layers();
        let layer_of: BTreeMap<&str, u32> = live.iter().map(|id| (*id, all[*id])).collect();
        let mut counts: BTreeMap<u32, (usize, usize)> = BTreeMap::new();
        for id in &live {
            let at = counts.entry(layer_of[id]).or_default();
            at.0 += 1;
            at.1 += usize::from(self.nodes[*id].parent.is_some());
        }
        let mut stale = vec![];
        for id in &live {
            let kids = self.children(id);
            if kids.is_empty() {
                continue;
            }
            let touched = self.checked_at(id);
            let newer: Vec<String> = kids
                .iter()
                .filter(|k| self.changed_at(k) > touched)
                .cloned()
                .collect();
            if newer.len() >= STALE_MIN.max(kids.len().div_ceil(4)) {
                stale.push(((*id).to_owned(), newer));
            }
        }
        stale.sort();
        Maintenance {
            layers: counts.into_iter().map(|(l, (n, c))| (l, n, c)).collect(),
            stale,
            gaps: self.gaps(&live, &layer_of),
        }
    }

    fn gaps(&self, live: &[&str], layer_of: &BTreeMap<&str, u32>) -> Option<Gaps> {
        let roots: Vec<&str> = live
            .iter()
            .copied()
            .filter(|id| self.nodes[*id].parent.is_none())
            .collect();
        let tallest = roots.iter().map(|id| layer_of[id]).max()?;
        if roots.len() <= TOP_MAX && roots.iter().all(|id| layer_of[id] == tallest) {
            return None;
        }
        let layers: BTreeSet<u32> = roots.iter().map(|id| layer_of[id]).collect();
        for layer in layers {
            let mut nodes: Vec<&str> = roots
                .iter()
                .copied()
                .filter(|id| layer_of[id] == layer)
                .collect();
            nodes.sort_by(|a, b| {
                self.nodes[*b]
                    .created
                    .cmp(&self.nodes[*a].created)
                    .then_with(|| a.cmp(b))
            });
            nodes.truncate(SCAN);
            let mut homes: Vec<(&str, usize)> = live
                .iter()
                .copied()
                .filter(|id| layer_of[id] == layer + 1)
                .map(|id| (id, KIDS_MAX.saturating_sub(self.children(id).len())))
                .filter(|(_, room)| *room > 0)
                .collect();
            let mut joins: BTreeMap<&str, Vec<String>> = BTreeMap::new();
            let mut loose: Vec<&str> = vec![];
            for id in nodes {
                let Some(v) = self.vectors.get(id) else {
                    continue;
                };
                let best = homes
                    .iter_mut()
                    .filter_map(|(home, room)| {
                        (*room > 0).then_some(())?;
                        Some((cosine(v, self.vectors.get(*home)?), *home, room))
                    })
                    .max_by(|a, b| a.0.total_cmp(&b.0));
                match best {
                    Some((sim, home, room)) if sim >= ATTACH_SIM => {
                        *room -= 1;
                        joins.entry(home).or_default().push(id.to_owned());
                    }
                    _ => loose.push(id),
                }
            }
            let groups: Vec<Vec<String>> = if loose.len() >= GROUP_MIN {
                let rows: Vec<Vec<f32>> =
                    loose.iter().map(|id| self.vectors[*id].clone()).collect();
                partition(&rows, GROUP_MIN, GROUP_MAX)
                    .into_iter()
                    .map(|g| g.into_iter().map(|i| loose[i].to_owned()).collect())
                    .collect()
            } else {
                vec![]
            };
            if joins.is_empty() && groups.is_empty() {
                continue;
            }
            let extend = joins
                .into_iter()
                .map(|(h, ids)| (h.to_owned(), ids))
                .collect();
            return Some((layer, extend, groups));
        }
        None
    }
}
