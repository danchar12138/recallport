//! Behavior tests against real LanceDB tables in temporary directories (never the user's library).
use crate::{
    embedding::{self, Embed},
    jobs::Job,
    memory::*,
    store::Store,
};
use anyhow::Result;
use std::{
    io::Write,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

struct Fixed;
impl Embed for Fixed {
    fn encode(&self, input: &str) -> Result<Vec<f32>> {
        let mut v = vec![0.0; embedding::DIMENSIONS as usize];
        v[input.chars().count() % 7] = 1.0;
        Ok(v)
    }
}

/// Counts how many passages the "model" had to encode.
#[derive(Default)]
struct Counting(AtomicUsize);
impl Embed for Counting {
    fn encode(&self, input: &str) -> Result<Vec<f32>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Fixed.encode(input)
    }
}

async fn open(dir: &Path) -> Result<Arc<Store>> {
    Ok(Arc::new(Store::open(dir, Some(Arc::new(Fixed))).await?))
}
fn say(text: &str) -> Draft {
    Draft {
        text: text.into(),
        ..Default::default()
    }
}
fn entity(name: &str) -> EntityInput {
    EntityInput {
        name: name.into(),
        ..Default::default()
    }
}
fn lexical(text: &str) -> Query {
    Query {
        text: text.into(),
        strategy: "lexical".into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn entities_are_never_merged_by_name() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let s = open(dir.path()).await?;
    let first = s
        .remember(
            Draft {
                entities: vec![entity("Redis")],
                ..say("缓存层使用 Redis")
            },
            "t",
            "T",
        )
        .await?;
    assert!(first.choices.is_empty());
    let id = first.entry.mentions[0].entity.clone().unwrap();

    // The same name again is not guessed: it stays unresolved and the candidates come back.
    let second = s
        .remember(
            Draft {
                entities: vec![entity("redis")],
                ..say("再次提到 redis")
            },
            "t",
            "T",
        )
        .await?;
    assert_eq!(second.choices.len(), 1);
    assert!(second.entry.mentions[0].entity.is_none());
    assert_eq!(second.choices[0].candidates[0].id, id);
    assert_eq!(s.unresolved().await?.len(), 1);

    // The AI settles it by id.
    let bound = s
        .bind_entity(
            &second.entry.id,
            EntityInput {
                name: "redis".into(),
                id: id.clone(),
                ..Default::default()
            },
            "T",
        )
        .await?;
    assert_eq!(bound.entity.unwrap().id, id);
    let read = s.read(&second.entry.id).await?;
    assert_eq!(read.mentions.len(), 1);
    assert_eq!(read.mentions[0].entity.as_deref(), Some(id.as_str()));
    assert!(s.unresolved().await?.is_empty());

    // A different thing with the same name is only created on purpose.
    let third = s
        .remember(
            Draft {
                entities: vec![EntityInput {
                    new: true,
                    hint: "另一套系统".into(),
                    ..entity("Redis")
                }],
                ..say("另一个 Redis")
            },
            "t",
            "T",
        )
        .await?;
    let other = third.entry.mentions[0].entity.clone().unwrap();
    assert_ne!(other, id);
    assert_eq!(s.entity_search("redis", 10).await?.len(), 2);

    // Merging is explicit and can be undone; mentions keep their own ids.
    s.entity_merge(&other, &id).await?;
    assert_eq!(s.entity_search("redis", 10).await?.len(), 1);
    assert_eq!(s.entity_detail(&id, 10).await?.entity.mentions, 3);
    s.entity_unmerge(&other).await?;
    assert_eq!(s.entity_search("redis", 10).await?.len(), 2);
    assert_eq!(s.entity_detail(&id, 10).await?.entity.mentions, 2);

    // Same entity found through filtering recall.
    let only = s
        .recall(&Query {
            entity: id.clone(),
            ..Default::default()
        })
        .await?;
    assert_eq!(only.memories.len(), 2);
    Ok(())
}

#[tokio::test]
async fn edits_keep_history_and_corrections_leave_a_trail() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let s = open(dir.path()).await?;
    let old = s.remember(say("方案 A 可行"), "t", "Codex").await?;
    s.edit(&old.entry.id, 1, say("方案 A 在特定条件下可行"), "Web")
        .await?;
    let detail = s.detail(&old.entry.id).await?;
    assert_eq!(detail.entry.revision, 2);
    assert_eq!(detail.entry.history.len(), 1);
    assert_eq!(detail.entry.history[0].text, "方案 A 可行");
    assert_eq!(detail.entry.history[0].replaced_by, "Web");
    assert!(
        s.edit(&old.entry.id, 1, say("过期的编辑"), "Web")
            .await
            .is_err()
    );

    let newer = s
        .remember(
            Draft {
                references: vec![Reference {
                    id: old.entry.id.clone(),
                    kind: RefKind::Corrects,
                    reason: "实测不可行".into(),
                }],
                ..say("方案 A 实测不可行")
            },
            "t",
            "Claude Code",
        )
        .await?;
    let detail = s.detail(&old.entry.id).await?;
    assert!(detail.trail.iter().any(|t| t.relation == "corrected_by"
        && t.id == newer.entry.id
        && t.source == "Claude Code"));
    // A search shows the correction in place of the memory it corrected, and says so ...
    let found = s.recall(&lexical("方案")).await?;
    assert!(!found.memories.iter().any(|h| h.entry.id == old.entry.id));
    let hit = found
        .memories
        .iter()
        .find(|h| h.entry.id == newer.entry.id)
        .unwrap();
    assert_eq!(hit.replaces, vec![old.entry.id.clone()]);
    assert!(
        hit.trail
            .iter()
            .any(|t| t.relation == "corrects" && t.id == old.entry.id)
    );
    // ... but the old one is never hidden: ask for the history and both are there, with their trails.
    let all = s
        .recall(&Query {
            history: true,
            ..lexical("方案")
        })
        .await?;
    let hit = all
        .memories
        .iter()
        .find(|h| h.entry.id == old.entry.id)
        .unwrap();
    assert!(!hit.trail.is_empty());
    assert!(all.memories.iter().any(|h| h.entry.id == newer.entry.id));

    // A summary keeps the originals.
    let summary = s
        .remember(
            Draft {
                references: vec![
                    Reference {
                        id: old.entry.id.clone(),
                        kind: RefKind::Summarizes,
                        reason: String::new(),
                    },
                    Reference {
                        id: newer.entry.id.clone(),
                        kind: RefKind::Summarizes,
                        reason: String::new(),
                    },
                ],
                ..say("方案 A 的来龙去脉")
            },
            "t",
            "Codex",
        )
        .await?;
    assert_eq!(s.detail(&summary.entry.id).await?.trail.len(), 2);
    assert!(s.read(&old.entry.id).await.is_ok());
    Ok(())
}

