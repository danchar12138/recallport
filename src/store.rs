//! One authoritative Lance table; a memory, its outgoing links and its write key commit together.
use crate::{
    embedding::{self, Embed},
    entities::{EntityChoice, Index as EntityIndex},
    kv,
    memory::*,
};
use anyhow::{Context, Result, ensure};
use arrow_array::{
    Array, BooleanArray, Float32Array, RecordBatch, RecordBatchIterator, StringArray,
    builder::{FixedSizeListBuilder, Float32Builder, ListBuilder},
};
use arrow_schema::{DataType, Field, Schema};
use chrono::Utc;
use fs4::fs_std::FileExt;
use futures::TryStreamExt;
use lance_index::scalar::FullTextSearchQuery;
use lancedb::{
    DistanceType, Table,
    index::{Index, scalar::FtsIndexBuilder, vector::IvfHnswFlatIndexBuilder},
    query::{ExecutableQuery, QueryBase, Select},
    table::AddDataMode,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::{Mutex, RwLock};

pub const CANDIDATES: usize = 120;
pub const SEMANTIC: f32 = 0.80;
pub const GRAPH: f32 = 0.86;
pub const ANN_MIN: usize = 256;
pub const ANN_PROBES: usize = 20;
pub const MAX_PASSAGES: usize = 9;
pub const MIN_PASSAGE: usize = 7;
pub const RRF: f64 = 45.0;
pub const LEXICAL_WEIGHT: f64 = 1.0;
pub const SEMANTIC_WEIGHT: f64 = 0.9;
const TABLE: &str = "memories_v1";
const FTS: &str = "search_text_idx";
const ANN: &str = "vector_idx";
const EMBEDDING_VERSION: &str =
    "granite-embedding-97m-multilingual-r2/835ad140/fp32/cls-v1/passages-v1";

#[derive(Debug, Default, Clone, Serialize)]
pub struct Lane {
    pub state: &'static str,
    pub checked_at: Option<String>,
    pub repaired_at: Option<String>,
    pub problem: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct IndexStatus {
    pub keyword: Lane,
    pub semantic: Lane,
    pub model: &'static str,
    pub total: usize,
    pub indexed: usize,
    pub skipped: usize,
    pub dimensions: i32,
    pub ann: bool,
    pub ann_indexed: usize,
    pub keyword_indexed: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LaneKind {
    All,
    Keyword,
    Semantic,
}

/// The result of a write: the saved memory, plus entity names that need the AI to choose.
pub struct Saved {
    pub entry: Entry,
    pub choices: Vec<EntityChoice>,
}

pub struct Store {
    pub(crate) table: Table,
    pub(crate) etable: Table,
    pub(crate) atable: Table,
    pub(crate) dir: PathBuf,
    embedder: Option<Arc<dyn Embed>>,
    pub status: RwLock<IndexStatus>,
    pub(crate) writes: Mutex<()>,
    maintenance: Arc<Mutex<()>>,
    pub(crate) idx: RwLock<EntityIndex>,
    snapshot: RwLock<Option<Arc<Vec<Entry>>>>,
    generation: AtomicU64,
    _lock: File,
}

pub(crate) struct Row {
    record: Record,
    vector: Option<Vec<Vec<f32>>>,
    deleted: bool,
}

fn vector_item() -> Field {
    Field::new(
        "item",
        DataType::FixedSizeList(
            Arc::new(Field::new("item", DataType::Float32, true)),
            embedding::DIMENSIONS,
        ),
        true,
    )
}

fn schema() -> Arc<Schema> {
    let mut fields = [
        "id",
        "payload",
        "start_day",
        "end_day",
        "recorded",
        "actor",
        "request_id",
        "origin",
        "occurred",
        "search_text",
        "body",
        "embedding_id",
    ]
    .into_iter()
    .map(|name| Field::new(name, DataType::Utf8, false))
    .collect::<Vec<_>>();
    fields.extend([
        Field::new("deleted", DataType::Boolean, false),
        Field::new("vector", DataType::List(Arc::new(vector_item())), true),
    ]);
    Arc::new(Schema::new(fields))
}

/// Text the keyword index sees: the memory plus the entity names and attachment captions around it.
fn searchable(e: &Entry) -> String {
    let mut text = format!("{}\n{}", e.text, e.context);
    for m in &e.mentions {
        text.push('\n');
        text.push_str(&m.name);
    }
    for a in &e.attachments {
        text.push('\n');
        text.push_str(&a.name);
        if !a.caption.is_empty() {
            text.push('\n');
            text.push_str(&a.caption);
        }
    }
    text
}

fn batch(rows: &[Row]) -> Result<RecordBatch> {
    let mut columns: [Vec<String>; 12] = Default::default();
    for row in rows {
        let e = &row.record.entry;
        let (start_day, end_day) = date_range(&e.occurred)?;
        let values = [
            e.id.clone(),
            serde_json::to_string(&row.record)?,
            start_day,
            end_day,
            e.recorded.clone(),
            row.record
                .request
                .as_ref()
                .map(|r| r.actor.clone())
                .unwrap_or_default(),
            row.record
                .request
                .as_ref()
                .map(|r| r.request_id.clone())
                .unwrap_or_default(),
            e.origin.clone(),
            e.occurred.clone(),
            if row.deleted {
                String::new()
            } else {
                terms(&searchable(e), usize::MAX)
            },
            if row.deleted {
                String::new()
            } else {
                searchable(e).to_lowercase()
            },
            if row.vector.is_some() {
                EMBEDDING_VERSION.into()
            } else {
                String::new()
            },
        ];
        for (column, value) in columns.iter_mut().zip(values) {
            column.push(value);
        }
    }
    let mut data: Vec<Arc<dyn Array>> = columns
        .into_iter()
        .map(|v| Arc::new(StringArray::from(v)) as Arc<dyn Array>)
        .collect();
    data.push(Arc::new(BooleanArray::from_iter(
        rows.iter().map(|r| Some(r.deleted)),
    )));
    let mut vectors = ListBuilder::new(FixedSizeListBuilder::new(
        Float32Builder::new(),
        embedding::DIMENSIONS,
    ))
    .with_field(vector_item());
    for row in rows {
        if let Some(group) = &row.vector {
            for v in group {
                vectors.values().values().append_slice(v);
                vectors.values().append(true);
            }
            vectors.append(true);
        } else {
            vectors.append(false);
        }
    }
    data.push(Arc::new(vectors.finish()));
    Ok(RecordBatch::try_new(schema(), data)?)
}

fn reader(rows: &[Row]) -> Result<impl arrow_array::RecordBatchReader + Send + 'static + use<>> {
    Ok(RecordBatchIterator::new(vec![Ok(batch(rows)?)], schema()))
}

pub(crate) fn col<'a, T: 'static>(b: &'a RecordBatch, name: &str) -> Result<&'a T> {
    b.column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref())
        .with_context(|| format!("Unexpected Lance column: {name}"))
}

pub(crate) fn sql(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn han(c: char) -> bool {
    matches!(c, '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}' | '\u{20000}'..='\u{323af}')
}

fn terms(text: &str, limit: usize) -> String {
    static JIEBA: OnceLock<jieba_rs::Jieba> = OnceLock::new();
    let lower = text.to_lowercase();
    let mut words: Vec<_> = JIEBA
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
    if limit != usize::MAX {
        let mut seen = HashSet::new();
        words.retain(|w| seen.insert(w.clone()));
    }
    words.truncate(limit);
    words.join(" ")
}

fn fingerprint(draft: &Draft) -> Result<String> {
    // Match the old on-disk SHA-256 format so imported retry IDs continue to work.
    let bytes = serde_json::to_string(draft)?
        .replace('&', "\\u0026")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");
    Ok(format!("{:x}", Sha256::digest(bytes.as_bytes())))
}

impl Store {
    pub async fn open(dir: &Path, embedder: Option<Arc<dyn Embed>>) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.join("recallport.lock"))?;
        ensure!(
            FileExt::try_lock_exclusive(&lock)?,
            "此记忆库已被另一个 RecallPort 进程打开"
        );
        let db_path = dir.join("recallport.lance");
        // Pass a native path, including any spaces or non-ASCII characters.
        let db = lancedb::connect(db_path.to_str().context("数据目录必须是有效 Unicode")?)
            .execute()
            .await?;
        let table = if db.table_names().execute().await?.iter().any(|n| n == TABLE) {
            let table = db.open_table(TABLE).execute().await?;
            ensure!(
                table.schema().await?.fields == schema().fields,
                "此数据目录的 LanceDB 表版本不受支持"
            );
            table
        } else {
            db.create_empty_table(TABLE, schema()).execute().await?
        };
        let etable = kv::open(&db, "entities_v1").await?;
        let atable = kv::open(&db, "attachments_v1").await?;
        let model = if embedder.is_some() {
            embedding::ID
        } else {
            ""
        };
        let store = Self {
            table,
            etable,
            atable,
            dir: dir.to_path_buf(),
            embedder,
            status: RwLock::new(IndexStatus {
                model,
                dimensions: embedding::DIMENSIONS,
                ..Default::default()
            }),
            writes: Mutex::new(()),
            maintenance: Arc::new(Mutex::new(())),
            idx: RwLock::new(EntityIndex::default()),
            snapshot: RwLock::new(None),
            generation: AtomicU64::new(0),
            _lock: lock,
        };
        store.load_index().await?;
        Ok(store)
    }

    pub fn uses_vectors(&self) -> bool {
        self.embedder.is_some()
    }

    async fn embedding(&self, s: String) -> Result<Option<Vec<f32>>> {
        let Some(model) = self.embedder.clone() else {
            return Ok(None);
        };
        let vector = tokio::task::spawn_blocking(move || model.encode(&s)).await??;
        ensure!(
            vector.len() == embedding::DIMENSIONS as usize && vector.iter().all(|x| x.is_finite()),
            "Invalid embedding"
        );
        Ok(Some(vector))
    }

    pub(crate) async fn row(&self, record: Record) -> Result<Row> {
        let text = if record.entry.context.is_empty() {
            record.entry.text.clone()
        } else {
            format!("{}\n{}", record.entry.text, record.entry.context)
        };
        let mut encoded = Vec::new();
        if let Some(vector) = self.embedding(text.clone()).await? {
            encoded.push(vector);
            let mut seen = HashSet::from([text.trim()]);
            let fragments = text
                .split(['\n', '。', '！', '？', '；', '，', '.', '!', '?', ';', ','])
                .map(str::trim)
                .filter(|s| s.chars().count() >= MIN_PASSAGE && seen.insert(*s))
                .take(MAX_PASSAGES - 1);
            for fragment in fragments {
                if let Some(vector) = self.embedding(fragment.into()).await? {
                    encoded.push(vector);
                }
            }
        }
        Ok(Row {
            vector: (!encoded.is_empty()).then_some(encoded),
            record,
            deleted: false,
        })
    }

    pub(crate) async fn records(&self, filter: &str) -> Result<Vec<Record>> {
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .only_if(filter)
            .select(Select::columns(&["payload"]))
            .execute()
            .await?
            .try_collect()
            .await?;
        let mut records = Vec::new();
        for b in batches {
            for item in col::<StringArray>(&b, "payload")?.iter().flatten() {
                records.push(serde_json::from_str(item)?);
            }
        }
        Ok(records)
    }

    pub(crate) async fn find(&self, id: &str) -> Result<Record> {
        ensure!(valid_id(id), "记忆 ID 无效");
        self.records(&format!("NOT deleted AND id = {}", sql(id)))
            .await?
            .pop()
            .context("未找到记忆")
    }

    async fn upsert(&self, rows: &[Row]) -> Result<()> {
        let mut op = self.table.merge_insert(&["id"]);
        op.when_matched_update_all(None)
            .when_not_matched_insert_all();
        op.execute(Box::new(reader(rows)?)).await?;
        Ok(())
    }

    pub(crate) async fn upsert_rows(&self, rows: &[Row]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        self.upsert(rows).await
    }

    /// Encode vectors for records one by one, reporting how many are done.
    pub(crate) async fn embed_records(
        &self,
        records: Vec<Record>,
        mut step: impl FnMut(usize),
    ) -> Result<Vec<Row>> {
        let mut rows = Vec::with_capacity(records.len());
        for (i, record) in records.into_iter().enumerate() {
            rows.push(self.row(record).await?);
            step(i + 1);
        }
        Ok(rows)
    }

    pub(crate) async fn overwrite_memories(&self, rows: &[Row]) -> Result<()> {
        self.table
            .add(batch(rows)?)
            .mode(AddDataMode::Overwrite)
            .execute()
            .await?;
        Ok(())
    }

    pub(crate) async fn invalidate(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        *self.snapshot.write().await = None;
    }

    /// Persist an edited record without touching its vector, and refresh the derived lookups.
    pub(crate) async fn rewrite_locked(&self, record: &Record) -> Result<()> {
        let text = searchable(&record.entry);
        self.table
            .update()
            .only_if(format!("NOT deleted AND id = {}", sql(&record.entry.id)))
            .column("payload", sql(&serde_json::to_string(record)?))
            .column("search_text", sql(&terms(&text, usize::MAX)))
            .column("body", sql(&text.to_lowercase()))
            .execute()
            .await?;
        self.idx.write().await.put_memory(&record.entry);
        self.invalidate().await;
        Ok(())
    }

    /// Outgoing references that still exist, and the memories that cite this one.
    async fn hydrate(&self, mut item: Entry) -> Result<Entry> {
        let idx = self.idx.read().await;
        item.references.retain(|r| idx.live.contains(&r.id));
        item.cited_by = idx
            .cites
            .get(&item.id)
            .map(|list| {
                list.iter()
                    .filter(|(source, _)| idx.live.contains(source))
                    .map(|(source, r)| Reference {
                        id: source.clone(),
                        reason: r.reason.clone(),
                        kind: r.kind,
                    })
                    .collect()
            })
            .unwrap_or_default();
        item.cited_by.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(item)
    }

    pub async fn read(&self, id: &str) -> Result<Entry> {
        self.hydrate(self.find(id).await?.entry).await
    }

    async fn replay(&self, actor: &str, request_id: &str, hash: &str) -> Result<Option<Entry>> {
        if request_id.is_empty() {
            return Ok(None);
        }
        let filter = format!(
            "NOT deleted AND actor = {} AND request_id = {}",
            sql(actor),
            sql(request_id)
        );
        match self.records(&filter).await?.pop() {
            Some(r) => {
                ensure!(
                    r.request.as_ref().is_some_and(|r| r.fingerprint == hash),
                    "此请求 ID 已用于不同的内容"
                );
                Ok(Some(self.hydrate(r.entry).await?))
            }
            None => Ok(None),
        }
    }

    pub async fn remember(&self, draft: Draft, actor: &str, source: &str) -> Result<Saved> {
        let draft = draft.clean()?;
        ensure!(source.len() <= 160, "来源名称过长");
        let hash = fingerprint(&draft)?;
        if let Some(entry) = self.replay(actor, &draft.request_id, &hash).await? {
            return Ok(Saved {
                entry,
                choices: vec![],
            });
        }
        // Files are content-addressed, so they can be copied in before the write lock is taken.
        let attachments = self.ingest(&draft.attachments, source).await?;
        let _guard = self.writes.lock().await;
        if let Some(entry) = self.replay(actor, &draft.request_id, &hash).await? {
            return Ok(Saved {
                entry,
                choices: vec![],
            });
        }
        for target in &draft.references {
            self.find(&target.id).await.context("引用的记忆不存在")?;
        }
        // Entities first, the memory row last: the row is the commit point, an orphan entity is harmless.
        let (mentions, choices) = self.resolve_entities(&draft.entities, source).await?;
        let stamp = now();
        let record = Record {
            entry: Entry {
                id: uuid::Uuid::new_v4().simple().to_string(),
                text: draft.text,
                context: draft.context,
                occurred: draft.occurred,
                origin: draft.origin,
                source: source.into(),
                recorded: stamp.clone(),
                updated: stamp,
                revision: 1,
                references: draft.references,
                mentions,
                attachments,
                ..Default::default()
            },
            request: (!draft.request_id.is_empty()).then(|| Request {
                actor: actor.into(),
                request_id: draft.request_id,
                fingerprint: hash,
            }),
        };
        let row = self.row(record).await?;
        self.table
            .add(batch(std::slice::from_ref(&row))?)
            .execute()
            .await?;
        self.idx.write().await.put_memory(&row.record.entry);
        self.invalidate().await;
        Ok(Saved {
            entry: self.hydrate(row.record.entry).await?,
            choices,
        })
    }

    /// Edit by a person. The previous text is kept in the memory's history, never overwritten silently.
    pub async fn edit(&self, id: &str, revision: u32, draft: Draft, by: &str) -> Result<()> {
        let draft = draft.clean()?;
        let _guard = self.writes.lock().await;
        let mut row = self.find(id).await?;
        ensure!(row.entry.revision == revision, "记忆已被修改，请刷新后重试");
        if row.entry.text == draft.text
            && row.entry.context == draft.context
            && row.entry.occurred == draft.occurred
        {
            return Ok(());
        }
        let stamp = now();
        row.entry.history.push(Revision {
            revision,
            text: std::mem::take(&mut row.entry.text),
            context: std::mem::take(&mut row.entry.context),
            occurred: std::mem::take(&mut row.entry.occurred),
            replaced_at: stamp.clone(),
            replaced_by: by.into(),
        });
        row.entry.text = draft.text;
        row.entry.context = draft.context;
        row.entry.occurred = draft.occurred;
        row.entry.updated = stamp;
        row.entry.revision = revision.checked_add(1).context("版本超出范围")?;
        let entry = row.entry.clone();
        self.upsert(&[self.row(row).await?]).await?;
        self.idx.write().await.put_memory(&entry);
        self.invalidate().await;
        Ok(())
    }

    pub async fn delete(&self, id: &str, revision: u32) -> Result<()> {
        let _guard = self.writes.lock().await;
        let mut row = self.find(id).await?;
        ensure!(row.entry.revision == revision, "记忆已被修改，请刷新后重试");
        // A tombstone hides incoming references atomically without multi-row referential writes.
        row.entry = Entry {
            id: row.entry.id,
            ..Default::default()
        };
        row.request = None;
        let id = row.entry.id.clone();
        self.upsert(&[Row {
            record: row,
            vector: None,
            deleted: true,
        }])
        .await?;
        self.idx.write().await.forget_memory(&id);
        self.invalidate().await;
        Ok(())
    }

    /// Connect a memory to another memory.
    pub async fn link(&self, id: &str, link: Reference, remove: bool) -> Result<()> {
        ensure!(id != link.id && link.reason.len() <= 800, "引用无效");
        let _guard = self.writes.lock().await;
        self.find(&link.id).await?;
        let mut record = self.find(id).await?;
        {
            let idx = self.idx.read().await;
            record.entry.references.retain(|r| idx.live.contains(&r.id));
        }
        record.entry.references.retain(|r| r.id != link.id);
        if !remove {
            ensure!(
                record.entry.references.len() < MAX_REFERENCES,
                "最多引用 64 条记忆"
            );
            record.entry.references.push(link);
        }
        record.entry.updated = now();
        self.rewrite_locked(&record).await
    }

    async fn text_search(&self, filter: &str, query: &str) -> Result<Vec<String>> {
        let query = terms(query, 36);
        if query.is_empty() {
            return Ok(vec![]);
        }
        let q = FullTextSearchQuery::new(query).with_columns(&["search_text".into()])?;
        ids(self
            .table
            .query()
            .only_if(filter)
            .select(Select::columns(&["id"]))
            .full_text_search(q)
            .limit(CANDIDATES)
            .execute()
            .await?
            .try_collect()
            .await?)
    }

    async fn exact_search(&self, filter: &str, query: &str) -> Result<Vec<String>> {
        let filter = format!(
            "({filter}) AND contains(body,{})",
            sql(&query.to_lowercase())
        );
        ids(self
            .table
            .query()
            .only_if(filter)
            .select(Select::columns(&["id"]))
            .limit(CANDIDATES)
            .execute()
            .await?
            .try_collect()
            .await?)
    }

    async fn neighbors(
        &self,
        filter: &str,
        vector: Vec<f32>,
        threshold: f32,
        limit: usize,
    ) -> Result<Vec<(String, f32)>> {
        let q = self
            .table
            .query()
            .only_if(format!(
                "({filter}) AND embedding_id = {}",
                sql(EMBEDDING_VERSION)
            ))
            .select(Select::columns(&["id"]))
            .limit(limit)
            .nearest_to(vector)?
            .column("vector")
            .distance_type(DistanceType::Cosine)
            .nprobes(ANN_PROBES)?
            .refine_factor(3);
        let batches: Vec<RecordBatch> = q.execute().await?.try_collect().await?;
        let mut result = vec![];
        for b in batches {
            for (id, distance) in col::<StringArray>(&b, "id")?
                .iter()
                .zip(col::<Float32Array>(&b, "_distance")?.iter())
            {
                if let (Some(id), Some(d)) = (id, distance) {
                    let score = 1.0 - d;
                    if score >= threshold {
                        result.push((id.into(), score));
                    }
                }
            }
        }
        Ok(result)
    }

    async fn result_page(
        &self,
        entries: Vec<Entry>,
        offset: usize,
        limit: usize,
        mode: &'static str,
        degraded: bool,
    ) -> Result<Results> {
        let mut found = Vec::new();
        let mut seen = HashSet::new();
        for entry in entries {
            if !entry.origin.is_empty()
                && !seen.insert((entry.origin.clone(), entry.occurred.clone()))
            {
                continue;
            }
            found.push(entry);
        }
        let more = found.len() > offset.saturating_add(limit);
        let mut memories = vec![];
        let snapshot = self.all().await?;
        let by_id: HashMap<&str, &Entry> = snapshot.iter().map(|e| (e.id.as_str(), e)).collect();
        for entry in found.into_iter().skip(offset).take(limit) {
            let trail = self.trail(&entry, &by_id).await;
            let mut other_accounts = vec![];
            if !entry.origin.is_empty() {
                let filter = format!(
                    "NOT deleted AND origin = {} AND occurred = {} AND id != {}",
                    sql(&entry.origin),
                    sql(&entry.occurred),
                    sql(&entry.id)
                );
                other_accounts = self
                    .records(&filter)
                    .await?
                    .into_iter()
                    .take(5)
                    .map(|r| excerpt(r.entry))
                    .collect();
            }
            memories.push(Hit {
                entry: excerpt(entry),
                other_accounts,
                trail,
            });
        }
        Ok(Results {
            memories,
            mode,
            more,
            degraded,
        })
    }

    pub async fn timeline(&self, q: &Query, offset: usize) -> Result<Results> {
        let mut entries: Vec<_> = self
            .records(&self.scope(q).await?)
            .await?
            .into_iter()
            .map(|r| r.entry)
            .collect();
        entries.sort_by(|a, b| {
            (&b.occurred, &b.recorded, &b.id).cmp(&(&a.occurred, &a.recorded, &a.id))
        });
        self.result_page(entries, offset, q.limit.max(1), "timeline", false)
            .await
    }

    pub async fn recall(&self, q: &Query) -> Result<Results> {
        let filter = self.scope(q).await?;
        let limit = if q.limit == 0 { 8 } else { q.limit };
        let text = q.text.trim();
        if text.is_empty() {
            let mut query = q.clone();
            query.limit = limit;
            return self.timeline(&query, 0).await;
        }
        if self.table.count_rows(Some(filter.clone())).await? == 0 {
            return Ok(Results {
                memories: vec![],
                more: false,
                mode: "lexical",
                degraded: false,
            });
        }
        let path = text.starts_with('/')
            || text.starts_with("\\\\")
            || text
                .as_bytes()
                .get(1..3)
                .is_some_and(|v| v == b":\\" || v == b":/");
        let mut score = HashMap::<String, (bool, f64)>::new();
        let mut mode = "lexical";
        let mut degraded = false;
        if !path && q.strategy != "lexical" {
            match self.embedding(text.into()).await {
                Ok(Some(v)) => match self.neighbors(&filter, v, SEMANTIC, CANDIDATES).await {
                    Ok(neighbors) => {
                        mode = if q.strategy == "semantic" {
                            "semantic"
                        } else {
                            "hybrid"
                        };
                        for (i, (id, _)) in neighbors.into_iter().enumerate() {
                            score.entry(id).or_default().1 += SEMANTIC_WEIGHT / (RRF + i as f64);
                        }
                    }
                    Err(error) => {
                        crate::log!("Vector query: {error:#}");
                        degraded = true;
                    }
                },
                Ok(None) => {
                    degraded = q.strategy == "semantic";
                }
                Err(error) => {
                    crate::log!("Embedding: {error:#}");
                    degraded = true;
                }
            }
        }
        if mode != "semantic" {
            for id in self.exact_search(&filter, text).await? {
                score.entry(id).or_default().0 = true;
            }
            if !path {
                match self.text_search(&filter, text).await {
                    Ok(hits) => {
                        for (i, id) in hits.into_iter().enumerate() {
                            score.entry(id).or_default().1 += LEXICAL_WEIGHT / (RRF + i as f64);
                        }
                    }
                    Err(error) => {
                        crate::log!("Full-text query: {error:#}");
                        degraded = true;
                    }
                }
            }
        }
        let mut ranked: Vec<_> = score.into_iter().collect();
        ranked.sort_by(|(a, x), (b, y)| {
            y.0.cmp(&x.0)
                .then_with(|| y.1.total_cmp(&x.1))
                .then_with(|| a.cmp(b))
        });
        let mut entries = vec![];
        if !ranked.is_empty() {
            let ids = ranked
                .iter()
                .map(|(s, _)| sql(s))
                .collect::<Vec<_>>()
                .join(",");
            let mut rows: HashMap<_, _> = self
                .records(&format!("NOT deleted AND id IN ({ids})"))
                .await?
                .into_iter()
                .map(|r| (r.entry.id.clone(), r.entry))
                .collect();
            entries.extend(ranked.into_iter().filter_map(|(id, _)| rows.remove(&id)));
        }
        self.result_page(entries, 0, limit, mode, degraded).await
    }

    /// Every live memory, cached until the next write.
    pub async fn all(&self) -> Result<Arc<Vec<Entry>>> {
        if let Some(cached) = self.snapshot.read().await.clone() {
            return Ok(cached);
        }
        let before = self.generation.load(Ordering::SeqCst);
        let list = Arc::new(
            self.records("NOT deleted")
                .await?
                .into_iter()
                .map(|r| r.entry)
                .collect::<Vec<_>>(),
        );
        if self.generation.load(Ordering::SeqCst) == before {
            *self.snapshot.write().await = Some(list.clone());
        }
        Ok(list)
    }

    /// The query filter, narrowed to one entity's memories when asked.
    async fn scope(&self, q: &Query) -> Result<String> {
        let mut filter = q.filter()?;
        if !q.entity.is_empty() {
            ensure!(valid_id(&q.entity), "实体 ID 无效");
            let idx = self.idx.read().await;
            let canon = idx
                .canonical(&q.entity)
                .map(|e| e.id.clone())
                .context("未找到实体")?;
            let ids = idx.memory_ids(&canon);
            let list = if ids.is_empty() {
                "''".to_owned()
            } else {
                ids.iter().map(|i| sql(i)).collect::<Vec<_>>().join(",")
            };
            filter.push_str(&format!(" AND id IN ({list})"));
        }
        Ok(filter)
    }

    /// How a memory was corrected or summarized, with when and by whom.
    pub(crate) async fn trail(&self, entry: &Entry, by_id: &HashMap<&str, &Entry>) -> Vec<Trail> {
        let idx = self.idx.read().await;
        let step = |relation: &'static str, other: &str, reason: &str| -> Option<Trail> {
            let e = by_id.get(other)?;
            Some(Trail {
                relation,
                id: e.id.clone(),
                occurred: e.occurred.clone(),
                recorded: e.recorded.clone(),
                source: e.source.clone(),
                reason: reason.to_owned(),
                text: snippet(&e.text, 120),
            })
        };
        let mut out = Vec::new();
        for r in &entry.references {
            match r.kind {
                RefKind::Corrects => out.extend(step("corrects", &r.id, &r.reason)),
                RefKind::Summarizes => out.extend(step("summarizes", &r.id, &r.reason)),
                RefKind::Relates => {}
            }
        }
        for (source, r) in idx.cites.get(&entry.id).into_iter().flatten() {
            match r.kind {
                RefKind::Corrects => out.extend(step("corrected_by", source, &r.reason)),
                RefKind::Summarizes => out.extend(step("summarized_by", source, &r.reason)),
                RefKind::Relates => {}
            }
        }
        out.truncate(8);
        out
    }

    pub async fn maintain(&self, selected: Option<LaneKind>) -> Result<()> {
        let _maintenance = self.maintenance.lock().await;
        self.maintain_locked(selected).await
    }

    pub async fn queue_rebuild(self: &Arc<Self>, kind: LaneKind) -> bool {
        let Ok(maintenance) = self.maintenance.clone().try_lock_owned() else {
            return false;
        };
        {
            let mut status = self.status.write().await;
            if kind != LaneKind::Semantic {
                status.keyword.state = "queued";
            }
            if self.uses_vectors() && kind != LaneKind::Keyword {
                status.semantic.state = "queued";
            }
        }
        let store = self.clone();
        tokio::spawn(async move {
            let _maintenance = maintenance;
            if let Err(e) = store.maintain_locked(Some(kind)).await {
                crate::log!("Index rebuild: {e:#}");
            }
        });
        true
    }

    async fn maintain_locked(&self, selected: Option<LaneKind>) -> Result<()> {
        let _writing = self.writes.lock().await;
        let force_keyword = matches!(selected, Some(LaneKind::All | LaneKind::Keyword));
        let force_semantic = matches!(selected, Some(LaneKind::All | LaneKind::Semantic));
        {
            let mut status = self.status.write().await;
            if force_keyword || status.keyword.state.is_empty() {
                status.keyword.state = "working";
            }
            if self.uses_vectors() && (force_semantic || status.semantic.state.is_empty()) {
                status.semantic.state = "working";
            }
        }
        let keyword = self.check_keyword(force_keyword).await;
        let semantic = self.check_semantic(force_semantic).await;
        let mut status = self.status.write().await;
        status.total = self.table.count_rows(Some("NOT deleted".into())).await?;
        match keyword {
            Ok((count, repaired)) => {
                status.keyword_indexed = count;
                update_lane(&mut status.keyword, "ready", repaired, None);
            }
            Err(err) => {
                crate::log!("Keyword index: {err:#}");
                update_lane(
                    &mut status.keyword,
                    "degraded",
                    false,
                    Some("关键词索引不可用，已保留字面匹配。".into()),
                );
            }
        }
        match semantic {
            Ok((indexed, ann, repaired)) => {
                status.indexed = indexed;
                status.skipped = if self.uses_vectors() {
                    status.total.saturating_sub(indexed)
                } else {
                    0
                };
                status.ann = ann.is_some();
                status.ann_indexed = ann.unwrap_or(0);
                let state = if !self.uses_vectors() {
                    "disabled"
                } else if status.skipped > 0 {
                    "degraded"
                } else {
                    "ready"
                };
                update_lane(&mut status.semantic, state, repaired, None);
            }
            Err(err) => {
                crate::log!("Semantic index: {err:#}");
                update_lane(
                    &mut status.semantic,
                    "degraded",
                    false,
                    Some("向量索引维护未完成。".into()),
                );
            }
        }
        Ok(())
    }

    async fn check_keyword(&self, force: bool) -> Result<(usize, bool)> {
        let before = self.table.index_stats(FTS).await?;
        let rebuild = force || before.as_ref().is_none_or(|s| s.num_unindexed_rows >= 64);
        if rebuild {
            let builder = FtsIndexBuilder::default()
                .base_tokenizer("whitespace".into())
                .stem(false)
                .remove_stop_words(false)
                .ascii_folding(false)
                .max_token_length(Some(256));
            self.table
                .create_index(&["search_text"], Index::FTS(builder))
                .name(FTS.into())
                .replace(true)
                .execute()
                .await?;
        }
        Ok((
            self.table
                .index_stats(FTS)
                .await?
                .map_or(0, |s| s.num_indexed_rows),
            rebuild,
        ))
    }

    async fn check_semantic(&self, force: bool) -> Result<(usize, Option<usize>, bool)> {
        if !self.uses_vectors() {
            return Ok((0, None, false));
        }
        let filter = format!(
            "NOT deleted AND (embedding_id != {} OR vector IS NULL)",
            sql(EMBEDDING_VERSION)
        );
        let missing = self.records(&filter).await?;
        let mut repaired = !missing.is_empty();
        for records in missing.chunks(64) {
            let mut rows = Vec::with_capacity(records.len());
            for record in records {
                rows.push(self.row(record.clone()).await?);
            }
            self.upsert(&rows).await?;
        }
        let count = self
            .table
            .count_rows(Some(format!("NOT deleted AND NOT ({filter})")))
            .await?;
        let before = self.table.index_stats(ANN).await?;
        if count < ANN_MIN {
            if before.is_some() {
                self.table.drop_index(ANN).await?;
                repaired = true;
            }
            return Ok((count, None, repaired));
        }
        if force || before.as_ref().is_none_or(|s| s.num_unindexed_rows >= 128) {
            let builder = IvfHnswFlatIndexBuilder::default()
                .distance_type(DistanceType::Cosine)
                .num_partitions((count as f64).sqrt().round() as u32);
            self.table
                .create_index(&["vector"], Index::IvfHnswFlat(builder))
                .name(ANN.into())
                .replace(true)
                .execute()
                .await?;
            repaired = true;
        }
        Ok((
            count,
            self.table
                .index_stats(ANN)
                .await?
                .map(|s| s.num_indexed_rows),
            repaired,
        ))
    }

    /// Graph data. Memories and entities are nodes; edges are saved references and AI-provided mentions.
    /// `overview` shows the most mentioned entities and how often they appear together.
    pub async fn graph(
        &self,
        selected: &[String],
        focus: &str,
        suggested: bool,
        overview: bool,
    ) -> Result<serde_json::Value> {
        use serde_json::json;
        let entries = self.all().await?;
        let by_id: HashMap<&str, &Entry> = entries.iter().map(|e| (e.id.as_str(), e)).collect();
        let last_of = |e: &Entry| (e.occurred.clone(), e.recorded.clone());
        let memory_node = |e: &Entry| json!({"id": e.id, "kind": "memory", "text": snippet(&e.text, 110), "occurred": e.occurred, "source": e.source});
        let idx = self.idx.read().await;
        let entity_node = |id: &str, count: usize| {
            idx.entities.get(id).map(|e| {
                json!({"id": e.id, "kind": "entity", "text": e.name, "hint": e.hint, "aliases": e.aliases, "count": count})
            })
        };
        let mut nodes: Vec<serde_json::Value> = vec![];
        let mut edges: Vec<serde_json::Value> = vec![];
        let more: bool;

        if overview {
            let mut ranked: Vec<(String, usize)> = idx
                .entities
                .values()
                .filter(|e| e.merged_into.is_none())
                .map(|e| (e.id.clone(), idx.memory_ids(&e.id).len()))
                .filter(|(_, n)| *n > 0)
                .collect();
            ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            more = ranked.len() > 40;
            ranked.truncate(40);
            let top: HashSet<&str> = ranked.iter().map(|r| r.0.as_str()).collect();
            let mut pairs: HashMap<(String, String), usize> = HashMap::new();
            for e in entries.iter() {
                let mut here: Vec<String> = e
                    .mentions
                    .iter()
                    .filter_map(|m| m.entity.as_deref())
                    .filter_map(|id| idx.canonical(id))
                    .map(|c| c.id.clone())
                    .filter(|id| top.contains(id.as_str()))
                    .collect();
                here.sort();
                here.dedup();
                for (i, a) in here.iter().enumerate() {
                    for b in &here[i + 1..] {
                        *pairs.entry((a.clone(), b.clone())).or_default() += 1;
                    }
                }
            }
            nodes.extend(ranked.iter().filter_map(|(id, n)| entity_node(id, *n)));
            for ((a, b), n) in pairs {
                edges.push(
                    json!({"from": a, "to": b, "co": n, "reason": format!("{n} 条记忆同时提及")}),
                );
            }
        } else if let Some(entity) = (!focus.is_empty()).then(|| idx.canonical(focus)).flatten() {
            let mut ids: Vec<&Entry> = idx
                .memory_ids(&entity.id)
                .iter()
                .filter_map(|id| by_id.get(id.as_str()).copied())
                .collect();
            ids.sort_by_key(|e| std::cmp::Reverse(last_of(e)));
            more = ids.len() > 60;
            ids.truncate(60);
            nodes.extend(entity_node(&entity.id, ids.len()));
            let visible: HashSet<&str> = ids.iter().map(|e| e.id.as_str()).collect();
            for e in &ids {
                nodes.push(memory_node(e));
                edges.push(json!({"from": e.id, "to": entity.id, "mention": true}));
                for r in &e.references {
                    if visible.contains(r.id.as_str()) {
                        edges.push(
                            json!({"from": e.id, "to": r.id, "reason": r.reason, "rk": r.kind}),
                        );
                    }
                }
            }
        } else {
            let mut ids = selected.to_vec();
            let mut guesses = vec![];
            if !focus.is_empty() {
                let f = by_id.get(focus).context("未找到记忆")?;
                ids = vec![focus.into()];
                ids.extend(f.references.iter().map(|r| r.id.clone()));
                ids.extend(
                    idx.cites
                        .get(focus)
                        .into_iter()
                        .flatten()
                        .map(|(source, _)| source.clone()),
                );
                if suggested
                    && let Some(v) = self.embedding(format!("{}\n{}", f.text, f.context)).await?
                {
                    for (id, similarity) in self
                        .neighbors(
                            &format!("NOT deleted AND id != {}", sql(focus)),
                            v,
                            GRAPH,
                            20,
                        )
                        .await?
                    {
                        if !ids.contains(&id) && guesses.len() < 8 {
                            guesses.push(json!({"from": focus, "to": id, "suggested": true, "reason": format!("语义接近 {similarity:.2}")}));
                            ids.push(id);
                        }
                    }
                }
            }
            ids.retain(|id| by_id.contains_key(id.as_str()));
            let mut seen = HashSet::new();
            ids.retain(|id| seen.insert(id.clone()));
            more = ids.len() > 100;
            ids.truncate(100);
            let visible: HashSet<&str> = ids.iter().map(String::as_str).collect();
            let mut shown_entities: Vec<String> = vec![];
            for id in &ids {
                let e = by_id[id.as_str()];
                nodes.push(memory_node(e));
                for r in &e.references {
                    if visible.contains(r.id.as_str()) {
                        edges.push(
                            json!({"from": id, "to": r.id, "reason": r.reason, "rk": r.kind}),
                        );
                    }
                }
                for m in &e.mentions {
                    if let Some(c) = m.entity.as_deref().and_then(|x| idx.canonical(x)) {
                        if !shown_entities.contains(&c.id) && shown_entities.len() < 60 {
                            shown_entities.push(c.id.clone());
                        }
                        if shown_entities.contains(&c.id) {
                            edges.push(json!({"from": id, "to": c.id, "mention": true}));
                        }
                    }
                }
            }
            for id in &shown_entities {
                nodes.extend(entity_node(id, idx.memory_ids(id).len()));
            }
            edges.extend(
                guesses
                    .into_iter()
                    .filter(|g| g["to"].as_str().is_some_and(|id| visible.contains(id))),
            );
            edges.dedup_by(|a, b| a == b);
        }
        Ok(json!({"nodes": nodes, "edges": edges, "more": more}))
    }
}

fn update_lane(lane: &mut Lane, state: &'static str, repair: bool, problem: Option<String>) {
    lane.state = state;
    lane.checked_at = Some(now());
    if repair {
        lane.repaired_at = lane.checked_at.clone();
    }
    lane.problem = problem;
}

fn ids(batches: Vec<RecordBatch>) -> Result<Vec<String>> {
    let mut out = vec![];
    for b in batches {
        out.extend(
            col::<StringArray>(&b, "id")?
                .iter()
                .flatten()
                .map(str::to_owned),
        );
    }
    Ok(out)
}

fn excerpt(mut e: Entry) -> Entry {
    e.text = snippet(&e.text, 420);
    e.context = snippet(&e.context, 240);
    e.history.clear();
    e.cited_by.clear();
    e
}
pub fn now() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
}

#[cfg(test)]
mod tests {}
