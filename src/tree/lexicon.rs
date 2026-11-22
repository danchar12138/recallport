//! Keyword search over node text: BM25 on the same words the memory store indexes (jieba words and Chinese
//! character pairs), kept in memory.
use crate::store::terms;
use std::collections::HashMap;

#[derive(Default)]
pub struct Lexicon {
    /// term -> node id -> how often it occurs
    postings: HashMap<String, HashMap<String, u32>>,
    /// node id -> (its distinct terms, its length in terms)
    docs: HashMap<String, (Vec<String>, u32)>,
    total: u64,
}

const K1: f32 = 1.2;
const B: f32 = 0.75;

impl Lexicon {
    pub fn add(&mut self, id: &str, text: &str) {
        self.remove(id);
        let words: Vec<String> = terms(text, usize::MAX)
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        let mut tf: HashMap<String, u32> = HashMap::new();
        for w in &words {
            *tf.entry(w.clone()).or_default() += 1;
        }
        let distinct: Vec<String> = tf.keys().cloned().collect();
        for (w, n) in tf {
            self.postings.entry(w).or_default().insert(id.to_owned(), n);
        }
        self.total += words.len() as u64;
        self.docs
            .insert(id.to_owned(), (distinct, words.len() as u32));
    }

    pub fn remove(&mut self, id: &str) {
        let Some((words, len)) = self.docs.remove(id) else {
            return;
        };
        for w in words {
            if let Some(p) = self.postings.get_mut(&w) {
                p.remove(id);
                if p.is_empty() {
                    self.postings.remove(&w);
                }
            }
        }
        self.total -= u64::from(len);
    }

    /// Nodes that contain any word of the query, best first; only those `allow` accepts.
    pub fn score(&self, query: &str, allow: &dyn Fn(&str) -> bool) -> Vec<(String, f32)> {
        let n = self.docs.len() as f32;
        if n == 0.0 {
            return vec![];
        }
        let avg = (self.total as f32 / n).max(1.0);
        let mut seen = std::collections::HashSet::new();
        let mut scores: HashMap<&str, f32> = HashMap::new();
        for w in terms(query, usize::MAX).split_whitespace() {
            if !seen.insert(w.to_owned()) {
                continue;
            }
            let Some(posting) = self.postings.get(w) else {
                continue;
            };
            let df = posting.len() as f32;
            let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
            for (id, &tf) in posting {
                if !allow(id) {
                    continue;
                }
                let len = self.docs[id.as_str()].1 as f32;
                let tf = tf as f32;
                *scores.entry(id.as_str()).or_default() +=
                    idf * tf * (K1 + 1.0) / (tf + K1 * (1.0 - B + B * len / avg));
            }
        }
        let mut out: Vec<(String, f32)> = scores
            .into_iter()
            .map(|(id, s)| (id.to_owned(), s))
            .collect();
        out.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out
    }
}