#[tokio::test]
async fn retried_writes_are_not_duplicated() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let s = open(dir.path()).await?;
    let draft = Draft {
        request_id: "r1".into(),
        entities: vec![entity("服务器")],
        ..say("写入重试")
    };
    let a = s.remember(draft.clone(), "t", "T").await?;
    let b = s.remember(draft, "t", "T").await?;
    assert_eq!(a.entry.id, b.entry.id);
    assert_eq!(s.all().await?.len(), 1);
    assert_eq!(s.entity_search("", 10).await?.len(), 1);
    let different = Draft {
        request_id: "r1".into(),
        ..say("同一个请求 ID 却是不同的内容")
    };
    assert!(s.remember(different, "t", "T").await.is_err());
    Ok(())
}

#[tokio::test]
async fn attachments_are_stored_referenced_and_deduplicated() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let s = open(&dir.path().join("lib")).await?;
    let file = dir.path().join("note.txt");
    std::fs::write(&file, "hello 附件")?;
    let path = file.to_string_lossy().into_owned();
    let saved = s
        .remember(
            Draft {
                attachments: vec![AttachInput {
                    path: path.clone(),
                    caption: "一份蓝色笔记".into(),
                    ..Default::default()
                }],
                ..say("带附件的记忆")
            },
            "t",
            "T",
        )
        .await?;
    let attached = &saved.entry.attachments[0];
    let stored = s.attachment(&attached.hash).await.unwrap();
    assert_eq!(stored.storage, Storage::Stored);
    assert_eq!(stored.mime, "text/plain");
    assert!(s.blob(&attached.hash).is_file());
    // Captions take part in search.
    assert!(
        s.recall(&lexical("蓝色笔记"))
            .await?
            .memories
            .iter()
            .any(|h| h.entry.id == saved.entry.id)
    );

    // Same content from elsewhere: one blob, one record.
    let copy = dir.path().join("copy.txt");
    std::fs::copy(&file, &copy)?;
    let again = s
        .link_attachment(
            &saved.entry.id,
            AttachInput {
                path: copy.to_string_lossy().into_owned(),
                ..Default::default()
            },
            "T",
        )
        .await?;
    assert_eq!(again.hash, attached.hash);

    // Reference mode copies nothing and says so.
    let doc = dir.path().join("ref.md");
    std::fs::write(&doc, "# 只记录位置")?;
    let reference = s
        .link_attachment(
            &saved.entry.id,
            AttachInput {
                path: doc.to_string_lossy().into_owned(),
                mode: AttachMode::Reference,
                ..Default::default()
            },
            "T",
        )
        .await?;
    let record = s.attachment(&reference.hash).await.unwrap();
    assert_eq!(record.storage, Storage::Referenced);
    assert!(!s.blob(&reference.hash).exists());
    let views = s
        .attachment_views(&s.read(&saved.entry.id).await?.attachments)
        .await;
    assert_eq!(views.len(), 2);
    assert!(views.iter().all(|v| v.available));

    // Small inline data works; network and relative paths do not.
    let inline = s
        .link_attachment(
            &saved.entry.id,
            AttachInput {
                data_base64: "aGk=".into(),
                name: "hi.txt".into(),
                ..Default::default()
            },
            "T",
        )
        .await?;
    assert_eq!(s.attachment(&inline.hash).await.unwrap().size, 2);
    for bad in [
        r"\\server\share\x.txt",
        "relative/file.txt",
        "C:\\nope\\missing.bin",
    ] {
        let outcome = s
            .link_attachment(
                &saved.entry.id,
                AttachInput {
                    path: bad.into(),
                    ..Default::default()
                },
                "T",
            )
            .await;
        assert!(outcome.is_err(), "{bad} must be refused");
    }

    // Unlinking leaves the file until it is cleaned up.
    s.unlink_attachment(&saved.entry.id, &inline.hash).await?;
    assert!(s.blob(&inline.hash).is_file());
    let (files, _) = s.gc_files().await?;
    assert_eq!(files, 1);
    assert!(!s.blob(&inline.hash).exists());
    assert!(s.blob(&attached.hash).is_file());
    Ok(())
}

async fn library(dir: &Path, tag: &str) -> Result<Arc<Store>> {
    let s = open(dir).await?;
    let file = dir.join("keep.bin");
    std::fs::write(&file, format!("附件内容 {tag}"))?;
    let old = s
        .remember(
            Draft {
                occurred: "2026-01-02".into(),
                entities: vec![EntityInput {
                    hint: "生产".into(),
                    ..entity("主服务器")
                }],
                attachments: vec![AttachInput {
                    path: file.to_string_lossy().into_owned(),
                    caption: format!("{tag} 的附件"),
                    ..Default::default()
                }],
                ..say(&format!("{tag}：服务器迁移到新机房"))
            },
            "t",
            "Codex",
        )
        .await?;
    s.remember(
        Draft {
            references: vec![Reference {
                id: old.entry.id,
                kind: RefKind::Corrects,
                reason: "迁移取消".into(),
            }],
            ..say(&format!("{tag}：迁移被取消，仍在旧机房"))
        },
        "t",
        "Claude Code",
    )
    .await?;
    Ok(s)
}

