//! Semantic vectors that already exist, kept next to a backup or a rollback snapshot so that restoring
//! does not have to run the model over every memory again.
//!
//! A vector is only reused when the memory id, the exact text that was embedded and the model version all
//! match; anything else is encoded again. Values are stored as 16-bit floats (about three digits), which
//! does not change what a search finds.
use crate::embedding;
use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

const MAGIC: &[u8; 8] = b"RPVEC1\0\0";

/// The text a memory is embedded from.
pub fn embed_text(text: &str, context: &str) -> String {
    if context.is_empty() {
        text.to_owned()
    } else {
        format!("{text}\n{context}")
    }
}

fn key(text: &str) -> u64 {
    let digest = Sha256::digest(text.as_bytes());
    u64::from_le_bytes(digest[..8].try_into().expect("eight bytes"))
}

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

#[derive(Default)]
pub struct VectorCache {
    entries: HashMap<String, (u64, Vec<Vec<u16>>)>,
}

impl VectorCache {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn insert(&mut self, id: &str, text: &str, vectors: &[Vec<f32>]) {
        let half = vectors
            .iter()
            .map(|v| v.iter().map(|&x| to_half(x)).collect())
            .collect();
        self.entries.insert(id.to_owned(), (key(text), half));
    }

    /// The saved passages for this memory, if they were made from exactly this text.
    pub fn get(&self, id: &str, text: &str) -> Option<Vec<Vec<f32>>> {
        let (k, vectors) = self.entries.get(id)?;
        if *k != key(text) {
            return None;
        }
        let dims = embedding::DIMENSIONS as usize;
        vectors
            .iter()
            .map(|v| {
                let mut out: Vec<f32> = v.iter().map(|&h| from_half(h)).collect();
                let norm = out
                    .iter()
                    .map(|x| f64::from(*x).powi(2))
                    .sum::<f64>()
                    .sqrt();
                (out.len() == dims && norm.is_finite() && norm > 0.0).then(|| {
                    out.iter_mut().for_each(|x| *x /= norm as f32);
                    out
                })
            })
            .collect()
    }

    pub fn encode(&self, version: &str) -> Vec<u8> {
        let dims = embedding::DIMENSIONS as usize;
        let mut out = Vec::with_capacity(64 + self.entries.len() * (48 + dims * 2));
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(version.len() as u16).to_le_bytes());
        out.extend_from_slice(version.as_bytes());
        out.extend_from_slice(&(dims as u32).to_le_bytes());
        out.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        let mut ids: Vec<_> = self.entries.keys().collect();
        ids.sort();
        for id in ids {
            let (k, vectors) = &self.entries[id];
            out.extend_from_slice(id.as_bytes());
            out.extend_from_slice(&k.to_le_bytes());
            out.push(vectors.len() as u8);
            for v in vectors {
                for h in v {
                    out.extend_from_slice(&h.to_le_bytes());
                }
            }
        }
        out
    }

    /// Read a saved set. A set made by a different model is ignored (empty), not an error.
    pub fn decode(bytes: &[u8], version: &str) -> Result<Self> {
        let mut at = 0usize;
        let mut take = |n: usize| -> Result<&[u8]> {
            ensure!(at + n <= bytes.len(), "向量文件不完整");
            let slice = &bytes[at..at + n];
            at += n;
            Ok(slice)
        };
        ensure!(take(8)? == MAGIC, "不是向量文件");
        let len = u16::from_le_bytes(take(2)?.try_into()?) as usize;
        let saved = String::from_utf8_lossy(take(len)?).into_owned();
        let dims = u32::from_le_bytes(take(4)?.try_into()?) as usize;
        let count = u32::from_le_bytes(take(4)?.try_into()?) as usize;
        if saved != version || dims != embedding::DIMENSIONS as usize {
            return Ok(Self::default());
        }
        let mut entries = HashMap::with_capacity(count.min(1 << 20));
        for _ in 0..count {
            let id = String::from_utf8(take(32)?.to_vec())?;
            let k = u64::from_le_bytes(take(8)?.try_into()?);
            let n = take(1)?[0] as usize;
            let mut vectors = Vec::with_capacity(n);
            for _ in 0..n {
                let raw = take(dims * 2)?;
                vectors.push(
                    raw.as_chunks::<2>()
                        .0
                        .iter()
                        .map(|c| u16::from_le_bytes(*c))
                        .collect::<Vec<_>>(),
                );
            }
            entries.insert(id, (k, vectors));
        }
        Ok(Self { entries })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_floats_round_trip_closely() {
        for x in [0.0f32, 1.0, -1.0, 0.5, 0.0123, -0.0456, 0.9999, 1e-6, -0.25] {
            let back = from_half(to_half(x));
            assert!((back - x).abs() <= x.abs() * 1e-3 + 1e-6, "{x} -> {back}");
        }
    }

    #[test]
    fn cache_survives_a_round_trip_and_notices_changed_text() {
        let mut v = vec![0.0f32; embedding::DIMENSIONS as usize];
        v[3] = 0.6;
        v[100] = 0.8;
        let mut cache = VectorCache::default();
        let id = "0123456789abcdef0123456789abcdef";
        cache.insert(id, "hello", &[v.clone(), v.clone()]);
        let bytes = cache.encode("v1");
        let back = VectorCache::decode(&bytes, "v1").unwrap();
        let got = back.get(id, "hello").unwrap();
        assert_eq!(got.len(), 2);
        assert!((got[0][3] - 0.6).abs() < 1e-3 && (got[0][100] - 0.8).abs() < 1e-3);
        assert!(
            back.get(id, "hello!").is_none(),
            "other text must be re-encoded"
        );
        assert!(
            VectorCache::decode(&bytes, "v2").unwrap().is_empty(),
            "another model's vectors are ignored"
        );
        assert!(VectorCache::decode(&bytes[..bytes.len() - 5], "v1").is_err());
    }
}
