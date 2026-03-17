#[cfg(feature = "local-embeddings")]
mod local;

use anyhow::{Context, Result};
use reqwest::StatusCode;
use serde::Deserialize;

use crate::config::{Config, EmbeddingBackend};

enum Backend {
    Gemini(GeminiBackend),
    #[cfg(feature = "local-embeddings")]
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

#[derive(Deserialize)]
struct GeminiModelsResponse {
    models: Vec<GeminiModel>,
}

#[derive(Deserialize)]
struct GeminiModel {
    #[allow(dead_code)]
    name: String,
}

#[derive(Deserialize)]
struct GeminiErrorEnvelope {
    error: GeminiErrorBody,
}

#[derive(Deserialize)]
struct GeminiErrorBody {
    #[allow(dead_code)]
    code: Option<u16>,
    message: Option<String>,
    status: Option<String>,
}

impl GeminiBackend {
    fn new(api_key: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.to_string(),
        }
    }

    fn with_http_client(client: reqwest::Client, api_key: &str) -> Self {
        Self {
            client,
            api_key: api_key.to_string(),
        }
    }

    async fn test_api_key(&self) -> Result<()> {
        let resp = self
            .client
            .get("https://generativelanguage.googleapis.com/v1beta/models?pageSize=1")
            .header("x-goog-api-key", &self.api_key)
            .send()
            .await
            .context("Failed to call Gemini models API")?;

        let status = resp.status();
        let body = resp
            .text()
            .await
            .context("Failed to read Gemini models response")?;

        if !status.is_success() {
            anyhow::bail!("{}", summarize_gemini_error(status, &body));
        }

        let response: GeminiModelsResponse =
            serde_json::from_str(&body).context("Failed to parse Gemini models response")?;
        if response.models.is_empty() {
            anyhow::bail!("Gemini returned no models");
        }

        Ok(())
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

fn summarize_gemini_error(status: StatusCode, body: &str) -> String {
    if let Ok(error) = serde_json::from_str::<GeminiErrorEnvelope>(body) {
        return compact_gemini_error(
            status,
            error.error.message.as_deref(),
            error.error.status.as_deref(),
        );
    }

    compact_gemini_error(status, None, None)
}

fn compact_gemini_error(
    status: StatusCode,
    message: Option<&str>,
    api_status: Option<&str>,
) -> String {
    let normalized = message.unwrap_or("").trim().to_lowercase();

    if normalized.contains("api key not valid") || normalized.contains("invalid api key") {
        return "Invalid API key".into();
    }

    if normalized.contains("reported as leaked") || normalized.contains("disabled") {
        return "Key blocked".into();
    }

    if normalized.contains("billing")
        || matches!(api_status, Some("FAILED_PRECONDITION" | "SERVICE_DISABLED"))
    {
        return "Setup required".into();
    }

    if status == StatusCode::UNAUTHORIZED || matches!(api_status, Some("UNAUTHENTICATED")) {
        return "Unauthorized".into();
    }

    if status == StatusCode::FORBIDDEN || matches!(api_status, Some("PERMISSION_DENIED")) {
        return "Access denied".into();
    }

    if status == StatusCode::TOO_MANY_REQUESTS || matches!(api_status, Some("RESOURCE_EXHAUSTED")) {
        return "Rate limited".into();
    }

    if status.is_server_error() {
        return "Gemini unavailable".into();
    }

    if let Some(message) = message {
        let trimmed = message.trim();
        if !trimmed.is_empty() && trimmed.len() <= 48 {
            return trimmed.to_string();
        }
    }

    status
        .canonical_reason()
        .unwrap_or("Request failed")
        .to_string()
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
                Self::new_api(&key)
            }
            #[cfg(feature = "local-embeddings")]
            EmbeddingBackend::Local => {
                let backend = local::LocalBackend::new(config)?;
                let dimensions = backend.dimensions();
                Ok(Self {
                    dimensions,
                    backend: Backend::Local(backend),
                })
            }
            #[cfg(not(feature = "local-embeddings"))]
            EmbeddingBackend::Local => {
                anyhow::bail!(
                    "Local embeddings not available — compile with `local-embeddings` feature"
                )
            }
        }
    }

    /// Create an embedder using the Gemini API backend.
    pub fn new_api(api_key: &str) -> Result<Self> {
        Ok(Self {
            dimensions: 768,
            backend: Backend::Gemini(GeminiBackend::new(api_key)),
        })
    }

    /// Create an embedder using the Gemini API backend with a pre-built HTTP client.
    pub fn new_api_with_http_client(client: reqwest::Client, api_key: &str) -> Result<Self> {
        Ok(Self {
            dimensions: 768,
            backend: Backend::Gemini(GeminiBackend::with_http_client(client, api_key)),
        })
    }

    pub async fn test_api_key(&self) -> Result<()> {
        match &self.backend {
            Backend::Gemini(b) => b.test_api_key().await,
            #[cfg(feature = "local-embeddings")]
            Backend::Local(_) => anyhow::bail!("Gemini API key not in use"),
        }
    }

    /// Create an embedder using the local GGUF backend.
    #[cfg(feature = "local-embeddings")]
    pub fn new_local(_models_dir: &std::path::Path) -> Result<Self> {
        // TODO: pass models_dir through to LocalBackend instead of using Config default
        let config = Config {
            embedding: crate::config::EmbeddingConfig {
                backend: EmbeddingBackend::Local,
                gemini_api_key: None,
            },
            ..Config::default()
        };
        let backend = local::LocalBackend::new(&config)?;
        let dimensions = backend.dimensions();
        Ok(Self {
            dimensions,
            backend: Backend::Local(backend),
        })
    }

    pub async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        match &self.backend {
            Backend::Gemini(b) => b.embed_batch(texts).await,
            #[cfg(feature = "local-embeddings")]
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
            #[cfg(feature = "local-embeddings")]
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

        let new_start = floor_char(
            text,
            if break_at > overlap_chars {
                break_at - overlap_chars
            } else {
                break_at
            },
        );
        // Ensure forward progress — overlap must never push start backwards
        start = if new_start <= start {
            break_at
        } else {
            new_start
        };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compacts_invalid_api_key_errors() {
        let body = r#"{
            "error": {
                "code": 400,
                "message": "API key not valid. Please pass a valid API key.",
                "status": "INVALID_ARGUMENT"
            }
        }"#;

        assert_eq!(
            summarize_gemini_error(StatusCode::BAD_REQUEST, body),
            "Invalid API key"
        );
    }

    #[test]
    fn compacts_rate_limit_errors() {
        let body = r#"{
            "error": {
                "code": 429,
                "message": "Quota exceeded.",
                "status": "RESOURCE_EXHAUSTED"
            }
        }"#;

        assert_eq!(
            summarize_gemini_error(StatusCode::TOO_MANY_REQUESTS, body),
            "Rate limited"
        );
    }

    #[test]
    fn falls_back_to_status_for_unknown_errors() {
        assert_eq!(
            summarize_gemini_error(StatusCode::SERVICE_UNAVAILABLE, "not-json"),
            "Gemini unavailable"
        );
    }
}
