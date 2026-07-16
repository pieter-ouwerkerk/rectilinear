use anyhow::{Context, Result};
use clap::Subcommand;

use crate::config::Config;
use crate::db::Database;
use crate::linear::{
    CreateProjectInput, CreateProjectMilestoneInput, LinearClient, UpdateProjectInput,
    UpdateProjectMilestoneInput,
};

#[derive(Subcommand)]
pub enum ProjectAction {
    /// List projects and their metadata
    List {
        /// Include archived projects
        #[arg(long)]
        include_archived: bool,
        /// Read only the local cache
        #[arg(long)]
        no_refresh: bool,
    },
    /// Show a project and its milestones
    Show {
        /// Project UUID, slug, or name
        id: String,
        /// Import and include every linked issue
        #[arg(long)]
        include_issues: bool,
        /// Read only the local cache
        #[arg(long)]
        no_refresh: bool,
    },
    /// Create a project
    Create {
        #[arg(long)]
        name: String,
        /// Owning team keys
        #[arg(long, value_delimiter = ',', required = true)]
        teams: Vec<String>,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        content: Option<String>,
        #[arg(long)]
        icon: Option<String>,
        #[arg(long)]
        color: Option<String>,
        /// Project status name or UUID
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        priority: Option<i32>,
        /// Lead name or "me"
        #[arg(long)]
        lead: Option<String>,
        #[arg(long)]
        start_date: Option<String>,
        #[arg(long)]
        target_date: Option<String>,
        /// Project member names or "me"
        #[arg(long, value_delimiter = ',')]
        members: Option<Vec<String>>,
        /// Project label names or UUIDs
        #[arg(long, value_delimiter = ',')]
        labels: Option<Vec<String>>,
    },
    /// Update project metadata
    Update {
        /// Project UUID, slug, or current name
        id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, value_delimiter = ',')]
        teams: Option<Vec<String>>,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        content: Option<String>,
        #[arg(long)]
        icon: Option<String>,
        #[arg(long)]
        color: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        priority: Option<i32>,
        #[arg(long)]
        lead: Option<String>,
        #[arg(long)]
        start_date: Option<String>,
        #[arg(long)]
        target_date: Option<String>,
        /// Replacement member names or "me"; use "none" to clear
        #[arg(long, value_delimiter = ',')]
        members: Option<Vec<String>>,
        /// Replacement project label names or UUIDs; use "none" to clear
        #[arg(long, value_delimiter = ',')]
        labels: Option<Vec<String>>,
    },
    /// Delete (archive) a project
    Delete { id: String },
    /// Import a complete project with milestones and linked issues
    Import { id: String },
    /// Refresh the project and milestone cache
    Sync,
}

#[derive(Subcommand)]
pub enum MilestoneAction {
    /// List a project's milestones
    List {
        #[arg(long)]
        project: String,
        #[arg(long)]
        no_refresh: bool,
    },
    /// Show a milestone
    Show {
        id: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        include_issues: bool,
        #[arg(long)]
        no_refresh: bool,
    },
    /// Create a milestone
    Create {
        #[arg(long)]
        project: String,
        #[arg(long)]
        name: String,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        target_date: Option<String>,
        #[arg(long)]
        sort_order: Option<f64>,
    },
    /// Update or move a milestone
    Update {
        id: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        target_date: Option<String>,
        #[arg(long)]
        sort_order: Option<f64>,
    },
    /// Delete a milestone
    Delete {
        id: String,
        #[arg(long)]
        project: Option<String>,
    },
    /// Import a complete milestone with its linked issues
    Import {
        id: String,
        #[arg(long)]
        project: Option<String>,
    },
}

