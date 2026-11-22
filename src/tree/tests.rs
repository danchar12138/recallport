use super::lexicon::terms;
use super::{model, search::Hit, *};
use crate::embedding;
use std::sync::Arc;

/// A bag-of-words "model": every word of the text lights one direction, so texts that share words are close.
struct Words;
impl Embed for Words {
    fn encode(&self, input: &str) -> Result<Vec<f32>> {
        let mut v = vec![0.0f32; embedding::DIMENSIONS as usize];
        for w in terms(input) {
            let h = w.bytes().fold(7usize, |h, b| {
                h.wrapping_mul(31).wrapping_add(usize::from(b))
            });
            let at = h % v.len();
            v[at] += 1.0;
        }
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1.0);
        v.iter_mut().for_each(|x| *x /= norm);
        Ok(v)
    }
}

fn open(dir: &Path) -> Tree {
    Tree::open(dir, Arc::new(Words)).unwrap()
}

fn leaf(t: &mut Tree, text: &str, day: &str) -> Node {
    t.write(
        Draft {
            content: text.into(),
            occurred: day.into(),
            ..Default::default()
        },
        "t",
    )
    .unwrap()
}

fn find(t: &Tree, q: &str) -> Vec<Hit> {
    t.search(&SearchOpts {
        query: q.into(),
        ..Default::default()
    })
    .unwrap()
}

#[test]
fn a_summary_adopts_nodes_and_layers_follow_from_the_shape() {
    let dir = tempfile::tempdir().unwrap();
    let mut t = open(dir.path());
    let a = leaf(&mut t, "备份每天凌晨三点跑", "2024-01-01");
    let b = leaf(&mut t, "备份保留最近七份", "2024-01-02");
    let sum = t
        .write(
            Draft {
                content: "备份策略的结论".into(),
                adopt: vec![a.id.clone(), b.id.clone()],
                ..Default::default()
            },
            "t",
        )
        .unwrap();
    let top = t
        .write(
            Draft {
                content: "整个服务器的总结".into(),
                adopt: vec![sum.id.clone()],
                ..Default::default()
            },
            "t",
        )
        .unwrap();
    assert_eq!(
        (t.layer(&a.id), t.layer(&sum.id), t.layer(&top.id)),
        (0, 1, 2)
    );
    assert_eq!(
        t.live(&a.id).unwrap().parent.as_deref(),
        Some(sum.id.as_str())
    );
    assert_eq!(t.children(&sum.id).len(), 2);
    assert_eq!(
        t.ancestors(&a.id)
            .iter()
            .map(|n| n.id.as_str())
            .collect::<Vec<_>>(),
        vec![sum.id.as_str(), top.id.as_str()]
    );
    // a node cannot be adopted by one of its own descendants
    let err = t
        .write(
            Draft {
                content: "绕回来".into(),
                parent: Some(a.id.clone()),
                adopt: vec![top.id.clone()],
                ..Default::default()
            },
            "t",
        )
        .unwrap_err();
    assert!(err.to_string().contains("子孙"), "{err}");
}

#[test]
fn updates_need_the_current_version_and_keep_every_revision() {
    let dir = tempfile::tempdir().unwrap();
    let mut t = open(dir.path());
    let n = leaf(&mut t, "NAS 可用 12TB", "2024-02-01");
    let newer = t
        .update(
            &n.id,
            1,
            Patch {
                content: Some("NAS 可用 11.6TB".into()),
                ..Default::default()
            },
            "codex",
        )
        .unwrap();
    assert_eq!(newer.version, 2);
    // a second writer holding the old version is refused instead of overwriting
    let err = t
        .update(
            &n.id,
            1,
            Patch {
                content: Some("别的".into()),
                ..Default::default()
            },
            "other",
        )
        .unwrap_err();
    assert!(err.to_string().contains("已被改动"), "{err}");
    let revisions = t.history(&n.id);
    assert_eq!(revisions.len(), 2);
    assert_eq!(revisions[0].node.content, "NAS 可用 12TB");
    assert_eq!(revisions[1].actor, "codex");
    // search sees only the current text
    assert!(find(&t, "11.6TB").iter().any(|h| h.id == n.id));
    assert!(find(&t, "12TB").iter().all(|h| !h.content.contains("12TB")));
}

