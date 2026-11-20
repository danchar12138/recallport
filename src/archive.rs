//! Portable ZIP archive: `manifest.json`, `files/<sha256>` and, when the library has them, `vectors.bin`
//! (the semantic vectors, so restoring does not have to run the model again). Everything needed to rebuild
//! a library on an empty machine. Models and programs are not included.
//!
//! Restore replaces the library; merge adds another library's memories to this one. Both validate the
//! whole archive before touching live data, keep a rollback snapshot, and survive a crash mid-switch.
use crate::{
    entities::norm,
    jobs::Job,
    kv,
    memory::*,
    store::{EMBEDDING_VERSION, Store, now},
    vectors::VectorCache,
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{BufWriter, Read, Write},
    path::{Path, PathBuf},
};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

pub const FORMAT: &str = "recallport-archive";
pub const VERSION: u32 = 1;
const MAX_MANIFEST: u64 = 1 << 30;
const MAX_ENTRIES: usize = 500_000;
const MAX_VECTORS: u64 = 2 << 30;
const VECTORS: &str = "vectors.bin";
pub const MAX_ARCHIVE: u64 = 64 << 30;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub memories: Vec<Record>,
    pub entities: Vec<Entity>,
    pub attachments: Vec<Attachment>,
}

fn yes() -> bool {
    true
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    format: String,
    version: u32,
    app: String,
    created: String,
    /// False for the automatic backups: they list the attachments but do not copy the files, which stay in
    /// the data folder beside them.
    #[serde(default = "yes")]
    files: bool,
    #[serde(flatten)]
    data: Snapshot,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ArchiveInfo {
    pub memories: usize,
    pub entities: usize,
    pub attachments: usize,
    pub bytes: u64,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct MergeReport {
    pub added: usize,
    pub merged: usize,
    pub unchanged: usize,
    pub entities_added: usize,
    pub attachments_added: usize,
    /// Names that now belong to more than one entity. They are never merged automatically.
    pub same_name_entities: Vec<String>,
}

impl Snapshot {
    /// Check an incoming snapshot is internally consistent. Nothing here touches the live library.
    pub fn validate(&self) -> Result<()> {
        let mut ids = HashSet::new();
        let mut keys = HashSet::new();
        let entity_ids: HashSet<&str> = self.entities.iter().map(|e| e.id.as_str()).collect();
        ensure!(
            entity_ids.len() == self.entities.len(),
            "备份含有重复的实体"
        );
        for e in &self.entities {
            ensure!(
                valid_id(&e.id)
                    && !e.name.trim().is_empty()
                    && e.name.chars().count() <= 200
                    && e.aliases.len() <= 64
                    && e.hint.chars().count() <= 400,
                "备份含有无效的实体"
            );
            if let Some(target) = &e.merged_into {
                ensure!(
                    entity_ids.contains(target.as_str()),
                    "实体合并指向了不存在的实体"
                );
            }
        }
        for e in &self.entities {
            let mut cur = e;
            for _ in 0..17 {
                match &cur.merged_into {
                    Some(next) => {
                        cur = self
                            .entities
                            .iter()
                            .find(|x| x.id == *next)
                            .context("实体合并链断裂")?
                    }
                    None => break,
                }
            }
            ensure!(cur.merged_into.is_none(), "实体合并出现循环");
        }
        let mut hashes = HashSet::new();
        for a in &self.attachments {
            ensure!(
                valid_hash(&a.hash)
                    && hashes.insert(a.hash.as_str())
                    && !a.name.is_empty()
                    && a.size <= MAX_ATTACHMENT
                    && a.path.len() <= 4096,
                "备份含有无效或重复的附件"
            );
        }
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
                ..Default::default()
            }
            .clean()?;
            ensure!(e.source.len() <= 160, "来源名称过长");
            ensure!(e.mentions.len() <= MAX_ENTITIES * 2, "记忆关联了过多实体");
            ensure!(e.attachments.len() <= 64, "记忆关联了过多附件");
            for m in &e.mentions {
                ensure!(m.name.chars().count() <= 200, "实体名称过长");
                if let Some(id) = &m.entity {
                    ensure!(entity_ids.contains(id.as_str()), "记忆提及了不存在的实体");
                }
            }
            for a in &e.attachments {
                ensure!(valid_hash(&a.hash), "附件哈希无效");
            }
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

    fn stored(&self) -> impl Iterator<Item = &Attachment> {
        self.attachments
            .iter()
            .filter(|a| a.storage == Storage::Stored)
    }
}

fn compressible(mime: &str) -> bool {
    mime.starts_with("text/")
        || mime.contains("json")
        || mime.contains("xml")
        || mime == "image/svg+xml"
}

fn options(method: CompressionMethod) -> SimpleFileOptions {
    SimpleFileOptions::default()
        .compression_method(method)
        .large_file(true)
}

/// Write the archive. Runs on a blocking thread.
pub fn write_zip(
    dest: &Path,
    snap: &Snapshot,
    files_root: &Path,
    vectors: Option<&VectorCache>,
    with_files: bool,
    job: &Job,
) -> Result<ArchiveInfo> {
    let blob = |hash: &str| files_root.join(&hash[..2]).join(hash);
    let mut warnings = Vec::new();
    let mut attachments = Vec::new();
    for a in &snap.attachments {
        match a.storage {
            Storage::Stored if with_files && !blob(&a.hash).is_file() => {
                warnings.push(format!("附件文件已缺失，未写入备份：{}", a.name));
            }
            _ => attachments.push(a.clone()),
        }
    }
    let data = Snapshot {
        memories: snap.memories.clone(),
        entities: snap.entities.clone(),
        attachments,
    };
    let total: u64 = data.stored().map(|a| a.size).sum();
    let mut zip = ZipWriter::new(BufWriter::new(File::create(dest)?));
    job.phase("写入记忆与关系", 0.0, 0.1);
    zip.start_file("manifest.json", options(CompressionMethod::Deflated))?;
    let manifest = Manifest {
        format: FORMAT.into(),
        version: VERSION,
        app: env!("CARGO_PKG_VERSION").into(),
        created: now(),
        files: with_files,
        data,
    };
    serde_json::to_writer(&mut zip, &manifest)?;
    if let Some(cache) = vectors.filter(|c| !c.is_empty()) {
        zip.start_file(VECTORS, options(CompressionMethod::Stored))?;
        zip.write_all(&cache.encode(EMBEDDING_VERSION))?;
    }
    job.phase("写入附件文件", 0.1, 1.0);
    let mut done = 0u64;
    let mut buffer = vec![0u8; 1 << 20];
    for a in manifest.data.stored().filter(|_| with_files) {
        let method = if compressible(&a.mime) {
            CompressionMethod::Deflated
        } else {
            CompressionMethod::Stored
        };
        zip.start_file(format!("files/{}", a.hash), options(method))?;
        let mut input = File::open(blob(&a.hash))?;
        loop {
            let n = input.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            zip.write_all(&buffer[..n])?;
            done += n as u64;
            job.step(done, total);
        }
    }
    zip.finish()?.flush()?;
    Ok(ArchiveInfo {
        memories: manifest.data.memories.len(),
        entities: manifest.data.entities.len(),
        attachments: manifest.data.attachments.len(),
        bytes: fs::metadata(dest)?.len(),
        warnings,
    })
}

/// Open an archive, check its structure and manifest, and return the snapshot with the sizes of the
/// files it carries. Nothing is extracted.
pub fn read_zip(path: &Path) -> Result<(Snapshot, HashMap<String, u64>)> {
    let mut zip = ZipArchive::new(File::open(path)?)
        .map_err(|_| anyhow!("不是有效的 RecallPort 备份（无法读取 ZIP）"))?;
    ensure!(zip.len() <= MAX_ENTRIES, "备份包含的条目过多");
    let mut files: HashMap<String, u64> = HashMap::new();
    let mut manifest = false;
    let mut declared = 0u64;
    for i in 0..zip.len() {
        let f = zip.by_index(i)?;
        let name = f.name()?.into_owned();
        if name == "manifest.json" {
            ensure!(f.size() <= MAX_MANIFEST, "备份清单过大");
            manifest = true;
        } else if name == VECTORS {
            ensure!(f.size() <= MAX_VECTORS, "备份中的向量文件过大");
        } else if name == "files/" {
            continue;
        } else if let Some(hash) = name.strip_prefix("files/") {
            ensure!(valid_hash(hash), "备份包含不认识的文件：{name}");
            ensure!(f.size() <= MAX_ATTACHMENT, "备份中的文件过大");
            ensure!(
                files.insert(hash.to_owned(), f.size()).is_none(),
                "备份含有重复的文件"
            );
            declared += f.size();
        } else {
            bail!("备份包含不认识的条目：{name}");
        }
    }
    ensure!(manifest, "备份缺少 manifest.json（不是 RecallPort 备份）");
    ensure!(declared <= MAX_ARCHIVE, "备份总大小超出上限");
    let reader = zip.by_name("manifest.json")?;
    let parsed: Manifest = serde_json::from_reader(reader.take(MAX_MANIFEST))
        .context("备份清单无法解析（不是 RecallPort 备份）")?;
    ensure!(
        parsed.format == FORMAT && parsed.version == VERSION,
        "不支持的备份格式或版本（需要 RecallPort 的 ZIP 备份）"
    );
    parsed.data.validate()?;
    for a in parsed.data.stored().filter(|_| parsed.files) {
        let size = files.get(&a.hash).context("备份缺少附件文件")?;
        ensure!(*size == a.size, "备份中的附件大小与清单不一致");
    }
    Ok((parsed.data, files))
}

/// The semantic vectors saved in an archive, if it has any made by the model in use. Anything unreadable is
/// ignored: the vectors are then simply computed again.
pub fn read_vectors(path: &Path) -> Option<VectorCache> {
    let mut zip = ZipArchive::new(File::open(path).ok()?).ok()?;
    let mut entry = zip.by_name(VECTORS).ok()?;
    let mut bytes = Vec::new();
    entry
        .by_ref()
        .take(MAX_VECTORS)
        .read_to_end(&mut bytes)
        .ok()?;
    VectorCache::decode(&bytes, EMBEDDING_VERSION)
        .ok()
        .filter(|c| !c.is_empty())
}

/// Extract and verify the files in `want` into `dest`. Each file must match its name's SHA-256.
fn extract(path: &Path, want: &[(String, u64)], dest: &Path, job: &Job) -> Result<()> {
    fs::create_dir_all(dest)?;
    let mut zip = ZipArchive::new(File::open(path)?)?;
    let total: u64 = want.iter().map(|w| w.1).sum();
    let mut done = 0u64;
    let mut buffer = vec![0u8; 1 << 20];
    for (hash, size) in want {
        let mut entry = zip.by_name(&format!("files/{hash}"))?;
        let mut out = File::create(dest.join(hash))?;
        let mut hasher = Sha256::new();
        let mut seen = 0u64;
        loop {
            let n = entry.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            seen += n as u64;
            ensure!(seen <= *size, "备份中的文件比声明的更大");
            hasher.update(&buffer[..n]);
            out.write_all(&buffer[..n])?;
            done += n as u64;
            job.step(done, total);
        }
        out.flush()?;
        ensure!(seen == *size, "备份中的文件不完整");
        ensure!(
            format!("{:x}", hasher.finalize()) == *hash,
            "备份中的文件已损坏（哈希不符）"
        );
    }
    Ok(())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("path")?;
    fs::create_dir_all(dir)?;
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path)?;
    Ok(())
}

impl Store {
    fn recovery_manifest(&self) -> PathBuf {
        self.dir.join("recovery").join("manifest.json")
    }

    fn recovery_vectors(&self) -> PathBuf {
        self.dir.join("recovery").join(VECTORS)
    }

    fn load_recovery_vectors(&self) -> Option<VectorCache> {
        let bytes = fs::read(self.recovery_vectors()).ok()?;
        VectorCache::decode(&bytes, EMBEDDING_VERSION)
            .ok()
            .filter(|c| !c.is_empty())
    }

    fn journal(&self) -> PathBuf {
        self.dir.join("restore.journal")
    }

    pub fn export_dir(&self) -> PathBuf {
        self.dir.join("tmp")
    }

    /// The whole library as it is now.
    pub async fn snapshot(&self) -> Result<Snapshot> {
        let mut memories = self.records("NOT deleted").await?;
        let live: HashSet<String> = memories.iter().map(|r| r.entry.id.clone()).collect();
        for r in &mut memories {
            r.entry.references.retain(|l| live.contains(&l.id));
            r.entry.cited_by.clear();
        }
        memories.sort_by(|a, b| a.entry.id.cmp(&b.entry.id));
        let idx = self.idx.read().await;
        let mut entities: Vec<Entity> = idx.entities.values().cloned().collect();
        entities.sort_by(|a, b| a.id.cmp(&b.id));
        let mut attachments: Vec<Attachment> = idx.files.values().cloned().collect();
        attachments.sort_by(|a, b| a.hash.cmp(&b.hash));
        Ok(Snapshot {
            memories,
            entities,
            attachments,
        })
    }

    pub async fn export_to(&self, dest: PathBuf, job: Job) -> Result<ArchiveInfo> {
        self.export_with(dest, job, true).await
    }

    /// `with_files` false writes the memories, entities and vectors only (the automatic backups).
    pub async fn export_with(
        &self,
        dest: PathBuf,
        job: Job,
        with_files: bool,
    ) -> Result<ArchiveInfo> {
        let snap = self.snapshot().await?;
        let vectors = self.vector_cache(&snap.memories).await.ok();
        let root = self.dir.join("files");
        tokio::task::spawn_blocking(move || {
            write_zip(&dest, &snap, &root, vectors.as_ref(), with_files, &job)
        })
        .await?
    }

    /// A rollback snapshot of the library before the last restore or merge, if there is one.
    pub fn recovery_info(&self) -> Option<ArchiveInfo> {
        let bytes = fs::read(self.recovery_manifest()).ok()?;
        let snap: Snapshot = serde_json::from_slice(&bytes).ok()?;
        Some(ArchiveInfo {
            memories: snap.memories.len(),
            entities: snap.entities.len(),
            attachments: snap.attachments.len(),
            bytes: fs::metadata(self.recovery_manifest())
                .map(|m| m.len())
                .unwrap_or(0),
            warnings: vec![],
        })
    }

    fn load_recovery(&self) -> Result<Snapshot> {
        let bytes = fs::read(self.recovery_manifest()).context("没有可用的回退快照")?;
        let snap: Snapshot = serde_json::from_slice(&bytes)?;
        snap.validate()?;
        Ok(snap)
    }

    pub async fn export_recovery_to(&self, dest: PathBuf, job: Job) -> Result<ArchiveInfo> {
        let snap = self.load_recovery()?;
        let vectors = self.load_recovery_vectors();
        let root = self.dir.join("files");
        tokio::task::spawn_blocking(move || {
            write_zip(&dest, &snap, &root, vectors.as_ref(), true, &job)
        })
        .await?
    }

    async fn save_recovery_locked(&self) -> Result<()> {
        let snap = self.snapshot().await?;
        // Keep the vectors too, so going back is a copy, not a rebuild.
        let vectors = self.vector_cache(&snap.memories).await?;
        if vectors.is_empty() {
            let _ = fs::remove_file(self.recovery_vectors());
        } else {
            write_atomic(&self.recovery_vectors(), &vectors.encode(EMBEDDING_VERSION))?;
        }
        write_atomic(&self.recovery_manifest(), &serde_json::to_vec(&snap)?)
    }

    /// Move verified files from `staging` into managed storage.
    fn adopt_files(&self, staging: &Path, hashes: &[String]) -> Result<()> {
        for hash in hashes {
            let target = self.blob(hash);
            // Already here, or (in an automatic backup) never copied: nothing to move.
            if target.exists() || !staging.join(hash).exists() {
                continue;
            }
            fs::create_dir_all(target.parent().context("blob path")?)?;
            if fs::rename(staging.join(hash), &target).is_err() {
                fs::copy(staging.join(hash), &target)?;
            }
        }
        Ok(())
    }

    async fn stage(
        &self,
        path: &Path,
        snap: &Snapshot,
        sizes: &HashMap<String, u64>,
        job: &Job,
    ) -> Result<(PathBuf, Vec<String>)> {
        let staging = self
            .dir
            .join("files")
            .join(".incoming")
            .join(uuid::Uuid::new_v4().simple().to_string());
        let want: Vec<(String, u64)> = snap
            .stored()
            .filter(|a| !self.blob(&a.hash).exists())
            .filter_map(|a| sizes.get(&a.hash).map(|size| (a.hash.clone(), *size)))
            .collect();
        let hashes = snap.stored().map(|a| a.hash.clone()).collect();
        job.phase("校验并解压附件", 0.05, 0.4);
        let (p, s, j) = (path.to_path_buf(), staging.clone(), job.clone());
        let result = tokio::task::spawn_blocking(move || extract(&p, &want, &s, &j)).await?;
        if let Err(e) = result {
            let _ = fs::remove_dir_all(&staging);
            return Err(e);
        }
        Ok((staging, hashes))
    }

    /// Replace the library with the content of an archive.
    pub async fn restore_zip(&self, path: PathBuf, job: Job) -> Result<ArchiveInfo> {
        job.phase("校验备份包", 0.0, 0.05);
        let p = path.clone();
        let (snap, sizes) = tokio::task::spawn_blocking(move || read_zip(&p)).await??;
        let p = path.clone();
        let vectors = tokio::task::spawn_blocking(move || read_vectors(&p)).await?;
        let (staging, hashes) = self.stage(&path, &snap, &sizes, &job).await?;
        let info = ArchiveInfo {
            memories: snap.memories.len(),
            entities: snap.entities.len(),
            attachments: snap.attachments.len(),
            bytes: fs::metadata(&path)?.len(),
            warnings: vec![],
        };
        let applied = self
            .apply(snap, vectors.as_ref(), &staging, &hashes, &job)
            .await;
        let _ = fs::remove_dir_all(&staging);
        applied?;
        Ok(info)
    }

    /// Put the library back to the snapshot saved before the last restore or merge.
    pub async fn rollback(&self, job: Job) -> Result<ArchiveInfo> {
        job.phase("读取回退快照", 0.0, 0.05);
        let snap = self.load_recovery()?;
        let info = ArchiveInfo {
            memories: snap.memories.len(),
            entities: snap.entities.len(),
            attachments: snap.attachments.len(),
            ..Default::default()
        };
        let hashes: Vec<String> = snap.stored().map(|a| a.hash.clone()).collect();
        let empty = self.dir.join("files").join(".incoming").join("none");
        let vectors = self.load_recovery_vectors();
        self.apply(snap, vectors.as_ref(), &empty, &hashes, &job)
            .await?;
        Ok(info)
    }

    /// The common switch: bring the vectors in first (reused when the archive has them, otherwise encoded; no
    /// lock), then swap everything under the write lock.
    async fn apply(
        &self,
        snap: Snapshot,
        vectors: Option<&VectorCache>,
        staging: &Path,
        hashes: &[String],
        job: &Job,
    ) -> Result<()> {
        job.phase(
            if vectors.is_some() {
                "载入语义向量"
            } else {
                "重建语义向量"
            },
            0.4,
            0.95,
        );
        let total = snap.memories.len() as u64;
        let rows = self
            .embed_records(snap.memories.clone(), vectors, |done| {
                job.step(done as u64, total)
            })
            .await?;
        job.phase("切换数据", 0.95, 1.0);
        let _guard = self.writes.lock().await;
        // From here on a crash is repaired at the next start by returning to the rollback snapshot.
        self.save_recovery_locked().await?;
        write_atomic(&self.journal(), b"switching")?;
        self.adopt_files(staging, hashes)?;
        self.overwrite_memories(&rows).await?;
        kv::replace(
            &self.etable,
            &snap
                .entities
                .iter()
                .map(|e| Ok((e.id.clone(), serde_json::to_string(e)?)))
                .collect::<Result<Vec<_>>>()?,
        )
        .await?;
        kv::replace(
            &self.atable,
            &snap
                .attachments
                .iter()
                .map(|a| Ok((a.hash.clone(), serde_json::to_string(a)?)))
                .collect::<Result<Vec<_>>>()?,
        )
        .await?;
        self.load_index().await?;
        self.invalidate().await;
        let _ = fs::remove_file(self.journal());
        Ok(())
    }

    /// If the last restore was interrupted, return to the rollback snapshot. Returns a notice to show.
    pub async fn recover_interrupted(&self) -> Result<Option<String>> {
        if !self.journal().exists() {
            return Ok(None);
        }
        let notice = match self.load_recovery() {
            Ok(snap) => {
                let job = Job::detached();
                let hashes: Vec<String> = snap.stored().map(|a| a.hash.clone()).collect();
                let empty = self.dir.join("files").join(".incoming").join("none");
                let vectors = self.load_recovery_vectors();
                self.apply(snap, vectors.as_ref(), &empty, &hashes, &job)
                    .await?;
                "上次恢复被意外中断，已自动回到恢复之前的状态。".to_owned()
            }
            Err(_) => {
                let _ = fs::remove_file(self.journal());
                "上次恢复被意外中断，且没有可用的回退快照，请检查数据是否完整。".to_owned()
            }
        };
        let _ = fs::remove_file(self.journal());
        Ok(Some(notice))
    }

    /// Add another library's memories to this one. Same id: keep both versions' history and union the links.
    pub async fn merge_zip(&self, path: PathBuf, job: Job) -> Result<MergeReport> {
        job.phase("校验备份包", 0.0, 0.05);
        let p = path.clone();
        let (incoming, sizes) = tokio::task::spawn_blocking(move || read_zip(&p)).await??;
        let p = path.clone();
        let vectors = tokio::task::spawn_blocking(move || read_vectors(&p)).await?;
        let (staging, hashes) = self.stage(&path, &incoming, &sizes, &job).await?;
        let merged = self
            .merge_snapshot(incoming, vectors.as_ref(), &staging, &hashes, &job)
            .await;
        let _ = fs::remove_dir_all(&staging);
        merged
    }

    async fn merge_snapshot(
        &self,
        incoming: Snapshot,
        vectors: Option<&VectorCache>,
        staging: &Path,
        hashes: &[String],
        job: &Job,
    ) -> Result<MergeReport> {
        let mut report = MergeReport::default();
        job.phase("合并记忆", 0.4, 0.95);
        let current = self.snapshot().await?;
        let local: HashMap<&str, &Record> = current
            .memories
            .iter()
            .map(|r| (r.entry.id.as_str(), r))
            .collect();
        let known: HashSet<&str> = local
            .keys()
            .copied()
            .chain(incoming.memories.iter().map(|r| r.entry.id.as_str()))
            .collect();
        let mut keys: HashSet<(String, String)> = current
            .memories
            .iter()
            .filter_map(|r| {
                r.request
                    .as_ref()
                    .map(|k| (k.actor.clone(), k.request_id.clone()))
            })
            .collect();
        let mut upsert = Vec::new();
        let mut metadata_only = Vec::new();
        for theirs in &incoming.memories {
            let mut theirs = theirs.clone();
            theirs
                .entry
                .references
                .retain(|r| known.contains(r.id.as_str()));
            if theirs.entry.updated.is_empty() {
                theirs.entry.updated = theirs.entry.recorded.clone();
            }
            match local.get(theirs.entry.id.as_str()) {
                None => {
                    if let Some(k) = &theirs.request
                        && !keys.insert((k.actor.clone(), k.request_id.clone()))
                    {
                        theirs.request = None;
                    }
                    report.added += 1;
                    upsert.push(theirs);
                }
                Some(mine) => {
                    let merged = merge_record(mine, &theirs);
                    if merged == **mine {
                        report.unchanged += 1;
                    } else {
                        report.merged += 1;
                        if merged.entry.text == mine.entry.text
                            && merged.entry.context == mine.entry.context
                        {
                            metadata_only.push(merged);
                        } else {
                            upsert.push(merged);
                        }
                    }
                }
            }
        }
        let total = upsert.len() as u64;
        let rows = self
            .embed_records(upsert.clone(), vectors, |done| job.step(done as u64, total))
            .await?;
        job.phase("写入", 0.95, 1.0);
        let _guard = self.writes.lock().await;
        self.save_recovery_locked().await?;
        self.adopt_files(staging, hashes)?;
        // Entities and attachments first, memory rows last (the commit point).
        let mut entities: HashMap<String, Entity> = current
            .entities
            .iter()
            .map(|e| (e.id.clone(), e.clone()))
            .collect();
        let mut changed_entities = Vec::new();
        for theirs in incoming.entities {
            match entities.get(&theirs.id) {
                None => {
                    report.entities_added += 1;
                    changed_entities.push(theirs.clone());
                    entities.insert(theirs.id.clone(), theirs);
                }
                Some(mine) => {
                    let mut winner = if theirs.updated > mine.updated {
                        theirs.clone()
                    } else {
                        mine.clone()
                    };
                    let mut seen: HashSet<String> = std::iter::once(norm(&winner.name))
                        .chain(winner.aliases.iter().map(|a| norm(a)))
                        .collect();
                    for name in std::iter::once(&theirs.name)
                        .chain(theirs.aliases.iter())
                        .chain(std::iter::once(&mine.name))
                        .chain(mine.aliases.iter())
                    {
                        if seen.insert(norm(name)) {
                            winner.aliases.push(name.clone());
                        }
                    }
                    if winner != *mine {
                        changed_entities.push(winner.clone());
                        entities.insert(winner.id.clone(), winner);
                    }
                }
            }
        }
        kv::put(
            &self.etable,
            &changed_entities
                .iter()
                .map(|e| Ok((e.id.clone(), serde_json::to_string(e)?)))
                .collect::<Result<Vec<_>>>()?,
        )
        .await?;
        let have: HashMap<&str, &Attachment> = current
            .attachments
            .iter()
            .map(|a| (a.hash.as_str(), a))
            .collect();
        let mut new_files = Vec::new();
        for a in incoming.attachments {
            match have.get(a.hash.as_str()) {
                None => {
                    report.attachments_added += 1;
                    new_files.push(a);
                }
                Some(mine)
                    if mine.storage == Storage::Referenced && a.storage == Storage::Stored =>
                {
                    new_files.push(a)
                }
                _ => {}
            }
        }
        kv::put(
            &self.atable,
            &new_files
                .iter()
                .map(|a| Ok((a.hash.clone(), serde_json::to_string(a)?)))
                .collect::<Result<Vec<_>>>()?,
        )
        .await?;
        self.upsert_rows(&rows).await?;
        self.load_index().await?;
        for record in &metadata_only {
            self.rewrite_locked(record).await?;
        }
        self.invalidate().await;
        let idx = self.idx.read().await;
        let mut groups: HashMap<String, Vec<&Entity>> = HashMap::new();
        for e in idx.entities.values().filter(|e| e.merged_into.is_none()) {
            groups.entry(norm(&e.name)).or_default().push(e);
        }
        report.same_name_entities = groups
            .values()
            .filter(|g| g.len() > 1)
            .map(|g| g[0].name.clone())
            .collect();
        report.same_name_entities.sort();
        report.same_name_entities.truncate(30);
        Ok(report)
    }

    /// Delete managed files no memory and no rollback snapshot refers to. Returns (files, bytes).
    pub async fn gc_files(&self) -> Result<(usize, u64)> {
        let _guard = self.writes.lock().await;
        let mut keep: HashSet<String> = self
            .all()
            .await?
            .iter()
            .flat_map(|e| e.attachments.iter().map(|a| a.hash.clone()))
            .collect();
        if let Ok(snap) = self.load_recovery() {
            keep.extend(
                snap.memories
                    .iter()
                    .flat_map(|r| r.entry.attachments.iter().map(|a| a.hash.clone())),
            );
        }
        let unused: Vec<Attachment> = self
            .idx
            .read()
            .await
            .files
            .values()
            .filter(|a| !keep.contains(&a.hash))
            .cloned()
            .collect();
        let (mut files, mut bytes) = (0usize, 0u64);
        for a in &unused {
            if a.storage == Storage::Stored {
                let path = self.blob(&a.hash);
                if fs::remove_file(&path).is_ok() {
                    files += 1;
                    bytes += a.size;
                }
            }
            kv::delete(&self.atable, &a.hash).await?;
            self.idx.write().await.files.remove(&a.hash);
        }
        let _ = fs::remove_dir_all(self.tmp_dir());
        let _ = fs::remove_dir_all(self.dir.join("files").join(".incoming"));
        Ok((files, bytes))
    }

    /// Remove leftovers of interrupted exports and uploads.
    pub fn clean_temporary(&self) {
        let _ = fs::remove_dir_all(self.export_dir());
        let _ = fs::remove_dir_all(self.tmp_dir());
        let _ = fs::remove_dir_all(self.dir.join("files").join(".incoming"));
    }
}

/// Combine two versions of the same memory. The newer one wins; the other's text is kept in history.
fn merge_record(mine: &Record, theirs: &Record) -> Record {
    let key = |r: &Record| (r.entry.revision, r.entry.updated.clone());
    let (win, lose) = if key(theirs) > key(mine) {
        (theirs, mine)
    } else {
        (mine, theirs)
    };
    let mut out = win.clone();
    let seen = |out: &Record, text: &str| {
        out.entry.text == text || out.entry.history.iter().any(|h| h.text == text)
    };
    if !seen(&out, &lose.entry.text) {
        out.entry.history.push(Revision {
            revision: lose.entry.revision,
            text: lose.entry.text.clone(),
            context: lose.entry.context.clone(),
            occurred: lose.entry.occurred.clone(),
            replaced_at: now(),
            replaced_by: "合并导入".into(),
        });
    }
    for h in &lose.entry.history {
        if !seen(&out, &h.text) {
            out.entry.history.push(h.clone());
        }
    }
    out.entry.history.sort_by_key(|h| h.revision);
    for r in &lose.entry.references {
        if !out.entry.references.iter().any(|x| x.id == r.id) {
            out.entry.references.push(r.clone());
        }
    }
    for m in &lose.entry.mentions {
        if !out.entry.mentions.iter().any(|x| {
            (m.entity.is_some() && x.entity == m.entity)
                || (m.entity.is_none() && x.entity.is_none() && norm(&x.name) == norm(&m.name))
        }) {
            out.entry.mentions.push(m.clone());
        }
    }
    for a in &lose.entry.attachments {
        if !out.entry.attachments.iter().any(|x| x.hash == a.hash) {
            out.entry.attachments.push(a.clone());
        }
    }
    out.entry.updated = std::cmp::max(mine.entry.updated.clone(), theirs.entry.updated.clone());
    out
}
