use anyhow::{Context, Result};
use colored::Colorize;

use crate::config::Config;
use crate::db::{Database, HydrationPolicy};
use crate::linear::LinearClient;

pub async fn handle_hydrate(
    db: &Database,
    config: &Config,
    team: Option<&str>,
    issue: Option<&str>,
    limit: usize,
    open_only: bool,
    all: bool,
    workspace: &str,
) -> Result<()> {
    db.upsert_workspace(workspace, None, None)?;
    let api_key = config.workspace_api_key(workspace)?;
    let client = LinearClient::with_api_key(&api_key);
    if let Some(issue) = issue {
        let result = client.hydrate_issue(db, issue, workspace, None).await?;
        println!(
            "{} Hydrated {}: {:?} ({} resources, {} retryable, {} permanent)",
            "Done!".green().bold(),
            issue.bold(),
            result.status,
            result.hydrated_resources,
            result.retryable_failures,
            result.permanent_failures
        );
        return Ok(());
    }

    let team_key = team
        .map(ToString::to_string)
        .or(config.workspace_default_team(workspace)?)
        .context("--team is required when the workspace has no default team")?;
    let policy = if all {
        HydrationPolicy::All
    } else if open_only {
        HydrationPolicy::OpenOnly
    } else {
        HydrationPolicy::OpenAndRecent
    };
    let result = client
        .hydrate_pending_issues(db, &team_key, workspace, limit, policy, None)
        .await?;
    println!(
        "{} Hydration batch for {}: {} hydrated, {} partial, {} deferred, {} retryable, {} permanent{}",
        "Done!".green().bold(),
        team_key.bold(),
        result.hydrated,
        result.partial,
        result.deferred,
        result.retryable_failures,
        result.permanent_failures,
        if result.rate_limited { " (rate limited)" } else { "" }
    );
    Ok(())
}
