use std::future::ready;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde::Deserialize;
use uuid::Uuid;

use crate::db::{self, Database};

use super::pagination::{paginate, ConnectionPage, LinearOperation, PageInfo};
use super::{IssueConnection, LinearClient};

// Linear charges query complexity per connection item, including every nested
// connection selected for that item. Project metadata is unusually rich, so
// conservative page sizes keep these queries below the workspace complexity
// limit even when a workspace has hundreds of projects.
const MILESTONE_PAGE_SIZE: usize = 50;
const RESOURCE_LOOKUP_PAGE_SIZE: usize = 50;

const PROJECT_FIELDS: &str = r#"
    id slugId name description content icon color priority
    startDate targetDate createdAt updatedAt archivedAt url progress
    status { id name type color }
    lead { id name }
"#;

const MILESTONE_FIELDS: &str = r#"
    id name description targetDate status progress sortOrder
    createdAt updatedAt archivedAt
    project { id name }
"#;

const ISSUE_FIELDS: &str = r#"
    id identifier url title description priority branchName
    createdAt updatedAt
    state { name type }
    team { key }
    assignee { name }
    project { id name }
    projectMilestone { id name }
    cycle { id name number }
"#;

#[derive(Debug, Clone, Default)]
pub struct CreateProjectInput {
    pub name: String,
    pub team_ids: Vec<String>,
    pub description: Option<String>,
    pub content: Option<String>,
    pub icon: Option<String>,
    pub color: Option<String>,
    pub status_id: Option<String>,
    pub priority: Option<i32>,
    pub lead_id: Option<String>,
    pub start_date: Option<String>,
    pub target_date: Option<String>,
    pub member_ids: Option<Vec<String>>,
    pub label_ids: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default)]
pub struct UpdateProjectInput {
    pub name: Option<String>,
    pub team_ids: Option<Vec<String>>,
    pub description: Option<String>,
    pub content: Option<String>,
    pub icon: Option<String>,
    pub color: Option<String>,
    pub status_id: Option<String>,
    pub priority: Option<i32>,
    pub lead_id: Option<String>,
    pub start_date: Option<String>,
    pub target_date: Option<String>,
    pub member_ids: Option<Vec<String>>,
    pub label_ids: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default)]
