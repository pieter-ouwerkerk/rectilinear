use anyhow::Result;
use colored::Colorize;

use crate::config::Config;

pub fn handle_assume(name: &str, config: &Config) -> Result<()> {
    // Validate workspace exists in config
    if !config.workspaces.contains_key(name) {
        let available: Vec<&str> = config.workspaces.keys().map(|k| k.as_str()).collect();
        if available.is_empty() {
            anyhow::bail!(
                "No workspaces configured. Add [workspaces.{}] to your config.toml",
                name
            );
        }
        anyhow::bail!(
            "Workspace '{}' not found in config. Available: {}",
            name,
            available.join(", ")
        );
    }

    Config::set_active_workspace(name)?;
    println!(
        "{} Active workspace set to {}",
        "Done!".green().bold(),
        name.bold()
    );
    Ok(())
}

pub fn handle_list(config: &Config) -> Result<()> {
    if config.workspaces.is_empty() {
        println!("{}", "No workspaces configured.".dimmed());
        return Ok(());
    }

    let active = config.resolve_active_workspace().ok();

    println!("{}", "Configured workspaces:".bold());
    for name in config.workspaces.keys() {
        let marker = if active.as_deref() == Some(name.as_str()) {
            " *".green().bold().to_string()
        } else {
            String::new()
        };
        println!("  {}{}", name, marker);
    }

    if active.is_some() {
        println!("\n{} marks the active workspace", "* ".green().bold());
    }

    Ok(())
}

pub fn handle_current(config: &Config) -> Result<()> {
    match config.resolve_active_workspace() {
        Ok(name) => println!("{}", name),
        Err(_) => {
            eprintln!("{}", "No active workspace set.".dimmed());
            std::process::exit(1);
        }
    }
    Ok(())
}
