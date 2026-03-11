use anyhow::Result;
use colored::Colorize;

use crate::config::Config;

pub fn handle_set(key: &str, value: &str) -> Result<()> {
    let mut config = Config::load()?;

    match key {
        "linear-api-key" => config.linear.api_key = Some(value.to_string()),
        "default-team" => config.linear.default_team = Some(value.to_string()),
        "embedding.backend" => {
            config.embedding.backend = match value {
                "local" => crate::config::EmbeddingBackend::Local,
                "api" => crate::config::EmbeddingBackend::Api,
                _ => anyhow::bail!("Invalid backend: {}. Use 'local' or 'api'", value),
            };
        }
        "anthropic-api-key" => config.anthropic.api_key = Some(value.to_string()),
        "embedding.gemini-api-key" => config.embedding.gemini_api_key = Some(value.to_string()),
        "search.default-limit" => {
            config.search.default_limit = value
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid number"))?;
        }
        "search.duplicate-threshold" => {
            config.search.duplicate_threshold = value
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid number"))?;
        }
        "triage.mode" => {
            config.triage.mode = match value {
                "native" => crate::config::TriageMode::Native,
                "claude-code" => crate::config::TriageMode::ClaudeCode,
                "codex" => crate::config::TriageMode::Codex,
                _ => anyhow::bail!(
                    "Invalid triage mode: {}. Use 'native', 'claude-code', or 'codex'",
                    value
                ),
            };
        }
        _ => anyhow::bail!("Unknown config key: {}", key),
    }

    config.save()?;
    println!("{} {} = {}", "Set".green(), key, value);
    Ok(())
}

pub fn handle_get(key: &str) -> Result<()> {
    let config = Config::load()?;

    let value = match key {
        "linear-api-key" => config.linear.api_key.map(|k| {
            if k.len() > 8 {
                format!("{}...{}", &k[..4], &k[k.len() - 4..])
            } else {
                "****".to_string()
            }
        }),
        "default-team" => config.linear.default_team,
        "embedding.backend" => Some(format!("{:?}", config.embedding.backend).to_lowercase()),
        "anthropic-api-key" => config.anthropic.api_key.map(|k| {
            if k.len() > 8 {
                format!("{}...{}", &k[..4], &k[k.len() - 4..])
            } else {
                "****".to_string()
            }
        }),
        "embedding.gemini-api-key" => config.embedding.gemini_api_key.map(|k| {
            if k.len() > 8 {
                format!("{}...{}", &k[..4], &k[k.len() - 4..])
            } else {
                "****".to_string()
            }
        }),
        "search.default-limit" => Some(config.search.default_limit.to_string()),
        "search.duplicate-threshold" => Some(config.search.duplicate_threshold.to_string()),
        "triage.mode" => Some(config.triage.mode.to_string()),
        _ => anyhow::bail!("Unknown config key: {}", key),
    };

    match value {
        Some(v) => println!("{}", v),
        None => println!("{}", "(not set)".dimmed()),
    }
    Ok(())
}

pub fn handle_show() -> Result<()> {
    let config = Config::load()?;
    let path = Config::config_path()?;

    println!("{} {}", "Config file:".bold(), path.display());
    println!("{} {}", "Database:".bold(), Config::db_path()?.display());
    println!();

    println!("{}", "[linear]".bold());
    println!(
        "  api-key: {}",
        config
            .linear
            .api_key
            .as_ref()
            .map(|k| if k.len() > 8 {
                format!("{}...{}", &k[..4], &k[k.len() - 4..])
            } else {
                "****".to_string()
            })
            .unwrap_or_else(|| "(not set)".dimmed().to_string())
    );
    println!(
        "  default-team: {}",
        config
            .linear
            .default_team
            .as_deref()
            .unwrap_or(&"(not set)".dimmed().to_string())
    );

    println!();
    println!("{}", "[anthropic]".bold());
    println!(
        "  api-key: {}",
        config
            .anthropic
            .api_key
            .as_ref()
            .map(|k| if k.len() > 8 {
                format!("{}...{}", &k[..4], &k[k.len() - 4..])
            } else {
                "****".to_string()
            })
            .unwrap_or_else(|| "(not set)".dimmed().to_string())
    );

    println!();
    println!("{}", "[embedding]".bold());
    println!(
        "  backend: {}",
        format!("{:?}", config.embedding.backend).to_lowercase()
    );
    println!(
        "  gemini-api-key: {}",
        config
            .embedding
            .gemini_api_key
            .as_ref()
            .map(|k| if k.len() > 8 {
                format!("{}...{}", &k[..4], &k[k.len() - 4..])
            } else {
                "****".to_string()
            })
            .unwrap_or_else(|| "(not set)".dimmed().to_string())
    );

    println!();
    println!("{}", "[search]".bold());
    println!("  default-limit: {}", config.search.default_limit);
    println!(
        "  duplicate-threshold: {}",
        config.search.duplicate_threshold
    );

    println!();
    println!("{}", "[triage]".bold());
    println!("  mode: {}", config.triage.mode);

    Ok(())
}