#[tokio::test]
async fn archive_round_trip_into_an_empty_library() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let source = library(&dir.path().join("a"), "甲").await?;
    let zip = dir.path().join("backup.zip");
    let info = source.export_to(zip.clone(), Job::detached()).await?;
    assert_eq!((info.memories, info.entities, info.attachments), (2, 1, 1));
    assert!(info.warnings.is_empty());

    let target = open(&dir.path().join("b")).await?;
    target.restore_zip(zip, Job::detached()).await?;
    let (a, b) = (source.snapshot().await?, target.snapshot().await?);
    assert_eq!(serde_json::to_value(&a)?, serde_json::to_value(&b)?);
    let hash = &a.attachments[0].hash;
    assert!(target.blob(hash).is_file());
    assert_eq!(
        std::fs::read(target.blob(hash))?,
        std::fs::read(source.blob(hash))?
    );
    // Search, entities and links work on the restored copy.
    // The newer memory corrected the older one, so a search leads with the correction.
    assert_eq!(target.recall(&lexical("机房")).await?.memories.len(), 1);
    assert_eq!(
        target
            .recall(&Query {
                history: true,
                ..lexical("机房")
            })
            .await?
            .memories
            .len(),
        2
    );
    assert_eq!(target.entity_search("主服务器", 5).await?[0].mentions, 1);
    let newest = &b
        .memories
        .iter()
        .find(|r| r.entry.text.contains("取消"))
        .unwrap()
        .entry;
    assert_eq!(target.detail(&newest.id).await?.links_out.len(), 1);
    Ok(())
}

fn craft(path: &Path, entries: &[(&str, &[u8])]) -> Result<()> {
    let mut zip = zip::ZipWriter::new(std::fs::File::create(path)?);
    for (name, bytes) in entries {
        zip.start_file(*name, zip::write::SimpleFileOptions::default())?;
        zip.write_all(bytes)?;
    }
    zip.finish()?;
    Ok(())
}

#[tokio::test]
async fn hostile_or_broken_archives_are_refused_without_touching_data() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let s = library(&dir.path().join("lib"), "甲").await?;
    let before = serde_json::to_value(s.snapshot().await?)?;
    let manifest = |extra: &str| {
        format!(
            r#"{{"format":"recallport-archive","version":1,"app":"t","created":"2026-01-01T00:00:00Z","memories":[],"entities":[],"attachments":[{extra}]}}"#
        )
    };
    let h = "ab".repeat(32);
    let stored = format!(
        r#"{{"hash":"{h}","name":"x.bin","mime":"application/octet-stream","size":3,"storage":"stored","added":"2026-01-01T00:00:00Z","source":"t"}}"#
    );
    type Case = (&'static str, Vec<(String, Vec<u8>)>);
    let cases: Vec<Case> = vec![
        (
            "path traversal",
            vec![
                ("manifest.json".into(), manifest("").into_bytes()),
                ("../evil.txt".into(), b"x".to_vec()),
            ],
        ),
        (
            "unknown entry",
            vec![
                ("manifest.json".into(), manifest("").into_bytes()),
                ("notes.txt".into(), b"x".to_vec()),
            ],
        ),
        ("no manifest", vec![("files/readme".into(), b"x".to_vec())]),
        (
            "not a backup",
            vec![("manifest.json".into(), b"{\"hello\":1}".to_vec())],
        ),
        (
            "old json format",
            vec![(
                "manifest.json".into(),
                br#"{"format":"recallport","version":1,"memories":[]}"#.to_vec(),
            )],
        ),
        (
            "missing file",
            vec![("manifest.json".into(), manifest(&stored).into_bytes())],
        ),
        (
            "wrong size",
            vec![
                ("manifest.json".into(), manifest(&stored).into_bytes()),
                (format!("files/{h}"), b"toolong".to_vec()),
            ],
        ),
        (
            "wrong hash",
            vec![
                ("manifest.json".into(), manifest(&stored).into_bytes()),
                (format!("files/{h}"), b"abc".to_vec()),
            ],
        ),
    ];
    for (name, entries) in cases {
        let zip = dir.path().join("bad.zip");
        let borrowed: Vec<(&str, &[u8])> = entries
            .iter()
            .map(|(n, b)| (n.as_str(), b.as_slice()))
            .collect();
        craft(&zip, &borrowed)?;
        let outcome = s.restore_zip(zip.clone(), Job::detached()).await;
        assert!(outcome.is_err(), "{name} must be refused");
        assert!(
            s.merge_zip(zip, Job::detached()).await.is_err(),
            "{name} must be refused when merging"
        );
        assert_eq!(
            serde_json::to_value(s.snapshot().await?)?,
            before,
            "{name} changed live data"
        );
    }
    let garbage = dir.path().join("garbage.zip");
    std::fs::write(&garbage, b"this is not a zip")?;
    assert!(s.restore_zip(garbage, Job::detached()).await.is_err());
    Ok(())
}

