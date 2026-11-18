//! One authoritative Lance table; a memory, its outgoing links and its write key commit together.
use crate::{
    embedding::{self, Embed},
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
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
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
pub const MAX_BACKUP: usize = 1 << 30;
const TABLE: &str = "memories_v1";
const FTS: &str = "search_text_idx";
const ANN: &str = "vector_idx";
const EMBEDDING_VERSION: &str =
    "granite-embedding-97m-multilingual-r2/835ad140/fp32/cls-v1/passages-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Backup {
    pub format: String,
    pub version: u32,
    pub memories: Vec<Record>,
}

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

pub struct Store {
    table: Table,
    embedder: Option<Arc<dyn Embed>>,
    pub status: RwLock<IndexStatus>,
    writes: Mutex<()>,
    maintenance: Arc<Mutex<()>>,
    recovery: PathBuf,
    _lock: File,
}

struct Row {
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
                terms(&format!("{} {}", e.text, e.context), usize::MAX)
            },
            if row.deleted {
                String::new()
            } else {
                format!("{}\n{}", e.text.to_lowercase(), e.context.to_lowercase())
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

fn col<'a, T: 'static>(b: &'a RecordBatch, name: &str) -> Result<&'a T> {
    b.column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref())
        .with_context(|| format!("Unexpected Lance column: {name}"))
}

fn sql(s: &str) -> String {
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
        let model = if embedder.is_some() {
            embedding::ID
        } else {
            ""
        };
        Ok(Self {
            table,
            embedder,
            status: RwLock::new(IndexStatus {
                model,
                dimensions: embedding::DIMENSIONS,
                ..Default::default()
            }),
            writes: Mutex::new(()),
            maintenance: Arc::new(Mutex::new(())),
            recovery: dir.join("recovery.recallport.json"),
            _lock: lock,
        })
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

    async fn row(&self, record: Record) -> Result<Row> {
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

    async fn records(&self, filter: &str) -> Result<Vec<Record>> {
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

    async fn find(&self, id: &str) -> Result<Record> {
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

    async fn hydrate(&self, mut item: Entry) -> Result<Entry> {
        let ids: Vec<_> = item.references.iter().map(|r| sql(&r.id)).collect();
        if !ids.is_empty() {
            let existing = self
                .records(&format!("NOT deleted AND id IN ({})", ids.join(",")))
                .await?;
            let keep: HashSet<_> = existing.iter().map(|r| r.entry.id.as_str()).collect();
            item.references.retain(|r| keep.contains(r.id.as_str()));
        }
        for row in self
            .records(&format!(
                "NOT deleted AND contains(payload,{})",
                sql(&item.id)
            ))
            .await?
        {
            if let Some(link) = row.entry.references.iter().find(|r| r.id == item.id) {
                item.cited_by.push(Reference {
                    id: row.entry.id.clone(),
                    reason: link.reason.clone(),
                });
            }
        }
        item.cited_by.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(item)
    }

    pub async fn read(&self, id: &str) -> Result<Entry> {
        self.hydrate(self.find(id).await?.entry).await
    }

    pub async fn remember(&self, draft: Draft, actor: &str, source: &str) -> Result<Entry> {
        let draft = draft.clean()?;
        ensure!(source.len() <= 160, "来源名称过长");
        let hash = fingerprint(&draft)?;
        let _guard = self.writes.lock().await;
        if !draft.request_id.is_empty() {
            let filter = format!(
                "NOT deleted AND actor = {} AND request_id = {}",
                sql(actor),
                sql(&draft.request_id)
            );
            if let Some(r) = self.records(&filter).await?.pop() {
                ensure!(
                    r.request.as_ref().is_some_and(|r| r.fingerprint == hash),
                    "此请求 ID 已用于不同的内容"
                );
                return self.hydrate(r.entry).await;
            }
        }
        for target in &draft.references {
            self.find(&target.id).await.context("引用的记忆不存在")?;
        }
        let record = Record {
            entry: Entry {
                id: uuid::Uuid::new_v4().simple().to_string(),
                text: draft.text,
                context: draft.context,
                occurred: draft.occurred,
                origin: draft.origin,
                source: source.into(),
                recorded: now(),
                revision: 1,
                references: draft.references,
                cited_by: vec![],
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
        self.hydrate(row.record.entry).await
    }

    pub async fn edit(&self, id: &str, revision: u32, draft: Draft) -> Result<()> {
        let draft = draft.clean()?;
        let _guard = self.writes.lock().await;
        let mut row = self.find(id).await?;
        ensure!(row.entry.revision == revision, "记忆已被修改，请刷新后重试");
        row.entry.text = draft.text;
        row.entry.context = draft.context;
        row.entry.occurred = draft.occurred;
        row.entry.revision = revision.checked_add(1).context("版本超出范围")?;
        self.upsert(&[self.row(row).await?]).await
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
        self.upsert(&[Row {
            record: row,
            vector: None,
            deleted: true,
        }])
        .await
    }

    pub async fn link(&self, id: &str, link: Reference, remove: bool) -> Result<()> {
        ensure!(id != link.id && link.reason.len() <= 800, "引用无效");
        let _guard = self.writes.lock().await;
        self.find(&link.id).await?;
        let mut row = self.find(id).await?;
        let targets: Vec<_> = row.entry.references.iter().map(|r| sql(&r.id)).collect();
        if !targets.is_empty() {
            let active = self
                .records(&format!("NOT deleted AND id IN ({})", targets.join(",")))
                .await?;
            let ids: HashSet<_> = active.into_iter().map(|r| r.entry.id).collect();
            row.entry.references.retain(|r| ids.contains(&r.id));
        }
        row.entry.references.retain(|r| r.id != link.id);
        if !remove {
            ensure!(row.entry.references.len() < 16, "最多引用 16 条记忆");
            row.entry.references.push(link);
        }
        let filter = format!("NOT deleted AND id = {}", sql(id));
        self.table
            .update()
            .only_if(filter)
            .column("payload", sql(&serde_json::to_string(&row)?))
            .execute()
            .await?;
        Ok(())
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
        for entry in found.into_iter().skip(offset).take(limit) {
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
            .records(&q.filter()?)
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
        let filter = q.filter()?;
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
                        eprintln!("Vector query: {error:#}");
                        degraded = true;
                    }
                },
                Ok(None) => {
                    degraded = q.strategy == "semantic";
                }
                Err(error) => {
                    eprintln!("Embedding: {error:#}");
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
                        eprintln!("Full-text query: {error:#}");
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

    pub async fn all(&self) -> Result<Vec<Entry>> {
        Ok(self
            .records("NOT deleted")
            .await?
            .into_iter()
            .map(|r| r.entry)
            .collect())
    }

    pub async fn backup(&self) -> Result<Backup> {
        let mut memories = self.records("NOT deleted").await?;
        let ids: HashSet<_> = memories.iter().map(|r| r.entry.id.clone()).collect();
        for r in &mut memories {
            r.entry.references.retain(|link| ids.contains(&link.id));
            r.entry.references.sort_by(|a, b| a.id.cmp(&b.id));
        }
        memories.sort_by(|a, b| a.entry.id.cmp(&b.entry.id));
        Ok(Backup {
            format: "recallport".into(),
            version: 1,
            memories,
        })
    }

    pub fn recovery(&self) -> &Path {
        &self.recovery
    }

    pub async fn replace(&self, backup: Backup, save_recovery: bool) -> Result<()> {
        backup.validate()?;
        let mut rows = Vec::with_capacity(backup.memories.len());
        // Finish model inference before replacing a single live row.
        for record in backup.memories {
            rows.push(self.row(record).await?);
        }
        let _lock = self.writes.lock().await;
        if save_recovery {
            use std::io::Write;
            let mut temp =
                tempfile::NamedTempFile::new_in(self.recovery.parent().context("Recovery path")?)?;
            serde_json::to_writer(temp.as_file_mut(), &self.backup().await?)?;
            temp.flush()?;
            temp.as_file().sync_all()?;
            temp.persist(&self.recovery)?;
        }
        self.table
            .add(batch(&rows)?)
            .mode(AddDataMode::Overwrite)
            .execute()
            .await?;
        Ok(())
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
                eprintln!("Index rebuild: {e:#}");
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
                eprintln!("Keyword index: {err:#}");
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
                eprintln!("Semantic index: {err:#}");
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

    pub async fn graph(
        &self,
        selected: &[String],
        focus: &str,
        suggested: bool,
    ) -> Result<serde_json::Value> {
        let entries = self.all().await?;
        let by_id: HashMap<_, _> = entries.iter().map(|e| (e.id.as_str(), e)).collect();
        let mut nodes = selected.to_vec();
        let mut guesses = vec![];
        if !focus.is_empty() {
            let f = by_id.get(focus).context("未找到记忆")?;
            nodes = vec![focus.into()];
            nodes.extend(f.references.iter().map(|r| r.id.clone()));
            nodes.extend(
                entries
                    .iter()
                    .filter(|e| e.references.iter().any(|r| r.id == focus))
                    .map(|e| e.id.clone()),
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
                    if !nodes.contains(&id) && guesses.len() < 8 {
                        guesses.push(serde_json::json!({"from":focus,"to":id,"suggested":true,"reason":format!("语义接近 {similarity:.2}")}));
                        nodes.push(id);
                    }
                }
            }
        }
        nodes.retain(|id| by_id.contains_key(id.as_str()));
        let mut seen = HashSet::new();
        nodes.retain(|id| seen.insert(id.clone()));
        let more = nodes.len() > 100;
        nodes.truncate(100);
        let mut edges = vec![];
        let visible = nodes.iter().collect::<HashSet<_>>();
        for id in &nodes {
            for r in &by_id[id.as_str()].references {
                if visible.contains(&r.id) {
                    edges.push(serde_json::json!({"from":id,"to":r.id,"reason":r.reason}));
                }
            }
        }
        edges.extend(guesses.into_iter().filter(|g| {
            g["to"]
                .as_str()
                .is_some_and(|id| nodes.iter().any(|n| n == id))
        }));
        Ok(
            serde_json::json!({"nodes": nodes.iter().map(|id| { let e=by_id[id.as_str()]; serde_json::json!({"id":id,"text":snippet(&e.text,110),"occurred":e.occurred,"source":e.source}) }).collect::<Vec<_>>(),"edges":edges,"more":more}),
        )
    }
}

impl Backup {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        ensure!(bytes.len() <= MAX_BACKUP, "备份文件过大");
        let backup: Self = serde_json::from_slice(bytes).context("不是 RecallPort 备份")?;
        backup.validate()?;
        Ok(backup)
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            self.format == "recallport" && self.version == 1,
            "不支持的备份格式或版本"
        );
        let mut ids = HashSet::new();
        let mut keys = HashSet::new();
        for r in &self.memories {
            let e = &r.entry;
            ensure!(
                valid_id(&e.id) && ids.insert(&e.id) && e.revision > 0 && e.cited_by.is_empty(),
                "备份含有无效或重复的记忆"
            );
            chrono::DateTime::parse_from_rfc3339(&e.recorded)?;
            Draft {
                text: e.text.clone(),
                context: e.context.clone(),
                occurred: e.occurred.clone(),
                origin: e.origin.clone(),
                references: e.references.clone(),
                request_id: String::new(),
            }
            .clean()?;
            ensure!(e.source.len() <= 160, "来源名称过长");
            if let Some(k) = &r.request {
                ensure!(
                    !k.actor.is_empty()
                        && k.actor.len() <= 164
                        && !k.request_id.is_empty()
                        && k.request_id.len() <= 128
                        && k.fingerprint.len() == 64
                        && k.fingerprint.bytes().all(|x| x.is_ascii_hexdigit())
                        && keys.insert((&k.actor, &k.request_id)),
                    "备份含有无效或重复的去重记录"
                );
            }
        }
        for r in &self.memories {
            for link in &r.entry.references {
                ensure!(
                    link.id != r.entry.id && ids.contains(&link.id),
                    "备份含有悬空或自指引用"
                );
            }
        }
        Ok(())
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
    e
}
pub fn now() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed;
    impl Embed for Fixed {
        fn encode(&self, input: &str) -> Result<Vec<f32>> {
            let mut v = vec![0.0; embedding::DIMENSIONS as usize];
            if input.contains("自启") || input.contains("开机") {
                v[0] = 1.0;
            } else {
                v[1] = 1.0;
            }
            Ok(v)
        }
    }

    #[tokio::test]
    async fn transactions_search_and_backup() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let store = Store::open(tmp.path(), Some(Arc::new(Fixed))).await?;
        assert!(Store::open(tmp.path(), None).await.is_err());
        store.maintain(None).await?;
        let d = Draft {
            text: "服务器配置为自启服务".into(),
            occurred: "2024-02".into(),
            request_id: "same".into(),
            ..Default::default()
        };
        let (a, b) = tokio::join!(
            store.remember(d.clone(), "mcp:test", "test"),
            store.remember(d.clone(), "mcp:test", "test")
        );
        let a = a?;
        assert_eq!(a.id, b?.id);
        let changed = Draft {
            text: "把配置改成手动".into(),
            ..d
        };
        assert!(
            store
                .remember(changed.clone(), "mcp:test", "test")
                .await
                .is_err()
        );
        let other = store
            .remember(
                Draft {
                    text: "配置回滚策略".into(),
                    occurred: "2025".into(),
                    ..Default::default()
                },
                "web",
                "Web",
            )
            .await?;
        store
            .link(
                &a.id,
                Reference {
                    id: other.id.clone(),
                    reason: "与旧配置的差异 ' 中文".into(),
                },
                false,
            )
            .await?;
        assert_eq!(store.read(&other.id).await?.cited_by[0].id, a.id);
        for (text, strategy, id) in [
            ("自启", "lexical", &a.id),
            ("开机", "semantic", &a.id),
            ("回滚", "hybrid", &other.id),
        ] {
            let r = store
                .recall(&Query {
                    text: text.into(),
                    strategy: strategy.into(),
                    ..Default::default()
                })
                .await?;
            assert_eq!(&r.memories[0].entry.id, id);
            assert!(!r.degraded);
        }
        let found = store
            .recall(&Query {
                text: "开机".into(),
                strategy: "semantic".into(),
                from: "2024-02-29".into(),
                through: "2024-02-29".into(),
                ..Default::default()
            })
            .await?;
        assert_eq!(found.memories[0].entry.id, a.id);
        assert!(
            !store
                .recall(&Query {
                    text: "回滚".into(),
                    strategy: "lexical".into(),
                    from: "2024".into(),
                    through: "2024".into(),
                    ..Default::default()
                })
                .await?
                .memories
                .iter()
                .any(|h| h.entry.id == other.id)
        );
        let backup = store.backup().await?;
        store.edit(&a.id, 1, changed.clone()).await?;
        assert!(store.edit(&a.id, 1, changed).await.is_err());
        store.delete(&other.id, 1).await?;
        assert!(store.read(&a.id).await?.references.is_empty());
        let mut malformed = backup.clone();
        malformed.memories.push(malformed.memories[0].clone());
        assert!(store.replace(malformed, true).await.is_err());
        store
            .replace(Backup::parse(&serde_json::to_vec(&backup)?)?, true)
            .await?;
        store.maintain(Some(LaneKind::All)).await?;
        assert!(store.recovery().is_file());
        drop(store);
        let reopened = Store::open(tmp.path(), Some(Arc::new(Fixed))).await?;
        assert_eq!(reopened.backup().await?, backup);
        Ok(())
    }

    #[tokio::test]
    async fn ann_and_chinese_terms_on_a_populated_table() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let store = Store::open(tmp.path(), Some(Arc::new(Fixed))).await?;
        let items = (0..ANN_MIN + 2)
            .map(|i| Record {
                entry: Entry {
                    id: format!("{i:032x}"),
                    text: if i == 0 {
                        "自启策略".into()
                    } else {
                        format!("其他经验 {i}")
                    },
                    occurred: if i == 0 {
                        "2024-02".into()
                    } else {
                        "2025-02".into()
                    },
                    source: "test".into(),
                    recorded: now(),
                    revision: 1,
                    ..Default::default()
                },
                request: None,
            })
            .collect();
        store
            .replace(
                Backup {
                    format: "recallport".into(),
                    version: 1,
                    memories: items,
                },
                false,
            )
            .await?;
        store.maintain(None).await?;
        let s = store.status.read().await;
        assert!(s.ann, "{:?}", s.semantic);
        assert_eq!(s.indexed, ANN_MIN + 2);
        drop(s);
        let r = store
            .recall(&Query {
                text: "开机".into(),
                strategy: "semantic".into(),
                from: "2024".into(),
                through: "2024".into(),
                ..Default::default()
            })
            .await?;
        assert_eq!(r.memories.len(), 1);
        assert_eq!(r.memories[0].entry.id, format!("{:032x}", 0));
        Ok(())
    }
}
