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
    workspace: &str,
) -> Result<()> {
    let api_key = config.workspace_api_key(workspace)?;
    let client = LinearClient::with_api_key(&api_key);

    let team_key = team
        .or(config.workspace_default_team(workspace)?.as_deref())
        .ok_or_else(|| {
            anyhow::anyhow!("No team specified. Use --team or set default-team in config")
        })?
        .to_string();

    let team_id = client.get_team_id(&team_key).await?;

    println!(
        "{} Creating issue in team {}...",
        "→".blue(),
        team_key.bold()
    );

    let (issue_id, identifier) = client
        .create_issue(&team_id, title, description, priority, labels, None, None) // assignee_id: out of scope (CLI does not expose --assignee)
        .await?;

    println!("{} Created {}", "✓".green().bold(), identifier.bold());

    // Sync the created issue back to local DB
    let (issue, relations, label_ids) = client.fetch_single_issue(&issue_id).await?;
    db.upsert_issue(&issue)?;
    db.upsert_relations(&issue.id, &relations)?;
    db.replace_issue_labels(&issue.id, &label_ids)?;
    println!("{} Synced to local database", "✓".green());

    Ok(())
}
