//! Behavior tests against real LanceDB tables in temporary directories (never the user's library).
use crate::{
    embedding::{self, Embed},
    jobs::Job,
    memory::*,
    store::Store,
};
use anyhow::Result;
use std::{io::Write, path::Path, sync::Arc};

struct Fixed;
impl Embed for Fixed {
    fn encode(&self, input: &str) -> Result<Vec<f32>> {
        let mut v = vec![0.0; embedding::DIMENSIONS as usize];
        v[input.chars().count() % 7] = 1.0;
        Ok(v)
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
    // The old memory is not demoted or hidden: it is still found, with its trail attached.
    let found = s.recall(&lexical("方案")).await?;
    let hit = found
        .memories
        .iter()
        .find(|h| h.entry.id == old.entry.id)
        .unwrap();
    assert!(!hit.trail.is_empty());
    assert!(found.memories.iter().any(|h| h.entry.id == newer.entry.id));

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
    assert_eq!(target.recall(&lexical("机房")).await?.memories.len(), 2);
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