pub async fn handle_project_action(
    action: ProjectAction,
    db: &Database,
    config: &Config,
    workspace: &str,
) -> Result<()> {
    let api_key = config.workspace_api_key(workspace)?;
    let client = LinearClient::with_api_key(&api_key);
    match action {
        ProjectAction::List {
            include_archived,
            no_refresh,
        } => {
            if !no_refresh {
                client.sync_projects(db, workspace).await?;
            }
            print_json(&db.list_projects(workspace, include_archived)?)
        }
        ProjectAction::Show {
            id,
            include_issues,
            no_refresh,
        } => {
            if include_issues {
                return print_json(&client.import_project(db, workspace, &id).await?);
            }
            if !no_refresh {
                client.sync_projects(db, workspace).await?;
            }
            print_json(
                &db.get_project_bundle(workspace, &id)?
                    .with_context(|| format!("Project '{id}' not found"))?,
            )
        }
        ProjectAction::Create {
            name,
            teams,
            description,
            content,
            icon,
            color,
            status,
            priority,
            lead,
            start_date,
            target_date,
            members,
            labels,
        } => {
            let input = CreateProjectInput {
                name,
                team_ids: resolve_team_ids(&client, &teams).await?,
                description,
                content,
                icon,
                color,
                status_id: resolve_status_id(&client, status.as_deref()).await?,
                priority,
                lead_id: resolve_user_id(&client, lead.as_deref()).await?,
                start_date,
                target_date,
                member_ids: resolve_user_ids(&client, members.as_deref()).await?,
                label_ids: resolve_project_label_ids(&client, labels.as_deref()).await?,
            };
            let id = client.create_project(&input).await?;
            let project = client.fetch_project(&id, workspace).await?;
            db.upsert_project(&project)?;
            print_json(&project)
        }
        ProjectAction::Update {
            id,
            name,
            teams,
            description,
            content,
            icon,
            color,
            status,
            priority,
            lead,
            start_date,
            target_date,
            members,
            labels,
        } => {
            let id = client.find_project_by_name(&id).await?;
            let input = UpdateProjectInput {
                name,
                team_ids: match teams.as_deref() {
                    Some(teams) => Some(resolve_team_ids(&client, teams).await?),
                    None => None,
                },
                description,
                content,
                icon,
                color,
                status_id: resolve_status_id(&client, status.as_deref()).await?,
                priority,
                lead_id: resolve_user_id(&client, lead.as_deref()).await?,
                start_date,
                target_date,
                member_ids: resolve_user_ids(&client, members.as_deref()).await?,
                label_ids: resolve_project_label_ids(&client, labels.as_deref()).await?,
            };
            client.update_project(&id, &input).await?;
            let project = client.fetch_project(&id, workspace).await?;
            db.upsert_project(&project)?;
            print_json(&project)
        }
        ProjectAction::Delete { id } => {
            let id = client.find_project_by_name(&id).await?;
            client.delete_project(&id).await?;
            db.delete_project_local(&id)?;
            print_json(&serde_json::json!({ "status": "deleted", "project_id": id }))
        }
        ProjectAction::Import { id } => {
            print_json(&client.import_project(db, workspace, &id).await?)
        }
        ProjectAction::Sync => {
            let (projects, milestones) = client.sync_projects(db, workspace).await?;
            print_json(&serde_json::json!({
                "projects": projects,
                "milestones": milestones,
            }))
        }
    }
}