#[tokio::test]
async fn merge_unions_libraries_and_never_merges_same_named_entities() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mine = library(&dir.path().join("a"), "甲").await?;
    let theirs = library(&dir.path().join("b"), "乙").await?;
    let zip = dir.path().join("theirs.zip");
    theirs.export_to(zip.clone(), Job::detached()).await?;

    let report = mine.merge_zip(zip.clone(), Job::detached()).await?;
    assert_eq!(report.added, 2);
    assert_eq!(report.entities_added, 1);
    assert_eq!(report.attachments_added, 1);
    assert_eq!(report.same_name_entities, vec!["主服务器".to_string()]);
    assert_eq!(mine.all().await?.len(), 4);
    assert_eq!(mine.entity_search("主服务器", 10).await?.len(), 2);

    // Merging the same archive again changes nothing.
    let again = mine.merge_zip(zip, Job::detached()).await?;
    assert_eq!((again.added, again.merged, again.unchanged), (0, 0, 2));
    assert_eq!(mine.all().await?.len(), 4);

    // And the rollback snapshot returns the library to before the merge.
    mine.rollback(Job::detached()).await?;
    assert_eq!(mine.all().await?.len(), 4);
    Ok(())
}

#[tokio::test]
async fn restore_can_be_rolled_back_and_a_crashed_restore_heals_itself() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let a = library(&dir.path().join("a"), "甲").await?;
    let b = library(&dir.path().join("b"), "乙").await?;
    let zip = dir.path().join("b.zip");
    b.export_to(zip.clone(), Job::detached()).await?;
    let original = serde_json::to_value(a.snapshot().await?)?;

    a.restore_zip(zip, Job::detached()).await?;
    assert_ne!(serde_json::to_value(a.snapshot().await?)?, original);
    assert!(a.recovery_info().is_some());
    a.rollback(Job::detached()).await?;
    assert_eq!(serde_json::to_value(a.snapshot().await?)?, original);

    // Simulate a crash between "journal written" and "journal removed".
    let live = serde_json::to_value(a.snapshot().await?)?;
    std::fs::write(dir.path().join("a").join("restore.journal"), b"switching")?;
    assert!(a.recover_interrupted().await?.is_some());
    assert!(!dir.path().join("a").join("restore.journal").exists());
    assert_ne!(
        serde_json::to_value(a.snapshot().await?)?,
        serde_json::Value::Null
    );
    drop(live);
    Ok(())
}

#[tokio::test]
async fn repeating_the_same_new_entity_is_idempotent_but_a_different_hint_is_a_new_thing()
-> Result<()> {
    let dir = tempfile::tempdir()?;
    let s = open(dir.path()).await?;
    let named = |hint: &str| EntityInput {
        name: "Mercury".into(),
        hint: hint.into(),
        new: true,
        ..Default::default()
    };
    let write = |text: &'static str, hint: &str| {
        let draft = Draft {
            entities: vec![named(hint)],
            ..say(text)
        };
        let s = s.clone();
        async move { s.remember(draft, "t", "T").await }
    };
    let a = write("水星探测", "行星").await?;
    let b = write("水星凌日", "行星").await?; // same name + same hint: the same entity
    let c = write("汞中毒", "化学元素").await?; // same name, different hint: another thing
    let id = |s: &crate::store::Saved| s.entry.mentions[0].entity.clone().unwrap();
    assert_eq!(id(&a), id(&b));
    assert_ne!(id(&a), id(&c));
    Ok(())
}

#[tokio::test]
async fn compaction_runs_after_many_writes_and_keeps_every_memory() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let s = open(dir.path()).await?;
    assert!(!s.compact().await?, "nothing to do on a fresh library");
    for n in 0..70 {
        s.remember(say(&format!("第 {n} 条经历")), "t", "T").await?;
    }
    assert!(s.compact().await?);
    assert!(!s.compact().await?, "not again right away");
    assert_eq!(s.all().await?.len(), 70);
    assert!(!s.recall(&lexical("经历")).await?.memories.is_empty());
    Ok(())
}

#[tokio::test]
async fn recall_says_how_well_each_hit_matches_and_when_nothing_matches_well() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let s = open(dir.path()).await?;
    s.remember(say("缓存层使用 Redis 做会话存储"), "t", "T")
        .await?;
    s.remember(say("机房迁移计划已经取消"), "t", "T").await?;

    let literal = s.recall(&lexical("Redis")).await?;
    assert_eq!(literal.memories[0].match_kind, "exact");
    assert_eq!(literal.memories[0].relevance, Some(1.0));
    assert!(!literal.no_strong_match);

    // The words are there but not as typed: only a keyword match, which is not a strong one.
    let loose = s.recall(&lexical("迁移 机房")).await?;
    assert_eq!(loose.memories[0].match_kind, "keyword");
    assert!(loose.no_strong_match);

    let nothing = s.recall(&lexical("完全无关的词汇")).await?;
    assert!(nothing.memories.is_empty() && nothing.no_strong_match);
    Ok(())
}

#[tokio::test]
async fn several_phrasings_are_searched_in_one_call() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let s = open(dir.path()).await?;
    let a = s
        .remember(say("数据库连接池耗尽导致超时"), "t", "T")
        .await?;
    let b = s.remember(say("配置文件路径写错了"), "t", "T").await?;
    s.remember(say("与此无关的一条"), "t", "T").await?;
    let found = s
        .recall(&Query {
            queries: vec!["配置文件".into(), "  ".into(), "超时".into()],
            ..lexical("超时")
        })
        .await?;
    let ids: Vec<&str> = found.memories.iter().map(|h| h.entry.id.as_str()).collect();
    assert!(ids.contains(&a.entry.id.as_str()) && ids.contains(&b.entry.id.as_str()));
    assert_eq!(found.memories.len(), 2);
    let too_many = Query {
        queries: (0..6).map(|i| format!("说法{i}")).collect(),
        ..lexical("超时")
    };
    assert!(s.recall(&too_many).await.is_err());
    Ok(())
}

