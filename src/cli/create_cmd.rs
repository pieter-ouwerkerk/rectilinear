use anyhow::Result;
use colored::Colorize;

use crate::config::Config;
use crate::db::Database;
use crate::linear::LinearClient;

pub struct HandleCreateParams<'a> {
    pub team: Option<&'a str>,
    pub title: &'a str,
    pub description: Option<&'a str>,
    pub priority: Option<i32>,
    pub labels: &'a [String],
    pub project: Option<&'a str>,
    pub project_milestone: Option<&'a str>,
    pub workspace: &'a str,
}

pub async fn handle_create(
    db: &Database,
    config: &Config,
    params: HandleCreateParams<'_>,
) -> Result<()> {
    let HandleCreateParams {
        team,
        title,
        description,
        priority,
        labels,
        project,
        project_milestone,
        workspace,
    } = params;
    let api_key = config.workspace_api_key(workspace)?;
    let client = LinearClient::with_api_key(&api_key);

    let team_key = team
        .or(config.workspace_default_team(workspace)?.as_deref())
        .ok_or_else(|| {
            anyhow::anyhow!("No team specified. Use --team or set default-team in config")
        })?
        .to_string();

    let team_id = client.get_team_id(&team_key).await?;

    let mut project_id = match project {
        Some(value) => Some(client.get_project_id(value).await?),
        None => None,
    };
    let project_milestone_id = match project_milestone {
        Some(value) => {
            let milestone_id = client
                .find_project_milestone(project_id.as_deref(), value)
                .await?;
            let milestone = client
                .fetch_project_milestone(&milestone_id, workspace)
                .await?;
            if project_id.is_none() {
                project_id = Some(milestone.project_id);
            }
            Some(milestone_id)
        }
        None => None,
    };

    println!(
        "{} Creating issue in team {}...",
        "→".blue(),
        team_key.bold()
    );

    let (issue_id, identifier) = client
        .create_issue(crate::linear::CreateIssueInput {
            team_id: &team_id,
            title,
            description,
            priority,
            label_ids: labels,
            assignee_id: None,
            parent_id: None,
            project_id: project_id.as_deref(),
            project_milestone_id: project_milestone_id.as_deref(),
        })
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
