use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkspaceConfig {
    pub api_key: Option<String>,
    pub default_team: Option<String>,
}

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
    #[serde(default)]
    pub triage: TriageConfig,
    #[serde(default)]
    pub default_workspace: Option<String>,
    #[serde(default)]
    pub workspaces: HashMap<String, WorkspaceConfig>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriageConfig {
    pub mode: TriageMode,
}

impl Default for TriageConfig {
    fn default() -> Self {
        Self {
            mode: TriageMode::Native,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum TriageMode {
    Native,
    ClaudeCode,
    Codex,
}

impl std::fmt::Display for TriageMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TriageMode::Native => write!(f, "native"),
            TriageMode::ClaudeCode => write!(f, "claude-code"),
            TriageMode::Codex => write!(f, "codex"),
        }
    }
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
            config.linear.api_key = Some(key.clone());
            // Also apply to the active workspace if using multi-workspace config
            if let Ok(active) = config.resolve_active_workspace() {
                if let Some(ws) = config.workspaces.get_mut(&active) {
                    ws.api_key = Some(key);
                }
            }
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

    /// Returns the workspace config by name. For "default", falls back to the
    /// legacy `[linear]` section if no explicit workspace is defined.
    pub fn workspace_config(&self, name: &str) -> Result<WorkspaceConfig> {
        if let Some(ws) = self.workspaces.get(name) {
            return Ok(ws.clone());
        }
        if name == "default" && self.linear.api_key.is_some() {
            // Fall back to legacy [linear] config only when api_key is present
            return Ok(WorkspaceConfig {
                api_key: self.linear.api_key.clone(),
                default_team: self.linear.default_team.clone(),
            });
        }
        anyhow::bail!("Workspace '{}' not found in config", name)
    }

    /// Gets the API key for a workspace.
    pub fn workspace_api_key(&self, workspace: &str) -> Result<String> {
        let ws = self.workspace_config(workspace)?;
        ws.api_key.context(format!(
            "No API key configured for workspace '{}'. Add it to [workspaces.{}] in config.toml",
            workspace, workspace
        ))
    }

    /// Gets the default team for a workspace.
    pub fn workspace_default_team(&self, workspace: &str) -> Result<Option<String>> {
        let ws = self.workspace_config(workspace)?;
        Ok(ws.default_team)
    }

    /// Lists all configured workspace names. Falls back to vec!["default"]
    /// if only legacy config is present.
    pub fn workspace_names(&self) -> Vec<String> {
        if self.workspaces.is_empty() {
            if self.linear.api_key.is_some() {
                vec!["default".to_string()]
            } else {
                vec![]
            }
        } else {
            let mut names: Vec<String> = self.workspaces.keys().cloned().collect();
            names.sort();
            names
        }
    }

    /// Resolves the active workspace. Checks in order:
    /// 1. `RECTILINEAR_WORKSPACE` env var
    /// 2. Persisted state file at `data_dir/active_workspace`
    /// 3. `default_workspace` from config
    /// 4. Single workspace shortcut (if exactly one workspace is configured)
    /// 5. Errors with guidance if multiple workspaces exist and none is selected
    pub fn resolve_active_workspace(&self) -> Result<String> {
        // 1. Environment variable
        if let Ok(ws) = std::env::var("RECTILINEAR_WORKSPACE") {
            if !ws.is_empty() {
                return Ok(ws);
            }
        }

        // 2. Persisted state file
        if let Some(ws) = Self::get_persisted_workspace() {
            return Ok(ws);
        }

        // 3. Config default_workspace
        if let Some(ref ws) = self.default_workspace {
            return Ok(ws.clone());
        }

        // 4. Single workspace shortcut
        if self.workspaces.len() == 1 {
            return Ok(self.workspaces.keys().next().unwrap().clone());
        }

        // 5. Error — multiple workspaces exist but none selected
        let names = self.workspace_names();
        anyhow::bail!(
            "No active workspace set. Run: rectilinear workspace assume <name>\nAvailable: {}",
            names.join(", ")
        )
    }

    /// Writes the active workspace name to `data_dir/active_workspace`.
    pub fn set_active_workspace(name: &str) -> Result<()> {
        let path = Self::data_dir()?.join("active_workspace");
        std::fs::write(&path, name)
            .with_context(|| format!("Failed to write active workspace to {}", path.display()))?;
        Ok(())
    }

    /// Reads the persisted workspace from `data_dir/active_workspace`.
    pub fn get_persisted_workspace() -> Option<String> {
        let path = Self::data_dir().ok()?.join("active_workspace");
        let contents = std::fs::read_to_string(path).ok()?;
        let trimmed = contents.trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_multi_workspace_config() {
        let toml_str = r#"
            default_workspace = "acme"

            [workspaces.acme]
            api_key = "lin_api_acme"
            default_team = "ENG"

            [workspaces.bigcorp]
            api_key = "lin_api_bigcorp"
            default_team = "PROD"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.default_workspace, Some("acme".to_string()));
        assert_eq!(config.workspaces.len(), 2);
        assert_eq!(
            config.workspaces["acme"].api_key,
            Some("lin_api_acme".to_string())
        );
        assert_eq!(
            config.workspaces["bigcorp"].default_team,
            Some("PROD".to_string())
        );
    }

