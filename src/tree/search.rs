//! Search over every layer at once, then context around what was found.
//!
//! Keyword (BM25) and meaning (cosine of the vectors) each rank all the live nodes in scope; the two lists are fused
//! by reciprocal rank. A hit comes with the chain of summaries above it, and a hit that is itself a summary comes
//! with the children that match the question best, so an overview question lands on the evidence behind it
//! without the caller walking down the tree.
use super::{Tree, model::Node};
use anyhow::Result;
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct SearchOpts {
    pub query: String,
    /// How many hits.
    pub k: usize,
    /// Only this node and what is under it.
    pub scope: Option<String>,
    pub min_layer: Option<u32>,
    pub max_layer: Option<u32>,
    /// Keep nodes whose `occurred` lies in this range (`YYYY-MM-DD` prefixes); nodes with no date always stay.
    pub since: String,
    pub until: String,
    /// How many of the best-matching children come along with a hit that is a summary.
    pub expand: usize,
    /// Only nodes that carry this tag.
    pub tag: Option<String>,
}

impl Default for SearchOpts {
    fn default() -> Self {
        Self {
            query: String::new(),
            k: 8,
            scope: None,
            min_layer: None,
            max_layer: None,
            since: String::new(),
            until: String::new(),
            expand: 3,
            tag: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Hit {
    pub id: String,
    pub layer: u32,
    pub score: f32,
    /// Cosine of the query and the node: how close in meaning, whatever the keywords did.
    pub relevance: f32,
    /// The whole query (lower-cased) occurs in the content or the tags.
    pub exact: bool,
    /// keyword, semantic or both.
    pub matched: &'static str,
    pub content: String,
    /// The content was cut short; `get` returns all of it.
    pub truncated: bool,
    /// The summaries above it, nearest first, as (id, start of the content).
    pub path: Vec<(String, String)>,
    /// For a summary: the children that match the query best, as (id, content).
    pub expanded: Vec<(String, String)>,
}

const RRF: f32 = 60.0;

/// Cosine from which a node counts as clearly about the question.
pub const STRONG: f32 = 0.88;

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn start(text: &str, chars: usize) -> String {
    text.chars().take(chars).collect()
}

/// How much of a node a search result carries.
const SNIPPET: usize = 420;
const CHILD_SNIPPET: usize = 200;

impl Tree {
    pub fn search(&self, opts: &SearchOpts) -> Result<Vec<Hit>> {
        let query = opts.query.trim();
        if query.is_empty() {
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
        let keyword = self.lex.score(query, &allowed);
        let qv = self.embed.encode(query)?;
        let mut semantic: Vec<(&str, f32)> = self
            .vectors
            .iter()
            .filter(|(id, _)| allowed(id))
            .map(|(id, v)| (id.as_str(), cosine(&qv, v)))
            .collect();
        semantic.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        let pool = (opts.k * 8).max(40);
        let mut fused: HashMap<&str, (f32, bool, bool)> = HashMap::new();
        for (rank, (id, _)) in keyword.iter().take(pool).enumerate() {
            let e = fused.entry(id.as_str()).or_default();
            e.0 += 1.0 / (RRF + rank as f32 + 1.0);
            e.1 = true;
        }
        for (rank, (id, _)) in semantic.iter().take(pool).enumerate() {
            let e = fused.entry(id).or_default();
            e.0 += 1.0 / (RRF + rank as f32 + 1.0);
            e.2 = true;
        }
        let mut ranked: Vec<(&str, (f32, bool, bool))> = fused.into_iter().collect();
        ranked.sort_by(|a, b| b.1.0.total_cmp(&a.1.0).then_with(|| a.0.cmp(b.0)));
        Ok(ranked
            .into_iter()
            .take(opts.k.max(1))
            .map(|(id, (score, kw, sem))| {
                let node = &self.nodes[id];
                let mut expanded: Vec<(String, f32)> = self
                    .children(id)
                    .iter()
                    .filter_map(|c| Some((c.clone(), cosine(&qv, self.vectors.get(c)?))))
                    .collect();
                expanded.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                Hit {
                    id: id.to_owned(),
                    layer: self.layer(id),
                    score,
                    matched: match (kw, sem) {
                        (true, true) => "both",
                        (true, false) => "keyword",
                        _ => "semantic",
                    },
                    relevance: self.vectors.get(id).map_or(0.0, |v| cosine(&qv, v)),
                    exact: format!(
                        "{}
{}",
                        node.content,
                        node.tags.join(" ")
                    )
                    .to_lowercase()
                    .contains(&query.to_lowercase()),
                    content: start(&node.content, SNIPPET),
                    truncated: node.content.chars().count() > SNIPPET,
                    path: self
                        .ancestors(id)
                        .into_iter()
                        .take(4)
                        .map(|a| (a.id.clone(), start(&a.content, 40)))
                        .collect(),
                    expanded: expanded
                        .into_iter()
                        .take(opts.expand)
                        .map(|(c, _)| (c.clone(), start(&self.nodes[&c].content, CHILD_SNIPPET)))
                        .collect(),
                }
            })
            .collect())
    }
}

/// True when no hit is clearly about the question: the hits are only clues.
pub fn no_strong_match(hits: &[Hit]) -> bool {
    !hits.iter().any(|h| h.exact || h.relevance >= STRONG)
}
