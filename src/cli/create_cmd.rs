use anyhow::Result;
use colored::Colorize;

use crate::config::Config;
use crate::db::Database;
use crate::linear::{CreateIssueInput, LinearClient};

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

struct CreateLinearIssueParams<'a> {
    team_id: &'a str,
    title: &'a str,
    description: Option<&'a str>,
    priority: Option<i32>,
    label_names: &'a [String],
    project_id: Option<&'a str>,
    project_milestone_id: Option<&'a str>,
}

trait IssueCreateClient {
    async fn get_label_ids(&self, label_names: &[String]) -> Result<Vec<String>>;
    async fn create_issue(&self, create: CreateIssueInput<'_>) -> Result<(String, String)>;
}

impl IssueCreateClient for LinearClient {
    async fn get_label_ids(&self, label_names: &[String]) -> Result<Vec<String>> {
        LinearClient::get_label_ids(self, label_names).await
    }

    async fn create_issue(&self, create: CreateIssueInput<'_>) -> Result<(String, String)> {
        LinearClient::create_issue(self, create).await
    }
}

async fn create_issue_with_resolved_labels(
    client: &impl IssueCreateClient,
    params: CreateLinearIssueParams<'_>,
) -> Result<(String, String)> {
    let label_ids = client.get_label_ids(params.label_names).await?;
    client
        .create_issue(CreateIssueInput {
            team_id: params.team_id,
            title: params.title,
            description: params.description,
            priority: params.priority,
            due_date: None,
            label_ids: &label_ids,
            assignee_id: None,
            parent_id: None,
            project_id: params.project_id,
            project_milestone_id: params.project_milestone_id,
        })
        .await
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

    let (issue_id, identifier) = create_issue_with_resolved_labels(
        &client,
        CreateLinearIssueParams {
            team_id: &team_id,
            title,
            description,
            priority,
            label_names: labels,
            project_id: project_id.as_deref(),
            project_milestone_id: project_milestone_id.as_deref(),
        },
    )
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct MockIssueCreateClient {
        requested_label_names: Mutex<Vec<String>>,
        created_label_ids: Mutex<Vec<String>>,
    }

    impl IssueCreateClient for MockIssueCreateClient {
        async fn get_label_ids(&self, label_names: &[String]) -> Result<Vec<String>> {
            *self.requested_label_names.lock().unwrap() = label_names.to_vec();
            Ok(vec!["label-id-feature".to_string()])
        }

        async fn create_issue(&self, create: CreateIssueInput<'_>) -> Result<(String, String)> {
            *self.created_label_ids.lock().unwrap() = create.label_ids.to_vec();
            Ok(("issue-id".to_string(), "ENG-123".to_string()))
        }
    }

    #[tokio::test]
    async fn create_resolves_label_names_before_sending_label_ids() {
        let client = MockIssueCreateClient::default();
        let label_names = vec!["Feature".to_string()];

        let result = create_issue_with_resolved_labels(
            &client,
            CreateLinearIssueParams {
                team_id: "team-id",
                title: "Add drill loop",
                description: None,
                priority: None,
                label_names: &label_names,
                project_id: None,
                project_milestone_id: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(result, ("issue-id".to_string(), "ENG-123".to_string()));
        assert_eq!(
            *client.requested_label_names.lock().unwrap(),
            vec!["Feature"]
        );
        assert_eq!(
            *client.created_label_ids.lock().unwrap(),
            vec!["label-id-feature"]
        );
    }
}