pub async fn handle_milestone_action(
    action: MilestoneAction,
    db: &Database,
    config: &Config,
    workspace: &str,
) -> Result<()> {
    let api_key = config.workspace_api_key(workspace)?;
    let client = LinearClient::with_api_key(&api_key);
    match action {
        MilestoneAction::List {
            project,
            no_refresh,
        } => {
            if !no_refresh {
                client.sync_projects(db, workspace).await?;
            }
            let project_id = client.find_project_by_name(&project).await?;
            print_json(&db.list_project_milestones(&project_id)?)
        }
        MilestoneAction::Show {
            id,
            project,
            include_issues,
            no_refresh,
        } => {
            let project_id = resolve_project_id(&client, project.as_deref()).await?;
            if include_issues {
                return print_json(
                    &client
                        .import_project_milestone(db, workspace, project_id.as_deref(), &id)
                        .await?,
                );
            }
            if !no_refresh {
                client.sync_projects(db, workspace).await?;
            }
            print_json(
                &db.get_project_milestone(workspace, &id, project_id.as_deref())?
                    .with_context(|| format!("Project milestone '{id}' not found"))?,
            )
        }
        MilestoneAction::Create {
            project,
            name,
            description,
            target_date,
            sort_order,
        } => {
            let project_id = client.find_project_by_name(&project).await?;
            let id = client
                .create_project_milestone(&CreateProjectMilestoneInput {
                    project_id,
                    name,
                    description,
                    target_date,
                    sort_order,
                })
                .await?;
            print_json(&cache_milestone(&client, db, workspace, &id).await?)
        }
        MilestoneAction::Update {
            id,
            project,
            name,
            description,
            target_date,
            sort_order,
        } => {
            let project_id = resolve_project_id(&client, project.as_deref()).await?;
            let id = client
                .find_project_milestone(project_id.as_deref(), &id)
                .await?;
            client
                .update_project_milestone(
                    &id,
                    &UpdateProjectMilestoneInput {
                        project_id,
                        name,
                        description,
                        target_date,
                        sort_order,
                    },
                )
                .await?;
            print_json(&cache_milestone(&client, db, workspace, &id).await?)
        }
        MilestoneAction::Delete { id, project } => {
            let project_id = resolve_project_id(&client, project.as_deref()).await?;
            let id = client
                .find_project_milestone(project_id.as_deref(), &id)
                .await?;
            client.delete_project_milestone(&id).await?;
            db.delete_project_milestone_local(&id)?;
            print_json(&serde_json::json!({ "status": "deleted", "milestone_id": id }))
        }
        MilestoneAction::Import { id, project } => {
            let project_id = resolve_project_id(&client, project.as_deref()).await?;
            print_json(
                &client
                    .import_project_milestone(db, workspace, project_id.as_deref(), &id)
                    .await?,
            )
        }
    }
}

async fn resolve_team_ids(client: &LinearClient, values: &[String]) -> Result<Vec<String>> {
    let teams = client.list_teams().await?;
    values
        .iter()
        .map(|value| {
            teams
                .iter()
                .find(|team| {
                    team.id == *value
                        || team.key.eq_ignore_ascii_case(value)
                        || team.name.eq_ignore_ascii_case(value)
                })
                .map(|team| team.id.clone())
                .with_context(|| format!("Team '{value}' not found"))
        })
        .collect()
}

async fn resolve_status_id(client: &LinearClient, status: Option<&str>) -> Result<Option<String>> {
    match status {
        Some(status) => Ok(Some(client.get_project_status_id(status).await?)),
        None => Ok(None),
    }
}

async fn resolve_user_id(client: &LinearClient, user: Option<&str>) -> Result<Option<String>> {
    match user {
        Some(user) => Ok(Some(client.resolve_assignee_id(user).await?)),
        None => Ok(None),
    }
}

async fn resolve_user_ids(
    client: &LinearClient,
    users: Option<&[String]>,
) -> Result<Option<Vec<String>>> {
    let Some(users) = users else { return Ok(None) };
    if users.len() == 1 && users[0].eq_ignore_ascii_case("none") {
        return Ok(Some(Vec::new()));
    }
    let mut ids = Vec::new();
    for user in users {
        ids.push(client.resolve_assignee_id(user).await?);
    }
    Ok(Some(ids))
}

async fn resolve_project_label_ids(
    client: &LinearClient,
    labels: Option<&[String]>,
) -> Result<Option<Vec<String>>> {
    match labels {
        Some(labels) if labels.len() == 1 && labels[0].eq_ignore_ascii_case("none") => {
            Ok(Some(Vec::new()))
        }
        Some(labels) => Ok(Some(client.get_project_label_ids(labels).await?)),
        None => Ok(None),
    }
}

async fn resolve_project_id(
    client: &LinearClient,
    project: Option<&str>,
) -> Result<Option<String>> {
    match project {
        Some(project) => Ok(Some(client.find_project_by_name(project).await?)),
        None => Ok(None),
    }
}

async fn cache_milestone(
    client: &LinearClient,
    db: &Database,
    workspace: &str,
    milestone_id: &str,
) -> Result<crate::db::ProjectMilestone> {
    let milestone = client
        .fetch_project_milestone(milestone_id, workspace)
        .await?;
    let project = client
        .fetch_project(&milestone.project_id, workspace)
        .await?;
    db.upsert_project(&project)?;
    db.upsert_project_milestone(&milestone)?;
    Ok(milestone)
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
