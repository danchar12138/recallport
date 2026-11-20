//! Attachments: content-addressed files in the data directory, metadata and links in LanceDB.
//! Files are never put into MCP messages or the memory text; the AI hands over a path (or a small
//! inline snippet) and the server copies it in, or just remembers where it is.
use crate::{
    kv,
    memory::*,
    store::{Store, now},
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use base64::Engine;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub const MAX_PER_MEMORY: usize = 64;

#[derive(Clone, Debug, Serialize)]
pub struct AttachmentView {
    pub hash: String,
    pub name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub caption: String,
    pub mime: String,
    pub size: u64,
    pub storage: Storage,
    /// False when the managed copy or the referenced original is missing.
    pub available: bool,
    /// Where the file is on this computer (managed copy, or the original for referenced files).
    pub path: String,
    /// Web address for viewing or downloading.
    pub url: String,
}

#[derive(Default, Clone, Copy, Debug, Serialize)]
pub struct FileStats {
    pub stored: usize,
    pub referenced: usize,
    pub bytes: u64,
}

pub fn mime_of(name: &str, head: &[u8]) -> String {
    if let Some(m) = mime_guess::from_path(name).first_raw() {
        return m.to_owned();
    }
    let sniff = if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if head.starts_with(b"GIF8") {
        "image/gif"
    } else if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        "image/webp"
    } else if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WAVE" {
        "audio/wav"
    } else if head.starts_with(b"%PDF") {
        "application/pdf"
    } else if head.len() >= 12 && &head[4..8] == b"ftyp" {
        "video/mp4"
    } else if head.starts_with(b"OggS") {
        "audio/ogg"
    } else if head.starts_with(b"ID3") {
        "audio/mpeg"
    } else if head.starts_with(b"PK\x03\x04") {
        "application/zip"
    } else {
        "application/octet-stream"
    };
    sniff.to_owned()
}

/// Accept only absolute paths to ordinary local files. Network shares and device paths are refused
/// because merely opening them can send credentials to another machine.
fn check_path(raw: &str) -> Result<PathBuf> {
    ensure!(!raw.contains('\0'), "路径无效");
    let path = Path::new(raw);
    ensure!(
        path.is_absolute(),
        "附件的 path 必须是绝对路径（例如 C:/Users/me/a.png），收到：{raw}"
    );
    ensure!(
        !raw.replace('/', "\\").starts_with("\\\\"),
        "不支持网络共享（UNC）或设备路径"
    );
    let canon = fs::canonicalize(path).with_context(|| format!("无法读取文件：{raw}"))?;
    let text = canon.to_string_lossy().into_owned();
    let canon = match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.starts_with("UNC\\") => bail!("不支持网络共享（UNC）路径"),
        Some(rest) => PathBuf::from(rest),
        None => canon,
    };
    ensure!(
        fs::metadata(&canon)?.is_file(),
        "只能收纳普通文件，不能是文件夹或设备"
    );
    Ok(canon)
}

/// Stream `source`, returning (sha256, size, first bytes); optionally copy into `copy`.
fn digest(source: &Path, copy: Option<&Path>) -> Result<(String, u64, Vec<u8>)> {
    let mut input =
        fs::File::open(source).with_context(|| format!("无法打开 {}", source.display()))?;
    let mut output = match copy {
        Some(p) => Some(fs::File::create(p)?),
        None => None,
    };
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    let mut size = 0u64;
    let mut head = Vec::new();
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        if head.len() < 16 {
            head.extend_from_slice(&buffer[..n.min(16 - head.len())]);
        }
        size += n as u64;
        ensure!(size <= MAX_ATTACHMENT, "附件超过 4 GiB 上限");
        hasher.update(&buffer[..n]);
        if let Some(out) = output.as_mut() {
            out.write_all(&buffer[..n])?;
        }
    }
    if let Some(out) = output.as_mut() {
        out.flush()?;
        out.sync_all()?;
    }
    Ok((format!("{:x}", hasher.finalize()), size, head))
}

impl Store {
    pub fn blob(&self, hash: &str) -> PathBuf {
        self.dir.join("files").join(&hash[..2]).join(hash)
    }

    pub fn tmp_dir(&self) -> PathBuf {
        self.dir.join("files").join(".tmp")
    }

    pub async fn attachment(&self, hash: &str) -> Option<Attachment> {
        self.idx.read().await.files.get(hash).cloned()
    }

    pub fn attachment_path(&self, a: &Attachment) -> PathBuf {
        match a.storage {
            Storage::Stored => self.blob(&a.hash),
            Storage::Referenced => PathBuf::from(&a.path),
        }
    }

    pub async fn file_stats(&self) -> FileStats {
        let idx = self.idx.read().await;
        let mut stats = FileStats::default();
        for a in idx.files.values() {
            match a.storage {
                Storage::Stored => {
                    stats.stored += 1;
                    stats.bytes += a.size;
                }
                Storage::Referenced => stats.referenced += 1,
            }
        }
        stats
    }

    pub async fn attachment_views(&self, refs: &[AttachmentRef]) -> Vec<AttachmentView> {
        let idx = self.idx.read().await;
        refs.iter()
            .map(|r| match idx.files.get(&r.hash) {
                Some(a) => {
                    let path = self.attachment_path(a);
                    AttachmentView {
                        hash: r.hash.clone(),
                        name: if r.name.is_empty() {
                            a.name.clone()
                        } else {
                            r.name.clone()
                        },
                        caption: r.caption.clone(),
                        mime: a.mime.clone(),
                        size: a.size,
                        storage: a.storage,
                        available: path.is_file(),
                        path: path.to_string_lossy().into_owned(),
                        url: format!("/files/{}", a.hash),
                    }
                }
                None => AttachmentView {
                    hash: r.hash.clone(),
                    name: r.name.clone(),
                    caption: r.caption.clone(),
                    mime: "application/octet-stream".into(),
                    size: 0,
                    storage: Storage::Stored,
                    available: false,
                    path: String::new(),
                    url: String::new(),
                },
            })
            .collect()
    }