#[test]
fn delete_is_soft_children_move_up_and_everything_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (a, mid, top);
    {
        let mut t = open(dir.path());
        a = leaf(&mut t, "路由器密码改过了", "2024-03-01");
        mid = t
            .write(
                Draft {
                    content: "家里网络的小结".into(),
                    adopt: vec![a.id.clone()],
                    ..Default::default()
                },
                "t",
            )
            .unwrap();
        top = t
            .write(
                Draft {
                    content: "家里设备总览".into(),
                    adopt: vec![mid.id.clone()],
                    ..Default::default()
                },
                "t",
            )
            .unwrap();
        let version = t.live(&mid.id).unwrap().version;
        assert!(
            t.delete(&mid.id, version, "", "t").is_err(),
            "a reason is required"
        );
        assert!(
            t.delete(&mid.id, version - 1, "并入总览", "t").is_err(),
            "the version must match"
        );
        let moved = t.delete(&mid.id, version, "并入总览", "t").unwrap();
        assert_eq!(moved, vec![a.id.clone()]);
        assert_eq!(
            t.live(&a.id).unwrap().parent.as_deref(),
            Some(top.id.as_str())
        );
        assert!(t.live(&mid.id).is_none() && t.trash().len() == 1);
        assert!(find(&t, "家里网络的小结").iter().all(|h| h.id != mid.id));
    }
    let mut t = open(dir.path());
    assert_eq!(t.len(), 2);
    assert_eq!(t.layer(&top.id), 1);
    assert_eq!(t.trash()[0].id, mid.id);
    let back = t.restore(&mid.id, "t").unwrap();
    assert_eq!(
        back.parent.as_deref(),
        Some(top.id.as_str()),
        "it returns to its old parent"
    );
    assert!(find(&t, "家里网络的小结").iter().any(|h| h.id == mid.id));
}

#[test]
fn reparenting_cannot_make_a_loop() {
    let dir = tempfile::tempdir().unwrap();
    let mut t = open(dir.path());
    let a = leaf(&mut t, "甲", "");
    let b = t
        .write(
            Draft {
                content: "乙".into(),
                parent: Some(a.id.clone()),
                ..Default::default()
            },
            "t",
        )
        .unwrap();
    let err = t
        .reparent(std::slice::from_ref(&a.id), Some(&b.id), "t")
        .unwrap_err();
    assert!(err.to_string().contains("子孙"), "{err}");
    t.reparent(std::slice::from_ref(&b.id), None, "t").unwrap();
    assert!(t.live(&b.id).unwrap().parent.is_none());
}

#[test]
fn a_summary_hit_brings_its_best_children_and_the_chain_above() {
    let dir = tempfile::tempdir().unwrap();
    let mut t = open(dir.path());
    let kids: Vec<Node> = [
        "评论先试了 Disqus 加载慢",
        "改用 Giscus 存在 GitHub",
        "预览域名要加进白名单",
        "RSS 只输出最近二十篇",
    ]
    .iter()
    .enumerate()
    .map(|(i, text)| leaf(&mut t, text, &format!("2024-0{}-01", i + 1)))
    .collect();
    let sum = t
        .write(
            Draft {
                content: "订阅与评论的结论".into(),
                adopt: kids.iter().map(|k| k.id.clone()).collect(),
                ..Default::default()
            },
            "t",
        )
        .unwrap();
    t.write(
        Draft {
            content: "个人网站总结".into(),
            adopt: vec![sum.id.clone()],
            ..Default::default()
        },
        "t",
    )
    .unwrap();
    let hits = t
        .search(&SearchOpts {
            query: "订阅与评论 Giscus 评论".into(),
            expand: 2,
            ..Default::default()
        })
        .unwrap();
    let top = hits
        .iter()
        .find(|h| h.id == sum.id)
        .expect("the summary is found");
    assert_eq!(top.layer, 1);
    assert_eq!(top.expanded.len(), 2);
    assert!(
        top.expanded.iter().any(|(_, c)| c.contains("Giscus")),
        "{:?}",
        top.expanded
    );
    assert_eq!(top.path.len(), 1);
    // a leaf reports the summaries above it
    let leaf_hit = hits
        .iter()
        .find(|h| h.id == kids[1].id)
        .expect("the leaf is found");
    assert_eq!(leaf_hit.path.len(), 2);
    // scope and layer filters
    let only_below = t
        .search(&SearchOpts {
            query: "评论".into(),
            scope: Some(sum.id.clone()),
            max_layer: Some(0),
            ..Default::default()
        })
        .unwrap();
    assert!(
        only_below
            .iter()
            .all(|h| h.layer == 0 && t.within(&h.id, &sum.id))
    );
    // time range
    let early = t
        .search(&SearchOpts {
            query: "评论 RSS 预览".into(),
            since: "2024-02".into(),
            until: "2024-03".into(),
            max_layer: Some(0),
            ..Default::default()
        })
        .unwrap();
    let days: Vec<_> = early
        .iter()
        .map(|h| t.live(&h.id).unwrap().occurred.clone())
        .collect();
    assert!(
        !days.is_empty()
            && days
                .iter()
                .all(|d| ("2024-02".."2024-04").contains(&d.as_str())),
        "{days:?}"
    );
}

