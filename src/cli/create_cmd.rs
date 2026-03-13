use anyhow::Result;
use colored::Colorize;

use crate::config::Config;
use crate::db::Database;
use crate::linear::LinearClient;

pub async fn handle_create(
    db: &Database,
    config: &Config,
    team: Option<&str>,
    title: &str,
    description: Option<&str>,
    priority: Option<i32>,
    labels: &[String],
) -> Result<()> {
    let client = LinearClient::new(config)?;

    let team_key = team
        .or(config.linear.default_team.as_deref())
        .ok_or_else(|| {
            anyhow::anyhow!("No team specified. Use --team or set default-team in config")
        })?;

    let team_id = client.get_team_id(team_key).await?;

    println!(
        "{} Creating issue in team {}...",
        "→".blue(),
        team_key.bold()
    );

    let (issue_id, identifier) = client
        .create_issue(&team_id, title, description, priority, labels, None)
        .await?;

    println!("{} Created {}", "✓".green().bold(), identifier.bold());

    // Sync the created issue back to local DB
    let (issue, relations) = client.fetch_single_issue(&issue_id).await?;
    db.upsert_issue(&issue)?;
    db.upsert_relations(&issue.id, &relations)?;
    println!("{} Synced to local database", "✓".green());

    Ok(())
}
