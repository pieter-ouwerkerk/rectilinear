use anyhow::{Context, Result};
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Mutex;

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::LlamaModel;

use crate::config::Config;

const MODEL_FILENAME: &str = "embeddinggemma-300m-qat-Q8_0.gguf";
const MODEL_URL: &str = "https://huggingface.co/ggml-org/embeddinggemma-300m-qat-q8_0-GGUF/resolve/main/embeddinggemma-300m-qat-Q8_0.gguf";
const EMBEDDING_DIM: usize = 256;
const MAX_TOKENS: u32 = 512;

pub struct LocalBackend {
    backend: LlamaBackend,
    model: LlamaModel,
    ctx_mutex: Mutex<()>, // serialize access to context creation
}

// Safety: LlamaBackend and LlamaModel are thread-safe for read-only operations.
// We serialize context creation/use via the mutex.
unsafe impl Send for LocalBackend {}
unsafe impl Sync for LocalBackend {}

impl LocalBackend {
    pub fn new(config: &Config) -> Result<Self> {
        let model_path = Self::ensure_model(config)?;

        // Suppress llama.cpp's verbose C-level logging
        llama_cpp_2::send_logs_to_tracing(
            llama_cpp_2::LogOptions::default().with_logs_enabled(false),
        );

        let backend = LlamaBackend::init().context("Failed to initialize llama.cpp backend")?;

        let model_params = LlamaModelParams::default();

        let model = LlamaModel::load_from_file(&backend, &model_path, &model_params)
            .map_err(|e| anyhow::anyhow!("Failed to load model: {:?}", e))?;

        let n_embd = model.n_embd() as usize;
        if n_embd < EMBEDDING_DIM {
            anyhow::bail!(
                "Model embedding dimension ({}) is smaller than requested ({})",
                n_embd,
                EMBEDDING_DIM
            );
        }

        Ok(Self {
            backend,
            model,
            ctx_mutex: Mutex::new(()),
        })
    }

    pub fn dimensions(&self) -> usize {
        EMBEDDING_DIM
    }

    pub fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let _lock = self.ctx_mutex.lock().unwrap();

        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(MAX_TOKENS))
            .with_embeddings(true);

        let mut ctx = self
            .model
            .new_context(&self.backend, ctx_params)
            .map_err(|e| anyhow::anyhow!("Failed to create context: {:?}", e))?;

        let mut results = Vec::with_capacity(texts.len());

        for (seq_idx, text) in texts.iter().enumerate() {
            // Tokenize
            let tokens = self
                .model
                .str_to_token(text, llama_cpp_2::model::AddBos::Always)
                .map_err(|e| anyhow::anyhow!("Tokenization failed: {:?}", e))?;

            // Truncate to max context
            let tokens = if tokens.len() > MAX_TOKENS as usize {
                &tokens[..MAX_TOKENS as usize]
            } else {
                &tokens
            };

            let seq_id = seq_idx as i32;

            // Build batch
            let mut batch = LlamaBatch::new(MAX_TOKENS as usize, 1);
            for (pos, &token) in tokens.iter().enumerate() {
                batch
                    .add(token, pos as i32, &[seq_id], pos == tokens.len() - 1)
                    .map_err(|e| anyhow::anyhow!("Failed to add token to batch: {:?}", e))?;
            }

            // Encode
            ctx.encode(&mut batch)
                .map_err(|e| anyhow::anyhow!("Encoding failed: {:?}", e))?;

            // Extract embedding — use sequence embedding (pooled)
            let emb = ctx
                .embeddings_seq_ith(seq_id)
                .map_err(|e| anyhow::anyhow!("Failed to get embedding: {:?}", e))?;

            // Truncate to desired dimensionality (Matryoshka)
            let emb: Vec<f32> = emb.iter().take(EMBEDDING_DIM).copied().collect();

            // L2 normalize
            let norm: f32 = emb.iter().map(|x| x * x).sum::<f32>().sqrt();
            let emb = if norm > 0.0 {
                emb.iter().map(|x| x / norm).collect()
            } else {
                emb
            };

            results.push(emb);

            // Clear the batch for next text
            ctx.clear_kv_cache();
        }

        Ok(results)
    }

    fn model_path() -> Result<PathBuf> {
        let dir = Config::models_dir()?;
        Ok(dir.join(MODEL_FILENAME))
    }

    fn ensure_model(_config: &Config) -> Result<PathBuf> {
        let path = Self::model_path()?;

        if path.exists() {
            return Ok(path);
        }

        eprintln!("Downloading embedding model ({})...", MODEL_FILENAME);
        eprintln!("This is a one-time download (~329 MB).");
        eprintln!("Source: {}", MODEL_URL);
        eprintln!();

        Self::download_model(&path)?;

        eprintln!("Model saved to {}", path.display());
        Ok(path)
    }

    fn download_model(dest: &PathBuf) -> Result<()> {
        use std::io::Write;

        // Use a blocking HTTP client for the download since we need
        // progress reporting and this only happens once
        let resp = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(600))
            .build()?
            .get(MODEL_URL)
            .send()
            .context("Failed to start model download")?;

        if !resp.status().is_success() {
            anyhow::bail!("Failed to download model: HTTP {}", resp.status());
        }

        let total_size = resp.content_length();

        let pb = if let Some(size) = total_size {
            let pb = indicatif::ProgressBar::new(size);
            pb.set_style(
                indicatif::ProgressStyle::default_bar()
                    .template("{spinner:.green} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
                    .unwrap()
                    .progress_chars("█▉▊▋▌▍▎▏ "),
            );
            pb
        } else {
            let pb = indicatif::ProgressBar::new_spinner();
            pb.set_style(
                indicatif::ProgressStyle::default_spinner()
                    .template("{spinner:.green} {bytes} downloaded")
                    .unwrap(),
            );
            pb
        };

        // Write to a temp file first, then rename (atomic-ish)
        let tmp_path = dest.with_extension("gguf.tmp");
        let mut file = std::fs::File::create(&tmp_path)
            .with_context(|| format!("Failed to create {}", tmp_path.display()))?;

        let mut downloaded: u64 = 0;
        let mut reader = resp;
        let mut buf = vec![0u8; 64 * 1024];

        loop {
            let n = std::io::Read::read(&mut reader, &mut buf)
                .context("Error during model download")?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n])?;
            downloaded += n as u64;
            pb.set_position(downloaded);
        }

        file.flush()?;
        drop(file);

        std::fs::rename(&tmp_path, dest)
            .with_context(|| format!("Failed to move model to {}", dest.display()))?;

        pb.finish_with_message("Download complete");
        Ok(())
    }
}
