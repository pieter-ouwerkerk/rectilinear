use anyhow::Result;
use colored::Colorize;

use crate::config::Config;
use crate::db::Database;
use crate::linear::LinearClient;

pub async fn handle_append(
    db: &Database,
    config: &Config,
    id: &str,
    comment: Option<&str>,
    description: Option<&str>,
) -> Result<()> {
    let issue = db
        .get_issue(id)?
        .ok_or_else(|| anyhow::anyhow!("Issue '{}' not found locally. Try syncing first.", id))?;

    let client = LinearClient::new(config)?;

    if let Some(comment_text) = comment {
        println!(
            "{} Adding comment to {}...",
            "→".blue(),
            issue.identifier.bold()
        );
        client.add_comment(&issue.id, comment_text).await?;
        println!("{} Comment added", "✓".green().bold());
    }

    if let Some(desc_text) = description {
        // Append to existing description
        let new_desc = match &issue.description {
            Some(existing) => format!("{}\n\n{}", existing, desc_text),
            None => desc_text.to_string(),
        };
        println!(
            "{} Updating description for {}...",
            "→".blue(),
            issue.identifier.bold()
        );
        client
            .update_issue(&issue.id, None, Some(&new_desc), None, None, None, None)
            .await?;
        println!("{} Description updated", "✓".green().bold());
    }

    // Sync the updated issue back
    let (updated, relations) = client.fetch_single_issue(&issue.id).await?;
    db.upsert_issue(&updated)?;
    db.upsert_relations(&updated.id, &relations)?;
    println!("{} Synced to local database", "✓".green());

    Ok(())
}
