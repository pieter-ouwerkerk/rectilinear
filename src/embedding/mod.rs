mod local;

use anyhow::{Context, Result};

use crate::config::{Config, EmbeddingBackend};

enum Backend {
    Gemini(GeminiBackend),
    Local(local::LocalBackend),
}

pub struct Embedder {
    backend: Backend,
    dimensions: usize,
}

// --- Gemini API backend ---

struct GeminiBackend {
    client: reqwest::Client,
    api_key: String,
}

impl GeminiBackend {
    fn new(api_key: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.to_string(),
        }
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-embedding-2-preview:batchEmbedContents?key={}",
            self.api_key
        );

        let mut all_embeddings = Vec::new();
        for batch in texts.chunks(100) {
            let requests: Vec<_> = batch
                .iter()
                .map(|text| {
                    serde_json::json!({
                        "model": "models/gemini-embedding-2-preview",
                        "content": {
                            "parts": [{"text": text}]
                        },
                        "outputDimensionality": 768
                    })
                })
                .collect();

            let body = serde_json::json!({ "requests": requests });

            let resp = self
                .client
                .post(&url)
                .json(&body)
                .send()
                .await
                .context("Failed to call Gemini embedding API")?;

            let status = resp.status();
            if !status.is_success() {
                let text = resp.text().await.unwrap_or_default();
                anyhow::bail!("Gemini API returned {}: {}", status, text);
            }

            let data: serde_json::Value = resp.json().await?;
            let embeddings = data["embeddings"]
                .as_array()
                .context("No embeddings in response")?;

            for emb in embeddings {
                let values: Vec<f32> = emb["values"]
                    .as_array()
                    .context("No values in embedding")?
                    .iter()
                    .map(|v| v.as_f64().unwrap_or(0.0) as f32)
                    .collect();
                all_embeddings.push(values);
            }
        }

        Ok(all_embeddings)
    }
}

// --- Embedder (main interface) ---

impl Embedder {
    pub fn new(config: &Config) -> Result<Self> {
        let gemini_key = std::env::var("GEMINI_API_KEY")
            .ok()
            .or_else(|| config.embedding.gemini_api_key.clone());

        match config.embedding.backend {
            EmbeddingBackend::Api => {
                let key = gemini_key.context(
                    "Gemini API key required for API backend. Set GEMINI_API_KEY or configure in config.",
                )?;
                let backend = GeminiBackend::new(&key);
                Ok(Self {
                    dimensions: 768,
                    backend: Backend::Gemini(backend),
                })
            }
            EmbeddingBackend::Local => {
                let backend = local::LocalBackend::new(config)?;
                let dimensions = backend.dimensions();
                Ok(Self {
                    dimensions,
                    backend: Backend::Local(backend),
                })
            }
        }
    }

    pub async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        match &self.backend {
            Backend::Gemini(b) => b.embed_batch(texts).await,
            Backend::Local(b) => b.embed_batch(texts),
        }
    }

    pub async fn embed_single(&self, text: &str) -> Result<Vec<f32>> {
        let results: Vec<Vec<f32>> = self.embed_batch(&[text.to_string()]).await?;
        results.into_iter().next().context("No embedding returned")
    }

    pub fn dimensions(&self) -> usize {
        self.dimensions
    }

    pub fn backend_name(&self) -> &str {
        match &self.backend {
            Backend::Gemini(_) => "gemini-api",
            Backend::Local(_) => "local-gguf",
        }
    }
}

// --- Text chunking ---

/// Chunk text into segments of approximately `max_tokens` tokens with `overlap` token overlap.
pub fn chunk_text(title: &str, text: &str, max_tokens: usize, overlap: usize) -> Vec<String> {
    let prefix = format!("title: {}\n\n", title);

    if text.is_empty() {
        return vec![format!("{}(no description)", prefix)];
    }

    let max_chars = max_tokens * 4;
    let overlap_chars = overlap * 4;

    if text.len() <= max_chars {
        return vec![format!("{}{}", prefix, text)];
    }

    // Snap a byte offset to the nearest char boundary (rounding down)
    let floor_char = |s: &str, pos: usize| {
        let pos = pos.min(s.len());
        let mut i = pos;
        while i > 0 && !s.is_char_boundary(i) {
            i -= 1;
        }
        i
    };

    let mut chunks = Vec::new();
    let mut start = 0;

    while start < text.len() {
        let end = floor_char(text, start + max_chars);

        let chunk_slice = &text[start..end];
        let break_at = if end < text.len() {
            chunk_slice
                .rfind("\n\n")
                .or_else(|| chunk_slice.rfind('\n'))
                .or_else(|| chunk_slice.rfind(". "))
                .or_else(|| chunk_slice.rfind(' '))
                .map(|p| start + p + 1)
                .unwrap_or(end)
        } else {
            end
        };

        chunks.push(format!("{}{}", prefix, &text[start..break_at]));

        if break_at >= text.len() {
            break;
        }

        let new_start = floor_char(text, if break_at > overlap_chars {
            break_at - overlap_chars
        } else {
            break_at
        });
        // Ensure forward progress — overlap must never push start backwards
        start = if new_start <= start { break_at } else { new_start };
    }

    chunks
}

/// Convert f32 embedding to bytes for storage
pub fn embedding_to_bytes(embedding: &[f32]) -> Vec<u8> {
    embedding.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// Convert bytes back to f32 embedding
pub fn bytes_to_embedding(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

/// Cosine similarity between two embeddings
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;

    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }

    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom == 0.0 {
        0.0
    } else {
        dot / denom
    }
}