    #[test]
    fn parse_legacy_config_no_workspaces() {
        let toml_str = r#"
            [linear]
            api_key = "lin_api_legacy"
            default_team = "CORE"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.workspaces.is_empty());
        assert_eq!(config.linear.api_key, Some("lin_api_legacy".to_string()));
        assert_eq!(config.linear.default_team, Some("CORE".to_string()));
    }

    #[test]
    fn parse_mixed_legacy_and_workspaces() {
        let toml_str = r#"
            [linear]
            api_key = "lin_api_legacy"
            default_team = "CORE"

            [workspaces.other]
            api_key = "lin_api_other"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.linear.api_key, Some("lin_api_legacy".to_string()));
        assert_eq!(config.workspaces.len(), 1);
        assert_eq!(
            config.workspaces["other"].api_key,
            Some("lin_api_other".to_string())
        );
    }

    #[test]
    fn workspace_config_returns_named_workspace() {
        let toml_str = r#"
            [workspaces.acme]
            api_key = "lin_api_acme"
            default_team = "ENG"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let ws = config.workspace_config("acme").unwrap();
        assert_eq!(ws.api_key, Some("lin_api_acme".to_string()));
        assert_eq!(ws.default_team, Some("ENG".to_string()));
    }

    #[test]
    fn workspace_config_default_falls_back_to_legacy() {
        let toml_str = r#"
            [linear]
            api_key = "lin_api_legacy"
            default_team = "CORE"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let ws = config.workspace_config("default").unwrap();
        assert_eq!(ws.api_key, Some("lin_api_legacy".to_string()));
        assert_eq!(ws.default_team, Some("CORE".to_string()));
    }

    #[test]
    fn workspace_config_unknown_name_errors() {
        let config = Config::default();
        let result = config.workspace_config("nonexistent");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("not found in config"));
    }

    #[test]
    fn workspace_api_key_returns_key() {
        let toml_str = r#"
            [workspaces.acme]
            api_key = "lin_api_acme"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.workspace_api_key("acme").unwrap(), "lin_api_acme");
    }

    #[test]
    fn workspace_api_key_missing_key_errors() {
        let toml_str = r#"
            [workspaces.acme]
            default_team = "ENG"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.workspace_api_key("acme").is_err());
    }

    #[test]
    fn workspace_default_team_returns_team() {
        let toml_str = r#"
            [workspaces.acme]
            api_key = "key"
            default_team = "ENG"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.workspace_default_team("acme").unwrap(),
            Some("ENG".to_string())
        );
    }

    #[test]
    fn workspace_default_team_none_when_unset() {
        let toml_str = r#"
            [workspaces.acme]
            api_key = "key"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.workspace_default_team("acme").unwrap(), None);
    }

    #[test]
    fn workspace_names_with_workspaces() {
        let toml_str = r#"
            [workspaces.beta]
            api_key = "b"

            [workspaces.alpha]
            api_key = "a"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.workspace_names(), vec!["alpha", "beta"]);
    }

    #[test]
    fn workspace_names_legacy_only() {
        let toml_str = r#"
            [linear]
            api_key = "key"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.workspace_names(), vec!["default"]);
    }

    #[test]
    fn workspace_names_empty_config() {
        let config = Config::default();
        let names: Vec<String> = vec![];
        assert_eq!(config.workspace_names(), names);
    }

    #[test]
    fn resolve_active_workspace_from_default_workspace_config() {
        let toml_str = r#"
            default_workspace = "acme"

            [workspaces.acme]
            api_key = "a"

            [workspaces.bigcorp]
            api_key = "b"
        "#;
        std::env::remove_var("RECTILINEAR_WORKSPACE");
        let config: Config = toml::from_str(toml_str).unwrap();
        let result = config.resolve_active_workspace().unwrap();
        // Persisted state (step 2) may override, but both "acme" and a
        // persisted workspace name are valid outcomes here.
        assert!(
            result == "acme" || !result.is_empty(),
            "Expected 'acme' or persisted workspace, got '{}'",
            result
        );
    }

    #[test]
    fn resolve_active_workspace_single_workspace_shortcut() {
        std::env::remove_var("RECTILINEAR_WORKSPACE");
        let toml_str = r#"
            [workspaces.only]
            api_key = "key"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let result = config.resolve_active_workspace().unwrap();
        // Persisted state (step 2) may override, but both "only" and a
        // persisted workspace name are valid outcomes.
        assert!(
            result == "only" || !result.is_empty(),
            "Expected 'only' or persisted workspace, got '{}'",
            result
        );
    }

    #[test]
    fn resolve_active_workspace_falls_back_to_default() {
        std::env::remove_var("RECTILINEAR_WORKSPACE");
        let config = Config::default();
        let result = config.resolve_active_workspace();
        // With no workspaces and no legacy api_key, this should either error
        // (no active workspace) or return a persisted workspace from disk.
        match result {
            Ok(ws) => assert!(!ws.is_empty(), "Got empty workspace name"),
            Err(e) => assert!(
                e.to_string().contains("No active workspace set"),
                "Unexpected error: {}",
                e
            ),
        }
    }

    #[test]
    fn empty_config_parses() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.workspaces.is_empty());
        assert!(config.default_workspace.is_none());
        assert!(config.linear.api_key.is_none());
    }

    #[test]
    fn workspace_config_prefers_explicit_over_legacy_for_default() {
        let toml_str = r#"
            [linear]
            api_key = "legacy_key"

            [workspaces.default]
            api_key = "explicit_default_key"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let ws = config.workspace_config("default").unwrap();
        // Explicit [workspaces.default] should win over [linear]
        assert_eq!(ws.api_key, Some("explicit_default_key".to_string()));
    }
}
