//! Ask the current memory store (v0.6 `recall`) and the tree the same questions about the same memories, and
//! compare what comes back. A question names the memories that answer it (`gold`, by the key the seed script
//! gave them); a system is good at a question when those memories are in what it returns, near the top, and
//! within a budget of text (what an AI can afford to read).
use super::{SearchOpts, Tree, import};
use crate::store::STRONG;
use crate::{embedding::Granite, memory::Query, store::Store};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
    sync::Arc,
    time::Instant,
};

#[derive(Deserialize)]
struct Question {
    id: String,
    kind: String,
    q: String,
    gold: Vec<String>,
    #[serde(default)]
    from: String,
    #[serde(default)]
    through: String,
}

/// How many results count for "found it" and how much text an AI is assumed to read.
const TOP: usize = 5;
const FETCH: usize = 8;
const BUDGET: usize = 1200;

#[derive(Default, Clone)]
struct Score {
    hit: bool,
    first: Option<usize>,
    covered: f64,
    chars: usize,
}

/// `items` are (id, characters of text) in the order an AI would read them.
fn score(gold: &[String], ids: &[String], items: &[(String, usize)]) -> Score {
    let first = ids.iter().position(|id| gold.contains(id)).map(|p| p + 1);
    let mut used = 0;
    let mut seen = std::collections::HashSet::new();
    for (id, chars) in items {
        if used + chars > BUDGET {
            break;
        }
        used += chars;
        if gold.contains(id) {
            seen.insert(id.clone());
        }
    }
    Score {
        hit: first.is_some_and(|p| p <= TOP),
        first,
        covered: seen.len() as f64 / gold.len().max(1) as f64,
        chars: items.iter().take(TOP).map(|(_, c)| c).sum(),
    }
}

struct Row {
    kind: String,
    v06: Score,
    plain: Score,
    floor: Score,
    tree: Score,
}