pub struct CreateProjectMilestoneInput {
    pub project_id: String,
    pub name: String,
    pub description: Option<String>,
    pub target_date: Option<String>,
    pub sort_order: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct UpdateProjectMilestoneInput {
    pub project_id: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub target_date: Option<String>,
    pub sort_order: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct ProjectConnectionData {
    projects: ProjectConnection,
}

#[derive(Debug, Deserialize)]
struct ProjectConnection {
    nodes: Vec<LinearProjectNode>,
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
}

#[derive(Debug, Deserialize)]
struct SingleProjectData {
    project: LinearProjectNode,
}

#[derive(Debug, Deserialize)]
struct LinearProjectNode {
    id: String,
    #[serde(rename = "slugId")]
    slug_id: String,
    name: String,
    description: String,
    content: Option<String>,
    icon: Option<String>,
    color: String,
    status: LinearProjectStatus,
    lead: Option<LinearProjectUser>,
    #[serde(default)]
    teams: LinearProjectTeamConnection,
    #[serde(default)]
    members: LinearProjectUserConnection,
    #[serde(default)]
    labels: LinearProjectLabelConnection,
    priority: i32,
    #[serde(rename = "startDate")]
    start_date: Option<String>,
    #[serde(rename = "targetDate")]
    target_date: Option<String>,
    #[serde(rename = "createdAt")]
    created_at: String,
    #[serde(rename = "updatedAt")]
    updated_at: String,
    #[serde(rename = "archivedAt")]
    archived_at: Option<String>,
    url: String,
    progress: f64,
}

#[derive(Debug, Deserialize)]
struct LinearProjectStatus {
    id: String,
    name: String,
    #[serde(rename = "type")]
    status_type: String,
    color: String,
}

#[derive(Debug, Deserialize)]
struct LinearProjectUser {
    id: String,
    name: String,
}

#[derive(Debug, Deserialize, Default)]
struct LinearProjectUserConnection {
    nodes: Vec<LinearProjectUser>,
}

#[derive(Debug, Deserialize)]
struct LinearProjectTeam {
    id: String,
    key: String,
    name: String,
}

#[derive(Debug, Deserialize, Default)]
struct LinearProjectTeamConnection {
    nodes: Vec<LinearProjectTeam>,
}

#[derive(Debug, Deserialize)]
struct LinearProjectLabel {
    id: String,
    name: String,
    color: String,
    description: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct LinearProjectLabelConnection {
    nodes: Vec<LinearProjectLabel>,
}

#[derive(Debug, Deserialize)]
struct ProjectTeamsData {
    project: ProjectTeamsNode,
}

#[derive(Debug, Deserialize)]
struct ProjectTeamsNode {
    teams: PaginatedProjectTeamConnection,
}

#[derive(Debug, Deserialize)]
struct PaginatedProjectTeamConnection {
    nodes: Vec<LinearProjectTeam>,
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
}

#[derive(Debug, Deserialize)]
struct ProjectMembersData {
    project: ProjectMembersNode,
}

#[derive(Debug, Deserialize)]
struct ProjectMembersNode {
    members: PaginatedProjectMemberConnection,
}

#[derive(Debug, Deserialize)]
struct PaginatedProjectMemberConnection {
    nodes: Vec<LinearProjectUser>,
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
}

#[derive(Debug, Deserialize)]
struct ProjectLabelsData {
    project: ProjectLabelsNode,
}

#[derive(Debug, Deserialize)]
struct ProjectLabelsNode {
    labels: PaginatedProjectLabelConnection,
}

#[derive(Debug, Deserialize)]
struct PaginatedProjectLabelConnection {
    nodes: Vec<LinearProjectLabel>,
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
}

#[derive(Debug, Deserialize)]
struct MilestoneConnectionData {
    #[serde(rename = "projectMilestones")]
    project_milestones: MilestoneConnection,
}

#[derive(Debug, Deserialize)]
struct MilestoneConnection {
    nodes: Vec<LinearProjectMilestoneNode>,
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
}

#[derive(Debug, Deserialize)]
struct SingleMilestoneData {
    #[serde(rename = "projectMilestone")]
    project_milestone: LinearProjectMilestoneNode,
}

#[derive(Debug, Deserialize)]
struct ProjectMilestonesData {
    project: ProjectMilestones,
}

#[derive(Debug, Deserialize)]
struct ProjectMilestones {
    #[serde(rename = "projectMilestones")]
    project_milestones: MilestoneConnection,
}

#[derive(Debug, Deserialize)]
struct LinearProjectMilestoneNode {
    id: String,
    name: String,
    description: Option<String>,
    #[serde(rename = "targetDate")]
    target_date: Option<String>,
    status: String,
    progress: f64,
    #[serde(rename = "sortOrder")]
    sort_order: f64,
    #[serde(rename = "createdAt")]
    created_at: String,
    #[serde(rename = "updatedAt")]
    updated_at: String,
    #[serde(rename = "archivedAt")]
    archived_at: Option<String>,
    project: LinearProjectRef,
}

#[derive(Debug, Deserialize)]
struct LinearProjectRef {
    id: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct ProjectIssuesData {
    project: ProjectIssues,
}

#[derive(Debug, Deserialize)]
struct ProjectIssues {
    issues: IssueConnection,
}

#[derive(Debug, Deserialize)]
struct MilestoneIssuesData {
    #[serde(rename = "projectMilestone")]
    project_milestone: MilestoneIssues,
}

#[derive(Debug, Deserialize)]
struct MilestoneIssues {
    issues: IssueConnection,
}

#[derive(Debug, Deserialize)]
struct MutationProjectData {
    #[serde(rename = "projectCreate", alias = "projectUpdate")]
    payload: ProjectPayload,
}

#[derive(Debug, Deserialize)]
struct ProjectPayload {
    success: bool,
    project: Option<MutationResource>,
}

#[derive(Debug, Deserialize)]
struct MutationMilestoneData {
    #[serde(rename = "projectMilestoneCreate", alias = "projectMilestoneUpdate")]
    payload: MilestonePayload,
}

#[derive(Debug, Deserialize)]
struct MilestonePayload {
    success: bool,
    #[serde(rename = "projectMilestone")]
    project_milestone: Option<MutationResource>,
}

#[derive(Debug, Deserialize)]
struct MutationResource {
    id: String,
}

#[derive(Debug, Deserialize)]
struct DeleteProjectData {
    #[serde(rename = "projectDelete")]
    project_delete: SuccessPayload,
}

#[derive(Debug, Deserialize)]
struct DeleteMilestoneData {
    #[serde(rename = "projectMilestoneDelete")]
    project_milestone_delete: SuccessPayload,
}

#[derive(Debug, Deserialize)]
struct SuccessPayload {
    success: bool,
}

impl LinearClient {
    pub async fn fetch_projects(
        &self,
        after_cursor: Option<&str>,
        include_archived: bool,
        workspace_id: &str,
    ) -> Result<(Vec<db::Project>, bool, Option<String>)> {
        let mut page = self
            .fetch_projects_page(
                after_cursor,
                include_archived,
                workspace_id,
                self.sync_query_config().page_size(LinearOperation::Projects),
                None,
            )
            .await?;
        for project in &mut page.nodes {
            project.teams = self.fetch_all_project_teams(&project.id).await?;
            project.members = self.fetch_all_project_members(&project.id).await?;
            project.labels = self.fetch_all_project_labels(&project.id).await?;
        }
        Ok((
            page.nodes,
            page.page_info.has_next_page,
            page.page_info.end_cursor,
        ))
    }

    async fn fetch_projects_page(
        &self,
        after_cursor: Option<&str>,
        include_archived: bool,
        workspace_id: &str,
        page_size: usize,
        team_id: Option<&str>,
    ) -> Result<ConnectionPage<db::Project>> {
        let (variables, filter) = if team_id.is_some() {
            (
                "$first: Int!, $after: String, $includeArchived: Boolean!, $teamId: ID!",
                "filter: { accessibleTeams: { some: { id: { eq: $teamId } } } },",
            )
        } else {
            (
                "$first: Int!, $after: String, $includeArchived: Boolean!",
                "",
            )
        };
        let query = format!(
            r#"
            query({variables}) {{
                projects(
                    first: $first,
                    after: $after,
                    {filter}
                    includeArchived: $includeArchived,
                    orderBy: updatedAt
                ) {{
                    nodes {{ __PROJECT_FIELDS__ }}
                    pageInfo {{ hasNextPage endCursor }}
                }}
            }}
        "#
        )
        .replace("__PROJECT_FIELDS__", PROJECT_FIELDS)
        .replace("{filter}", filter);
        let data: ProjectConnectionData = self
            .query_operation(
                LinearOperation::Projects.name(),
                after_cursor,
                &query,
                serde_json::json!({
                    "first": page_size,
                    "after": after_cursor,
                    "includeArchived": include_archived,
                    "teamId": team_id,
                }),
            )
            .await?;
        Ok(ConnectionPage {
            nodes: data
                .projects
                .nodes
                .into_iter()
                .map(|project| convert_project(project, workspace_id))
                .collect(),
            page_info: data.projects.page_info,
        })
    }

    pub async fn fetch_project(&self, id: &str, workspace_id: &str) -> Result<db::Project> {
        let query = r#"
            query($id: String!) {
                project(id: $id) { __PROJECT_FIELDS__ }
            }
        "#
        .replace("__PROJECT_FIELDS__", PROJECT_FIELDS);
        let data: SingleProjectData = self.query(&query, serde_json::json!({ "id": id })).await?;
        let mut project = convert_project(data.project, workspace_id);
        project.teams = self.fetch_all_project_teams(id).await?;
        project.members = self.fetch_all_project_members(id).await?;
        project.labels = self.fetch_all_project_labels(id).await?;
        Ok(project)
    }

    async fn fetch_project_teams_page(
        &self,
        project_id: &str,
        cursor: Option<&str>,
        page_size: usize,
    ) -> Result<ConnectionPage<db::ProjectTeam>> {
        let query = r#"
            query($id: String!, $first: Int!, $after: String) {
                project(id: $id) {
                    teams(first: $first, after: $after, orderBy: updatedAt) {
                        nodes { id key name }
                        pageInfo { hasNextPage endCursor }
                    }
                }
            }
        "#;
        let data: ProjectTeamsData = self
            .query_operation(
                LinearOperation::ProjectTeams.name(),
                cursor,
                query,
                serde_json::json!({
                    "id": project_id,
                    "first": page_size,
                    "after": cursor,
                }),
            )
            .await?;
        Ok(ConnectionPage {
            nodes: data
                .project
                .teams
                .nodes
                .into_iter()
                .map(|team| db::ProjectTeam { id: team.id, key: team.key, name: team.name })
                .collect(),
            page_info: data.project.teams.page_info,
        })
    }

    async fn fetch_project_members_page(
        &self,
        project_id: &str,
        cursor: Option<&str>,
        page_size: usize,
    ) -> Result<ConnectionPage<db::ProjectMember>> {
        let query = r#"
            query($id: String!, $first: Int!, $after: String) {
                project(id: $id) {
                    members(first: $first, after: $after, orderBy: updatedAt) {
                        nodes { id name }
                        pageInfo { hasNextPage endCursor }
                    }
                }
            }
        "#;
        let data: ProjectMembersData = self
            .query_operation(
                LinearOperation::ProjectMembers.name(),
                cursor,
                query,
                serde_json::json!({
                    "id": project_id,
                    "first": page_size,
                    "after": cursor,
                }),
            )
            .await?;
        Ok(ConnectionPage {
            nodes: data
                .project
                .members
                .nodes
                .into_iter()
                .map(|member| db::ProjectMember { id: member.id, name: member.name })
                .collect(),
            page_info: data.project.members.page_info,
        })
    }

    async fn fetch_project_labels_page(
        &self,
        project_id: &str,
        cursor: Option<&str>,
        page_size: usize,
    ) -> Result<ConnectionPage<db::ProjectLabel>> {
        let query = r#"
            query($id: String!, $first: Int!, $after: String) {
                project(id: $id) {
                    labels(first: $first, after: $after, orderBy: updatedAt) {
                        nodes { id name color description }
                        pageInfo { hasNextPage endCursor }
                    }
                }
            }
        "#;
        let data: ProjectLabelsData = self
            .query_operation(
                LinearOperation::ProjectLabels.name(),
                cursor,
                query,
                serde_json::json!({
                    "id": project_id,
                    "first": page_size,
                    "after": cursor,
                }),
            )
            .await?;
        Ok(ConnectionPage {
            nodes: data
                .project
                .labels
                .nodes
                .into_iter()
                .map(|label| db::ProjectLabel {
                    id: label.id,
                    name: label.name,
                    color: label.color,
                    description: label.description,
                })
                .collect(),
            page_info: data.project.labels.page_info,
        })
    }

    async fn fetch_all_project_teams(&self, project_id: &str) -> Result<Vec<db::ProjectTeam>> {
        let mut teams = Vec::new();
        paginate(
            self.sync_query_config(),
            LinearOperation::ProjectTeams,
            Some(project_id.to_string()),
            |request| async move {
                self.fetch_project_teams_page(
                    project_id,
                    request.cursor.as_deref(),
                    request.page_size,
                )
                .await
            },
            |nodes, _| {
                teams.extend(nodes);
                ready(Ok(()))
            },
            |team| team.id.clone(),
            |event| self.observe_sync_event(event),
        )
        .await?;
        Ok(teams)
    }

    async fn fetch_all_project_members(&self, project_id: &str) -> Result<Vec<db::ProjectMember>> {
        let mut members = Vec::new();
        paginate(
            self.sync_query_config(),
            LinearOperation::ProjectMembers,
            Some(project_id.to_string()),
            |request| async move {
                self.fetch_project_members_page(
                    project_id,
                    request.cursor.as_deref(),
                    request.page_size,
                )
                .await
            },
            |nodes, _| {
                members.extend(nodes);
                ready(Ok(()))
            },
            |member| member.id.clone(),
            |event| self.observe_sync_event(event),
        )
        .await?;
        Ok(members)
    }

    async fn fetch_all_project_labels(&self, project_id: &str) -> Result<Vec<db::ProjectLabel>> {
        let mut labels = Vec::new();
        paginate(
            self.sync_query_config(),
            LinearOperation::ProjectLabels,
            Some(project_id.to_string()),
            |request| async move {
                self.fetch_project_labels_page(
                    project_id,
                    request.cursor.as_deref(),
                    request.page_size,
                )
                .await
            },
            |nodes, _| {
                labels.extend(nodes);
                ready(Ok(()))
            },
            |label| label.id.clone(),
            |event| self.observe_sync_event(event),
        )
        .await?;
        Ok(labels)
    }

    async fn sync_project_connections(
        &self,
        db: &Database,
        project_id: &str,
        sync_token: &str,
    ) -> Result<()> {
        paginate(
            self.sync_query_config(),
            LinearOperation::ProjectTeams,
            Some(project_id.to_string()),
            |request| async move {
                self.fetch_project_teams_page(
                    project_id,
                    request.cursor.as_deref(),
                    request.page_size,
                )
                .await
            },
            |teams, _| ready(db.upsert_project_team_page(project_id, &teams, sync_token)),
            |team| team.id.clone(),
            |event| self.observe_sync_event(event),
        )
        .await?;
        db.complete_project_team_sync(project_id, sync_token)?;

        paginate(
            self.sync_query_config(),
            LinearOperation::ProjectMembers,
            Some(project_id.to_string()),
            |request| async move {
                self.fetch_project_members_page(
                    project_id,
                    request.cursor.as_deref(),
                    request.page_size,
                )
                .await
            },
            |members, _| ready(db.upsert_project_member_page(project_id, &members, sync_token)),
            |member| member.id.clone(),
            |event| self.observe_sync_event(event),
        )
        .await?;
        db.complete_project_member_sync(project_id, sync_token)?;

        paginate(
            self.sync_query_config(),
            LinearOperation::ProjectLabels,
            Some(project_id.to_string()),
            |request| async move {
                self.fetch_project_labels_page(
                    project_id,
                    request.cursor.as_deref(),
                    request.page_size,
                )
                .await
            },
            |labels, _| ready(db.upsert_project_label_page(project_id, &labels, sync_token)),
            |label| label.id.clone(),
            |event| self.observe_sync_event(event),
        )
        .await?;
        db.complete_project_label_sync(project_id, sync_token)?;
        Ok(())
    }

    pub async fn fetch_project_milestones(
        &self,
        after_cursor: Option<&str>,
        include_archived: bool,
        workspace_id: &str,
    ) -> Result<(Vec<db::ProjectMilestone>, bool, Option<String>)> {
        let query = r#"
            query($after: String, $includeArchived: Boolean!) {
                projectMilestones(
                    first: __MILESTONE_PAGE_SIZE__,
                    after: $after,
                    includeArchived: $includeArchived,
                    orderBy: updatedAt
                ) {
                    nodes { __MILESTONE_FIELDS__ }
                    pageInfo { hasNextPage endCursor }
                }
            }
        "#
        .replace("__MILESTONE_FIELDS__", MILESTONE_FIELDS)
        .replace("__MILESTONE_PAGE_SIZE__", &MILESTONE_PAGE_SIZE.to_string());
        let data: MilestoneConnectionData = self
            .query(
                &query,
                serde_json::json!({
                    "after": after_cursor,
                    "includeArchived": include_archived,
                }),
            )
            .await?;
        Ok((
            data.project_milestones
                .nodes
                .into_iter()
                .map(|milestone| convert_milestone(milestone, workspace_id))
                .collect(),
            data.project_milestones.page_info.has_next_page,
            data.project_milestones.page_info.end_cursor,
        ))
    }

    pub async fn fetch_project_milestone(
        &self,
        id: &str,
        workspace_id: &str,
    ) -> Result<db::ProjectMilestone> {
        let query = r#"
            query($id: String!) {
                projectMilestone(id: $id) { __MILESTONE_FIELDS__ }
            }
        "#
        .replace("__MILESTONE_FIELDS__", MILESTONE_FIELDS);
        let data: SingleMilestoneData = self.query(&query, serde_json::json!({ "id": id })).await?;
        Ok(convert_milestone(data.project_milestone, workspace_id))
    }

    pub async fn fetch_milestones_for_project(
        &self,
        project_id: &str,
        after_cursor: Option<&str>,
        include_archived: bool,
        workspace_id: &str,
    ) -> Result<(Vec<db::ProjectMilestone>, bool, Option<String>)> {
        let page = self
            .fetch_milestones_for_project_page(
                project_id,
                after_cursor,
                include_archived,
                workspace_id,
                self.sync_query_config()
                    .page_size(LinearOperation::ProjectMilestones),
            )
            .await?;
        Ok((
            page.nodes,
            page.page_info.has_next_page,
            page.page_info.end_cursor,
        ))
    }

    async fn fetch_milestones_for_project_page(
        &self,
        project_id: &str,
        after_cursor: Option<&str>,
        include_archived: bool,
        workspace_id: &str,
        page_size: usize,
    ) -> Result<ConnectionPage<db::ProjectMilestone>> {
        let query = r#"
            query($id: String!, $first: Int!, $after: String, $includeArchived: Boolean!) {
                project(id: $id) {
                    projectMilestones(
                        first: $first,
                        after: $after,
                        includeArchived: $includeArchived,
                        orderBy: updatedAt
                    ) {
                        nodes { __MILESTONE_FIELDS__ }
                        pageInfo { hasNextPage endCursor }
                    }
                }
            }
        "#
        .replace("__MILESTONE_FIELDS__", MILESTONE_FIELDS);
        let data: ProjectMilestonesData = self
            .query_operation(
                LinearOperation::ProjectMilestones.name(),
                after_cursor,
                &query,
                serde_json::json!({
                    "id": project_id,
                    "first": page_size,
                    "after": after_cursor,
                    "includeArchived": include_archived,
                }),
            )
            .await?;
        let milestones = data.project.project_milestones;
        Ok(ConnectionPage {
            nodes: milestones
                .nodes
                .into_iter()
                .map(|milestone| convert_milestone(milestone, workspace_id))
                .collect(),
            page_info: milestones.page_info,
        })
    }

    pub async fn sync_projects(&self, db: &Database, workspace_id: &str) -> Result<(usize, usize)> {
        self.sync_projects_scoped(db, workspace_id, None, None, true)
            .await
    }

    pub async fn sync_projects_for_team(
        &self,
        db: &Database,
        workspace_id: &str,
        team_key: &str,
        include_archived: bool,
    ) -> Result<(usize, usize)> {
        let team_id = self.get_team_id(team_key).await?;
        self.sync_projects_scoped(
            db,
            workspace_id,
            Some(&team_id),
            Some(team_key),
            include_archived,
        )
            .await
    }

    async fn sync_projects_scoped(
        &self,
        db: &Database,
        workspace_id: &str,
        team_id: Option<&str>,
        team_key: Option<&str>,
        include_archived: bool,
    ) -> Result<(usize, usize)> {
        let sync_token = Uuid::new_v4().to_string();
        let scope = team_key.unwrap_or("*");
        for (family, page_size) in [
            (
                "projects",
                Some(self.sync_query_config().page_size(LinearOperation::Projects)),
            ),
            (
                "project milestones",
                Some(
                    self.sync_query_config()
                        .page_size(LinearOperation::ProjectMilestones),
                ),
            ),
        ] {
            db.mark_sync_family_running(
                workspace_id,
                scope,
                family,
                None,
                page_size,
                &sync_token,
            )?;
        }
        let project_result = paginate(
            self.sync_query_config(),
            LinearOperation::Projects,
            team_key.map(ToString::to_string),
            |request| async move {
                self.fetch_projects_page(
                    request.cursor.as_deref(),
                    include_archived,
                    workspace_id,
                    request.page_size,
                    team_id,
                )
                .await
            },
            |projects, context| {
                let result = (|| {
                    for project in projects {
                        db.upsert_project_metadata(&project)?;
                        db.mark_project_sync_token(&project.id, &sync_token)?;
                    }
                    db.mark_sync_family_running(
                        workspace_id,
                        scope,
                        "projects",
                        context.cursor.as_deref(),
                        Some(context.page_size),
                        &sync_token,
                    )
                })();
                ready(result)
            },
            |project| project.id.clone(),
            |event| self.observe_sync_event(event),
        )
        .await;
        let project_stats = match project_result {
            Ok(stats) => stats,
            Err(error) => {
                let message = self.redacted_error_message(&error);
                for family in ["projects", "project milestones"] {
                    db.mark_sync_family_failed(
                        workspace_id,
                        scope,
                        family,
                        &sync_token,
                        &message,
                    )?;
                }
                return Err(error);
            }
        };

        let mut milestone_total = 0;
        let hydration_result: Result<()> = async {
            let mut after_id = None;
            loop {
                let project_ids = db.list_project_ids_for_sync_token(
                    workspace_id,
                    &sync_token,
                    after_id.as_deref(),
                    100,
                )?;
                if project_ids.is_empty() {
                    break;
                }
                for project_id in &project_ids {
                    self.sync_project_connections(db, project_id, &sync_token)
                        .await?;
                    milestone_total += self
                        .sync_project_milestones(
                            db,
                            workspace_id,
                            project_id,
                            &sync_token,
                            include_archived,
                        )
                        .await?;
                }
                after_id = project_ids.last().cloned();
            }
            Ok(())
        }
        .await;
        if let Err(error) = hydration_result {
            let message = self.redacted_error_message(&error);
            for family in ["projects", "project milestones"] {
                db.mark_sync_family_failed(
                    workspace_id,
                    scope,
                    family,
                    &sync_token,
                    &message,
                )?;
            }
            return Err(error);
        }

        if let Some(team_key) = team_key {
            db.reconcile_team_projects(workspace_id, team_key, &sync_token)?;
        } else {
            db.reconcile_workspace_projects(workspace_id, &sync_token)?;
        }
        db.mark_sync_family_complete(
            workspace_id,
            scope,
            "projects",
            Some(self.sync_query_config().page_size(LinearOperation::Projects)),
            &sync_token,
        )?;
        db.mark_sync_family_complete(
            workspace_id,
            scope,
            "project milestones",
            Some(
                self.sync_query_config()
                    .page_size(LinearOperation::ProjectMilestones),
            ),
            &sync_token,
        )?;
        Ok((project_stats.nodes, milestone_total))
    }

    async fn sync_project_milestones(
        &self,
        db: &Database,
        workspace_id: &str,
        project_id: &str,
        sync_token: &str,
        include_archived: bool,
    ) -> Result<usize> {
        let stats = paginate(
            self.sync_query_config(),
            LinearOperation::ProjectMilestones,
            Some(project_id.to_string()),
            |request| async move {
                self.fetch_milestones_for_project_page(
                    project_id,
                    request.cursor.as_deref(),
                    include_archived,
                    workspace_id,
                    request.page_size,
                )
                .await
            },
            |milestones, _| {
                let result = (|| {
                    for milestone in milestones {
                        db.upsert_project_milestone(&milestone)?;
                        db.mark_project_milestone_sync_token(&milestone.id, sync_token)?;
                    }
                    Ok(())
                })();
                ready(result)
            },
            |milestone| milestone.id.clone(),
            |event| self.observe_sync_event(event),
        )
        .await?;
        db.reconcile_project_milestones_by_token(project_id, sync_token)?;
        Ok(stats.nodes)
    }

    pub async fn create_project(&self, input: &CreateProjectInput) -> Result<String> {
        if input.name.trim().is_empty() {
            anyhow::bail!("Project name cannot be empty");
        }
        if input.team_ids.is_empty() {
            anyhow::bail!("At least one team is required to create a project");
        }
        let graphql_input = project_create_value(input);
        let query = r#"
            mutation($input: ProjectCreateInput!) {
                projectCreate(input: $input) {
                    success
                    project { id }
                }
            }
        "#;
        let data: MutationProjectData = self
            .query(query, serde_json::json!({ "input": graphql_input }))
            .await?;
        if !data.payload.success {
            anyhow::bail!("Failed to create project");
        }
        data.payload
            .project
            .map(|project| project.id)
            .context("Linear did not return the created project")
    }

    pub async fn update_project(&self, id: &str, input: &UpdateProjectInput) -> Result<()> {
        let graphql_input = project_update_value(input);
        if graphql_input.is_empty() {
            anyhow::bail!("No project fields were provided to update");
        }
        let query = r#"
            mutation($id: String!, $input: ProjectUpdateInput!) {
                projectUpdate(id: $id, input: $input) {
                    success
                    project { id }
                }
            }
        "#;
        let data: MutationProjectData = self
            .query(
                query,
                serde_json::json!({ "id": id, "input": graphql_input }),
            )
            .await?;
        if !data.payload.success {
            anyhow::bail!("Failed to update project");
        }
        Ok(())
    }

    pub async fn delete_project(&self, id: &str) -> Result<()> {
        let query = r#"
            mutation($id: String!) {
                projectDelete(id: $id) { success }
            }
        "#;
        let data: DeleteProjectData = self.query(query, serde_json::json!({ "id": id })).await?;
        if !data.project_delete.success {
            anyhow::bail!("Failed to delete project");
        }
        Ok(())
    }

    pub async fn create_project_milestone(
        &self,
        input: &CreateProjectMilestoneInput,
    ) -> Result<String> {
        if input.name.trim().is_empty() {
            anyhow::bail!("Milestone name cannot be empty");
        }
        let graphql_input = milestone_create_value(input);
        let query = r#"
            mutation($input: ProjectMilestoneCreateInput!) {
                projectMilestoneCreate(input: $input) {
                    success
                    projectMilestone { id }
                }
            }
        "#;
        let data: MutationMilestoneData = self
            .query(query, serde_json::json!({ "input": graphql_input }))
            .await?;
        if !data.payload.success {
            anyhow::bail!("Failed to create project milestone");
        }
        data.payload
            .project_milestone
            .map(|milestone| milestone.id)
            .context("Linear did not return the created milestone")
    }

    pub async fn update_project_milestone(
        &self,
        id: &str,
        input: &UpdateProjectMilestoneInput,
    ) -> Result<()> {
        let graphql_input = milestone_update_value(input);
        if graphql_input.is_empty() {
            anyhow::bail!("No milestone fields were provided to update");
        }
        let query = r#"
            mutation($id: String!, $input: ProjectMilestoneUpdateInput!) {
                projectMilestoneUpdate(id: $id, input: $input) {
                    success
                    projectMilestone { id }
                }
            }
        "#;
        let data: MutationMilestoneData = self
            .query(
                query,
                serde_json::json!({ "id": id, "input": graphql_input }),
            )
            .await?;
        if !data.payload.success {
            anyhow::bail!("Failed to update project milestone");
        }
        Ok(())
    }

    pub async fn delete_project_milestone(&self, id: &str) -> Result<()> {
        let query = r#"
            mutation($id: String!) {
                projectMilestoneDelete(id: $id) { success }
            }
        "#;
        let data: DeleteMilestoneData = self.query(query, serde_json::json!({ "id": id })).await?;
        if !data.project_milestone_delete.success {
            anyhow::bail!("Failed to delete project milestone");
        }
        Ok(())
    }

    pub async fn get_project_status_id(&self, status_name: &str) -> Result<String> {
        let query = r#"
            query {
                projectStatuses(first: 250, includeArchived: false) {
                    nodes { id name }
                }
            }
        "#;
        let data: serde_json::Value = self.query(query, serde_json::json!({})).await?;
        find_resource_id(
            &data["projectStatuses"]["nodes"],
            status_name,
            "project status",
        )
    }

    pub async fn get_project_label_ids(&self, names: &[String]) -> Result<Vec<String>> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let mut cursor = None;
        let mut labels = Vec::new();
        loop {
            let query = r#"
                query($after: String) {
                    projectLabels(first: 250, after: $after, includeArchived: false) {
                        nodes { id name }
                        pageInfo { hasNextPage endCursor }
                    }
                }
            "#;
            let data: serde_json::Value = self
                .query(query, serde_json::json!({ "after": cursor }))
                .await?;
            labels.extend(
                data["projectLabels"]["nodes"]
                    .as_array()
                    .context("No project labels in response")?
                    .iter()
                    .filter_map(|label| {
                        Some((
                            label["id"].as_str()?.to_string(),
                            label["name"].as_str()?.to_string(),
                        ))
                    }),
            );
            if !data["projectLabels"]["pageInfo"]["hasNextPage"]
                .as_bool()
                .unwrap_or(false)
            {
                break;
            }
            cursor = data["projectLabels"]["pageInfo"]["endCursor"]
                .as_str()
                .map(ToString::to_string);
        }

        names
            .iter()
            .map(|name| {
                labels
                    .iter()
                    .find(|(id, candidate)| id == name || candidate.eq_ignore_ascii_case(name))
                    .map(|(id, _)| id.clone())
                    .with_context(|| {
                        format!(
                            "Project label '{}' not found. Available: {}",
                            name,
                            labels
                                .iter()
                                .map(|(_, label)| label.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    })
            })
            .collect()
    }

    pub async fn find_project_by_name(&self, id_or_name: &str) -> Result<String> {
        let mut cursor = None;
        let mut available = Vec::new();
        loop {
            let query = r#"
                query($after: String) {
                    projects(first: __LOOKUP_PAGE_SIZE__, after: $after, includeArchived: true) {
                        nodes { id slugId name }
                        pageInfo { hasNextPage endCursor }
                    }
                }
            "#;
            let query = query.replace(
                "__LOOKUP_PAGE_SIZE__",
                &RESOURCE_LOOKUP_PAGE_SIZE.to_string(),
            );
            let data: serde_json::Value = self
                .query(&query, serde_json::json!({ "after": cursor }))
                .await?;
            let nodes = data["projects"]["nodes"]
                .as_array()
                .context("No projects in response")?;
            for project in nodes {
                let id = project["id"].as_str().unwrap_or_default();
                let slug = project["slugId"].as_str().unwrap_or_default();
                let name = project["name"].as_str().unwrap_or_default();
                if id == id_or_name
                    || slug.eq_ignore_ascii_case(id_or_name)
                    || name.eq_ignore_ascii_case(id_or_name)
                {
                    return Ok(id.to_string());
                }
                if !name.is_empty() {
                    available.push(name.to_string());
                }
            }
            if !data["projects"]["pageInfo"]["hasNextPage"]
                .as_bool()
                .unwrap_or(false)
            {
                break;
            }
            cursor = data["projects"]["pageInfo"]["endCursor"]
                .as_str()
                .map(ToString::to_string);
        }
        anyhow::bail!(
            "Project '{}' not found. Available: {}",
            id_or_name,
            available.join(", ")
        )
    }

    pub async fn find_project_milestone(
        &self,
        project_id: Option<&str>,
        id_or_name: &str,
    ) -> Result<String> {
        let mut cursor = None;
        let mut available = Vec::new();
        loop {
            let query = r#"
                query($after: String) {
                    projectMilestones(
                        first: __LOOKUP_PAGE_SIZE__,
                        after: $after,
                        includeArchived: true
                    ) {
                        nodes { id name project { id } }
                        pageInfo { hasNextPage endCursor }
                    }
                }
            "#;
            let query = query.replace(
                "__LOOKUP_PAGE_SIZE__",
                &RESOURCE_LOOKUP_PAGE_SIZE.to_string(),
            );
            let data: serde_json::Value = self
                .query(&query, serde_json::json!({ "after": cursor }))
                .await?;
            let nodes = data["projectMilestones"]["nodes"]
                .as_array()
                .context("No project milestones in response")?;
            for milestone in nodes {
                let id = milestone["id"].as_str().unwrap_or_default();
                let name = milestone["name"].as_str().unwrap_or_default();
                let owning_project = milestone["project"]["id"].as_str().unwrap_or_default();
                if project_id.is_some_and(|expected| expected != owning_project) {
                    continue;
                }
                if id == id_or_name || name.eq_ignore_ascii_case(id_or_name) {
                    return Ok(id.to_string());
                }
                if !name.is_empty() {
                    available.push(name.to_string());
                }
            }
            if !data["projectMilestones"]["pageInfo"]["hasNextPage"]
                .as_bool()
                .unwrap_or(false)
            {
                break;
            }
            cursor = data["projectMilestones"]["pageInfo"]["endCursor"]
                .as_str()
                .map(ToString::to_string);
        }
        anyhow::bail!(
            "Project milestone '{}' not found. Available: {}",
            id_or_name,
            available.join(", ")
        )
    }

    pub async fn import_project(
        &self,
        db: &Database,
        workspace_id: &str,
        id_or_name: &str,
    ) -> Result<db::ProjectBundle> {
        let project_id = self.find_project_by_name(id_or_name).await?;
        let project = self.fetch_project(&project_id, workspace_id).await?;
        db.upsert_project(&project)?;
        self.import_project_milestones(db, workspace_id, &project_id)
            .await?;
        self.import_hierarchy_issues(db, workspace_id, &project_id, false)
            .await?;
        db.get_project_bundle(workspace_id, &project_id)?
            .context("Imported project was not found in the local database")
    }

    pub async fn import_project_milestone(
        &self,
        db: &Database,
        workspace_id: &str,
        project_id: Option<&str>,
        id_or_name: &str,
    ) -> Result<db::ProjectMilestoneBundle> {
        let milestone_id = self.find_project_milestone(project_id, id_or_name).await?;
        let milestone = self
            .fetch_project_milestone(&milestone_id, workspace_id)
            .await?;
        let project = self
            .fetch_project(&milestone.project_id, workspace_id)
            .await?;
        db.upsert_project(&project)?;
        db.upsert_project_milestone(&milestone)?;
        self.import_hierarchy_issues(db, workspace_id, &milestone_id, true)
            .await?;
        db.get_project_milestone_bundle(workspace_id, &milestone_id, None)?
            .context("Imported milestone was not found in the local database")
    }

    async fn import_project_milestones(
        &self,
        db: &Database,
        workspace_id: &str,
        project_id: &str,
    ) -> Result<()> {
        let sync_token = Uuid::new_v4().to_string();
        self.sync_project_milestones(db, workspace_id, project_id, &sync_token, true)
            .await?;
        Ok(())
    }

    async fn import_hierarchy_issues(
        &self,
        db: &Database,
        workspace_id: &str,
        resource_id: &str,
        milestone: bool,
    ) -> Result<()> {
        self.sync_labels_catalog(db, workspace_id).await?;
        let issue_ids = Arc::new(Mutex::new(Vec::new()));
        paginate(
            self.sync_query_config(),
            LinearOperation::Issues,
            Some(resource_id.to_string()),
            |request| async move {
                self.fetch_hierarchy_issues(
                    resource_id,
                    milestone,
                    request.cursor.as_deref(),
                    request.page_size,
                )
                .await
            },
            |issues, _| {
                let issue_ids = Arc::clone(&issue_ids);
                async move {
                    for (mut issue, _relations, _label_ids) in issues {
                        issue.workspace_id = workspace_id.to_string();
                        db.upsert_issue_preserving_labels(&issue)?;
                        self.sync_issue_labels(db, &issue.id).await?;
                        self.sync_issue_relations(db, &issue.id).await?;
                        issue_ids.lock().unwrap().push(issue.id);
                    }
                    Ok(())
                }
            },
            |(issue, _, _)| issue.id.clone(),
            |event| self.observe_sync_event(event),
        )
        .await?;
        let issue_ids = issue_ids.lock().unwrap().clone();
        if milestone {
            db.reconcile_project_milestone_issue_membership(workspace_id, resource_id, &issue_ids)?;
        } else {
            db.reconcile_project_issue_membership(workspace_id, resource_id, &issue_ids)?;
        }
        Ok(())
    }

    async fn fetch_hierarchy_issues(
        &self,
        resource_id: &str,
        milestone: bool,
        after_cursor: Option<&str>,
        page_size: usize,
    ) -> Result<ConnectionPage<(db::Issue, Vec<db::Relation>, Vec<String>)>> {
        let query = if milestone {
            r#"
                query($id: String!, $after: String) {
                    projectMilestone(id: $id) {
                        issues(
                            first: __ISSUE_PAGE_SIZE__,
                            after: $after,
                            includeArchived: true
                        ) {
                            nodes { __ISSUE_FIELDS__ }
                            pageInfo { hasNextPage endCursor }
                        }
                    }
                }
            "#
        } else {
            r#"
                query($id: String!, $after: String) {
                    project(id: $id) {
                        issues(
                            first: __ISSUE_PAGE_SIZE__,
                            after: $after,
                            includeArchived: true
                        ) {
                            nodes { __ISSUE_FIELDS__ }
                            pageInfo { hasNextPage endCursor }
                        }
                    }
                }
            "#
        }
        .replace("__ISSUE_FIELDS__", ISSUE_FIELDS)
        .replace("__ISSUE_PAGE_SIZE__", &page_size.to_string());
        let variables = serde_json::json!({ "id": resource_id, "after": after_cursor });
        let connection = if milestone {
            let data: MilestoneIssuesData = self.query(&query, variables).await?;
            data.project_milestone.issues
        } else {
            let data: ProjectIssuesData = self.query(&query, variables).await?;
            data.project.issues
        };
        Ok(ConnectionPage {
            nodes: connection
                .nodes
                .into_iter()
                .map(Self::convert_linear_issue)
                .collect(),
            page_info: connection.page_info,
        })
    }
}

fn convert_project(project: LinearProjectNode, workspace_id: &str) -> db::Project {
    db::Project {
        id: project.id,
        workspace_id: workspace_id.to_string(),
        slug_id: project.slug_id,
        name: project.name,
        description: project.description,
        content: project.content,
        icon: project.icon,
        color: project.color,
        status_id: project.status.id,
        status_name: project.status.name,
        status_type: project.status.status_type,
        status_color: project.status.color,
        priority: project.priority,
        start_date: project.start_date,
        target_date: project.target_date,
        lead_id: project.lead.as_ref().map(|lead| lead.id.clone()),
        lead_name: project.lead.map(|lead| lead.name),
        created_at: project.created_at,
        updated_at: project.updated_at,
        archived_at: project.archived_at,
        url: project.url,
        progress: project.progress,
        synced_at: None,
        teams: project
            .teams
            .nodes
            .into_iter()
            .map(|team| db::ProjectTeam {
                id: team.id,
                key: team.key,
                name: team.name,
            })
            .collect(),
        members: project
            .members
            .nodes
            .into_iter()
            .map(|member| db::ProjectMember {
                id: member.id,
                name: member.name,
            })
            .collect(),
        labels: project
            .labels
            .nodes
            .into_iter()
            .map(|label| db::ProjectLabel {
                id: label.id,
                name: label.name,
                color: label.color,
                description: label.description,
            })
            .collect(),
    }
}

fn convert_milestone(
    milestone: LinearProjectMilestoneNode,
    workspace_id: &str,
) -> db::ProjectMilestone {
    db::ProjectMilestone {
        id: milestone.id,
        workspace_id: workspace_id.to_string(),
        project_id: milestone.project.id,
        project_name: milestone.project.name,
        name: milestone.name,
        description: milestone.description,
        target_date: milestone.target_date,
        status: milestone.status,
        progress: milestone.progress,
        sort_order: milestone.sort_order,
        created_at: milestone.created_at,
        updated_at: milestone.updated_at,
        archived_at: milestone.archived_at,
        synced_at: None,
    }
}

fn project_create_value(input: &CreateProjectInput) -> serde_json::Map<String, serde_json::Value> {
    let mut value = serde_json::Map::new();
    value.insert("name".into(), serde_json::json!(input.name));
    value.insert("teamIds".into(), serde_json::json!(input.team_ids));
    insert_optional_string(
        &mut value,
        "description",
        input.description.as_deref(),
        false,
    );
    insert_optional_string(&mut value, "content", input.content.as_deref(), false);
    insert_optional_string(&mut value, "icon", input.icon.as_deref(), true);
    insert_optional_string(&mut value, "color", input.color.as_deref(), true);
    insert_optional_string(&mut value, "statusId", input.status_id.as_deref(), true);
    insert_optional_string(&mut value, "leadId", input.lead_id.as_deref(), true);
    insert_optional_string(&mut value, "startDate", input.start_date.as_deref(), true);
    insert_optional_string(&mut value, "targetDate", input.target_date.as_deref(), true);
    if let Some(priority) = input.priority {
        value.insert("priority".into(), serde_json::json!(priority));
    }
    if let Some(member_ids) = &input.member_ids {
        value.insert("memberIds".into(), serde_json::json!(member_ids));
    }
    if let Some(label_ids) = &input.label_ids {
        value.insert("labelIds".into(), serde_json::json!(label_ids));
    }
    value
}

fn project_update_value(input: &UpdateProjectInput) -> serde_json::Map<String, serde_json::Value> {
    let mut value = serde_json::Map::new();
    insert_optional_string(&mut value, "name", input.name.as_deref(), false);
    insert_optional_string(
        &mut value,
        "description",
        input.description.as_deref(),
        false,
    );
    insert_optional_string(&mut value, "content", input.content.as_deref(), false);
    insert_optional_string(&mut value, "icon", input.icon.as_deref(), true);
    insert_optional_string(&mut value, "color", input.color.as_deref(), true);
    insert_optional_string(&mut value, "statusId", input.status_id.as_deref(), true);
    insert_optional_string(&mut value, "leadId", input.lead_id.as_deref(), true);
    insert_optional_string(&mut value, "startDate", input.start_date.as_deref(), true);
    insert_optional_string(&mut value, "targetDate", input.target_date.as_deref(), true);
    if let Some(team_ids) = &input.team_ids {
        value.insert("teamIds".into(), serde_json::json!(team_ids));
    }
    if let Some(member_ids) = &input.member_ids {
        value.insert("memberIds".into(), serde_json::json!(member_ids));
    }
    if let Some(label_ids) = &input.label_ids {
        value.insert("labelIds".into(), serde_json::json!(label_ids));
    }
    if let Some(priority) = input.priority {
        value.insert("priority".into(), serde_json::json!(priority));
    }
    value
}

fn milestone_create_value(
    input: &CreateProjectMilestoneInput,
) -> serde_json::Map<String, serde_json::Value> {
    let mut value = serde_json::Map::new();
    value.insert("projectId".into(), serde_json::json!(input.project_id));
    value.insert("name".into(), serde_json::json!(input.name));
    insert_optional_string(
        &mut value,
        "description",
        input.description.as_deref(),
        false,
    );
    insert_optional_string(&mut value, "targetDate", input.target_date.as_deref(), true);
    if let Some(sort_order) = input.sort_order {
        value.insert("sortOrder".into(), serde_json::json!(sort_order));
    }
    value
}

fn milestone_update_value(
    input: &UpdateProjectMilestoneInput,
) -> serde_json::Map<String, serde_json::Value> {
    let mut value = serde_json::Map::new();
    insert_optional_string(&mut value, "projectId", input.project_id.as_deref(), true);
    insert_optional_string(&mut value, "name", input.name.as_deref(), false);
    insert_optional_string(
        &mut value,
        "description",
        input.description.as_deref(),
        false,
    );
    insert_optional_string(&mut value, "targetDate", input.target_date.as_deref(), true);
    if let Some(sort_order) = input.sort_order {
        value.insert("sortOrder".into(), serde_json::json!(sort_order));
    }
    value
}

fn insert_optional_string(
    value: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    input: Option<&str>,
    nullable: bool,
) {
    let Some(input) = input else { return };
    if nullable && (input.is_empty() || input.eq_ignore_ascii_case("none")) {
        value.insert(key.into(), serde_json::Value::Null);
    } else {
        value.insert(key.into(), serde_json::json!(input));
    }
}

fn find_resource_id(nodes: &serde_json::Value, name: &str, kind: &str) -> Result<String> {
    let nodes = nodes
        .as_array()
        .with_context(|| format!("No {kind} values in response"))?;
    for resource in nodes {
        if resource["id"].as_str() == Some(name)
            || resource["name"]
                .as_str()
                .is_some_and(|candidate| candidate.eq_ignore_ascii_case(name))
        {
            return resource["id"]
                .as_str()
                .map(ToString::to_string)
                .with_context(|| format!("{kind} has no id"));
        }
    }
    let available = nodes
        .iter()
        .filter_map(|resource| resource["name"].as_str())
        .collect::<Vec<_>>()
        .join(", ");
    anyhow::bail!("{} '{}' not found. Available: {}", kind, name, available)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_update_serializes_clearable_metadata_as_null() {
        let input = UpdateProjectInput {
            lead_id: Some("none".into()),
            target_date: Some(String::new()),
            description: Some(String::new()),
            priority: Some(1),
            label_ids: Some(vec!["label-1".into()]),
            ..Default::default()
        };
        let value = project_update_value(&input);
        assert_eq!(value["leadId"], serde_json::Value::Null);
        assert_eq!(value["targetDate"], serde_json::Value::Null);
        assert_eq!(value["description"], serde_json::json!(""));
        assert_eq!(value["priority"], serde_json::json!(1));
        assert_eq!(value["labelIds"], serde_json::json!(["label-1"]));
    }

    #[test]
    fn milestone_create_serializes_project_relationship() {
        let input = CreateProjectMilestoneInput {
            project_id: "project-1".into(),
            name: "Beta".into(),
            target_date: Some("2026-09-01".into()),
            ..Default::default()
        };
        let value = milestone_create_value(&input);
        assert_eq!(value["projectId"], serde_json::json!("project-1"));
        assert_eq!(value["name"], serde_json::json!("Beta"));
        assert_eq!(value["targetDate"], serde_json::json!("2026-09-01"));
    }

    #[test]
    fn mutation_payloads_accept_create_and_update_field_names() {
        for field in ["projectCreate", "projectUpdate"] {
            let payload = serde_json::json!({
                (field): {
                    "success": true,
                    "project": { "id": "project-1" }
                }
            });
            let parsed: MutationProjectData = serde_json::from_value(payload).unwrap();
            assert!(parsed.payload.success);
            assert_eq!(parsed.payload.project.unwrap().id, "project-1");
        }

        for field in ["projectMilestoneCreate", "projectMilestoneUpdate"] {
            let payload = serde_json::json!({
                (field): {
                    "success": true,
                    "projectMilestone": { "id": "milestone-1" }
                }
            });
            let parsed: MutationMilestoneData = serde_json::from_value(payload).unwrap();
            assert!(parsed.payload.success);
            assert_eq!(parsed.payload.project_milestone.unwrap().id, "milestone-1");
        }
    }

    #[test]
    fn project_and_milestone_responses_preserve_graphql_metadata() {
        let project: LinearProjectNode = serde_json::from_value(serde_json::json!({
            "id": "project-1",
            "slugId": "api-reliability",
            "name": "API Reliability",
            "description": "Service resilience",
            "content": "Detailed rollout plan",
            "icon": "Cube",
            "color": "#f2994a",
            "priority": 2,
            "startDate": "2026-07-01",
            "targetDate": "2026-09-01",
            "createdAt": "2026-07-01T00:00:00Z",
            "updatedAt": "2026-07-16T00:00:00Z",
            "archivedAt": null,
            "url": "https://linear.app/acme/project/api-reliability",
            "progress": 0.25,
            "status": {
                "id": "status-1",
                "name": "Backlog",
                "type": "backlog",
                "color": "#888888"
            },
            "lead": { "id": "user-1", "name": "Alex Morgan" },
            "teams": { "nodes": [{
                "id": "team-1", "key": "ENG", "name": "Engineering"
            }]},
            "members": { "nodes": [{ "id": "user-1", "name": "Alex Morgan" }]},
            "labels": { "nodes": [{
                "id": "label-1",
                "name": "Infrastructure",
                "color": "#f2994a",
                "description": "Platform engineering"
            }]}
        }))
        .unwrap();
        let project = convert_project(project, "home");
        assert_eq!(project.status_name, "Backlog");
        assert_eq!(project.teams[0].key, "ENG");
        assert_eq!(project.lead_name.as_deref(), Some("Alex Morgan"));
        assert_eq!(project.labels[0].name, "Infrastructure");

        let milestone: LinearProjectMilestoneNode = serde_json::from_value(serde_json::json!({
            "id": "milestone-1",
            "name": "Request tracing",
            "description": "Instrument critical request paths",
            "targetDate": "2026-08-15",
            "status": "next",
            "progress": 0.5,
            "sortOrder": 1.0,
            "createdAt": "2026-07-01T00:00:00Z",
            "updatedAt": "2026-07-16T00:00:00Z",
            "archivedAt": null,
            "project": { "id": "project-1", "name": "API Reliability" }
        }))
        .unwrap();
        let milestone = convert_milestone(milestone, "home");
        assert_eq!(milestone.project_name, "API Reliability");
        assert_eq!(milestone.target_date.as_deref(), Some("2026-08-15"));
    }
}
