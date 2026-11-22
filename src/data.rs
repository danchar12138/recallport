//! Backups and moving libraries around: the portable ZIP, restoring it (replacing the library or merging into it),
//! reading an older version's backup, and one automatic backup a day.
//!
//! A ZIP holds `manifest.json`, `nodes.jsonl` (the whole history of every node), `vectors.bin` (so the model does
//! not have to run again) and `files/<sha256>` for the stored attachments. Entries are only ever read by their
//! fixed names, never extracted by the path they claim.
use crate::{
    jobs::Job,
    legacy,
    service::Service,
    tree::{Event, MergeReport, Storage},
    vectors,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

pub const FORMAT: &str = "recallport-tree";
pub const VERSION: u32 = 1;
/// How many automatic backups stay on disk, and how many "before a restore" copies.
pub const KEEP_AUTO: usize = 7;
const KEEP_BEFORE: usize = 3;
const EVERY_HOURS: i64 = 22;
const MAX_TEXT: u64 = 1 << 30;
const MAX_ENTRIES: usize = 500_000;

#[derive(Serialize, Deserialize)]
struct Manifest {
    format: String,
    version: u32,
    app: String,
    created: String,
    nodes: usize,
    /// False for automatic backups: they list the attachments but do not copy the files, which stay in the data
    /// folder beside them.
    files: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The library becomes exactly the backup (a copy of the old one is kept first).
    Replace,
    /// What the backup knows is added to the library.
    Merge,
}

#[derive(Clone, Debug, Serialize)]
pub struct ImportReport {
    pub nodes: usize,
    pub mode: &'static str,
    /// The backup came from an older version and was converted.
    pub legacy: bool,
    pub merge: Option<MergeReport>,
    pub missing_files: usize,
    /// The copy of the library taken before it was replaced.
    pub kept_as: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BackupFile {
    pub name: String,
    /// auto, manual or before-restore.
    pub kind: &'static str,
    pub bytes: u64,
    pub at: String,
}

#[derive(Default, Serialize, Deserialize)]
struct Settings {
    #[serde(default)]
    auto_backup_off: bool,
    /// When the last automatic backup finished, and what the library looked like then.
    #[serde(default)]
    auto_at: String,
    #[serde(default)]
    auto_signature: String,
}

/// `RecallPort-<kind>-YYYYMMDD-HHMM.zip`, the only names the backups folder serves or deletes.
pub fn valid_name(name: &str) -> bool {
    let Some(rest) = name
        .strip_prefix("RecallPort-")
        .and_then(|r| r.strip_suffix(".zip"))
    else {
        return false;
    };
    let Some((kind, stamp)) = rest.rsplit_once('-').and_then(|(head, time)| {
        head.rsplit_once('-')
            .map(|(kind, day)| (kind, format!("{day}-{time}")))
    }) else {
        return false;
    };
    matches!(kind, "auto" | "backup" | "before-restore")
        && stamp.len() == 13
        && stamp.bytes().enumerate().all(|(i, b)| {
            if i == 8 {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
}

fn kind_of(name: &str) -> &'static str {
    if name.starts_with("RecallPort-auto-") {
        "auto"
    } else if name.starts_with("RecallPort-before-restore-") {
        "before-restore"
    } else {
        "manual"
    }
}

fn options(method: CompressionMethod) -> SimpleFileOptions {
    SimpleFileOptions::default()
        .compression_method(method)
        .large_file(true)
}

/// What a backup file holds once read: the history, vectors that came along, and the stored files it carries.
struct Loaded {
    events: Vec<Event>,
    vectors: HashMap<String, (u64, Vec<f32>)>,
    /// Hashes of stored attachments the nodes need.
    wanted: HashSet<String>,
    legacy: bool,
}

fn read_entry(zip: &mut ZipArchive<BufReader<File>>, name: &str, limit: u64) -> Result<Vec<u8>> {
    let entry = zip
        .by_name(name)
        .with_context(|| format!("备份缺少 {name}"))?;
    ensure!(entry.size() <= limit, "备份里的 {name} 太大");
    let mut out = Vec::with_capacity(entry.size() as usize);
    entry.take(limit).read_to_end(&mut out)?;
    Ok(out)
}

fn load(path: &Path) -> Result<(Loaded, ZipArchive<BufReader<File>>)> {
    let mut zip =
        ZipArchive::new(BufReader::new(File::open(path)?)).context("不是有效的 ZIP 文件")?;
    ensure!(zip.len() <= MAX_ENTRIES, "备份里的文件太多");
    let manifest_bytes = read_entry(&mut zip, "manifest.json", MAX_TEXT)
        .context("不是 RecallPort 备份（没有 manifest.json）")?;
    let head: serde_json::Value =
        serde_json::from_slice(&manifest_bytes).context("manifest.json 无法解析")?;
    let format = head
        .get("format")
        .and_then(|f| f.as_str())
        .unwrap_or_default();
    if format == legacy::FORMAT {
        let archive: legacy::Archive = serde_json::from_value(head)?;
        let vectors = zip
            .by_name("vectors.bin")
            .ok()
            .and_then(|mut e| {
                let mut bytes = vec![];
                e.read_to_end(&mut bytes).ok().map(|_| bytes)
            })
            .and_then(|b| vectors::read_legacy(&b).ok())
            .unwrap_or_default();
        let loaded = Loaded {
            events: archive.events(),
            vectors,
            wanted: archive.stored_files(),
            legacy: true,
        };
        return Ok((loaded, zip));
    }
    let manifest: Manifest = serde_json::from_value(head).context("manifest.json 不符合格式")?;
    ensure!(
        manifest.format == FORMAT && manifest.version == VERSION,
        "这个备份的版本（{} {}）当前程序不能读取",
        manifest.format,
        manifest.version
    );
    let log = read_entry(&mut zip, "nodes.jsonl", MAX_TEXT)?;
    let mut events = vec![];
    for (n, line) in String::from_utf8(log)?.lines().enumerate() {
        if !line.trim().is_empty() {
            events.push(
                serde_json::from_str::<Event>(line)
                    .with_context(|| format!("备份的 nodes.jsonl 第 {} 行无法解析", n + 1))?,
            );
        }
    }
    let vectors = zip
        .by_name("vectors.bin")
        .ok()
        .map(|mut e| {
            let mut bytes = vec![];
            let _ = e.read_to_end(&mut bytes);
            vectors::read(&bytes)
        })
        .unwrap_or_default();
    let wanted = events
        .iter()
        .flat_map(|e| e.node.attachments.iter())
        .filter(|a| a.storage == Storage::Stored)
        .map(|a| a.hash.clone())
        .collect();
    Ok((
        Loaded {
            events,
            vectors,
            wanted,
            legacy: false,
        },
        zip,
    ))
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

impl Service {
    pub fn backup_dir(&self) -> PathBuf {
        self.dir.join("backups")
    }

    /// A file in the backups folder, by a name from `backups()`.
    pub fn backup_path(&self, name: &str) -> Option<PathBuf> {
        valid_name(name)
            .then(|| self.backup_dir().join(name))
            .filter(|p| p.is_file())
    }

    pub fn backups(&self) -> Vec<BackupFile> {
        let mut out: Vec<BackupFile> = fs::read_dir(self.backup_dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let meta = e.metadata().ok()?;
                (valid_name(&name) && meta.is_file()).then(|| BackupFile {
                    kind: kind_of(&name),
                    bytes: meta.len(),
                    at: meta
                        .modified()
                        .ok()
                        .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339())
                        .unwrap_or_default(),
                    name,
                })
            })
            .collect();
        out.sort_by(|a, b| b.name.cmp(&a.name));
        out
    }

    pub fn delete_backup(&self, name: &str) -> Result<()> {
        let path = self.backup_path(name).context("没有这个备份")?;
        fs::remove_file(path)?;
        Ok(())
    }

    fn stamp(&self, kind: &str) -> String {
        let mut n = chrono::Local::now();
        loop {
            let name = format!("RecallPort-{kind}-{}.zip", n.format("%Y%m%d-%H%M"));
            if !self.backup_dir().join(&name).exists() {
                return name;
            }
            n += chrono::Duration::minutes(1);
        }
    }

    /// Write the library as a ZIP. `files` false lists the attachments but leaves their bytes out.
    fn write_zip(&self, dest: &Path, files: bool, job: &Job) -> Result<usize> {
        let (events, vector_bytes) = self.export_events();
        let nodes = events
            .iter()
            .map(|e| &e.node.id)
            .collect::<HashSet<_>>()
            .len();
        let tmp = dest.with_extension("part");
        let result = (|| -> Result<()> {
            let mut zip = ZipWriter::new(BufWriter::new(File::create(&tmp)?));
            job.phase("写入记忆", 0.0, 0.3);
            zip.start_file("manifest.json", options(CompressionMethod::Deflated))?;
            serde_json::to_writer(
                &mut zip,
                &Manifest {
                    format: FORMAT.into(),
                    version: VERSION,
                    app: env!("CARGO_PKG_VERSION").into(),
                    created: crate::tree::now(),
                    nodes,
                    files,
                },
            )?;
            zip.start_file("nodes.jsonl", options(CompressionMethod::Deflated))?;
            for (i, e) in events.iter().enumerate() {
                writeln!(zip, "{}", serde_json::to_string(e)?)?;
                if i % 256 == 0 {
                    job.step(i as u64, events.len() as u64);
                }
            }
            if !vector_bytes.is_empty() {
                zip.start_file("vectors.bin", options(CompressionMethod::Deflated))?;
                zip.write_all(&vector_bytes)?;
            }
            if files {
                let used: Vec<String> = {
                    let mut seen = HashSet::new();
                    events
                        .iter()
                        .flat_map(|e| e.node.attachments.iter())
                        .filter(|a| a.storage == Storage::Stored && seen.insert(a.hash.clone()))
                        .map(|a| a.hash.clone())
                        .collect()
                };
                job.phase("复制附件", 0.3, 1.0);
                for (i, hash) in used.iter().enumerate() {
                    let blob = self.files.blob(hash);
                    if !blob.is_file() {
                        continue;
                    }
                    // photos, audio and video are already compressed
                    zip.start_file(format!("files/{hash}"), options(CompressionMethod::Stored))?;
                    std::io::copy(&mut File::open(blob)?, &mut zip)?;
                    job.step(i as u64 + 1, used.len() as u64);
                }
            }
            zip.finish()?.flush()?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                fs::rename(&tmp, dest)?;
                Ok(nodes)
            }
            Err(error) => {
                let _ = fs::remove_file(&tmp);
                Err(error)
            }
        }
    }

    /// A full backup (attachments included) into the backups folder; returns its name.
    pub fn make_backup(&self, job: &Job) -> Result<String> {
        fs::create_dir_all(self.backup_dir())?;
        let name = self.stamp("backup");
        self.write_zip(&self.backup_dir().join(&name), true, job)?;
        Ok(name)
    }

    /// Restore from a file: the library becomes the backup, or the backup is merged into it.
    pub fn import_archive(&self, path: &Path, mode: Mode, job: &Job) -> Result<ImportReport> {
        job.phase("读取备份", 0.0, 0.2);
        let (loaded, mut zip) = load(path)?;
        let nodes = loaded
            .events
            .iter()
            .map(|e| &e.node.id)
            .collect::<HashSet<_>>()
            .len();
        // the backup's files first: they are only put in place when they are really what their name says
        job.phase("复制附件", 0.2, 0.5);
        let mut missing = 0;
        let wanted: Vec<&String> = loaded.wanted.iter().collect();
        fs::create_dir_all(self.files.tmp_dir())?;
        for (i, hash) in wanted.iter().enumerate() {
            job.step(i as u64 + 1, wanted.len() as u64);
            if !valid_hash(hash) || self.files.blob(hash).is_file() {
                continue;
            }
            let Ok(entry) = zip.by_name(&format!("files/{hash}")) else {
                missing += 1;
                continue;
            };
            fs::create_dir_all(self.files.blob(hash).parent().context("附件目录")?)?;
            let tmp = self
                .files
                .tmp_dir()
                .join(uuid::Uuid::new_v4().simple().to_string());
            let mut out = File::create(&tmp)?;
            let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
            let mut limited = entry.take(crate::files::MAX_ATTACHMENT);
            let mut buffer = vec![0u8; 1 << 20];
            loop {
                let n = limited.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                sha2::Digest::update(&mut hasher, &buffer[..n]);
                out.write_all(&buffer[..n])?;
            }
            out.sync_all()?;
            if format!("{:x}", sha2::Digest::finalize(hasher)) == **hash {
                fs::rename(&tmp, self.files.blob(hash))?;
            } else {
                let _ = fs::remove_file(&tmp);
                missing += 1;
            }
        }
        let mut kept_as = None;
        if mode == Mode::Replace && self.read().len() + self.read().trash().len() > 0 {
            job.phase("保存当前记忆库的副本", 0.5, 0.6);
            fs::create_dir_all(self.backup_dir())?;
            let name = self.stamp("before-restore");
            self.write_zip(&self.backup_dir().join(&name), true, job)?;
            self.trim("before-restore", KEEP_BEFORE);
            kept_as = Some(name);
        }
        job.phase("建立索引", 0.6, 1.0);
        let merge = match mode {
            Mode::Replace => {
                self.write_lock().replace(loaded.events, &loaded.vectors)?;
                None
            }
            Mode::Merge => Some(self.write_lock().merge(loaded.events, &loaded.vectors)?),
        };
        let used = self.read().used_files();
        if mode == Mode::Replace {
            self.files.sweep(&used);
        }
        self.files.clean_temporary();
        Ok(ImportReport {
            nodes,
            mode: if mode == Mode::Replace {
                "replace"
            } else {
                "merge"
            },
            legacy: loaded.legacy,
            merge,
            missing_files: missing,
            kept_as,
        })
    }

    /// Keep only the newest `keep` backups of a kind.
    fn trim(&self, kind: &str, keep: usize) {
        for old in self
            .backups()
            .into_iter()
            .filter(|b| b.kind == kind)
            .skip(keep)
        {
            let _ = fs::remove_file(self.backup_dir().join(old.name));
        }
    }

    // ---------- settings and the daily backup ----------

    fn settings_path(&self) -> PathBuf {
        self.dir.join("settings.json")
    }

    fn settings(&self) -> Settings {
        fs::read(self.settings_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save_settings(&self, s: &Settings) -> Result<()> {
        let tmp = self.dir.join("settings.json.tmp");
        fs::write(&tmp, serde_json::to_vec(s)?)?;
        fs::rename(tmp, self.settings_path())?;
        Ok(())
    }

    pub fn auto_backup_enabled(&self) -> bool {
        !self.settings().auto_backup_off
    }

    pub fn set_auto_backup(&self, on: bool) -> Result<()> {
        let mut s = self.settings();
        s.auto_backup_off = !on;
        self.save_settings(&s)
    }

    /// One automatic backup a day, only when something changed since the last one.
    pub fn auto_backup(&self) -> Result<Option<String>> {
        let mut s = self.settings();
        if s.auto_backup_off {
            return Ok(None);
        }
        let signature = {
            let t = self.read();
            format!("{}|{}", t.len() + t.trash().len(), t.last_change())
        };
        if signature.starts_with("0|") || signature == s.auto_signature {
            return Ok(None);
        }
        if let Ok(last) = chrono::DateTime::parse_from_rfc3339(&s.auto_at)
            && chrono::Utc::now().signed_duration_since(last) < chrono::Duration::hours(EVERY_HOURS)
        {
            return Ok(None);
        }
        fs::create_dir_all(self.backup_dir())?;
        let name = self.stamp("auto");
        self.write_zip(&self.backup_dir().join(&name), false, &Job::detached())?;
        self.trim("auto", KEEP_AUTO);
        s.auto_at = chrono::Utc::now().to_rfc3339();
        s.auto_signature = signature;
        self.save_settings(&s)?;
        Ok(Some(name))
    }
}
