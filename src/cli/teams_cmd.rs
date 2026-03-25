use anyhow::Result;
use colored::Colorize;

use crate::config::Config;
use crate::linear::LinearClient;

pub async fn handle_teams(config: &Config, workspace: &str) -> Result<()> {
    let api_key = config.workspace_api_key(workspace)?;
    let client = LinearClient::with_api_key(&api_key);
    let teams = client.list_teams().await?;

    if teams.is_empty() {
        println!("{}", "No teams found.".dimmed());
        return Ok(());
    }

    println!("{}", "Available teams:".bold());
    for team in &teams {
        println!("  {} — {}", team.key.bold(), team.name);
    }

    println!(
        "\nUse the {} (e.g. {}) with --team, not the full name.",
        "key".bold(),
        teams.first().map(|t| t.key.as_str()).unwrap_or("ENG")
    );

    Ok(())
}
