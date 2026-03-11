use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub linear: LinearConfig,
    #[serde(default)]
    pub embedding: EmbeddingConfig,
    #[serde(default)]
    pub search: SearchConfig,
    #[serde(default)]
    pub anthropic: AnthropicConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AnthropicConfig {
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LinearConfig {
    pub api_key: Option<String>,
    pub default_team: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    pub backend: EmbeddingBackend,
    pub gemini_api_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum EmbeddingBackend {
    Local,
    Api,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            backend: if std::env::var("GEMINI_API_KEY").is_ok() {
                EmbeddingBackend::Api
            } else {
                EmbeddingBackend::Local
            },
            gemini_api_key: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchConfig {
    pub default_limit: usize,
    pub duplicate_threshold: f32,
    pub rrf_k: u32,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            default_limit: 10,
            duplicate_threshold: 0.7,
            rrf_k: 60,
        }
    }
}

impl Config {
    pub fn config_dir() -> Result<PathBuf> {
        let dir = dirs::home_dir()
            .context("Could not determine home directory")?
            .join(".config")
            .join("rectilinear");
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    pub fn config_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("config.toml"))
    }

    pub fn data_dir() -> Result<PathBuf> {
        let dir = dirs::home_dir()
            .context("Could not determine home directory")?
            .join(".local")
            .join("share")
            .join("rectilinear");
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    pub fn db_path() -> Result<PathBuf> {
        Ok(Self::data_dir()?.join("rectilinear.db"))
    }

    pub fn models_dir() -> Result<PathBuf> {
        let dir = Self::data_dir()?.join("models");
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read config from {}", path.display()))?;
        let mut config: Config = toml::from_str(&contents)
            .with_context(|| format!("Failed to parse config from {}", path.display()))?;

        // Env vars override config file
        if let Ok(key) = std::env::var("LINEAR_API_KEY") {
            config.linear.api_key = Some(key);
        }
        if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
            config.anthropic.api_key = Some(key);
        }
        if let Ok(key) = std::env::var("GEMINI_API_KEY") {
            config.embedding.gemini_api_key = Some(key);
            if config.embedding.backend == EmbeddingBackend::Local {
                // Don't override explicit local choice, but set key available
            }
        }

        Ok(config)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_path()?;
        let contents = toml::to_string_pretty(self)?;
        std::fs::write(&path, contents)?;
        Ok(())
    }

    pub fn linear_api_key(&self) -> Result<&str> {
        self.linear.api_key.as_deref().context(
            "Linear API key not configured. Run: rectilinear config set linear-api-key <KEY>",
        )
    }

    pub fn anthropic_api_key(&self) -> Result<&str> {
        self.anthropic
            .api_key
            .as_deref()
            .context("Anthropic API key not configured. Set ANTHROPIC_API_KEY or run: rectilinear config set anthropic-api-key <KEY>")
    }

    pub fn gemini_api_key(&self) -> Result<&str> {
        self.embedding
            .gemini_api_key
            .as_deref()
            .or_else(|| std::env::var("GEMINI_API_KEY").ok().as_deref().map(|_| ""))
            .context("Gemini API key not configured")
    }
}
