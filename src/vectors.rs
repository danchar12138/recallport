//! Semantic vectors kept on disk next to the nodes, so starting the program, restoring a backup or importing an
//! older library does not run the model over every text again.
//!
//! A vector is only reused when the node id, the exact text it was made from and the model version all match;
//! anything else is encoded again. Values are stored as 16-bit floats (about three digits), which does not change
//! what a search finds. The file is a log: a node that changes simply gets a later record.
use crate::embedding;
use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

const MAGIC: &[u8; 8] = b"RPVLOG1\0";
const LEGACY_MAGIC: &[u8; 8] = b"RPVEC1\0\0";
const ID_BYTES: usize = 32;

pub fn to_half(x: f32) -> u16 {
    let bits = x.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let mantissa = bits & 0x007f_ffff;
    if exponent >= 31 {
        return sign | 0x7bff; // clamp; embeddings are unit length, so this never happens in practice
    }
    if exponent <= 0 {
        if exponent < -10 {
            return sign;
        }
        let m = (mantissa | 0x0080_0000) >> (1 - exponent);
        return sign | ((m + 0x1000) >> 13) as u16;
    }
    let half = sign | ((exponent as u16) << 10) | (mantissa >> 13) as u16;
    // Round to nearest.
    half + ((mantissa >> 12) & 1) as u16
}

pub fn from_half(h: u16) -> f32 {
    let sign = u32::from(h & 0x8000) << 16;
    let exponent = u32::from((h >> 10) & 0x1f);
    let mantissa = u32::from(h & 0x03ff);
    let bits = match exponent {
        0 if mantissa == 0 => sign,
        0 => {
            // Subnormal half: normalise.
            let shift = mantissa.leading_zeros() - 21;
            let m = (mantissa << shift) & 0x03ff;
            sign | ((113 - shift) << 23) | (m << 13)
        }
        31 => sign | 0x7f80_0000 | (mantissa << 13),
        _ => sign | ((exponent + 112) << 23) | (mantissa << 13),
    };
    f32::from_bits(bits)
}

/// What identifies the text a vector was made from.
pub fn key(text: &str) -> u64 {
    let digest = Sha256::digest(text.as_bytes());
    u64::from_le_bytes(digest[..8].try_into().expect("eight bytes"))
}

fn dims() -> usize {
    embedding::DIMENSIONS as usize
}

fn unit(raw: &[u8]) -> Option<Vec<f32>> {
    let mut out: Vec<f32> = raw
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| from_half(u16::from_le_bytes(*c)))
        .collect();
    let norm = out.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt();
    (out.len() == dims() && norm.is_finite() && norm > 0.0).then(|| {
        out.iter_mut().for_each(|x| *x /= norm as f32);
        out
    })
}

fn header() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(embedding::ID.len() as u16).to_le_bytes());
    out.extend_from_slice(embedding::ID.as_bytes());
    out.extend_from_slice(&(dims() as u32).to_le_bytes());
    out
}

fn record(id: &str, key: u64, vector: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(ID_BYTES + 8 + dims() * 2);
    let mut name = [b'0'; ID_BYTES];
    let bytes = id.as_bytes();
    name[..bytes.len().min(ID_BYTES)].copy_from_slice(&bytes[..bytes.len().min(ID_BYTES)]);
    out.extend_from_slice(&name);
    out.extend_from_slice(&key.to_le_bytes());
    for x in vector {
        out.extend_from_slice(&to_half(*x).to_le_bytes());
    }
    out
}

/// The vectors in a file, by node id: (what the text was, the vector). A file made by another model or one that
/// is unreadable is simply empty; a torn last record is ignored.
pub fn read(bytes: &[u8]) -> HashMap<String, (u64, Vec<f32>)> {
    let mut out = HashMap::new();
    let head = header();
    if bytes.len() < head.len() || bytes[..head.len()] != head[..] {
        return out;
    }
    let size = ID_BYTES + 8 + dims() * 2;
    for rec in bytes[head.len()..].chunks_exact(size) {
        let Ok(id) = std::str::from_utf8(&rec[..ID_BYTES]) else {
            continue;
        };
        let key = u64::from_le_bytes(rec[ID_BYTES..ID_BYTES + 8].try_into().expect("eight bytes"));
        if let Some(v) = unit(&rec[ID_BYTES + 8..]) {
            out.insert(id.to_owned(), (key, v));
        }
    }
    out
}

