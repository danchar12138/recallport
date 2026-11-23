//! Attachments: content-addressed files in the data folder. Files are never put into MCP messages or node text:
//! the AI hands over a path (or a small inline snippet) and the server copies it in, or just remembers where it is.
use crate::tree::{Attachment, Storage};
use anyhow::{Context, Result, bail, ensure};
use base64::Engine;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub const MAX_ATTACHMENT: u64 = 4 << 30;
/// Inline data (`data_base64`) is for snippets; anything bigger is handed over by path.
pub const MAX_INLINE: usize = 8 << 20;

pub struct Files {
    dir: PathBuf,
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

/// Stream `source`, returning (sha256, size, first bytes); optionally copy it to `copy` on the way.
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

/// A file name that is safe to show and to put in a download header.
fn clean_name(name: &str) -> String {
    let base = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect::<String>();
    if base.is_empty() { "file".into() } else { base }
}

impl Files {
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_owned(),
        }
    }

    fn folder(&self) -> PathBuf {
        self.dir.join("files")
    }

    pub fn tmp_dir(&self) -> PathBuf {
        self.dir.join("tmp")
    }

    /// The managed copy of a file.
    pub fn blob(&self, hash: &str) -> PathBuf {
        self.folder().join(hash)
    }

    /// Where the bytes of an attachment are: the managed copy, or the original for a referenced file.
    pub fn path_of(&self, a: &Attachment) -> PathBuf {
        match a.storage {
            Storage::Stored => self.blob(&a.hash),
            Storage::Referenced => PathBuf::from(&a.path),
        }
    }

    pub fn available(&self, a: &Attachment) -> bool {
        self.path_of(a).is_file()
    }

    fn store(
        &self,
        source: &Path,
        name: &str,
        caption: &str,
        reference: bool,
    ) -> Result<Attachment> {
        fs::create_dir_all(self.folder())?;
        fs::create_dir_all(self.tmp_dir())?;
        let name = clean_name(name);
        let (hash, size, head, storage) = if reference {
            let (hash, size, head) = digest(source, None)?;
            (hash, size, head, Storage::Referenced)
        } else {
            let tmp = self
                .tmp_dir()
                .join(uuid::Uuid::new_v4().simple().to_string());
            let (hash, size, head) = match digest(source, Some(&tmp)) {
                Ok(done) => done,
                Err(error) => {
                    let _ = fs::remove_file(&tmp);
                    return Err(error);
                }
            };
            let target = self.blob(&hash);
            if target.is_file() {
                let _ = fs::remove_file(&tmp);
            } else {
                fs::rename(&tmp, &target)?;
            }
            (hash, size, head, Storage::Stored)
        };
        Ok(Attachment {
            mime: mime_of(&name, &head),
            hash,
            name,
            caption: caption.trim().chars().take(400).collect(),
            size,
            storage,
            path: if reference {
                source.to_string_lossy().into_owned()
            } else {
                String::new()
            },
        })
    }

    /// Take a file by its path: copied into the data folder, or (with `reference`) only remembered.
    pub fn ingest_path(
        &self,
        raw: &str,
        name: Option<&str>,
        caption: &str,
        reference: bool,
    ) -> Result<Attachment> {
        let path = check_path(raw)?;
        let name = name
            .filter(|n| !n.trim().is_empty())
            .map(str::to_owned)
            .or_else(|| path.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "file".into());
        self.store(&path, &name, caption, reference)
    }

    /// Take a small snippet handed over inline (base64).
    pub fn ingest_base64(&self, name: &str, data: &str, caption: &str) -> Result<Attachment> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data.trim())
            .context("data_base64 不是有效的 base64")?;
        self.ingest_bytes(name, &bytes, caption)
    }

    pub fn ingest_bytes(&self, name: &str, bytes: &[u8], caption: &str) -> Result<Attachment> {
        ensure!(
            bytes.len() <= MAX_INLINE,
            "内联数据最多 8 MiB，更大的文件请给出路径"
        );
        fs::create_dir_all(self.tmp_dir())?;
        let tmp = self
            .tmp_dir()
            .join(uuid::Uuid::new_v4().simple().to_string());
        fs::write(&tmp, bytes)?;
        let done = self.store(&tmp, name, caption, false);
        let _ = fs::remove_file(&tmp);
        done
    }

    /// Delete stored copies that no node mentions any more.
    pub fn sweep(&self, used: &HashSet<String>) {
        let Ok(list) = fs::read_dir(self.folder()) else {
            return;
        };
        for entry in list.flatten() {
            if !used.contains(entry.file_name().to_string_lossy().as_ref()) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }

    pub fn clean_temporary(&self) {
        let _ = fs::remove_dir_all(self.tmp_dir());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copied_file_is_stored_once_by_its_hash_and_a_referenced_one_stays_put() {
        let dir = tempfile::tempdir().unwrap();
        let files = Files::new(dir.path());
        let original = dir.path().join("note.txt");
        fs::write(&original, "hello").unwrap();
        let a = files
            .ingest_path(original.to_str().unwrap(), None, " 备忘 ", false)
            .unwrap();
        let b = files.ingest_bytes("same.txt", b"hello", "").unwrap();
        assert_eq!(a.hash, b.hash);
        assert_eq!(a.storage, Storage::Stored);
        assert_eq!(a.caption, "备忘");
        assert_eq!(a.mime, "text/plain");
        assert!(files.available(&a) && files.blob(&a.hash).is_file());
        let r = files
            .ingest_path(original.to_str().unwrap(), None, "", true)
            .unwrap();
        assert_eq!(r.storage, Storage::Referenced);
        assert_eq!(
            files.path_of(&r),
            check_path(original.to_str().unwrap()).unwrap()
        );
        fs::remove_file(&original).unwrap();
        assert!(!files.available(&r) && files.available(&a));
        // relative and network paths are refused
        assert!(files.ingest_path("a.png", None, "", false).is_err());
        assert!(
            files
                .ingest_path(r"\\server\share\a.png", None, "", false)
                .is_err()
        );
        // unused copies are swept away
        files.sweep(&HashSet::new());
        assert!(!files.available(&a));
    }
}
