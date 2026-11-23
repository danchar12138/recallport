//! Keyword search over node text: BM25 over jieba words and Chinese character pairs, kept in memory.
use std::{
    collections::{HashMap, HashSet},
    sync::OnceLock,
};

fn han(c: char) -> bool {
    matches!(c, '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}' | '\u{20000}'..='\u{323af}')
}

/// Numbers, versions, addresses and dates written as one token (`4.59`, `2.4.0`, `192.168.31.20`, `2026-11-23`,
/// `12:30`, `27,000`, `node.js`): the pieces of a Chinese text are cut apart by the word cutter, which would lose
/// them, and they are exactly what a question asks for by name.
fn compounds(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let word = |c: char| c.is_ascii_alphanumeric();
    let joins = |before: char, joiner: char, after: char| match joiner {
        '.' | '-' | '_' => word(before) && word(after),
        ',' | ':' => before.is_ascii_digit() && after.is_ascii_digit(),
        _ => false,
    };
    let mut out = vec![];
    let mut i = 0;
    while i < chars.len() {
        if !word(chars[i]) {
            i += 1;
            continue;
        }
        let from = i;
        let mut joined = false;
        while i < chars.len() {
            if word(chars[i]) {
                i += 1;
            } else if i > from && i + 1 < chars.len() && joins(chars[i - 1], chars[i], chars[i + 1])
            {
                joined = true;
                i += 1;
            } else {
                break;
            }
        }
        if joined {
            out.push(chars[from..i].iter().collect());
            // `14.5tb` is also `14.5`: a question gives the number alone
            let unit = chars[from..i]
                .iter()
                .rev()
                .take_while(|c| c.is_ascii_alphabetic())
                .count();
            if unit > 0 && unit < i - from && chars[i - unit - 1].is_ascii_digit() {
                out.push(chars[from..i - unit].iter().collect());
            }
        }
    }
    out
}

/// The words a text is searched by: jieba words (cut for search), every pair of neighbouring Chinese characters, so
/// a name or a phrase is found whichever way it was split, and the numbers, versions and dates as written.
pub fn terms(text: &str) -> Vec<String> {
    static JIEBA: OnceLock<jieba_rs::Jieba> = OnceLock::new();
    let lower = text.to_lowercase();
    let mut words: Vec<String> = JIEBA
        .get_or_init(jieba_rs::Jieba::new)
        .cut_for_search(&lower, true)
        .into_iter()
        .filter(|w| w.chars().all(char::is_alphanumeric))
        .map(str::to_owned)
        .collect();
    for run in lower.split(|c: char| !han(c)).filter(|s| !s.is_empty()) {
        for pair in run.chars().collect::<Vec<_>>().windows(2) {
            words.push(pair.iter().collect());
        }
    }
    words.extend(compounds(&lower));
    words
}

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
        let words = terms(text);
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
        let mut seen = HashSet::new();
        let mut scores: HashMap<&str, f32> = HashMap::new();
        for w in terms(query) {
            if !seen.insert(w.clone()) {
                continue;
            }
            let Some(posting) = self.postings.get(&w) else {
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
