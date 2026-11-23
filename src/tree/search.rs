//! Search over every layer at once, then context around what was found.
//!
//! Every live node in scope gets one score per phrasing of the question: how far its cosine to the question and its
//! BM25 keyword score stand above the rest (both as z-scores), weighted `SEMANTIC` to the rest. The scores of the
//! phrasings add up, so a node that fits several of them wins over one that fits a single wording. A hit comes with
//! the chain of summaries above it, and a hit that is itself a summary comes with the children that match the
//! question best, so an overview question lands on the evidence behind it without the caller walking down the tree.
use super::{Tree, model::Node};
use anyhow::Result;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug)]
pub struct SearchOpts {
    /// The question as asked.
    pub query: String,
    /// Other ways to put it (synonyms, the other language, the exact error text); searched together with `query`.
    pub also: Vec<String>,
    /// How many hits.
    pub k: usize,
    /// Only this node and what is under it.
    pub scope: Option<String>,
    pub min_layer: Option<u32>,
    pub max_layer: Option<u32>,
    /// Keep nodes whose `occurred` lies in this range (`YYYY-MM-DD` prefixes); nodes with no date always stay.
    pub since: String,
    pub until: String,
    /// The most children that come along with a hit that is a summary; fewer when fewer of them match closely.
    pub expand: usize,
    /// Only nodes that carry this tag.
    pub tag: Option<String>,
}