pub async fn run(dir: &Path, model: &Path, questions: &Path, keys: &Path) -> Result<()> {
    let mut report = String::new();
    macro_rules! say {
        ($($arg:tt)*) => {{
            use std::fmt::Write as _;
            let _ = writeln!(report, $($arg)*);
        }};
    }
    let embed = Arc::new(Granite::open(model).context("无法加载语义模型")?);
    let store = Store::open(dir, Some(embed.clone())).await?;
    let entries = store.all().await?;
    let keys: HashMap<String, String> = serde_json::from_slice(&std::fs::read(keys)?)?;
    let report_path = questions.with_extension("report.md");
    let questions: Vec<Question> = serde_json::from_slice(&std::fs::read(questions)?)?;

    let tree_dir = std::env::temp_dir().join(format!("rp-tree-eval-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tree_dir);
    let started = Instant::now();
    let mut tree = Tree::open(&tree_dir, embed)?;
    let events = import::events(&entries);
    tree.import(events)?;
    tree.flush()?;
    say!(
        "导入 {} 条记忆 → {} 个节点，建索引 {:.1} 秒\n",
        entries.len(),
        tree.len(),
        started.elapsed().as_secs_f32()
    );

    let (mut v06_ms, mut tree_ms) = (0.0, 0.0);
    let mut rows: Vec<Row> = vec![];
    let mut misses: Vec<String> = vec![];
    let mut negatives: Vec<(String, bool, bool)> = vec![];
    for q in &questions {
        let gold: Vec<String> = q
            .gold
            .iter()
            .map(|k| {
                let key = keys
                    .get(k)
                    .with_context(|| format!("问题 {} 的答案 {k} 不在 keys 里", q.id))?;
                // a key is a memory id, or `text:` followed by the exact text of the memory
                match key.strip_prefix("text:") {
                    Some(text) => entries
                        .iter()
                        .find(|e| e.text == text)
                        .map(|e| e.id.clone())
                        .with_context(|| format!("问题 {} 的答案 {k} 在记忆库里找不到", q.id)),
                    None => Ok(key.clone()),
                }
            })
            .collect::<Result<_>>()?;
        if gold.is_empty() {
            let found = store
                .recall(&Query {
                    text: q.q.clone(),
                    limit: FETCH,
                    strategy: "hybrid".into(),
                    ..Default::default()
                })
                .await?;
            let hits = tree.search(&SearchOpts {
                query: q.q.clone(),
                k: FETCH,
                ..Default::default()
            })?;
            let strong = hits.iter().any(|h| h.exact || h.relevance >= STRONG);
            negatives.push((q.q.clone(), !found.no_strong_match, strong));
            continue;
        }
        let t = Instant::now();
        let found = store
            .recall(&Query {
                text: q.q.clone(),
                limit: FETCH,
                strategy: "hybrid".into(),
                from: q.from.clone(),
                through: q.through.clone(),
                ..Default::default()
            })
            .await?;
        v06_ms += t.elapsed().as_secs_f64() * 1000.0;
        let ids: Vec<String> = found.memories.iter().map(|h| h.entry.id.clone()).collect();
        let items: Vec<(String, usize)> = found
            .memories
            .iter()
            .map(|h| (h.entry.id.clone(), h.entry.text.chars().count()))
            .collect();
        let v06 = score(&gold, &ids, &items);

        let opts = SearchOpts {
            query: q.q.clone(),
            k: FETCH,
            since: q.from.clone(),
            until: q.through.clone(),
            expand: 3,
            ..Default::default()
        };
        let t = Instant::now();
        let hits = tree.search(&opts)?;
        tree_ms += t.elapsed().as_secs_f64() * 1000.0;
        let ids: Vec<String> = hits.iter().map(|h| h.id.clone()).collect();
        let mut items = vec![];
        for h in &hits {
            items.push((h.id.clone(), h.content.chars().count()));
            items.extend(
                h.expanded
                    .iter()
                    .map(|(id, c)| (id.clone(), c.chars().count())),
            );
        }
        let with_tree = score(&gold, &ids, &items);
        // the same search without bringing children along: what the structure adds on top of plain hybrid search
        let plain_hits = tree.search(&SearchOpts {
            expand: 0,
            ..opts.clone()
        })?;
        let plain_ids: Vec<String> = plain_hits.iter().map(|h| h.id.clone()).collect();
        let plain_items: Vec<(String, usize)> = plain_hits
            .iter()
            .map(|h| (h.id.clone(), h.content.chars().count()))
            .collect();
        let plain = score(&gold, &plain_ids, &plain_items);
        // the same plain search, ignoring everything less close in meaning than v0.6 does
        let floor_hits = tree.search(&SearchOpts {
            expand: 0,
            min_cosine: crate::store::SEMANTIC,
            ..opts.clone()
        })?;
        let floor_ids: Vec<String> = floor_hits.iter().map(|h| h.id.clone()).collect();
        let floor_items: Vec<(String, usize)> = floor_hits
            .iter()
            .map(|h| (h.id.clone(), h.content.chars().count()))
            .collect();
        let floor = score(&gold, &floor_ids, &floor_items);
        if v06.hit != with_tree.hit || (v06.covered - with_tree.covered).abs() > 0.2 {
            misses.push(format!(
                "{} [{}] {}  v0.6: 命中={} 首位={:?} 覆盖={:.0}%  树: 命中={} 首位={:?} 覆盖={:.0}%",
                q.id, q.kind, q.q, v06.hit, v06.first, v06.covered * 100.0, with_tree.hit, with_tree.first, with_tree.covered * 100.0
            ));
        }
        rows.push(Row {
            kind: q.kind.clone(),
            v06,
            plain,
            floor,
            tree: with_tree,
        });
    }

    let mut kinds: BTreeMap<String, Vec<&Row>> = BTreeMap::new();
    for r in &rows {
        kinds.entry(r.kind.clone()).or_default().push(r);
    }
    kinds.insert("合计".into(), rows.iter().collect());
    say!(
        "每类问题的平均成绩（命中@{TOP}＝答案在前 {TOP} 条里；首位率＝第一条就是答案；覆盖＝{BUDGET} 字预算内读到的答案占比；文字量＝前 {TOP} 条的平均字数）\n"
    );
    say!("| 问题类型 | 数量 | 系统 | 命中@{TOP} | 首位率 | MRR | 覆盖@{BUDGET} | 文字量 |");
    say!("|---|---|---|---|---|---|---|---|");
    for (kind, rs) in &kinds {
        let n = rs.len() as f64;
        for (name, pick) in [
            (
                "v0.6 recall",
                (|r: &Row| r.v06.clone()) as fn(&Row) -> Score,
            ),
            ("树·纯混检", |r: &Row| r.plain.clone()),
            ("树·纯混检+语义下限", |r: &Row| r.floor.clone()),
            ("树·带子节点", |r: &Row| r.tree.clone()),
        ] {
            let s: Vec<Score> = rs.iter().map(|r| pick(r)).collect();
            let hit = s.iter().filter(|x| x.hit).count() as f64 / n;
            let top1 = s.iter().filter(|x| x.first == Some(1)).count() as f64 / n;
            let mrr = s
                .iter()
                .map(|x| x.first.map_or(0.0, |p| 1.0 / p as f64))
                .sum::<f64>()
                / n;
            let cov = s.iter().map(|x| x.covered).sum::<f64>() / n;
            let chars = s.iter().map(|x| x.chars as f64).sum::<f64>() / n;
            say!(
                "| {kind} | {} | {name} | {:.0}% | {:.0}% | {:.2} | {:.0}% | {:.0} |",
                rs.len(),
                hit * 100.0,
                top1 * 100.0,
                mrr,
                cov * 100.0,
                chars
            );
        }
    }
    say!(
        "\n平均耗时：v0.6 recall {:.0} ms，树 {:.0} ms（含向量化问题）",
        v06_ms / questions.len() as f64,
        tree_ms / questions.len() as f64
    );
    if !negatives.is_empty() {
        let (a, b) = (
            negatives.iter().filter(|n| n.1).count(),
            negatives.iter().filter(|n| n.2).count(),
        );
        say!(
            "
没有答案的问题（谁声称「有强匹配」就是误报）：v0.6 误报 {a}/{n}，树误报 {b}/{n}",
            n = negatives.len()
        );
        for (q, a, b) in &negatives {
            say!("- {q}  v0.6={a} 树={b}");
        }
    }
    say!("\n两边差别较大的问题（{} 个）：", misses.len());
    for m in misses {
        say!("- {m}");
    }
    let _ = std::fs::remove_dir_all(&tree_dir);
    std::fs::write(report_path, report)?;
    Ok(())
}