/// Vectors in the file format of the 0.x series (one record per memory, a count in the header), keyed by the
/// memory id and the text they were made from.
pub fn read_legacy(bytes: &[u8]) -> Result<HashMap<String, (u64, Vec<f32>)>> {
    let mut at = 0usize;
    let mut take = |n: usize| -> Result<&[u8]> {
        ensure!(at + n <= bytes.len(), "向量文件不完整");
        let slice = &bytes[at..at + n];
        at += n;
        Ok(slice)
    };
    ensure!(take(8)? == LEGACY_MAGIC, "不是向量文件");
    let len = u16::from_le_bytes(take(2)?.try_into()?) as usize;
    let saved = String::from_utf8_lossy(take(len)?).into_owned();
    let saved_dims = u32::from_le_bytes(take(4)?.try_into()?) as usize;
    let count = u32::from_le_bytes(take(4)?.try_into()?) as usize;
    let mut out = HashMap::new();
    if saved != embedding::ID || saved_dims != dims() {
        return Ok(out);
    }
    for _ in 0..count {
        let id = String::from_utf8(take(ID_BYTES)?.to_vec())?;
        let k = u64::from_le_bytes(take(8)?.try_into()?);
        let n = take(1)?[0] as usize;
        let mut first = None;
        for i in 0..n {
            let raw = take(dims() * 2)?;
            if i == 0 {
                first = unit(raw);
            }
        }
        if let Some(v) = first {
            out.insert(id, (k, v));
        }
    }
    Ok(out)
}

/// Appends records to the vector file.
pub struct Log {
    path: PathBuf,
    file: File,
}

impl Log {
    /// Open the file, starting a new one when it does not exist or was made by another model.
    pub fn open(path: &Path) -> Result<(Self, HashMap<String, (u64, Vec<f32>)>)> {
        let bytes = fs::read(path).unwrap_or_default();
        let known = read(&bytes);
        if known.is_empty() && !bytes.is_empty() {
            fs::write(path, header())?;
        } else if bytes.is_empty() {
            fs::write(path, header())?;
        } else {
            // cut a torn last record off, so later records line up again
            let size = ID_BYTES + 8 + dims() * 2;
            let whole = header().len() + (bytes.len() - header().len()) / size * size;
            if whole != bytes.len() {
                OpenOptions::new().write(true).open(path)?.set_len(whole as u64)?;
            }
        }
        let file = OpenOptions::new().append(true).open(path)?;
        Ok((Self { path: path.to_owned(), file }, known))
    }

    pub fn append(&mut self, id: &str, key: u64, vector: &[f32]) -> Result<()> {
        self.file.write_all(&record(id, key, vector))?;
        Ok(())
    }

    /// Start the file over with only these records (dropping every superseded or deleted one).
    pub fn compact(&mut self, keep: impl Iterator<Item = (String, u64, Vec<f32>)>) -> Result<()> {
        let mut bytes = header();
        for (id, k, v) in keep {
            bytes.extend_from_slice(&record(&id, k, &v));
        }
        let tmp = self.path.with_extension("tmp");
        fs::write(&tmp, &bytes)?;
        fs::rename(&tmp, &self.path)?;
        self.file = OpenOptions::new().append(true).open(&self.path)?;
        Ok(())
    }

    pub fn bytes(&self) -> Result<Vec<u8>> {
        Ok(fs::read(&self.path)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vector(seed: usize) -> Vec<f32> {
        let mut v = vec![0.0f32; dims()];
        v[seed % dims()] = 1.0;
        v
    }

    #[test]
    fn a_log_keeps_the_latest_record_and_ignores_a_torn_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vectors.bin");
        let (mut log, known) = Log::open(&path).unwrap();
        assert!(known.is_empty());
        let id = "a".repeat(32);
        log.append(&id, key("one"), &vector(1)).unwrap();
        log.append(&id, key("two"), &vector(2)).unwrap();
        drop(log);
        let mut bytes = fs::read(&path).unwrap();
        bytes.extend_from_slice(&[1, 2, 3]); // a torn record
        fs::write(&path, &bytes).unwrap();
        let (mut log, known) = Log::open(&path).unwrap();
        let (k, v) = &known[&id];
        assert_eq!(*k, key("two"));
        assert!(v[2] > 0.99 && v[1] < 0.01);
        // appending after a torn tail still lines up
        log.append(&"b".repeat(32), key("three"), &vector(3)).unwrap();
        drop(log);
        assert_eq!(read(&fs::read(&path).unwrap()).len(), 2);
    }

    #[test]
    fn half_floats_round_trip_closely() {
        for x in [0.0f32, 1.0, -1.0, 0.123, -0.0456, 0.9999] {
            assert!((from_half(to_half(x)) - x).abs() < 2e-3, "{x}");
        }
    }
}