#[tokio::test]
async fn health_reports_what_needs_tidying_and_correction_loops_are_refused() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let s = open(dir.path()).await?;
    let clean = s.health().await?;
    assert!(clean.ok && clean.issues.is_empty());

    let a = s.remember(say("同样的一句话"), "t", "T").await?;
    s.remember(say("同样的一句话"), "t", "T").await?;
    let with = |text: &str, target: &str| Draft {
        references: vec![Reference {
            id: target.into(),
            kind: RefKind::Corrects,
            reason: String::new(),
        }],
        ..say(text)
    };
    let first = s.remember(with("更正之一", &a.entry.id), "t", "T").await?;
    s.remember(with("更正之二", &a.entry.id), "t", "T").await?;
    s.remember(
        Draft {
            entities: vec![entity("X")],
            ..say("提到了 X")
        },
        "t",
        "T",
    )
    .await?;
    s.remember(
        Draft {
            entities: vec![entity("x")],
            ..say("再提到 x")
        },
        "t",
        "T",
    )
    .await?;
    let health = s.health().await?;
    let kinds: Vec<&str> = health.issues.iter().map(|i| i.kind).collect();
    for expected in [
        "duplicate_memories",
        "without_entities",
        "correction_forks",
        "unresolved_names",
    ] {
        assert!(
            kinds.contains(&expected),
            "{expected} missing from {kinds:?}"
        );
    }
    assert!(!health.ok);

    // `first` already corrects `a`; making `a` correct `first` would close a loop.
    let loop_link = Reference {
        id: first.entry.id.clone(),
        kind: RefKind::Corrects,
        reason: String::new(),
    };
    assert!(s.link(&a.entry.id, loop_link, false).await.is_err());
    // A plain relation between the same two is fine.
    let relates = Reference {
        id: first.entry.id,
        ..Default::default()
    };
    s.link(&a.entry.id, relates, false).await?;
    Ok(())
}

#[tokio::test]
async fn backups_carry_vectors_so_restore_and_rollback_do_not_encode_again() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let source = library(&dir.path().join("a"), "甲").await?;
    let zip = dir.path().join("backup.zip");
    source.export_to(zip.clone(), Job::detached()).await?;
    let names: Vec<String> = zip::ZipArchive::new(std::fs::File::open(&zip)?)?
        .file_names()
        .filter_map(|n| n.ok().map(|n| n.into_owned()))
        .collect();
    assert!(names.iter().any(|n| n == "vectors.bin"));

    let counter = Arc::new(Counting::default());
    let target = Arc::new(Store::open(&dir.path().join("b"), Some(counter.clone())).await?);
    target.remember(say("目标库里原有的记忆"), "t", "T").await?;
    let before = counter.0.load(Ordering::SeqCst);
    assert!(before > 0);

    target.restore_zip(zip, Job::detached()).await?;
    assert_eq!(
        counter.0.load(Ordering::SeqCst),
        before,
        "restore reused the saved vectors"
    );
    target.rollback(Job::detached()).await?;
    assert_eq!(
        counter.0.load(Ordering::SeqCst),
        before,
        "rollback reused them too"
    );
    assert_eq!(target.all().await?.len(), 1);

    // The restored vectors really work for search: the same words as a memory find it by meaning.
    target
        .restore_zip(dir.path().join("backup.zip"), Job::detached())
        .await?;
    let found = target
        .recall(&Query {
            text: "甲：服务器迁移到新机房".into(),
            strategy: "semantic".into(),
            history: true,
            ..Default::default()
        })
        .await?;
    assert!(!found.memories.is_empty());
    Ok(())
}

#[tokio::test]
async fn automatic_backups_are_daily_only_when_changed_and_keep_the_newest() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let s = library(&dir.path().join("a"), "甲").await?;
    let first = s.auto_backup().await?.expect("the first backup is made");
    assert!(crate::autobackup::valid_name(&first));
    assert!(s.auto_backup().await?.is_none(), "not due again so soon");

    let state = s.backup_dir().join("state.json");
    let backdate = || -> Result<()> {
        let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&state)?)?;
        v["at"] = "2020-01-01T00:00:00Z".into();
        std::fs::write(&state, serde_json::to_vec(&v)?)?;
        Ok(())
    };
    backdate()?;
    assert!(
        s.auto_backup().await?.is_none(),
        "nothing changed since the last one"
    );

    for day in 1..=8 {
        std::fs::write(
            s.backup_dir()
                .join(format!("RecallPort-auto-2020010{day}-0300.zip")),
            b"old",
        )?;
    }
    s.remember(say("之后发生的新变化"), "t", "T").await?;
    let made = s.auto_backup().await?.expect("changed, so it is backed up");
    let info = s.auto_backup_info();
    assert_eq!(info.files.len(), crate::autobackup::KEEP);
    assert_eq!(info.files[0].name, made);
    assert!(!info.files.iter().any(|f| f.name.contains("20200101")));

    // It carries no attachment files (they stay in the data folder) ...
    let names: Vec<String> =
        zip::ZipArchive::new(std::fs::File::open(s.backup_path(&made).unwrap())?)?
            .file_names()
            .filter_map(|n| n.ok().map(|n| n.into_owned()))
            .collect();
    assert!(
        names.iter().any(|n| n == "manifest.json")
            && !names.iter().any(|n| n.starts_with("files/"))
    );
    // ... and is a real backup that restores, even on a computer that does not have them.
    let copy = open(&dir.path().join("b")).await?;
    copy.restore_zip(s.backup_path(&made).unwrap(), Job::detached())
        .await?;
    assert_eq!(copy.all().await?.len(), s.all().await?.len());

    // Switched off, nothing more is made.
    s.set_auto_backup(false)?;
    assert!(!s.auto_backup_info().enabled);
    backdate()?;
    s.remember(say("关闭之后的变化"), "t", "T").await?;
    assert!(s.auto_backup().await?.is_none());
    assert!(s.backup_path("../x.zip").is_none());
    Ok(())
}

