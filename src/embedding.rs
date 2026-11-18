use anyhow::{Context, Result, ensure};
use ort::{session::Session, value::Tensor};
use std::{path::Path, sync::Mutex};
use tokenizers::{Tokenizer, TruncationParams};

pub const ID: &str = "granite-embedding-97m-multilingual-r2/835ad140/fp32/cls-v1";
pub const DIMENSIONS: i32 = 384;
pub const TOKENS: usize = 512;
pub const THREADS: usize = 4;

pub trait Embed: Send + Sync {
    fn encode(&self, text: &str) -> Result<Vec<f32>>;
}

pub struct Granite {
    tokenizer: Tokenizer,
    session: Mutex<Session>,
}

impl Granite {
    pub fn open(dir: &Path) -> Result<Self> {
        let dll = dir.join("onnxruntime.dll");
        let _ = ort::init_from(dll)?.commit();
        let mut tokenizer =
            Tokenizer::from_file(dir.join("tokenizer.json")).map_err(anyhow::Error::msg)?;
        tokenizer.with_padding(None);
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: TOKENS,
                ..Default::default()
            }))
            .map_err(anyhow::Error::msg)?;
        let session = Session::builder()
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .with_intra_threads(THREADS)
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .with_inter_threads(1)
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .commit_from_file(dir.join("model.onnx"))?;
        let model = Self {
            tokenizer,
            session: Mutex::new(session),
        };
        model.encode("RecallPort")?;
        Ok(model)
    }
}

impl Embed for Granite {
    fn encode(&self, text: &str) -> Result<Vec<f32>> {
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(anyhow::Error::msg)?;
        let ids: Vec<_> = encoding.get_ids().iter().map(|&v| i64::from(v)).collect();
        let len = ids.len();
        let masks: Vec<_> = encoding
            .get_attention_mask()
            .iter()
            .map(|&v| i64::from(v))
            .collect();
        let mut session = self
            .session
            .lock()
            .map_err(|_| anyhow::anyhow!("Granite 会话不可用"))?;
        let output = session.run(ort::inputs!["input_ids" => Tensor::from_array(([1, len], ids))?, "attention_mask" => Tensor::from_array(([1, len], masks))?])?;
        let (shape, data) = output
            .get("last_hidden_state")
            .context("Granite missing last_hidden_state")?
            .try_extract_tensor::<f32>()?;
        let n = DIMENSIONS as usize;
        ensure!(
            shape.as_ref() == [1, len as i64, i64::from(DIMENSIONS)] && data.len() >= n,
            "Granite output shape mismatch"
        );
        let mut vector = data[..n].to_vec();
        let norm = vector
            .iter()
            .map(|&x| f64::from(x).powi(2))
            .sum::<f64>()
            .sqrt();
        ensure!(
            norm.is_finite() && norm > 0.0,
            "Granite zero or invalid vector"
        );
        vector.iter_mut().for_each(|x| *x /= norm as f32);
        Ok(vector)
    }
}
