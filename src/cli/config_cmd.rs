use anyhow::Result;
use colored::Colorize;
use std::io::{self, Write};

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

pub fn handle_interactive() -> Result<()> {
    let mut config = Config::load()?;
    let path = Config::config_path()?;

    println!(
        "{} {}",
        "Rectilinear configuration".bold(),
        format!("({})", path.display()).dimmed()
    );
    println!(
        "{}",
        "Press Enter to keep current value, or type a new value.".dimmed()
    );
    println!();

    // --- Linear ---
    println!("{}", "[linear]".bold().cyan());

    if let Some(new) = prompt_secret("  api-key", config.linear.api_key.as_deref())? {
        config.linear.api_key = Some(new);
    }

    if let Some(new) = prompt_string("  default-team", config.linear.default_team.as_deref())? {
        config.linear.default_team = if new.is_empty() { None } else { Some(new) };
    }

    println!();

    // --- Anthropic ---
    println!("{}", "[anthropic]".bold().cyan());

    if let Some(new) = prompt_secret("  api-key", config.anthropic.api_key.as_deref())? {
        config.anthropic.api_key = Some(new);
    }

    println!();

    // --- Embedding ---
    println!("{}", "[embedding]".bold().cyan());

    if let Some(new) = prompt_choice(
        "  backend",
        &format!("{:?}", config.embedding.backend).to_lowercase(),
        &["local", "api"],
    )? {
        config.embedding.backend = match new.as_str() {
            "api" => crate::config::EmbeddingBackend::Api,
            _ => crate::config::EmbeddingBackend::Local,
        };
    }

    if let Some(new) = prompt_secret("  gemini-api-key", config.embedding.gemini_api_key.as_deref())?
    {
        config.embedding.gemini_api_key = Some(new);
    }

    println!();

    // --- Triage ---
    println!("{}", "[triage]".bold().cyan());

    if let Some(new) = prompt_choice(
        "  mode",
        &config.triage.mode.to_string(),
        &["native", "claude-code", "codex"],
    )? {
        config.triage.mode = match new.as_str() {
            "claude-code" => crate::config::TriageMode::ClaudeCode,
            "codex" => crate::config::TriageMode::Codex,
            _ => crate::config::TriageMode::Native,
        };
    }

    println!();

    config.save()?;
    println!("{} Configuration saved.", "Done!".green().bold());
    Ok(())
}

/// Prompt for a regular string value. Returns Some(new_value) if changed, None if kept.
fn prompt_string(label: &str, current: Option<&str>) -> Result<Option<String>> {
    let display = current.unwrap_or("(not set)");
    print!("{}: {} > ", label, display.dimmed());
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let trimmed = input.trim();

    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_string()))
    }
}

/// Prompt for a secret (API key). Shows masked current value.
fn prompt_secret(label: &str, current: Option<&str>) -> Result<Option<String>> {
    let display = match current {
        Some(k) if k.len() > 8 => format!("{}...{}", &k[..4], &k[k.len() - 4..]),
        Some(_) => "****".to_string(),
        None => "(not set)".to_string(),
    };
    print!("{}: {} > ", label, display.dimmed());
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let trimmed = input.trim();

    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_string()))
    }
}

/// Prompt for a choice from a fixed set of options.
fn prompt_choice(label: &str, current: &str, options: &[&str]) -> Result<Option<String>> {
    let options_str = options
        .iter()
        .map(|o| {
            if *o == current {
                format!("[{}]", o).bold().to_string()
            } else {
                o.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("/");

    print!("{}: {} > ", label, options_str);
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let trimmed = input.trim();

    if trimmed.is_empty() {
        Ok(None)
    } else if options.contains(&trimmed) {
        Ok(Some(trimmed.to_string()))
    } else {
        eprintln!(
            "  {} Invalid choice '{}', keeping '{}'",
            "Warning:".yellow(),
            trimmed,
            current
        );
        Ok(None)
    }
}
