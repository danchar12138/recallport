//! Retrieval study (not part of the product): replays a corpus of events into a real tree with the real model and
//! dumps everything a ranking experiment needs — node texts, terms, vectors in several flavours, query vectors —
//! so fusion and scoring variants can be tried offline without embedding again.
//!
//! `STUDY_CORPUS=… STUDY_OUT=… STUDY_MODEL=… cargo test --release study_dump -- --ignored --nocapture`
use super::{lexicon::terms, model::Patch, *};
use crate::embedding::Granite;
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc, time::Instant};

fn text(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().to_owned()
}

/// The title a corpus event is written with: its own, or for the memories of the corpus that have none (every node
/// has a title now) the first clause of its text, 16 characters at most.
fn title_of(ev: &Value) -> String {
    let title = text(ev, "title");
    if !title.is_empty() {
        return title;
    }
    let content = text(ev, "content");
    let clause = content
        .split(['。', '；', '！', '？', ';', '，', ',', '\n'])
        .map(str::trim)
        .find(|c| !c.is_empty())
        .unwrap_or_default();
    clause.chars().take(16).collect()
}

fn list(v: &Value, k: &str) -> Vec<String> {
    v[k].as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

struct Replayed {
    tree: Tree,
    _dir: tempfile::TempDir,
    ids: HashMap<String, String>,
    old_versions: Vec<Value>,
    deleted: Vec<String>,
    write_ms: Vec<f64>,
    embed: Arc<Granite>,
}

fn replay(corpus: &Value) -> Replayed {
    let embed = Arc::new(
        Granite::open(std::path::Path::new(&std::env::var("STUDY_MODEL").unwrap())).unwrap(),
    );
    let dir = tempfile::tempdir().unwrap();
    let mut tree = Tree::open(dir.path(), embed.clone()).unwrap();
    let mut ids: HashMap<String, String> = HashMap::new();
    let mut old_versions: Vec<Value> = vec![];
    let mut deleted: Vec<String> = vec![];
    let mut write_ms: Vec<f64> = vec![];
    for ev in corpus["events"].as_array().unwrap() {
        let key = text(ev, "key");
        match ev["op"].as_str().unwrap() {
            "write" | "summary" => {
                let t0 = Instant::now();
                let node = tree
                    .write(
                        Draft {
                            title: title_of(ev),
                            content: text(ev, "content"),
                            tags: list(ev, "tags"),
                            occurred: text(ev, "occurred"),
                            adopt: list(ev, "adopt").iter().map(|k| ids[k].clone()).collect(),
                            ..Default::default()
                        },
                        "study",
                    )
                    .unwrap_or_else(|e| panic!("write {key}: {e}"));
                write_ms.push(t0.elapsed().as_secs_f64() * 1000.0);
                ids.insert(key, node.id);
            }
            "update" => {
                let id = ids[&key].clone();
                let old = tree.node(&id).unwrap().clone();
                old_versions.push(json!({"key": key, "title": old.title, "content": old.content, "version": old.version}));
                let patch = Patch {
                    content: Some(text(ev, "content")),
                    title: ev["title"]
                        .as_str()
                        .filter(|t| !t.is_empty())
                        .map(str::to_owned),
                    occurred: ev["occurred"]
                        .as_str()
                        .filter(|t| !t.is_empty())
                        .map(str::to_owned),
                    ..Default::default()
                };
                tree.update(&id, old.version, patch, "study")
                    .unwrap_or_else(|e| panic!("update {key}: {e}"));
            }
            "delete" => {
                let id = ids[&key].clone();
                let v = tree.node(&id).unwrap().version;
                tree.delete(&id, v, "study", "study").unwrap();
                deleted.push(key);
            }
            other => panic!("op {other}"),
        }
    }
    Replayed {
        tree,
        _dir: dir,
        ids,
        old_versions,
        deleted,
        write_ms,
        embed,
    }
}

fn corpus() -> Value {
    serde_json::from_str(&std::fs::read_to_string(std::env::var("STUDY_CORPUS").unwrap()).unwrap())
        .unwrap()
}

#[test]
#[ignore]
fn study_dump() {
    let corpus = corpus();
    let Replayed {
        tree,
        ids,
        mut old_versions,
        deleted,
        write_ms,
        embed,
        ..
    } = replay(&corpus);
    let layers = tree.layers();
    let rev: HashMap<&String, &String> = ids.iter().map(|(k, v)| (v, k)).collect();
    let mut nodes = vec![];
    let mut ct = vec![];
    for n in tree.nodes() {
        if tree.live(&n.id).is_none() {
            continue;
        }
        ct.push((
            n.id.clone(),
            n.content.clone(),
            format!("{}\n{}\n{}", n.title, n.content, n.tags.join(" ")),
        ));
        nodes.push(json!({
            "key": rev[&n.id], "layer": layers.get(&n.id).copied().unwrap_or(0), "title": n.title, "content": n.content,
            "tags": n.tags, "occurred": n.occurred, "version": n.version, "parent": n.parent.as_ref().map(|p| rev[p].clone()),
            "terms_title": terms(&n.title), "terms_content": terms(&n.content), "terms_tags": terms(&n.tags.join(" ")),
            "v_cur": tree.vectors.get(&n.id),
        }));
    }
    let contents: Vec<String> = ct.iter().map(|c| c.1.clone()).collect();
    let tagged: Vec<String> = ct.iter().map(|c| c.2.clone()).collect();
    let v_content = embed.encode_many(&contents).unwrap();
    let v_tagged = embed.encode_many(&tagged).unwrap();
    let order: HashMap<&str, usize> = ct
        .iter()
        .enumerate()
        .map(|(i, c)| (c.0.as_str(), i))
        .collect();
    for n in nodes.iter_mut() {
        let id = &ids[n["key"].as_str().unwrap()];
        let i = order[id.as_str()];
        n["v_content"] = json!(v_content[i]);
        n["v_tagged"] = json!(v_tagged[i]);
    }
    let old_texts: Vec<String> = old_versions
        .iter()
        .map(|o| {
            format!(
                "{}\n{}",
                o["title"].as_str().unwrap(),
                o["content"].as_str().unwrap()
            )
        })
        .collect();
    let old_vecs = embed.encode_many(&old_texts).unwrap();
    for (o, v) in old_versions.iter_mut().zip(old_vecs) {
        o["v"] = json!(v);
        o["terms"] = json!(terms(&old_texts_of(o)));
    }
    let mut queries = vec![];
    for q in corpus["queries"].as_array().unwrap() {
        let mut qs = vec![];
        for s in list(q, "qs") {
            qs.push(json!({"q": s, "v": embed.encode(&s).unwrap(), "terms": terms(&s)}));
        }
        let mut item = q.clone();
        item["qv"] = json!(qs);
        queries.push(item);
    }
    let out = json!({
        "persona": corpus["persona"], "today": corpus["today"], "nodes": nodes, "old": old_versions,
        "queries": queries, "deleted": deleted, "write_ms": write_ms,
        "texts": {"note": "v_cur = title+content (embed_text), v_content, v_tagged = title+content+tags"},
    });
    std::fs::write(
        std::env::var("STUDY_OUT").unwrap(),
        serde_json::to_string(&out).unwrap(),
    )
    .unwrap();
    println!(
        "nodes {} queries {} writes avg {:.1} ms",
        nodes_len(&out),
        queries_len(&out),
        write_ms.iter().sum::<f64>() / write_ms.len() as f64
    );
}

fn old_texts_of(o: &Value) -> String {
    format!(
        "{}\n{}",
        o["title"].as_str().unwrap(),
        o["content"].as_str().unwrap()
    )
}

fn nodes_len(v: &Value) -> usize {
    v["nodes"].as_array().map_or(0, Vec::len)
}

fn queries_len(v: &Value) -> usize {
    v["queries"].as_array().map_or(0, Vec::len)
}

/// The product's own search over the same corpus: one line per query type, so the offline simulation can be checked.
#[test]
#[ignore]
fn study_eval() {
    use std::collections::BTreeMap;
    let corpus = corpus();
    let r = replay(&corpus);
    let key_of: HashMap<&String, &String> = r.ids.iter().map(|(k, v)| (v, k)).collect();
    let multi = std::env::var("STUDY_MULTI").is_ok();
    // type -> (n, @1, @3, @5, mrr sum)
    let mut table: BTreeMap<String, (f64, f64, f64, f64, f64)> = BTreeMap::new();
    let mut absent = (0.0f64, 0.0f64);
    for q in corpus["queries"].as_array().unwrap() {
        let qs = list(q, "qs");
        let hits = r
            .tree
            .search(&SearchOpts {
                query: qs[0].clone(),
                also: if multi { qs[1..].to_vec() } else { vec![] },
                ..Default::default()
            })
            .unwrap();
        let kind = text(q, "type");
        let gold = list(q, "gold");
        if gold.is_empty() {
            absent.0 += 1.0;
            absent.1 += f64::from(super::search::no_strong_match(&hits));
            continue;
        }
        // what the caller sees: the hit itself and the children that come along
        let reach: Vec<Vec<&str>> = hits
            .iter()
            .map(|h| {
                let mut v = vec![key_of[&h.id].as_str()];
                v.extend(h.expanded.iter().map(|(c, _, _)| key_of[c].as_str()));
                v
            })
            .collect();
        let best = gold
            .iter()
            .map(|g| {
                reach
                    .iter()
                    .position(|r| r.contains(&g.as_str()))
                    .unwrap_or(99)
            })
            .min()
            .unwrap();
        let e = table.entry(kind).or_default();
        e.0 += 1.0;
        e.1 += f64::from(best == 0);
        e.2 += f64::from(best < 3);
        e.3 += f64::from(best < 5);
        e.4 += if best < 8 {
            1.0 / (best as f64 + 1.0)
        } else {
            0.0
        };
    }
    let mut all = (0.0, 0.0, 0.0, 0.0, 0.0);
    for (k, v) in &table {
        println!(
            "EVAL {k:10} n={:3} @1={:5.1} @3={:5.1} @5={:5.1} mrr={:.3}",
            v.0,
            100.0 * v.1 / v.0,
            100.0 * v.2 / v.0,
            100.0 * v.3 / v.0,
            v.4 / v.0
        );
        if k != "history" {
            all = (
                all.0 + v.0,
                all.1 + v.1,
                all.2 + v.2,
                all.3 + v.3,
                all.4 + v.4,
            );
        }
    }
    println!(
        "EVAL ALL        n={:3} @1={:5.1} @3={:5.1} @5={:5.1} mrr={:.3} | abstain_ok={:.1}",
        all.0,
        100.0 * all.1 / all.0,
        100.0 * all.2 / all.0,
        100.0 * all.3 / all.0,
        all.4 / all.0,
        100.0 * absent.1 / absent.0.max(1.0)
    );
}

/// Write latency as the tree grows: the whole write against the embedding alone.
#[test]
#[ignore]
fn study_writes() {
    let corpus = corpus();
    let embed = Arc::new(
        Granite::open(std::path::Path::new(&std::env::var("STUDY_MODEL").unwrap())).unwrap(),
    );
    let dir = tempfile::tempdir().unwrap();
    let mut tree = Tree::open(dir.path(), embed.clone()).unwrap();
    let mut ids: HashMap<String, String> = HashMap::new();
    let (mut whole, mut model) = (vec![], vec![]);
    for ev in corpus["events"].as_array().unwrap() {
        if ev["op"] != "write" {
            continue;
        }
        let draft = Draft {
            title: title_of(ev),
            content: text(ev, "content"),
            tags: list(ev, "tags"),
            occurred: text(ev, "occurred"),
            ..Default::default()
        };
        let t0 = Instant::now();
        embed
            .encode(&format!("{}\n{}", draft.title, draft.content))
            .unwrap();
        model.push(t0.elapsed().as_secs_f64() * 1000.0);
        let t0 = Instant::now();
        let node = tree.write(draft, "study").unwrap();
        whole.push(t0.elapsed().as_secs_f64() * 1000.0);
        ids.insert(text(ev, "key"), node.id);
    }
    for (i, (w, m)) in whole.chunks(300).zip(model.chunks(300)).enumerate() {
        let avg = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
        println!(
            "WRITES {}..{}: whole write {:.1} ms (embedding alone {:.1} ms)",
            i * 300,
            i * 300 + w.len(),
            avg(w),
            avg(m)
        );
    }
}

#[test]
#[ignore]
fn study_terms() {
    for t in [
        "Hetzner CX22 月费 €4.59, tauri 2.4.0 IP 192.168.31.20 node.js C++ v1.2-beta 3,299元 12:30 snake_case /usr/bin 2026-11-23 ¥27,000",
        "NAS 可用空间约 14.5TB，内存 20GB",
    ] {
        println!("TERMS {:?}", terms(t));
    }
}
