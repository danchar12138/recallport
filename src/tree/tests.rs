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
            title: "记忆".into(),
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
                title: "汇总".into(),
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
                title: "汇总".into(),
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
                title: "汇总".into(),
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
                    title: "汇总".into(),
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
                    title: "汇总".into(),
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
                title: "乙".into(),
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
                title: "汇总".into(),
                adopt: kids.iter().map(|k| k.id.clone()).collect(),
                ..Default::default()
            },
            "t",
        )
        .unwrap();
    t.write(
        Draft {
            content: "个人网站总结".into(),
            title: "汇总".into(),
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
        top.expanded.iter().any(|(_, _, c)| c.contains("Giscus")),
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
                    title: "汇总".into(),
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
                title: String::new(),
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

#[test]
fn every_node_needs_a_short_title_and_cannot_lose_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut t = open(dir.path());
    let draft = |title: &str, adopt: Vec<String>| Draft {
        title: title.into(),
        content: "备份每天凌晨三点跑。保留七份。".into(),
        adopt,
        ..Default::default()
    };
    // a memory without a title is refused, and so is one that is only blanks
    for title in ["", "  \n "] {
        let error = t.write(draft(title, vec![]), "t").unwrap_err().to_string();
        assert!(error.contains("标题必填"), "{error}");
    }
    let long = "很长".repeat(30);
    assert!(
        t.write(draft(&long, vec![]), "t")
            .unwrap_err()
            .to_string()
            .contains("标题最多")
    );
    assert_eq!(
        t.nodes().count(),
        0,
        "a refused write leaves nothing behind"
    );
    // whitespace and line breaks are squeezed out of a title
    let a = t.write(draft("  备份\n时间 ", vec![]), "t").unwrap();
    assert_eq!(
        (a.title.as_str(), a.label().as_str()),
        ("备份 时间", "备份 时间")
    );
    // a summary needs one as well
    let error = t
        .write(draft("", vec![a.id.clone()]), "t")
        .unwrap_err()
        .to_string();
    assert!(error.contains("标题必填"), "{error}");
    let sum = t.write(draft("备份策略", vec![a.id.clone()]), "t").unwrap();
    // neither a memory nor a summary can lose its title; both can be renamed
    for id in [&a.id, &sum.id] {
        let version = t.node(id).unwrap().version;
        let error = t
            .update(
                id,
                version,
                Patch {
                    title: Some(" ".into()),
                    ..Default::default()
                },
                "t",
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("不能清空"), "{error}");
    }
    let a = t
        .update(
            &a.id,
            2,
            Patch {
                title: Some("备份时间".into()),
                ..Default::default()
            },
            "t",
        )
        .unwrap();
    assert_eq!(a.label(), "备份时间");
    // the title is searched, and counts for more than the same word in the text
    let hits = find(&t, "备份时间");
    assert_eq!(hits[0].id, a.id);
    assert_eq!(hits[0].title, "备份时间");
}

#[test]
fn imported_nodes_without_a_title_are_reported_for_the_ai_to_name() {
    let dir = tempfile::tempdir().unwrap();
    let mut t = open(dir.path());
    let stamp = now_for_tests();
    let id = |n: u32| format!("{n:032x}");
    let old = |n: u32, title: &str, parent: Option<u32>| Event {
        at: stamp.clone(),
        actor: "legacy".into(),
        op: "import".into(),
        reason: String::new(),
        node: Node {
            id: id(n),
            title: title.into(),
            content: format!("第 {n} 条的内容"),
            tags: vec![],
            parent: parent.map(id),
            refs: vec![],
            sources: vec![],
            attachments: vec![],
            version: 1,
            occurred: "2025-01-01".into(),
            created: stamp.clone(),
            updated: stamp.clone(),
            status: Status::Live,
        },
    };
    // an old memory, an old summary and a titled one, written before every node needed a title
    t.replace(
        vec![
            old(1, "", None),
            old(2, "", Some(1)),
            old(3, "有标题", None),
        ],
        &HashMap::new(),
    )
    .unwrap();
    // old data is not refused, it is listed (a memory too, not only a summary), and the label still reads
    let untitled = t.maintenance().untitled;
    assert_eq!(untitled, vec![id(1), id(2)]);
    assert_eq!(t.node(&id(1)).unwrap().label(), "第 1 条的内容");
    let version = t.node(&id(1)).unwrap().version;
    t.update(
        &id(1),
        version,
        Patch {
            title: Some("甲".into()),
            ..Default::default()
        },
        "t",
    )
    .unwrap();
    assert_eq!(t.maintenance().untitled, vec![id(2)]);
    let version = t.node(&id(2)).unwrap().version;
    t.update(
        &id(2),
        version,
        Patch {
            title: Some("乙".into()),
            ..Default::default()
        },
        "t",
    )
    .unwrap();
    assert!(t.maintenance().untitled.is_empty());
}

#[test]
fn phrasings_are_searched_together_and_a_node_that_fits_several_ranks_higher() {
    let dir = tempfile::tempdir().unwrap();
    let mut t = open(dir.path());
    let both = leaf(&mut t, "备份每天凌晨三点跑 restic 快照", "2024-01-01");
    leaf(&mut t, "备份目录放在移动硬盘上", "2024-01-02");
    leaf(&mut t, "快照保留最近七份", "2024-01-03");
    for i in 0..6 {
        leaf(
            &mut t,
            &format!("无关的第{i}条：周末去爬山，带了两瓶水"),
            "2024-02-01",
        );
    }
    let hits = t
        .search(&SearchOpts {
            query: "备份".into(),
            also: vec!["快照".into(), "restic".into(), "  ".into(), "备份".into()],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(hits[0].id, both.id, "{hits:?}");
    // an empty question finds nothing, whatever else is given
    assert!(
        t.search(&SearchOpts {
            query: " ".into(),
            ..Default::default()
        })
        .unwrap()
        .is_empty()
    );
}

#[test]
fn a_question_about_now_puts_the_newer_of_two_versions_first() {
    let dir = tempfile::tempdir().unwrap();
    let mut t = open(dir.path());
    // the older one fits the words of the question better, so by relevance it would come first
    let old = leaf(
        &mut t,
        "笔记应用 依赖 tauri 版本 2.1 rusqlite 0.31 serde 1.0",
        "2025-03-01",
    );
    let new = leaf(
        &mut t,
        "笔记应用 依赖 tauri 版本 2.4 rusqlite 0.33 serde 1.0",
        "2025-09-01",
    );
    leaf(&mut t, "周末去爬山带了两瓶水", "2025-09-02");
    let ask = |q: &str| -> Vec<String> { find(&t, q).into_iter().map(|h| h.id).collect() };
    let plain = ask("笔记应用 依赖 tauri 版本 2.1 rusqlite 0.31");
    assert_eq!(plain[0], old.id, "without a cue the closer text wins");
    let now = ask("现在 笔记应用 依赖 tauri 版本 2.1 rusqlite 0.31");
    assert_eq!(now[0], new.id, "{now:?}");
    assert!(
        now.contains(&old.id),
        "the older one is still there, just behind"
    );
    let english = ask("current 笔记应用 依赖 tauri 版本 2.1 rusqlite 0.31");
    assert_eq!(english[0], new.id);
    // "know" is not "now"
    let know = ask("know 笔记应用 依赖 tauri 版本 2.1 rusqlite 0.31");
    assert_eq!(know[0], old.id);
}

#[test]
fn a_summary_brings_more_children_while_they_match_about_as_well() {
    let dir = tempfile::tempdir().unwrap();
    let mut t = open(dir.path());
    let close: Vec<Node> = (0..6)
        .map(|i| {
            leaf(
                &mut t,
                &format!("日本行 订票 第{i}项 机票 酒店"),
                "2024-05-01",
            )
        })
        .collect();
    let far = leaf(&mut t, "完全无关的菜谱：番茄炒蛋", "2024-05-02");
    let mut adopt: Vec<String> = close.iter().map(|n| n.id.clone()).collect();
    adopt.push(far.id.clone());
    let sum = t
        .write(
            Draft {
                content: "日本行订票的结论".into(),
                title: "日本行订票".into(),
                adopt,
                ..Default::default()
            },
            "t",
        )
        .unwrap();
    let shown = |expand: usize| {
        t.search(&SearchOpts {
            query: "日本行 订票 机票 酒店".into(),
            expand,
            ..Default::default()
        })
        .unwrap()
        .into_iter()
        .find(|h| h.id == sum.id)
        .unwrap()
        .expanded
        .len()
    };
    assert_eq!(shown(8), 6, "the six that match, not the recipe");
    assert_eq!(shown(4), 4, "never more than asked for");
    assert_eq!(shown(0), 0);
}

#[test]
fn numbers_versions_addresses_and_dates_are_searchable_as_written() {
    let words = terms(
        "Hetzner 月费 €4.59，tauri 2.4.0，NAS 192.168.31.20 约 14.5TB，2026-11-23 12:30 收款 27,000 元",
    );
    for want in [
        "4.59",
        "2.4.0",
        "192.168.31.20",
        "14.5tb",
        "14.5",
        "2026-11-23",
        "12:30",
        "27,000",
    ] {
        assert!(words.iter().any(|w| w == want), "{want} in {words:?}");
    }
    // a full stop at the end of a sentence is not part of the number
    let end = terms("共 12 台。服务 v2 在用。");
    assert!(!end.iter().any(|w| w.contains('.')), "{end:?}");
    let dir = tempfile::tempdir().unwrap();
    let mut t = open(dir.path());
    let old = leaf(
        &mut t,
        "NAS 固定 IP 是 192.168.31.20，路由器 192.168.31.1",
        "2025-01-01",
    );
    leaf(
        &mut t,
        "NAS 固定 IP 是 192.168.31.21，路由器 192.168.31.1",
        "2025-01-02",
    );
    leaf(&mut t, "周末去爬山带了两瓶水", "2025-01-03");
    assert_eq!(find(&t, "192.168.31.20")[0].id, old.id);
}