#[tokio::test]
async fn entity_overview_groups_entities_that_appear_together() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let s = open(dir.path()).await?;
    let mut ids = std::collections::HashMap::new();
    for name in ["甲", "乙", "丙", "丁", "戊", "己"] {
        let saved = s
            .remember(
                Draft {
                    entities: vec![entity(name)],
                    ..say(&format!("关于{name}"))
                },
                "t",
                "T",
            )
            .await?;
        ids.insert(name, saved.entry.mentions[0].entity.clone().unwrap());
    }
    // 甲乙丙 appear together three times, 丁戊己 twice.
    for (names, times) in [(["甲", "乙", "丙"], 3), (["丁", "戊", "己"], 2)] {
        for i in 0..times {
            s.remember(
                Draft {
                    entities: names
                        .iter()
                        .map(|n| EntityInput {
                            id: ids[n].clone(),
                            ..entity(n)
                        })
                        .collect(),
                    ..say(&format!("{}一起出现 {i}", names.join("")))
                },
                "t",
                "T",
            )
            .await?;
        }
    }
    let graph = s.graph("", 0, true).await?;
    let group = |name: &str| {
        graph["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["text"] == name)
            .and_then(|n| n["group"].as_u64())
    };
    assert!(group("甲").is_some() && group("甲") == group("乙") && group("乙") == group("丙"));
    assert!(group("丁").is_some() && group("丁") == group("戊") && group("戊") == group("己"));
    assert_ne!(group("甲"), group("丁"));
    assert_eq!(graph["groups"].as_array().unwrap().len(), 2);
    assert!(graph["max_co"].as_u64().unwrap() >= 3);
    assert!(graph["suggested_min"].as_u64().unwrap() >= 1);
    Ok(())
}

