//! Behavior tests of the service: what the MCP tools and the web pages stand on. Everything runs in temporary
//! folders with a small stand-in for the semantic model.
use crate::{
    data::Mode,
    embedding::{self, Embed},
    jobs::Job,
    service::{AttachInput, ListQuery, Order, Service, UpdateInput, WriteInput},
    tree::SearchOpts,
};
use anyhow::Result;
use std::{io::Write, path::Path, sync::Arc};

/// A bag-of-words "model": every word lights one direction, so texts that share words are close.
struct Words;
impl Embed for Words {
    fn encode(&self, input: &str) -> Result<Vec<f32>> {
        let mut v = vec![0.0f32; embedding::DIMENSIONS as usize];
        for w in input
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
        {
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

fn open(dir: &Path) -> Arc<Service> {
    Service::open(dir, Arc::new(Words)).unwrap()
}

fn say(text: &str) -> WriteInput {
    WriteInput {
        content: text.into(),
        ..Default::default()
    }
}

#[test]
fn nodes_are_written_read_changed_moved_and_deleted_through_the_service() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(dir.path());
    let a = s
        .write(say("备份每天凌晨三点跑\n细节在后面"), "codex")
        .unwrap();
    let b = s.write(say("备份保留最近七份"), "codex").unwrap();
    let sum = s
        .write(
            WriteInput {
                content: "备份策略的结论".into(),
                title: "汇总".into(),
                adopt: vec![a.id.clone(), b.id.clone()],
                tags: vec!["家庭服务器".into()],
                ..say("")
            },
            "codex",
        )
        .unwrap();
    assert_eq!(sum.layer, 1);
    assert_eq!(sum.descendants, 2);
    assert_eq!(sum.children.len(), 2);
    let a = s.get(&a.id).unwrap();
    assert_eq!(
        a.path.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
        vec![sum.id.as_str()]
    );
    assert_eq!(a.children.len(), 0);
    // the list shows the first line only
    assert_eq!(
        s.list(&ListQuery::default())
            .items
            .iter()
            .find(|n| n.id == a.id)
            .unwrap()
            .title,
        "备份每天凌晨三点跑"
    );
    // an update without a version uses the latest; an old version is refused
    let changed = s
        .update(
            &a.id,
            None,
            UpdateInput {
                content: Some("备份每天凌晨四点跑".into()),
                ..Default::default()
            },
            "web",
        )
        .unwrap();
    assert_eq!(changed.version, a.version + 1);
    let err = s
        .update(
            &a.id,
            Some(a.version),
            UpdateInput {
                tags: Some(vec!["x".into()]),
                ..Default::default()
            },
            "web",
        )
        .unwrap_err();
    assert!(err.to_string().contains("已被改动"), "{err}");
    assert_eq!(changed.revisions[0].actor, "web");
    assert_eq!(
        changed.revisions.last().unwrap().content,
        "备份每天凌晨三点跑\n细节在后面"
    );
    // references show in both directions
    s.update(
        &b.id,
        None,
        UpdateInput {
            refs: Some(vec![a.id.clone()]),
            ..Default::default()
        },
        "codex",
    )
    .unwrap();
    assert_eq!(s.get(&a.id).unwrap().referenced_by[0].id, b.id);
    // search finds the new text and brings the context
    let found = s
        .search(&SearchOpts {
            query: "凌晨四点".into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(found.hits[0].id, a.id);
    assert_eq!(found.hits[0].path[0].id, sum.id);
    assert!(!found.no_strong_match);
    assert!(
        s.search(&SearchOpts {
            query: "红烧肉的做法".into(),
            ..Default::default()
        })
        .unwrap()
        .no_strong_match
    );
    // filters of the list
    let tagged = s.list(&ListQuery {
        tag: Some("家庭服务器".into()),
        order: Order::Created,
        ..Default::default()
    });
    assert_eq!(tagged.total, 1);
    assert_eq!(
        s.list(&ListQuery {
            min_layer: Some(1),
            ..Default::default()
        })
        .total,
        1
    );
    // stats, outline, activity
    let st = s.stats();
    assert_eq!((st.nodes, st.roots), (3, 1));
    assert_eq!(
        st.layers
            .iter()
            .map(|l| (l.layer, l.count, l.covered))
            .collect::<Vec<_>>(),
        vec![(0, 2, 2), (1, 1, 0)]
    );
    let outline = s.outline(None, 3).unwrap();
    assert_eq!(
        (
            outline.len(),
            outline[0].size,
            outline[0].children.as_ref().unwrap().len()
        ),
        (1, 3, 2)
    );
    assert_eq!(s.activity(3).len(), 3);
    // delete: children move up, the node goes to the trash and can come back
    assert_eq!(s.delete(&sum.id, None, "并入别处", "codex").unwrap(), 2);
    assert_eq!(s.stats().roots, 2);
    assert_eq!(s.trash()[0].id, sum.id);
    assert!(s.get(&sum.id).unwrap().deleted);
    s.restore(&sum.id, "web").unwrap();
    assert!(!s.get(&sum.id).unwrap().deleted);
    // moving
    s.move_nodes(&[a.id.clone(), b.id.clone()], Some(&sum.id), "web")
        .unwrap();
    assert_eq!(s.get(&sum.id).unwrap().children.len(), 2);
    assert!(
        s.move_nodes(std::slice::from_ref(&sum.id), Some(&a.id), "web")
            .is_err(),
        "no loops"
    );
}

#[test]
fn attachments_are_copied_travel_in_backups_and_are_swept_with_the_node() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(dir.path());
    let file = dir.path().join("note.txt");
    std::fs::write(&file, "hello").unwrap();
    let n = s
        .write(
            WriteInput {
                attachments: vec![
                    AttachInput {
                        path: Some(file.to_string_lossy().into_owned()),
                        caption: "备忘".into(),
                        ..Default::default()
                    },
                    AttachInput {
                        data_base64: Some("aGk=".into()),
                        name: Some("hi.txt".into()),
                        ..Default::default()
                    },
                ],
                ..say("带附件的节点")
            },
            "codex",
        )
        .unwrap();
    assert_eq!(n.attachments.len(), 2);
    assert!(n.attachments.iter().all(|a| a.available));
    assert_eq!(s.stats().files.stored, 2);
    // a full backup carries the files; restoring it elsewhere brings them back
    let name = s.make_backup(&Job::detached()).unwrap();
    let backup = s.backup_path(&name).unwrap();
    let other = tempfile::tempdir().unwrap();
    let t = open(other.path());
    let report = t
        .import_archive(&backup, Mode::Replace, &Job::detached())
        .unwrap();
    assert_eq!((report.nodes, report.missing_files), (1, 0));
    let copy = t.get(&n.id).unwrap();
    assert!(copy.attachments.iter().all(|a| a.available));
    assert_eq!(
        std::fs::read(t.files.blob(&copy.attachments[0].hash)).unwrap(),
        b"hello"
    );
    // removing an attachment and deleting + purging the node sweeps the copies
    let first = n.attachments[0].hash.clone();
    s.update(
        &n.id,
        None,
        UpdateInput {
            remove_attachments: vec![first],
            ..Default::default()
        },
        "web",
    )
    .unwrap();
    s.delete(&n.id, None, "不要了", "web").unwrap();
    assert_eq!(s.purge(std::slice::from_ref(&n.id)).unwrap(), 1);
    assert!(s.get(&n.id).is_err());
    assert_eq!(
        std::fs::read_dir(dir.path().join("files")).unwrap().count(),
        0
    );
}

#[test]
fn a_backup_replaces_or_merges_and_a_copy_is_kept_before_replacing() {
    let one = tempfile::tempdir().unwrap();
    let two = tempfile::tempdir().unwrap();
    let a = open(one.path());
    let b = open(two.path());
    let shared = a.write(say("两边都有的节点"), "codex").unwrap();
    let mine = a.write(say("只在这边的节点"), "codex").unwrap();
    let backup = a
        .backup_path(&a.make_backup(&Job::detached()).unwrap())
        .unwrap();
    // merge into a library that only has one other node
    let other = b.write(say("另一个库里的节点"), "codex").unwrap();
    let report = b
        .import_archive(&backup, Mode::Merge, &Job::detached())
        .unwrap();
    assert_eq!(report.merge.as_ref().unwrap().added, 2);
    assert_eq!(b.stats().nodes, 3);
    // merging the same backup again changes nothing
    let again = b
        .import_archive(&backup, Mode::Merge, &Job::detached())
        .unwrap();
    assert_eq!(
        again.merge.as_ref().unwrap().added + again.merge.as_ref().unwrap().updated,
        0
    );
    // a node both libraries changed ends up as the newer one, with both histories
    a.update(
        &shared.id,
        None,
        UpdateInput {
            content: Some("在 A 里改过的内容".into()),
            ..Default::default()
        },
        "codex",
    )
    .unwrap();
    let newer = a
        .backup_path(&a.make_backup(&Job::detached()).unwrap())
        .unwrap();
    b.import_archive(&newer, Mode::Merge, &Job::detached())
        .unwrap();
    let merged = b.get(&shared.id).unwrap();
    assert_eq!(merged.content, "在 A 里改过的内容");
    assert!(merged.revisions.len() >= 2);
    // replacing makes the library exactly the backup and keeps what was there
    let report = b
        .import_archive(&backup, Mode::Replace, &Job::detached())
        .unwrap();
    assert_eq!(b.stats().nodes, 2);
    assert!(b.get(&other.id).is_err());
    let kept = report.kept_as.unwrap();
    assert!(
        b.backups()
            .iter()
            .any(|f| f.name == kept && f.kind == "before-restore")
    );
    // going back is restoring that copy
    b.import_archive(
        &b.backup_path(&kept).unwrap(),
        Mode::Replace,
        &Job::detached(),
    )
    .unwrap();
    assert!(b.get(&other.id).is_ok() && b.get(&mine.id).is_ok());
    // searches work on the restored library without computing vectors again
    assert_eq!(
        b.search(&SearchOpts {
            query: "另一个库里的节点".into(),
            ..Default::default()
        })
        .unwrap()
        .hits[0]
            .id,
        other.id
    );
}

#[test]
fn a_backup_of_the_old_series_comes_along_with_corrections_summaries_and_tags() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b, s1) = ("a".repeat(32), "b".repeat(32), "c".repeat(32));
    let manifest = serde_json::json!({
        "format": "recallport-archive", "version": 1, "app": "0.6.0", "created": "2026-01-01T00:00:00Z", "files": true,
        "memories": [
            {"id": a, "text": "NAS 可用 12TB", "source": "codex", "recorded": "2024-01-01T00:00:00Z", "occurred": "2024-01-01",
             "mentions": [{"name": "NAS", "entity": "e1"}]},
            {"id": b, "text": "NAS 实际可用 11.6TB", "source": "codex", "recorded": "2024-02-01T00:00:00Z",
             "references": [{"id": a, "kind": "corrects", "reason": "容量单位"}], "mentions": [{"name": "NAS", "entity": "e1"}]},
            {"id": s1, "text": "存储的结论", "source": "codex", "recorded": "2024-03-01T00:00:00Z",
             "references": [{"id": a, "kind": "summarizes"}]}
        ],
        "entities": [{"id": "e1", "name": "家里的 NAS"}],
        "attachments": []
    });
    let zip_path = dir.path().join("old.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
    zip.start_file("manifest.json", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(manifest.to_string().as_bytes()).unwrap();
    zip.finish().unwrap();
    let lib = tempfile::tempdir().unwrap();
    let s = open(lib.path());
    let report = s
        .import_archive(&zip_path, Mode::Replace, &Job::detached())
        .unwrap();
    assert!(report.legacy);
    assert_eq!(s.stats().nodes, 2);
    let node = s.get(&b).unwrap();
    assert_eq!(node.content, "NAS 实际可用 11.6TB");
    assert_eq!(node.tags, vec!["家里的 NAS".to_owned()]);
    assert_eq!(node.path[0].id, s1);
    assert_eq!(node.revisions.last().unwrap().content, "NAS 可用 12TB");
    assert_eq!(s.get(&s1).unwrap().layer, 1);
    // not a backup at all
    let junk = lib.path().join("junk.zip");
    std::fs::write(&junk, b"nope").unwrap();
    assert!(
        s.import_archive(&junk, Mode::Merge, &Job::detached())
            .is_err()
    );
}

#[test]
fn the_daily_backup_runs_once_and_only_when_something_changed() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(dir.path());
    assert_eq!(s.auto_backup().unwrap(), None, "nothing to back up yet");
    s.write(say("第一条"), "codex").unwrap();
    let name = s.auto_backup().unwrap().expect("a backup");
    assert!(crate::data::valid_name(&name) && name.starts_with("RecallPort-auto-"));
    assert_eq!(s.auto_backup().unwrap(), None, "not again so soon");
    s.set_auto_backup(false).unwrap();
    assert!(!s.auto_backup_enabled());
    assert_eq!(s.backups().len(), 1);
    // only names of this shape are served
    assert!(s.backup_path(&name).is_some());
    assert!(
        s.backup_path("../nodes.jsonl").is_none() && s.backup_path("RecallPort-x-1.zip").is_none()
    );
}

#[test]
fn a_second_process_cannot_open_the_same_library() {
    let dir = tempfile::tempdir().unwrap();
    let _first = open(dir.path());
    let err = Service::open(dir.path(), Arc::new(Words))
        .err()
        .expect("locked");
    assert!(
        err.to_string().contains("已被另一个 RecallPort 进程打开"),
        "{err}"
    );
}

#[test]
fn the_library_survives_a_restart_and_a_half_written_last_line() {
    let dir = tempfile::tempdir().unwrap();
    let id = {
        let s = open(dir.path());
        s.write(say("重启之后还在"), "codex").unwrap().id
    };
    // a crash in the middle of writing the next line
    let mut log = std::fs::OpenOptions::new()
        .append(true)
        .open(dir.path().join("nodes.jsonl"))
        .unwrap();
    log.write_all(b"{\"at\":\"2026").unwrap();
    drop(log);
    let s = open(dir.path());
    assert_eq!(s.get(&id).unwrap().content, "重启之后还在");
    s.write(say("之后还能继续写"), "codex").unwrap();
    assert_eq!(open_again(dir.path(), s), 2);
}

fn open_again(dir: &Path, first: Arc<Service>) -> usize {
    drop(first);
    open(dir).stats().nodes
}

#[test]
fn the_outline_is_cut_at_the_depth_asked_for_and_the_rest_is_fetched_by_root() {
    let dir = tempfile::tempdir().unwrap();
    let s = Service::open(dir.path(), Arc::new(Words)).unwrap();
    let leaf = |text: &str, day: &str| {
        s.write(
            WriteInput {
                content: text.into(),
                occurred: day.into(),
                ..Default::default()
            },
            "t",
        )
        .unwrap()
    };
    let (a, b, c) = (
        leaf("甲 记忆", "2025-01-03"),
        leaf("乙 记忆", "2025-03-09"),
        leaf("丙 记忆", "2024-12-01"),
    );
    let mid = s
        .write(
            WriteInput {
                title: "中层".into(),
                content: "中层结论".into(),
                adopt: vec![a.id.clone(), b.id.clone()],
                ..Default::default()
            },
            "t",
        )
        .unwrap();
    let top = s
        .write(
            WriteInput {
                title: "顶层".into(),
                content: "顶层结论".into(),
                adopt: vec![mid.id.clone(), c.id.clone()],
                ..Default::default()
            },
            "t",
        )
        .unwrap();
    // two levels: the top and what is directly under it; the middle one says it has more but does not list it
    let cut = s.outline(None, 1).unwrap();
    assert_eq!(cut.len(), 1);
    assert_eq!(
        (cut[0].title.as_str(), cut[0].size, cut[0].layer),
        ("顶层", 5, 2)
    );
    assert_eq!(
        (cut[0].from.as_str(), cut[0].to.as_str()),
        ("2024-12-01", "2025-03-09")
    );
    let kids = cut[0].children.as_ref().unwrap();
    let mid_cut = kids.iter().find(|k| k.id == mid.id).unwrap();
    assert!(mid_cut.children.is_none() && mid_cut.size == 3);
    // a leaf is complete: it has no children to fetch
    assert_eq!(
        kids.iter()
            .find(|k| k.id == c.id)
            .unwrap()
            .children
            .as_ref()
            .map(Vec::len),
        Some(0)
    );
    // fetching the middle node by root returns its own children
    let more = s.outline(Some(&mid.id), 1).unwrap();
    assert_eq!(more[0].children.as_ref().unwrap().len(), 2);
    assert_eq!(more[0].children.as_ref().unwrap()[0].title, "甲 记忆");
    assert!(s.outline(Some("nope"), 1).is_err());
    assert_eq!(s.outline(None, 9).unwrap()[0].id, top.id);
}