#[test]
fn maintenance_reports_missing_layers_and_summaries_that_fell_behind() {
    let dir = tempfile::tempdir().unwrap();
    let mut t = open(dir.path());
    let leaves: Vec<Node> = (0..30)
        .map(|i| leaf(&mut t, &format!("经历 {i} 号 专题{}", i % 3), ""))
        .collect();
    let m = t.maintenance();
    assert_eq!(m.layers, vec![(0, 30, 0)]);
    let (layer, extend, groups) = m.gaps.expect("thirty loose nodes need summaries");
    assert_eq!(layer, 0);
    assert!(extend.is_empty());
    assert_eq!(groups.iter().map(Vec::len).sum::<usize>(), 30);
    // three summaries of ten: the tree is complete, nothing left to do
    let sums: Vec<Node> = leaves
        .chunks(10)
        .map(|c| {
            t.write(
                Draft {
                    content: "十条的汇总".into(),
                    adopt: c.iter().map(|n| n.id.clone()).collect(),
                    ..Default::default()
                },
                "t",
            )
            .unwrap()
        })
        .collect();
    let m = t.maintenance();
    assert!(m.gaps.is_none() && m.stale.is_empty(), "{m:?}");
    assert_eq!(m.layers, vec![(0, 30, 30), (1, 3, 0)]);
    // three children (a quarter of ten) change after their summary: it is flagged until it is touched again
    for n in &leaves[..3] {
        let version = t.live(&n.id).unwrap().version;
        t.update(
            &n.id,
            version,
            Patch {
                content: Some(format!("{} 改过", n.content)),
                ..Default::default()
            },
            "t",
        )
        .unwrap();
    }
    let m = t.maintenance();
    assert_eq!(m.stale.len(), 1);
    assert_eq!(m.stale[0].0, sums[0].id);
    let version = t.live(&sums[0].id).unwrap().version;
    t.update(&sums[0].id, version, Patch::default(), "t")
        .unwrap();
    assert!(t.maintenance().stale.is_empty());
}

/// Timing of a library of twenty thousand nodes (run with `cargo test --release scale -- --ignored --nocapture`).
#[test]
#[ignore = "timing only"]
fn scale_twenty_thousand_nodes() {
    let dir = tempfile::tempdir().unwrap();
    let mut t = open(dir.path());
    let stamp = now_for_tests();
    let events: Vec<Event> = (0..20_000)
        .map(|i| Event {
            at: stamp.clone(),
            actor: "bench".into(),
            op: "import".into(),
            reason: String::new(),
            node: Node {
                id: format!("{i:032x}"),
                content: format!(
                    "第 {i} 条经历：服务 {} 的端口、备份和故障 {} 的处理记录，结论 {}",
                    i % 97,
                    i % 31,
                    i % 13
                ),
                tags: vec![format!("项目{}", i % 50)],
                parent: None,
                refs: vec![],
                sources: vec![],
                attachments: vec![],
                version: 1,
                occurred: format!("2024-{:02}-{:02}", i % 12 + 1, i % 28 + 1),
                created: stamp.clone(),
                updated: stamp.clone(),
                status: Status::Live,
            },
        })
        .collect();
    let started = std::time::Instant::now();
    t.replace(events, &HashMap::new()).unwrap();
    println!("import + index: {:?}", started.elapsed());
    let started = std::time::Instant::now();
    for q in ["服务 5 端口 备份", "故障 7 处理记录", "结论 3"] {
        let hits = t
            .search(&SearchOpts {
                query: q.into(),
                ..Default::default()
            })
            .unwrap();
        assert!(!hits.is_empty());
    }
    println!("3 searches: {:?}", started.elapsed());
    let started = std::time::Instant::now();
    let m = t.maintenance();
    println!(
        "maintenance over 20k roots: {:?}, layers {:?}",
        started.elapsed(),
        m.layers
    );
}

fn now_for_tests() -> String {
    model::now()
}
