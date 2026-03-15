use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::Config;

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const MODEL: &str = "claude-sonnet-4-20250514";
const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Clone)]
pub struct LlmClient {
    client: reqwest::Client,
    api_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: content.into(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ApiResponse {
    content: Vec<ContentBlock>,
}

#[derive(Debug, Deserialize)]
struct ContentBlock {
    text: Option<String>,
}

impl LlmClient {
    pub fn new(config: &Config) -> Result<Self> {
        let api_key = config.anthropic_api_key()?.to_string();
        let client = reqwest::Client::new();
        Ok(Self { client, api_key })
    }

    pub async fn generate(&self, messages: &[Message], system: &str) -> Result<String> {
        let body = serde_json::json!({
            "model": MODEL,
            "max_tokens": 2048,
            "system": system,
            "messages": messages,
        });

        let resp = self
            .client
            .post(ANTHROPIC_API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Failed to send request to Anthropic API")?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Anthropic API returned {}: {}", status, text);
        }

        let response: ApiResponse = resp
            .json()
            .await
            .context("Failed to parse Anthropic API response")?;

        response
            .content
            .first()
            .and_then(|b| b.text.clone())
            .context("No text in Anthropic API response")
    }
}

/// Extract JSON from a response that may contain ```json fences
pub fn extract_json(text: &str) -> &str {
    if let Some(start) = text.find("```json") {
        let json_start = start + 7;
        if let Some(end) = text[json_start..].find("```") {
            return text[json_start..json_start + end].trim();
        }
    }
    if let Some(start) = text.find("```") {
        let block_start = start + 3;
        // skip optional language tag on same line
        let content_start = text[block_start..]
            .find('\n')
            .map(|i| block_start + i + 1)
            .unwrap_or(block_start);
        if let Some(end) = text[content_start..].find("```") {
            return text[content_start..content_start + end].trim();
        }
    }
    text.trim()
}