    /// Bring the files in. Safe to run before taking the write lock: the files are content-addressed.
    pub(crate) async fn ingest(
        &self,
        inputs: &[AttachInput],
        source: &str,
    ) -> Result<Vec<AttachmentRef>> {
        let mut refs = Vec::new();
        for input in inputs {
            let r = self.ingest_one(input, source).await?;
            if !refs.iter().any(|x: &AttachmentRef| x.hash == r.hash) {
                refs.push(r);
            }
        }
        Ok(refs)
    }

    async fn ingest_one(&self, input: &AttachInput, source: &str) -> Result<AttachmentRef> {
        if !input.hash.is_empty() {
            let known = self
                .attachment(&input.hash)
                .await
                .ok_or_else(|| anyhow!("附件不存在：{}", input.hash))?;
            return Ok(AttachmentRef {
                hash: known.hash,
                name: if input.name.is_empty() {
                    known.name
                } else {
                    input.name.clone()
                },
                caption: input.caption.clone(),
            });
        }
        let tmp_dir = self.tmp_dir();
        fs::create_dir_all(&tmp_dir)?;
        let tmp = tmp_dir.join(format!("{}.part", uuid::Uuid::new_v4().simple()));
        let reference = input.mode == AttachMode::Reference && input.data_base64.is_empty();
        let (display, original, copy) = if input.data_base64.is_empty() {
            let path = check_path(&input.path)?;
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "file".into());
            (name, Some(path), !reference)
        } else {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(input.data_base64.trim())
                .context("data_base64 不是有效的 base64")?;
            ensure!(
                bytes.len() <= MAX_INLINE,
                "附件的 data_base64 超过约 96 KB，请改用 path（绝对路径）"
            );
            fs::write(&tmp, &bytes)?;
            (String::new(), None, true)
        };
        let target = tmp.clone();
        let scan = tokio::task::spawn_blocking(move || -> Result<(String, u64, Vec<u8>)> {
            match (&original, copy) {
                (Some(path), true) => digest(path, Some(&target)),
                (Some(path), false) => digest(path, None),
                (None, _) => digest(&target, None),
            }
        })
        .await?;
        let (hash, size, head) = match scan {
            Ok(v) => v,
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                return Err(e);
            }
        };
        let name = if !input.name.is_empty() {
            input.name.clone()
        } else if !display.is_empty() {
            display
        } else {
            format!("file-{}", &hash[..8])
        };
        let mime = mime_of(&name, &head);
        let existing = self.attachment(&hash).await;
        let keep_stored = existing
            .as_ref()
            .is_some_and(|a| a.storage == Storage::Stored);
        let record = if reference && !keep_stored {
            let _ = fs::remove_file(&tmp);
            Attachment {
                hash: hash.clone(),
                name: name.clone(),
                mime,
                size,
                storage: Storage::Referenced,
                path: check_path(&input.path)?.to_string_lossy().into_owned(),
                added: now(),
                source: source.into(),
            }
        } else {
            let blob = self.blob(&hash);
            if blob.exists() {
                let _ = fs::remove_file(&tmp);
            } else {
                fs::create_dir_all(blob.parent().context("blob path")?)?;
                fs::rename(&tmp, &blob)?;
            }
            Attachment {
                hash: hash.clone(),
                name: name.clone(),
                mime,
                size,
                storage: Storage::Stored,
                path: String::new(),
                added: existing
                    .as_ref()
                    .map(|a| a.added.clone())
                    .unwrap_or_else(now),
                source: existing.map(|a| a.source).unwrap_or_else(|| source.into()),
            }
        };
        kv::put(
            &self.atable,
            &[(hash.clone(), serde_json::to_string(&record)?)],
        )
        .await?;
        self.idx.write().await.files.insert(hash.clone(), record);
        Ok(AttachmentRef {
            hash,
            name,
            caption: input.caption.clone(),
        })
    }

    pub async fn link_attachment(
        &self,
        memory_id: &str,
        input: AttachInput,
        source: &str,
    ) -> Result<AttachmentRef> {
        let mut input = input;
        input.clean()?;
        let attached = self
            .ingest(std::slice::from_ref(&input), source)
            .await?
            .into_iter()
            .next()
            .context("附件处理失败")?;
        let _guard = self.writes.lock().await;
        let mut record = self.find(memory_id).await?;
        record.entry.attachments.retain(|a| a.hash != attached.hash);
        ensure!(
            record.entry.attachments.len() < MAX_PER_MEMORY,
            "一条记忆最多 64 个附件"
        );
        record.entry.attachments.push(attached.clone());
        record.entry.updated = now();
        self.rewrite_locked(&record).await?;
        Ok(attached)
    }

    pub async fn unlink_attachment(&self, memory_id: &str, hash: &str) -> Result<()> {
        let _guard = self.writes.lock().await;
        let mut record = self.find(memory_id).await?;
        let before = record.entry.attachments.len();
        record.entry.attachments.retain(|a| a.hash != hash);
        ensure!(
            record.entry.attachments.len() < before,
            "这条记忆没有该附件"
        );
        record.entry.updated = now();
        self.rewrite_locked(&record).await
    }
}