impl Default for SearchOpts {
    fn default() -> Self {
        Self {
            query: String::new(),
            also: vec![],
            k: 8,
            scope: None,
            min_layer: None,
            max_layer: None,
            since: String::new(),
            until: String::new(),
            expand: 8,
            tag: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Hit {
    pub id: String,
    pub layer: u32,
    /// Cosine of the best-fitting phrasing and the node: how close in meaning, whatever the keywords did.
    pub relevance: f32,
    /// A whole phrasing (lower-cased) occurs in the title, the content or the tags.
    pub exact: bool,
    /// keyword, semantic or both.
    pub matched: &'static str,
    /// Its title, or for a memory without one the start of its text.
    pub title: String,
    pub content: String,
    /// The content was cut short; `get` returns all of it.
    pub truncated: bool,
    /// The summaries above it, nearest first, as (id, title).
    pub path: Vec<(String, String)>,
    /// For a summary: the children that match the query best, as (id, title, start of the content).
    pub expanded: Vec<(String, String, String)>,
}

/// Share of the fused score that comes from meaning; the rest comes from the keywords.
const SEMANTIC: f32 = 0.75;

/// Cosine from which a node counts as clearly about the question.
pub const STRONG: f32 = 0.88;

/// Cosine from which two memories count as the same thing told twice (an old and a new version of it).
pub const TWIN: f32 = 0.90;

/// A summary brings along at least this many children, and more while they match about as well as the best one.
const EXPAND_MIN: usize = 3;
const EXPAND_NEAR: f32 = 0.08;

/// How many top hits are looked at to put the newest of two versions of the same thing first.
const REORDER_POOL: usize = 12;

/// Words that say the question is about how things stand now: of two versions of the same memory the newer is the
/// answer.
const NOW_CJK: [&str; 6] = ["现在", "目前", "当前", "最新", "如今", "眼下"];
const NOW_WORDS: [&str; 5] = ["now", "current", "currently", "latest", "nowadays"];

fn asks_about_now(query: &str) -> bool {
    NOW_CJK.iter().any(|w| query.contains(w))
        || query.contains("at the moment")
        || query
            .split(|c: char| !c.is_alphanumeric())
            .any(|w| NOW_WORDS.contains(&w))
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn start(text: &str, chars: usize) -> String {
    text.chars().take(chars).collect()
}

/// How much of a node a search result carries.
const SNIPPET: usize = 420;
const CHILD_SNIPPET: usize = 200;

/// Scores as distances from their mean, in standard deviations.
fn standardize(scores: &mut [f32]) {
    let n = scores.len() as f32;
    let mean = scores.iter().sum::<f32>() / n;
    let spread = (scores.iter().map(|s| (s - mean).powi(2)).sum::<f32>() / n).sqrt() + 1e-9;
    for s in scores {
        *s = (*s - mean) / spread;
    }
}

/// Positions of the `n` largest values.
fn top_positions(scores: &[f32], n: usize) -> HashSet<usize> {
    let mut order: Vec<usize> = (0..scores.len()).collect();
    if n < order.len() {
        order.select_nth_unstable_by(n, |&a, &b| scores[b].total_cmp(&scores[a]));
        order.truncate(n);
    }
    order.into_iter().collect()
}

impl Tree {
    pub fn search(&self, opts: &SearchOpts) -> Result<Vec<Hit>> {
        let mut phrasings: Vec<&str> = vec![];
        for q in std::iter::once(&opts.query).chain(&opts.also) {
            let q = q.trim();
            if !q.is_empty() && !phrasings.contains(&q) {
                phrasings.push(q);
            }
        }
        if phrasings.is_empty() {
            return Ok(vec![]);
        }
        let layers = (opts.min_layer.is_some() || opts.max_layer.is_some()).then(|| self.layers());
        let allow = |n: &Node| {
            if let Some(tag) = &opts.tag
                && !n.tags.contains(tag)
            {
                return false;
            }
            if let Some(scope) = &opts.scope
                && !self.within(&n.id, scope)
            {
                return false;
            }
            if !n.occurred.is_empty() {
                if !opts.since.is_empty() && n.occurred.as_str() < opts.since.as_str() {
                    return false;
                }
                let head = n.occurred.get(..opts.until.len()).unwrap_or(&n.occurred);
                if !opts.until.is_empty() && head > opts.until.as_str() {
                    return false;
                }
            }
            if let Some(layers) = &layers {
                let layer = layers.get(&n.id).copied().unwrap_or(0);
                if opts.min_layer.is_some_and(|m| layer < m)
                    || opts.max_layer.is_some_and(|m| layer > m)
                {
                    return false;
                }
            }
            true
        };
        let allowed = |id: &str| self.live(id).is_some_and(&allow);
        let docs: Vec<(&str, &[f32])> = self
            .vectors
            .iter()
            .filter(|(id, _)| allowed(id))
            .map(|(id, v)| (id.as_str(), v.as_slice()))
            .collect();
        if docs.is_empty() {
            return Ok(vec![]);
        }
        let at: HashMap<&str, usize> = docs.iter().enumerate().map(|(i, d)| (d.0, i)).collect();
        let pool = (opts.k * 8).max(40);
        let mut fused = vec![0.0f32; docs.len()];
        let mut best_cos = vec![f32::MIN; docs.len()];
        let mut by_words = vec![false; docs.len()];
        let mut by_meaning: HashSet<usize> = HashSet::new();
        let mut vectors: Vec<Vec<f32>> = vec![];
        for phrasing in &phrasings {
            let qv = self.embed.encode(phrasing)?;
            let mut meaning: Vec<f32> = docs.iter().map(|d| cosine(&qv, d.1)).collect();
            let mut words = vec![0.0f32; docs.len()];
            for (id, score) in self.lex.score(phrasing, &allowed) {
                if let Some(&i) = at.get(id.as_str()) {
                    words[i] = score;
                    by_words[i] = true;
                }
            }
            by_meaning.extend(top_positions(&meaning, pool));
            for (b, &c) in best_cos.iter_mut().zip(&meaning) {
                *b = b.max(c);
            }
            standardize(&mut meaning);
            standardize(&mut words);
            for (i, f) in fused.iter_mut().enumerate() {
                *f += SEMANTIC * meaning[i] + (1.0 - SEMANTIC) * words[i];
            }
            vectors.push(qv);
        }
        let mut order: Vec<usize> = (0..docs.len()).collect();
        order.sort_by(|&a, &b| {
            fused[b]
                .total_cmp(&fused[a])
                .then_with(|| docs[a].0.cmp(docs[b].0))
        });
        order.truncate(opts.k.max(1).max(REORDER_POOL));
        let lower: Vec<String> = phrasings.iter().map(|p| p.to_lowercase()).collect();
        if lower.iter().any(|q| asks_about_now(q)) {
            self.newest_first(&mut order, &docs);
        }
        order.truncate(opts.k.max(1));
        let top_cos = |c: &str| {
            self.vectors.get(c).map_or(f32::MIN, |v| {
                vectors
                    .iter()
                    .map(|q| cosine(q, v))
                    .fold(f32::MIN, f32::max)
            })
        };
        Ok(order
            .into_iter()
            .map(|i| {
                let id = docs[i].0;
                let node = &self.nodes[id];
                let mut kids: Vec<(&String, f32)> =
                    self.children(id).iter().map(|c| (c, top_cos(c))).collect();
                kids.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
                let close = kids.first().map_or(0, |best| {
                    kids.iter().filter(|k| k.1 >= best.1 - EXPAND_NEAR).count()
                });
                let shown = close.max(EXPAND_MIN).min(opts.expand);
                let text = format!("{}\n{}\n{}", node.title, node.content, node.tags.join(" "))
                    .to_lowercase();
                Hit {
                    id: id.to_owned(),
                    layer: self.layer(id),
                    matched: match (by_words[i], by_meaning.contains(&i)) {
                        (true, true) => "both",
                        (true, false) => "keyword",
                        _ => "semantic",
                    },
                    relevance: best_cos[i],
                    exact: lower.iter().any(|q| text.contains(q.as_str())),
                    title: node.label(),
                    content: start(&node.content, SNIPPET),
                    truncated: node.content.chars().count() > SNIPPET,
                    path: self
                        .ancestors(id)
                        .into_iter()
                        .take(4)
                        .map(|a| (a.id.clone(), a.label()))
                        .collect(),
                    expanded: kids
                        .into_iter()
                        .take(shown)
                        .map(|(c, _)| {
                            let child = &self.nodes[c];
                            (
                                c.clone(),
                                child.label(),
                                start(&child.content, CHILD_SNIPPET),
                            )
                        })
                        .collect(),
                }
            })
            .collect())
    }

    /// Of two memories that say nearly the same thing, the one that happened earlier goes behind the newer one when
    /// both are among the hits: the question is about now.
    fn newest_first(&self, order: &mut Vec<usize>, docs: &[(&str, &[f32])]) {
        let when = |i: usize| self.nodes[docs[i].0].occurred.as_str();
        let plain = |i: usize| self.children(docs[i].0).is_empty();
        // Every move puts a node behind a strictly newer one, so this ends.
        'again: loop {
            for pos in 0..order.len() {
                let old = order[pos];
                if !plain(old) || when(old).is_empty() {
                    continue;
                }
                let last = (pos + 1..order.len()).rev().find(|&p| {
                    let new = order[p];
                    plain(new) && when(new) > when(old) && cosine(docs[old].1, docs[new].1) >= TWIN
                });
                if let Some(p) = last {
                    let moved = order.remove(pos);
                    order.insert(p, moved);
                    continue 'again;
                }
            }
            return;
        }
    }
}

/// True when no hit is clearly about the question: the hits are only clues.
pub fn no_strong_match(hits: &[Hit]) -> bool {
    !hits.iter().any(|h| h.exact || h.relevance >= STRONG)
}