/// Needs the real model: set RECALLPORT_MODEL to its folder. Checks that running passages as one padded
/// batch gives the same vectors as one by one, and prints how similar paraphrases and strangers look.
#[test]
fn granite_batches_match_single_runs() -> Result<()> {
    let Ok(dir) = std::env::var("RECALLPORT_MODEL") else {
        return Ok(());
    };
    let model = embedding::Granite::open(Path::new(&dir))?;
    let texts: Vec<String> = [
        "服务器迁移到了新的机房",
        "The cache layer uses Redis for session storage and it keeps timing out under load",
        "短",
        "我们把主机搬到了另一个数据中心",
        "Redis timeouts under heavy load in the session cache",
        "今天中午吃了面条",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let batch = model.encode_many(&texts)?;
    for (text, got) in texts.iter().zip(&batch) {
        let single = model.encode(text)?;
        let cosine: f32 = single.iter().zip(got).map(|(a, b)| a * b).sum();
        assert!(cosine > 0.9995, "{text}: batch vs single cosine {cosine}");
    }
    let cos =
        |a: usize, b: usize| -> f32 { batch[a].iter().zip(&batch[b]).map(|(x, y)| x * y).sum() };
    println!("paraphrase zh      {:.3}", cos(0, 3));
    println!("paraphrase en/zh   {:.3}", cos(1, 4));
    println!("cross-language     {:.3}", cos(0, 4));
    println!("unrelated          {:.3}", cos(0, 5));
    println!("unrelated 2        {:.3}", cos(1, 5));
    Ok(())
}

#[tokio::test]
async fn a_map_territory_comes_in_pages_and_search_hits_know_their_territory() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let s = open(dir.path()).await?;
    let first = s
        .remember(
            Draft {
                entities: vec![entity("大")],
                occurred: "2020-01-01".into(),
                ..say("第 0 条")
            },
            "t",
            "T",
        )
        .await?;
    let big = first.entry.mentions[0].entity.clone().unwrap();
    for i in 1..205 {
        s.remember(
            Draft {
                entities: vec![EntityInput {
                    id: big.clone(),
                    ..entity("大")
                }],
                occurred: format!("2021-{:02}-{:02}", 1 + i / 28 % 12, 1 + i % 28),
                ..say(&format!("第 {i} 条各不相同的经历"))
            },
            "t",
            "T",
        )
        .await?;
    }
    let page0 = s.graph(&big, 0, false).await?;
    assert_eq!(page0["nodes"].as_array().unwrap().len(), 200);
    assert_eq!(
        (page0["total"].as_u64(), page0["more"].as_bool()),
        (Some(205), Some(true))
    );
    let page1 = s.graph(&big, 1, false).await?;
    assert_eq!(page1["nodes"].as_array().unwrap().len(), 5);
    assert_eq!(
        (page1["from"].as_u64(), page1["more"].as_bool()),
        (Some(200), Some(false))
    );
    // Newest first, so the oldest memory is on the last page and never repeated.
    let ids = |g: &serde_json::Value| -> Vec<String> {
        g["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["id"].as_str().unwrap().to_owned())
            .collect()
    };
    assert!(ids(&page1).contains(&first.entry.id));
    assert!(ids(&page0).iter().all(|id| !ids(&page1).contains(id)));
    assert!(s.graph("nope", 0, false).await.is_err());
    // Search hits say which territory they are drawn in; memories without a drawn entity go to the shelf.
    let loose = s.remember(say("没有任何实体的一条"), "t", "T").await?;
    let lands = s
        .lands_of(&[loose.entry.id.clone(), first.entry.id.clone(), "x".into()])
        .await?;
    assert_eq!(lands[&loose.entry.id], vec![crate::store::LOOSE.to_owned()]);
    assert_eq!(lands[&first.entry.id], vec![big]);
    assert!(!lands.contains_key("x"));
    Ok(())
}

#[tokio::test]
async fn summaries_make_a_tree_and_a_page_brings_the_whole_tree() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let s = open(dir.path()).await?;
    let first = s
        .remember(
            Draft {
                entities: vec![entity("项目")],
                occurred: "2000-01-01".into(),
                ..say("旧细节 0")
            },
            "t",
            "T",
        )
        .await?;
    let project = first.entry.mentions[0].entity.clone().unwrap();
    let about = |text: &str, occurred: &str, refs: Vec<&str>| Draft {
        entities: vec![EntityInput {
            id: project.clone(),
            ..entity("项目")
        }],
        occurred: occurred.into(),
        references: refs
            .into_iter()
            .map(|id| Reference {
                id: id.into(),
                reason: "汇总".into(),
                kind: RefKind::Summarizes,
            })
            .collect(),
        ..say(text)
    };
    let mut old = vec![first.entry.id.clone()];
    for i in 1..4 {
        old.push(
            s.remember(
                about(&format!("旧细节 {i}"), "2000-02-01", vec![]),
                "t",
                "T",
            )
            .await?
            .entry
            .id,
        );
    }
    // Plenty of newer memories, so the old ones fall off the first page...
    for i in 0..205 {
        s.remember(
            about(
                &format!("后来的经历第 {i} 条各不相同"),
                "2024-01-01",
                vec![],
            ),
            "t",
            "T",
        )
        .await?;
    }
    // ...then a summary of the old ones, itself summarized by a top summary.
    let refs: Vec<&str> = old.iter().map(String::as_str).collect();
    let low = s
        .remember(about("这四条旧细节的汇总", "2024-06-01", refs), "t", "T")
        .await?
        .entry
        .id;
    let top = s
        .remember(about("再往上的总汇总", "2024-07-01", vec![&low]), "t", "T")
        .await?
        .entry
        .id;
    let page = s.graph(&project, 0, false).await?;
    let nodes = page["nodes"].as_array().unwrap();
    let find = |id: &str| {
        nodes
            .iter()
            .find(|n| n["id"] == id)
            .unwrap_or_else(|| panic!("{id} missing"))
    };
    // the page is the newest 200, and the four old details came along as extras with their parent
    assert_eq!(nodes.iter().filter(|n| n["extra"] == true).count(), 4);
    assert_eq!(nodes.len(), 204);
    assert_eq!(find(&old[0])["extra"], true);
    assert_eq!(find(&top)["level"], 2);
    assert_eq!(find(&top)["desc"], 5);
    assert_eq!(find(&low)["level"], 1);
    assert_eq!(find(&low)["summary"], top.as_str());
    assert_eq!(find(&low)["kids"].as_array().unwrap().len(), 4);
    assert_eq!(find(&old[2])["summary"], low.as_str());
    assert_eq!(find(&old[2])["level"], 0);
    // a memory summarized twice belongs to the newest summary
    let newest = s
        .remember(
            about("更新的汇总只汇总了一条", "2024-08-01", vec![&old[1]]),
            "t",
            "T",
        )
        .await?
        .entry
        .id;
    let lanes = s.lane_memories(1000).await?;
    let lane = |id: &str| {
        lanes
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(lane(&old[1])["summary"], newest.as_str());
    assert_eq!(lane(&old[0])["summary"], low.as_str());
    assert_eq!(lane(&low)["kids"].as_array().unwrap().len(), 3);
    Ok(())
}

#[test]
fn partition_puts_every_memory_in_a_group_that_follows_its_topic() {
    // Three topics with their own directions, six memories each and a little different from one another;
    // plus three loners that point somewhere else entirely.
    let mut vectors: Vec<Vec<f32>> = vec![];
    for topic in 0..3 {
        for k in 0..6 {
            let mut v = vec![0.0; 16];
            v[topic * 4] = 1.0;
            v[topic * 4 + 1 + k % 3] = 0.1 + 0.02 * k as f32;
            vectors.push(v);
        }
    }
    for loner in 0..3 {
        let mut v = vec![0.0; 16];
        v[12 + loner] = 1.0;
        vectors.push(v);
    }
    let groups = crate::clusters::partition(&vectors, 3, 8);
    let mut all: Vec<usize> = groups.iter().flatten().copied().collect();
    all.sort_unstable();
    assert_eq!(all, (0..21).collect::<Vec<_>>(), "{groups:?}");
    assert!(groups.iter().all(|g| g.len() >= 3), "{groups:?}");
    for topic in 0..3 {
        let home = groups.iter().find(|g| g.contains(&(topic * 6))).unwrap();
        assert!(
            (0..6).all(|k| home.contains(&(topic * 6 + k))),
            "{topic}: {groups:?}"
        );
    }
    // few enough to summarize in one go: one group; a single memory has nobody to be grouped with
    assert_eq!(
        crate::clusters::partition(&vectors[..5], 3, 8),
        vec![vec![0, 1, 2, 3, 4]]
    );
    assert!(crate::clusters::partition(&vectors[..1], 3, 8).is_empty());
}

#[tokio::test]
async fn corrections_keep_their_place_in_the_tree_and_old_summaries_are_flagged() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let s = open(dir.path()).await?;
    let cite = |kind: RefKind, ids: &[&str]| -> Vec<Reference> {
        ids.iter()
            .map(|id| Reference {
                id: (*id).into(),
                reason: "测试".into(),
                kind,
            })
            .collect()
    };
    let mut leaves = vec![];
    for i in 0..4 {
        leaves.push(
            s.remember(say(&format!("细节 {i}")), "t", "T")
                .await?
                .entry
                .id,
        );
    }
    let refs: Vec<&str> = leaves.iter().map(String::as_str).collect();
    let summary = s
        .remember(
            Draft {
                references: cite(RefKind::Summarizes, &refs),
                ..say("四条细节的汇总")
            },
            "t",
            "T",
        )
        .await?
        .entry
        .id;
    let health = s.health().await?;
    assert!(health.issues.iter().all(|i| i.kind != "stale_summaries"));
    // two details are corrected: each correction takes the old one's place under the summary
    let mut fixes = vec![];
    for (i, leaf) in leaves.iter().enumerate().take(2) {
        let fixed = s
            .remember(
                Draft {
                    references: cite(RefKind::Corrects, &[leaf]),
                    ..say(&format!("更正后的细节 {i}"))
                },
                "t",
                "T",
            )
            .await?;
        fixes.push(fixed.entry.id);
    }
    let lanes = s.lane_memories(1000).await?;
    let lane = |id: &str| {
        lanes
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(lane(&fixes[0])["summary"], summary.as_str());
    assert_eq!(lane(&fixes[1])["level"], 0);
    // the summary was written before half of what is under it: it is flagged, and the layers are counted
    // without the replaced memories
    let health = s.health().await?;
    let stale = health
        .issues
        .iter()
        .find(|i| i.kind == "stale_summaries")
        .unwrap();
    assert!(
        stale.examples[0].starts_with(&summary),
        "{:?}",
        stale.examples
    );
    assert_eq!(
        health
            .layers
            .iter()
            .map(|l| (l.layer, l.count, l.covered))
            .collect::<Vec<_>>(),
        vec![(0, 4, 4), (1, 1, 0)]
    );
    // a new summary that corrects it takes over all its children (and its place)
    let newer = s
        .remember(
            Draft {
                references: cite(RefKind::Corrects, &[&summary]),
                ..say("更新后的汇总")
            },
            "t",
            "T",
        )
        .await?
        .entry
        .id;
    let lanes = s.lane_memories(1000).await?;
    let lane = |id: &str| {
        lanes
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(lane(&leaves[2])["summary"], newer.as_str());
    assert_eq!(lane(&fixes[1])["summary"], newer.as_str());
    assert_eq!(lane(&newer)["level"], 1);
    assert!(
        s.health()
            .await?
            .issues
            .iter()
            .all(|i| i.kind != "stale_summaries")
    );
    Ok(())
}

#[tokio::test]
async fn health_hands_out_summary_work_one_layer_at_a_time() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let s = open(dir.path()).await?;
    let cite = |ids: &[String]| -> Vec<Reference> {
        ids.iter()
            .map(|id| Reference {
                id: id.clone(),
                reason: "汇总".into(),
                kind: RefKind::Summarizes,
            })
            .collect()
    };
    let mut leaves = vec![];
    for i in 0..30 {
        leaves.push(
            s.remember(say(&format!("经历 {i}")), "t", "T")
                .await?
                .entry
                .id,
        );
    }
    let gaps =
        |h: &crate::health::Health| h.issues.iter().find(|i| i.kind == "summary_gaps").cloned();
    // thirty loose memories: layer 0 needs summaries, in batches that together cover all of them
    let first = gaps(&s.health().await?).expect("work for layer 0");
    assert!(first.detail.contains("第 0 层"), "{}", first.detail);
    let batched: usize = first
        .examples
        .iter()
        .map(|e| e.split(' ').count() - 1)
        .sum();
    assert_eq!(batched, 30, "{:?}", first.examples);
    assert!(first.examples.iter().all(|e| e.starts_with("new ")));
    // three summaries of ten cover the layer; the next layer is already few enough to be the top
    let mut tops = vec![];
    for chunk in leaves.chunks(10) {
        tops.push(
            s.remember(
                Draft {
                    references: cite(chunk),
                    ..say("十条的汇总")
                },
                "t",
                "T",
            )
            .await?
            .entry
            .id,
        );
    }
    assert!(gaps(&s.health().await?).is_none());
    // a late memory resembles the existing summaries (identical vectors here): it is sent to one of them
    let late = s.remember(say("后来的经历"), "t", "T").await?.entry.id;
    let work = gaps(&s.health().await?).expect("the late memory has no summary");
    let line = &work.examples[0];
    assert!(
        line.starts_with("extend ") && line.ends_with(&late),
        "{line}"
    );
    assert!(tops.iter().any(|t| line.contains(t.as_str())));
    // ten summaries are too many at the top: they get a layer above them, which starts as one batch of ten
    let dir = tempfile::tempdir()?;
    let s = open(dir.path()).await?;
    let mut leaves = vec![];
    for i in 0..30 {
        leaves.push(
            s.remember(say(&format!("经历 {i}")), "t", "T")
                .await?
                .entry
                .id,
        );
    }
    let mut mids = vec![];
    for chunk in leaves.chunks(3) {
        mids.push(
            s.remember(
                Draft {
                    references: cite(chunk),
                    ..say("三条的汇总")
                },
                "t",
                "T",
            )
            .await?
            .entry
            .id,
        );
    }
    let work = gaps(&s.health().await?).expect("work for layer 1");
    assert!(work.detail.contains("第 1 层"), "{}", work.detail);
    assert_eq!(work.examples.len(), 1);
    assert!(work.examples[0].starts_with("new ") && work.examples[0].split(' ').count() == 11);
    assert!(mids.iter().all(|m| work.examples[0].contains(m.as_str())));
    Ok(())
}

#[tokio::test]
async fn results_say_which_fields_matched() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let s = open(dir.path()).await?;
    let saved = s
        .remember(
            Draft {
                text: "把备份改成每天凌晨三点".into(),
                context: "家庭服务器 Proxmox".into(),
                ..Default::default()
            },
            "t",
            "Codex",
        )
        .await?;
    let words = crate::store::query_words("Proxmox 备份");
    assert!(words.contains(&"proxmox".to_owned()) && words.contains(&"备份".to_owned()));
    assert_eq!(
        crate::store::found_in(&saved.entry, &words),
        vec!["正文", "情境"]
    );
    assert!(crate::store::found_in(&saved.entry, &crate::store::query_words("路由器")).is_empty());
    Ok(())
}
