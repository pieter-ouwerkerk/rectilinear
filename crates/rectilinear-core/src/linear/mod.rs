use std::future::ready;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::config::Config;
use crate::db::{self, Database};

mod cycles;
mod projects;
mod pagination;
pub use projects::*;
pub use pagination::{LinearErrorKind, LinearOperation, LinearOperationError, SyncEvent, SyncQueryConfig};

use pagination::{paginate, ConnectionPage, PageInfo};

const LINEAR_API_URL: &str = "https://api.linear.app/graphql";

#[derive(Clone)]
pub struct LinearClient {
    client: reqwest::Client,
    api_key: String,
    viewer_id: std::sync::Arc<std::sync::RwLock<Option<String>>>,
    sync_query_config: SyncQueryConfig,
}

#[derive(Debug, Deserialize)]
struct GraphQLResponse<T> {
    data: Option<T>,
    errors: Option<Vec<GraphQLError>>,
}

#[derive(Debug, Deserialize)]
struct GraphQLError {
    message: String,
}

// --- Query response types ---

#[derive(Debug, Deserialize)]
struct IssuesData {
    issues: IssueConnection,
}

#[derive(Debug, Deserialize)]
struct IssueConnection {
    nodes: Vec<LinearIssue>,
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
}

#[derive(Debug, Deserialize)]
struct LinearIssue {
    id: String,
    identifier: String,
    url: String,
    title: String,
    description: Option<String>,
    priority: i32,
    #[serde(rename = "createdAt")]
    created_at: String,
    #[serde(rename = "updatedAt")]
    updated_at: String,
    state: LinearState,
    team: LinearTeam,
    assignee: Option<LinearUser>,
    project: Option<LinearProject>,
    #[serde(rename = "projectMilestone")]
    project_milestone: Option<LinearProjectMilestoneRef>,
    cycle: Option<LinearCycleRef>,
    #[serde(default)]
    labels: LinearLabelConnection,
    #[serde(default)]
    relations: LinearRelationConnection,
    #[serde(rename = "branchName")]
    branch_name: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct LinearRelationConnection {
    nodes: Vec<LinearRelation>,
}

#[derive(Debug, Deserialize)]
struct IssueRelationsData {
    issue: IssueRelationsNode,
}

#[derive(Debug, Deserialize)]
struct IssueRelationsNode {
    relations: PaginatedRelationConnection,
}

#[derive(Debug, Deserialize)]
struct PaginatedRelationConnection {
    nodes: Vec<LinearRelation>,
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
}

#[derive(Debug, Deserialize)]
struct LinearRelation {
    id: String,
    #[serde(rename = "type")]
    relation_type: String,
    #[serde(rename = "relatedIssue")]
    related_issue: LinearRelatedIssue,
}

#[derive(Debug, Deserialize)]
struct LinearRelatedIssue {
    id: String,
    identifier: String,
}

#[derive(Debug, Deserialize)]
struct LinearState {
    name: String,
    #[serde(rename = "type")]
    state_type: String,
}

#[derive(Debug, Deserialize)]
struct LinearTeam {
    key: String,
}

#[derive(Debug, Deserialize)]
struct LinearUser {
    name: String,
}

#[derive(Debug, Deserialize)]
struct LinearExternalUser {
    name: Option<String>,
    #[serde(rename = "displayName")]
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LinearProject {
    id: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct LinearProjectMilestoneRef {
    id: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct LinearCycleRef {
    id: String,
    name: Option<String>,
    number: i32,
}

#[derive(Debug, Deserialize, Default)]
struct LinearLabelConnection {
    nodes: Vec<LinearLabel>,
}

#[derive(Debug, Deserialize)]
struct IssueLabelsForIssueData {
    issue: IssueLabelsForIssueNode,
}

#[derive(Debug, Deserialize)]
struct IssueLabelsForIssueNode {
    labels: PaginatedIssueLabelConnection,
}

#[derive(Debug, Deserialize)]
struct PaginatedIssueLabelConnection {
    nodes: Vec<LinearLabel>,
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
}

#[derive(Debug, Deserialize)]
struct LinearLabel {
    id: String,
    name: String,
}

// --- Team query types ---

#[derive(Debug, Deserialize)]
struct TeamsData {
    teams: TeamConnection,
}

#[derive(Debug, Deserialize)]
struct TeamConnection {
    nodes: Vec<TeamNode>,
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct TeamNode {
    pub id: String,
    pub key: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct LabelCatalogEntry {
    pub id: String,
    pub name: String,
    pub color: Option<String>,
    pub parent_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct IssueLabelsData {
    #[serde(rename = "issueLabels")]
    issue_labels: IssueLabelCatalogConnection,
}

#[derive(Debug, Deserialize)]
struct IssueLabelCatalogConnection {
    nodes: Vec<IssueLabelCatalogNode>,
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
}

#[derive(Debug, Deserialize)]
struct IssueLabelCatalogNode {
    id: String,
    name: String,
    color: Option<String>,
    parent: Option<IssueLabelParent>,
}

#[derive(Debug, Deserialize)]
struct IssueLabelParent {
    id: String,
}

// --- Issue creation types ---

#[derive(Debug, Deserialize)]
struct CreateIssueData {
    #[serde(rename = "issueCreate")]
    issue_create: CreateIssuePayload,
}

#[derive(Debug, Deserialize)]
struct CreateIssuePayload {
    success: bool,
    issue: Option<CreatedIssue>,
}

#[derive(Debug, Deserialize)]
struct CreatedIssue {
    id: String,
    identifier: String,
}

#[derive(Debug)]
pub struct CreateIssueInput<'a> {
    pub team_id: &'a str,
    pub title: &'a str,
    pub description: Option<&'a str>,
    pub priority: Option<i32>,
    pub label_ids: &'a [String],
    pub assignee_id: Option<&'a str>,
    pub parent_id: Option<&'a str>,
    pub project_id: Option<&'a str>,
    pub project_milestone_id: Option<&'a str>,
}

// --- Comment creation types ---

#[derive(Debug, Deserialize)]
struct CreateCommentData {
    #[serde(rename = "commentCreate")]
    comment_create: CreateCommentPayload,
}

#[derive(Debug, Deserialize)]
struct CreateCommentPayload {
    success: bool,
}

// --- Comment query types ---

#[derive(Debug, Deserialize)]
struct CommentsData {
    comments: LinearCommentConnection,
}

#[derive(Debug, Deserialize)]
struct LinearCommentConnection {
    nodes: Vec<LinearComment>,
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
}

#[derive(Debug, Deserialize)]
struct LinearComment {
    id: String,
    body: String,
    #[serde(rename = "createdAt")]
    created_at: String,
    #[serde(rename = "updatedAt")]
    updated_at: String,
    #[serde(rename = "parentId")]
    parent_id: Option<String>,
    url: String,
    user: Option<LinearUser>,
    #[serde(rename = "externalUser")]
    external_user: Option<LinearExternalUser>,
}

// --- Issue update types ---

#[derive(Debug, Deserialize)]
struct UpdateIssueData {
    #[serde(rename = "issueUpdate")]
    issue_update: UpdateIssuePayload,
}

#[derive(Debug, Deserialize)]
struct UpdateIssuePayload {
    success: bool,
}

#[derive(Debug, Default)]
pub struct UpdateIssueInput<'a> {
    pub title: Option<&'a str>,
    pub description: Option<&'a str>,
    pub priority: Option<i32>,
    pub state_id: Option<&'a str>,
    pub label_ids: Option<&'a [String]>,
    pub project_id: Option<&'a str>,
    pub assignee_id: Option<&'a str>,
    pub project_milestone_id: Option<&'a str>,
}

// --- Relation mutation types ---

#[derive(Debug, Deserialize)]
struct CreateRelationData {
    #[serde(rename = "issueRelationCreate")]
    issue_relation_create: CreateRelationPayload,
}

#[derive(Debug, Deserialize)]
struct CreateRelationPayload {
    success: bool,
    #[serde(rename = "issueRelation")]
    issue_relation: Option<CreatedRelation>,
}

#[derive(Debug, Deserialize)]
struct CreatedRelation {
    id: String,
}

#[derive(Debug, Deserialize)]
struct DeleteRelationData {
    #[serde(rename = "issueRelationDelete")]
    issue_relation_delete: DeleteRelationPayload,
}

#[derive(Debug, Deserialize)]
struct DeleteRelationPayload {
    success: bool,
}

// --- Single issue query ---

#[derive(Debug, Deserialize)]
struct SingleIssueData {
    issue: LinearIssue,
}

impl LinearClient {
    pub fn new(config: &Config) -> Result<Self> {
        let api_key = config.linear_api_key()?.to_string();
        let client = reqwest::Client::new();
        Ok(Self {
            client,
            api_key,
            viewer_id: std::sync::Arc::new(std::sync::RwLock::new(None)),
            sync_query_config: SyncQueryConfig::from_environment(),
        })
    }

    /// Create a client with an explicit API key (for FFI callers).
    pub fn with_api_key(api_key: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.to_string(),
            viewer_id: std::sync::Arc::new(std::sync::RwLock::new(None)),
            sync_query_config: SyncQueryConfig::from_environment(),
        }
    }

    /// Create a client reusing an existing `reqwest::Client`.
    ///
    /// Use this when the HTTP client was already constructed inside a tokio
    /// runtime context (e.g. from the FFI layer).
    pub fn with_http_client(client: reqwest::Client, api_key: &str) -> Self {
        Self {
            client,
            api_key: api_key.to_string(),
            viewer_id: std::sync::Arc::new(std::sync::RwLock::new(None)),
            sync_query_config: SyncQueryConfig::from_environment(),
        }
    }

    pub fn with_sync_query_config(mut self, sync_query_config: SyncQueryConfig) -> Self {
        self.sync_query_config = sync_query_config;
        self
    }

    pub fn sync_query_config(&self) -> &SyncQueryConfig {
        &self.sync_query_config
    }

    fn observe_sync_event(&self, event: SyncEvent) {
        if !self.sync_query_config.verbose {
            return;
        }
        let parent = event
            .parent
            .as_deref()
            .map(|value| format!(" parent={value}"))
            .unwrap_or_default();
        let reduction = if event.adaptive_reduction {
            " adaptive-page-size=true"
        } else {
            ""
        };
        if let Some(failure) = event.failure {
            eprintln!(
                "sync operation={}{} page={} nodes={} page_size={}{} status=failed error={}",
                event.operation,
                parent,
                event.page_number,
                event.nodes_received,
                event.page_size,
                reduction,
                failure
            );
        } else {
            eprintln!(
                "sync operation={}{} page={} nodes={} page_size={}{} status={}",
                event.operation,
                parent,
                event.page_number,
                event.nodes_received,
                event.page_size,
                reduction,
                if event.completed { "complete" } else { "running" }
            );
        }
    }

    async fn query<T: serde::de::DeserializeOwned>(
        &self,
        query: &str,
        variables: serde_json::Value,
    ) -> Result<T> {
        self.query_operation("GraphQL query", None, query, variables)
            .await
    }

    async fn query_operation<T: serde::de::DeserializeOwned>(
        &self,
        operation: &str,
        cursor: Option<&str>,
        query: &str,
        variables: serde_json::Value,
    ) -> Result<T> {
        let body = serde_json::json!({
            "query": query,
            "variables": variables,
        });

        let resp = self
            .client
            .post(LINEAR_API_URL)
            .header("Authorization", &self.api_key)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|error| {
                LinearOperationError::new(
                    LinearErrorKind::Transport,
                    operation,
                    cursor,
                    error.to_string(),
                )
            })?;

        let status = resp.status();
        if !status.is_success() {
            let retry_after = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .map(Duration::from_secs);
            let kind = classify_http_status(status.as_u16());
            return Err(LinearOperationError::new(
                kind,
                operation,
                cursor,
                format!("HTTP {status} (response body omitted)"),
            )
            .with_retry_after(retry_after)
            .into());
        }

        let response: GraphQLResponse<T> = resp
            .json()
            .await
            .map_err(|error| {
                LinearOperationError::new(
                    LinearErrorKind::Api,
                    operation,
                    cursor,
                    format!("failed to parse response: {error}"),
                )
            })?;

        if let Some(errors) = response.errors {
            let message = errors
                .iter()
                .map(|error| error.message.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let kind = classify_graphql_message(&message);
            return Err(LinearOperationError::new(kind, operation, cursor, message).into());
        }

        response.data.ok_or_else(|| {
            LinearOperationError::new(
                LinearErrorKind::Api,
                operation,
                cursor,
                "response did not contain data",
            )
            .into()
        })
    }

    pub async fn list_teams(&self) -> Result<Vec<TeamNode>> {
        let query = r#"
            query($first: Int!, $after: String) {
                teams(first: $first, after: $after, orderBy: updatedAt) {
                    nodes { id key name }
                    pageInfo { hasNextPage endCursor }
                }
            }
        "#;
        let mut teams = Vec::new();
        paginate(
            &self.sync_query_config,
            LinearOperation::Teams,
            None,
            |request| async move {
                let data: TeamsData = self
                    .query_operation(
                        LinearOperation::Teams.name(),
                        request.cursor.as_deref(),
                        query,
                        serde_json::json!({
                            "first": request.page_size,
                            "after": request.cursor,
                        }),
                    )
                    .await?;
                Ok(ConnectionPage {
                    nodes: data.teams.nodes,
                    page_info: data.teams.page_info,
                })
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

    fn extract_relations(issue_id: &str, linear_issue: &LinearIssue) -> Vec<db::Relation> {
        linear_issue
            .relations
            .nodes
            .iter()
            .map(|r| db::Relation {
                id: r.id.clone(),
                issue_id: issue_id.to_string(),
                related_issue_id: r.related_issue.id.clone(),
                related_issue_identifier: r.related_issue.identifier.clone(),
                relation_type: r.relation_type.clone(),
            })
            .collect()
    }

    fn convert_linear_relation(issue_id: &str, relation: LinearRelation) -> db::Relation {
        db::Relation {
            id: relation.id,
            issue_id: issue_id.to_string(),
            related_issue_id: relation.related_issue.id,
            related_issue_identifier: relation.related_issue.identifier,
            relation_type: relation.relation_type,
        }
    }

    pub async fn fetch_issues(
        &self,
        team_key: &str,
        after_cursor: Option<&str>,
        updated_after: Option<&str>,
        include_archived: bool,
    ) -> Result<(Vec<(db::Issue, Vec<db::Relation>, Vec<String>)>, bool, Option<String>)> {
        let page = self
            .fetch_issues_page(
                team_key,
                after_cursor,
                updated_after,
                include_archived,
                self.sync_query_config.page_size(LinearOperation::Issues),
            )
            .await?;
        Ok((
            page.nodes,
            page.page_info.has_next_page,
            page.page_info.end_cursor,
        ))
    }

    async fn fetch_issues_page(
        &self,
        team_key: &str,
        after_cursor: Option<&str>,
        updated_after: Option<&str>,
        include_archived: bool,
        page_size: usize,
    ) -> Result<ConnectionPage<(db::Issue, Vec<db::Relation>, Vec<String>)>> {
        let mut filter_parts = vec![format!("team: {{ key: {{ eq: \"{}\" }} }}", team_key)];
        if let Some(after) = updated_after {
            filter_parts.push(format!("updatedAt: {{ gt: \"{}\" }}", after));
        }
        let filter = filter_parts.join(", ");
        let query = format!(
            r#"query($first: Int!, $after: String, $includeArchived: Boolean!) {{
                issues(
                    first: $first,
                    after: $after,
                    filter: {{ {} }},
                    includeArchived: $includeArchived,
                    orderBy: updatedAt
                ) {{
                    nodes {{
                        id identifier url title description priority branchName
                        createdAt updatedAt
                        state {{ name type }}
                        team {{ key }}
                        assignee {{ name }}
                        project {{ id name }}
                        projectMilestone {{ id name }}
                        cycle {{ id name number }}
                    }}
                    pageInfo {{ hasNextPage endCursor }}
                }}
            }}"#,
            filter
        );

        let data: IssuesData = self
            .query_operation(
                LinearOperation::Issues.name(),
                after_cursor,
                &query,
                issue_page_variables(page_size, after_cursor, include_archived),
            )
            .await?;

        let issues: Vec<(db::Issue, Vec<db::Relation>, Vec<String>)> = data
            .issues
            .nodes
            .into_iter()
            .map(Self::convert_linear_issue)
            .collect();

        Ok(ConnectionPage { nodes: issues, page_info: data.issues.page_info })
    }

    pub async fn sync_team(
        &self,
        db: &Database,
        team_key: &str,
        workspace_id: &str,
        full: bool,
        include_archived: bool,
        progress: Option<&(dyn Fn(usize) + Send + Sync)>,
    ) -> Result<usize> {
        self.sync_projects(db, workspace_id)
            .await
            .with_context(|| format!("project synchronization failed for workspace '{workspace_id}'"))?;
        self.sync_labels_catalog(db, workspace_id)
            .await
            .with_context(|| format!("label synchronization failed for workspace '{workspace_id}'"))?;
        self.sync_cycles(db, team_key, workspace_id, include_archived)
            .await
            .with_context(|| format!("cycle synchronization failed for team '{team_key}'"))?;

        let updated_after = if full {
            None
        } else {
            db.get_sync_cursor(workspace_id, team_key)?
        };

        let sync_token = Uuid::new_v4().to_string();
        let mut max_updated: Option<String> = None;
        let mut persisted_total = 0;
        db.mark_sync_family_running(
            workspace_id,
            team_key,
            "issues",
            None,
            Some(self.sync_query_config.page_size(LinearOperation::Issues)),
            &sync_token,
        )?;
        let issue_result = paginate(
            &self.sync_query_config,
            LinearOperation::Issues,
            Some(team_key.to_string()),
            |request| {
                let updated_after = updated_after.clone();
                async move {
                    self.fetch_issues_page(
                        team_key,
                        request.cursor.as_deref(),
                        updated_after.as_deref(),
                        include_archived,
                        request.page_size,
                    )
                    .await
                }
            },
            |issues, context| {
                let count = issues.len();
                let result = (|| {
                    for (mut issue, _relations, _label_ids) in issues {
                        issue.workspace_id = workspace_id.to_string();
                        if max_updated.is_none()
                            || Some(&issue.updated_at) > max_updated.as_ref()
                        {
                            max_updated = Some(issue.updated_at.clone());
                        }
                        db.upsert_issue_preserving_labels(&issue)?;
                        db.mark_issue_sync_token(&issue.id, &sync_token)?;
                    }
                    persisted_total += count;
                    db.mark_sync_family_running(
                        workspace_id,
                        team_key,
                        "issues",
                        context.cursor.as_deref(),
                        Some(context.page_size),
                        &sync_token,
                    )?;
                    if let Some(callback) = progress {
                        callback(persisted_total);
                    }
                    Ok(())
                })();
                ready(result)
            },
            |(issue, _, _)| issue.id.clone(),
            |event| self.observe_sync_event(event),
        )
        .await;

        let stats = match issue_result {
            Ok(stats) => stats,
            Err(error) => {
                let message = Self::redacted_error_message(&error);
                db.mark_sync_family_failed(
                    workspace_id,
                    team_key,
                    "issues",
                    &sync_token,
                    &message,
                )?;
                return Err(error);
            }
        };
        if full {
            db.reconcile_full_issue_sync(workspace_id, team_key, &sync_token)?;
        }
        db.mark_sync_family_complete(
            workspace_id,
            team_key,
            "issues",
            Some(self.sync_query_config.page_size(LinearOperation::Issues)),
            &sync_token,
        )?;

        for family in ["issue labels", "relations", "comments"] {
            db.mark_sync_family_running(
                workspace_id,
                team_key,
                family,
                None,
                None,
                &sync_token,
            )?;
        }
        let hydration_result: Result<()> = async {
            let mut after_id = None;
            loop {
                let issue_refs = db.list_issue_sync_refs(
                    workspace_id,
                    team_key,
                    &sync_token,
                    after_id.as_deref(),
                    100,
                )?;
                if issue_refs.is_empty() {
                    break;
                }
                for issue in &issue_refs {
                    self.sync_issue_labels(db, &issue.id)
                        .await
                        .with_context(|| {
                            format!("label synchronization failed for {}", issue.identifier)
                        })?;
                    self.sync_issue_relations(db, &issue.id)
                        .await
                        .with_context(|| {
                            format!("relation synchronization failed for {}", issue.identifier)
                        })?;
                    self.sync_issue_comments(db, &issue.id, workspace_id)
                        .await
                        .with_context(|| {
                            format!("comment synchronization failed for {}", issue.identifier)
                        })?;
                }
                after_id = issue_refs.last().map(|issue| issue.id.clone());
            }
            Ok(())
        }
        .await;
        if let Err(error) = hydration_result {
            let message = Self::redacted_error_message(&error);
            for family in ["issue labels", "relations", "comments"] {
                db.mark_sync_family_failed(
                    workspace_id,
                    team_key,
                    family,
                    &sync_token,
                    &message,
                )?;
            }
            return Err(error);
        }
        for family in ["issue labels", "relations", "comments"] {
            db.mark_sync_family_complete(
                workspace_id,
                team_key,
                family,
                None,
                &sync_token,
            )?;
        }

        let next_updated = max_updated
            .or(updated_after)
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
        db.set_sync_cursor(workspace_id, team_key, &next_updated)?;
        Ok(stats.nodes)
    }

    pub async fn create_issue(&self, create: CreateIssueInput<'_>) -> Result<(String, String)> {
        let input = create_issue_value(&create);

        let query = r#"
            mutation($input: IssueCreateInput!) {
                issueCreate(input: $input) {
                    success
                    issue { id identifier }
                }
            }
        "#;

        let data: CreateIssueData = self
            .query(query, serde_json::json!({ "input": input }))
            .await?;

        if !data.issue_create.success {
            anyhow::bail!("Failed to create issue");
        }

        let issue = data.issue_create.issue.context("No issue returned")?;
        Ok((issue.id, issue.identifier))
    }

    pub async fn add_comment(&self, issue_id: &str, body: &str) -> Result<()> {
        let query = r#"
            mutation($input: CommentCreateInput!) {
                commentCreate(input: $input) {
                    success
                }
            }
        "#;

        let input = serde_json::json!({
            "issueId": issue_id,
            "body": body,
        });

        let data: CreateCommentData = self
            .query(query, serde_json::json!({ "input": input }))
            .await?;

        if !data.comment_create.success {
            anyhow::bail!("Failed to create comment");
        }

        Ok(())
    }

    async fn fetch_issue_comments_page(
        &self,
        issue_id: &str,
        cursor: Option<&str>,
        page_size: usize,
    ) -> Result<ConnectionPage<db::Comment>> {
        let query = r#"
            query($issueId: ID!, $first: Int!, $after: String) {
                comments(
                    filter: { issue: { id: { eq: $issueId } } },
                    first: $first,
                    after: $after,
                    includeArchived: true,
                    orderBy: createdAt
                ) {
                    nodes {
                        id body createdAt updatedAt parentId url
                        user { name }
                        externalUser { displayName name }
                    }
                    pageInfo { hasNextPage endCursor }
                }
            }
        "#;
        let data: CommentsData = self
            .query_operation(
                LinearOperation::Comments.name(),
                cursor,
                query,
                serde_json::json!({
                    "issueId": issue_id,
                    "first": page_size,
                    "after": cursor,
                }),
            )
            .await?;
        Ok(ConnectionPage {
            nodes: data
                .comments
                .nodes
                .into_iter()
                .map(|comment| Self::convert_linear_comment(issue_id, comment))
                .collect(),
            page_info: data.comments.page_info,
        })
    }

    pub async fn fetch_issue_comments(&self, issue_id: &str) -> Result<Vec<db::Comment>> {
        let mut comments = Vec::new();
        paginate(
            &self.sync_query_config,
            LinearOperation::Comments,
            Some(issue_id.to_string()),
            |request| async move {
                self.fetch_issue_comments_page(
                    issue_id,
                    request.cursor.as_deref(),
                    request.page_size,
                )
                .await
            },
            |nodes, _| {
                comments.extend(nodes);
                ready(Ok(()))
            },
            |comment| comment.id.clone(),
            |event| self.observe_sync_event(event),
        )
        .await?;
        Ok(comments)
    }

    pub async fn sync_issue_comments(
        &self,
        db: &Database,
        issue_id: &str,
        workspace_id: &str,
    ) -> Result<usize> {
        let sync_token = Uuid::new_v4().to_string();
        let result = paginate(
            &self.sync_query_config,
            LinearOperation::Comments,
            Some(issue_id.to_string()),
            |request| async move {
                self.fetch_issue_comments_page(
                    issue_id,
                    request.cursor.as_deref(),
                    request.page_size,
                )
                .await
            },
            |mut comments, _| {
                for comment in &mut comments {
                    comment.workspace_id = workspace_id.to_string();
                }
                ready(db.upsert_comment_page(
                    issue_id,
                    workspace_id,
                    &comments,
                    &sync_token,
                ))
            },
            |comment| comment.id.clone(),
            |event| self.observe_sync_event(event),
        )
        .await;
        match result {
            Ok(stats) => {
                db.complete_comment_sync(issue_id, workspace_id, &sync_token)?;
                db.mark_comments_synced(issue_id, workspace_id, stats.nodes)?;
                Ok(stats.nodes)
            }
            Err(error) => {
                let status = Self::comment_error_status(&error);
                let message = Self::redacted_error_message(&error);
                db.mark_comments_sync_failed(issue_id, workspace_id, status, &message)?;
                Err(error)
            }
        }
    }

    async fn fetch_issue_relations_page(
        &self,
        issue_id: &str,
        cursor: Option<&str>,
        page_size: usize,
    ) -> Result<ConnectionPage<db::Relation>> {
        let query = r#"
            query($issueId: String!, $first: Int!, $after: String) {
                issue(id: $issueId) {
                    relations(first: $first, after: $after) {
                        nodes { id type relatedIssue { id identifier } }
                        pageInfo { hasNextPage endCursor }
                    }
                }
            }
        "#;
        let data: IssueRelationsData = self
            .query_operation(
                LinearOperation::Relations.name(),
                cursor,
                query,
                serde_json::json!({
                    "issueId": issue_id,
                    "first": page_size,
                    "after": cursor,
                }),
            )
            .await?;
        Ok(ConnectionPage {
            nodes: data
                .issue
                .relations
                .nodes
                .into_iter()
                .map(|relation| Self::convert_linear_relation(issue_id, relation))
                .collect(),
            page_info: data.issue.relations.page_info,
        })
    }

    pub async fn sync_issue_relations(
        &self,
        db: &Database,
        issue_id: &str,
    ) -> Result<usize> {
        let sync_token = Uuid::new_v4().to_string();
        let stats = paginate(
            &self.sync_query_config,
            LinearOperation::Relations,
            Some(issue_id.to_string()),
            |request| async move {
                self.fetch_issue_relations_page(
                    issue_id,
                    request.cursor.as_deref(),
                    request.page_size,
                )
                .await
            },
            |relations, _| {
                ready(db.upsert_relation_page(issue_id, &relations, &sync_token))
            },
            |relation| relation.id.clone(),
            |event| self.observe_sync_event(event),
        )
        .await?;
        db.complete_relation_sync(issue_id, &sync_token)?;
        Ok(stats.nodes)
    }

    async fn fetch_issue_labels_page(
        &self,
        issue_id: &str,
        cursor: Option<&str>,
        page_size: usize,
    ) -> Result<ConnectionPage<LinearLabel>> {
        let query = r#"
            query($issueId: String!, $first: Int!, $after: String) {
                issue(id: $issueId) {
                    labels(first: $first, after: $after, orderBy: updatedAt) {
                        nodes { id name }
                        pageInfo { hasNextPage endCursor }
                    }
                }
            }
        "#;
        let data: IssueLabelsForIssueData = self
            .query_operation(
                "issue labels",
                cursor,
                query,
                serde_json::json!({
                    "issueId": issue_id,
                    "first": page_size,
                    "after": cursor,
                }),
            )
            .await?;
        Ok(ConnectionPage {
            nodes: data.issue.labels.nodes,
            page_info: data.issue.labels.page_info,
        })
    }

    pub async fn sync_issue_labels(&self, db: &Database, issue_id: &str) -> Result<usize> {
        let sync_token = Uuid::new_v4().to_string();
        let mut names = Vec::new();
        let stats = paginate(
            &self.sync_query_config,
            LinearOperation::Labels,
            Some(issue_id.to_string()),
            |request| async move {
                self.fetch_issue_labels_page(
                    issue_id,
                    request.cursor.as_deref(),
                    request.page_size,
                )
                .await
            },
            |labels, _| {
                let ids = labels
                    .iter()
                    .map(|label| label.id.clone())
                    .collect::<Vec<_>>();
                names.extend(labels.into_iter().map(|label| label.name));
                ready(db.upsert_issue_label_page(issue_id, &ids, &sync_token))
            },
            |label| label.id.clone(),
            |event| self.observe_sync_event(event),
        )
        .await?;
        db.complete_issue_label_sync(issue_id, &sync_token)?;

        let mut issue = db
            .get_issue(issue_id)?
            .with_context(|| format!("issue '{issue_id}' disappeared during label sync"))?;
        issue.labels_json = serde_json::to_string(&names)?;
        let mut hasher = Sha256::new();
        hasher.update(&issue.title);
        hasher.update(issue.description.as_deref().unwrap_or(""));
        hasher.update(&issue.labels_json);
        issue.content_hash = hex::encode(hasher.finalize());
        db.upsert_issue(&issue)?;
        Ok(stats.nodes)
    }

    async fn fetch_all_issue_labels_remote(&self, issue_id: &str) -> Result<Vec<LinearLabel>> {
        let mut labels = Vec::new();
        paginate(
            &self.sync_query_config,
            LinearOperation::Labels,
            Some(issue_id.to_string()),
            |request| async move {
                self.fetch_issue_labels_page(
                    issue_id,
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

    async fn fetch_all_issue_relations_remote(
        &self,
        issue_id: &str,
    ) -> Result<Vec<db::Relation>> {
        let mut relations = Vec::new();
        paginate(
            &self.sync_query_config,
            LinearOperation::Relations,
            Some(issue_id.to_string()),
            |request| async move {
                self.fetch_issue_relations_page(
                    issue_id,
                    request.cursor.as_deref(),
                    request.page_size,
                )
                .await
            },
            |nodes, _| {
                relations.extend(nodes);
                ready(Ok(()))
            },
            |relation| relation.id.clone(),
            |event| self.observe_sync_event(event),
        )
        .await?;
        Ok(relations)
    }

    pub fn comment_error_status(error: &anyhow::Error) -> &'static str {
        let message = error.to_string().to_lowercase();
        if message.contains("permission")
            || message.contains("forbidden")
            || message.contains("unauthorized")
            || message.contains("access")
        {
            "permission_denied"
        } else {
            "unavailable"
        }
    }

    fn redacted_error_message(error: &anyhow::Error) -> String {
        error.to_string().chars().take(500).collect()
    }

    pub async fn update_issue(
        &self,
        issue_id: &str,
        update: UpdateIssueInput<'_>,
    ) -> Result<()> {
        let mut input = serde_json::Map::new();
        if let Some(t) = update.title {
            input.insert("title".into(), serde_json::Value::String(t.to_string()));
        }
        if let Some(d) = update.description {
            input.insert(
                "description".into(),
                serde_json::Value::String(d.to_string()),
            );
        }
        if let Some(p) = update.priority {
            input.insert("priority".into(), serde_json::Value::Number(p.into()));
        }
        if let Some(sid) = update.state_id {
            input.insert("stateId".into(), serde_json::Value::String(sid.to_string()));
        }
        if let Some(lids) = update.label_ids {
            input.insert("labelIds".into(), serde_json::json!(lids));
        }
        if let Some(pid) = update.project_id {
            let value = if pid.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::Value::String(pid.to_string())
            };
            input.insert("projectId".into(), value);
        }
        if let Some(aid) = update.assignee_id {
            let value = if aid.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::Value::String(aid.to_string())
            };
            input.insert("assigneeId".into(), value);
        }
        if let Some(mid) = update.project_milestone_id {
            let value = if mid.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::Value::String(mid.to_string())
            };
            input.insert("projectMilestoneId".into(), value);
        }

        let query = r#"
            mutation($id: String!, $input: IssueUpdateInput!) {
                issueUpdate(id: $id, input: $input) {
                    success
                }
            }
        "#;

        let data: UpdateIssueData = self
            .query(query, serde_json::json!({ "id": issue_id, "input": input }))
            .await?;

        if !data.issue_update.success {
            anyhow::bail!("Failed to update issue");
        }

        Ok(())
    }

    pub async fn fetch_single_issue(
        &self,
        issue_id: &str,
    ) -> Result<(db::Issue, Vec<db::Relation>, Vec<String>)> {
        let query = r#"
            query($id: String!) {
                issue(id: $id) {
                    id identifier url title description priority branchName
                    createdAt updatedAt
                    state { name type }
                    team { key }
                    assignee { name }
                    project { id name }
                    projectMilestone { id name }
                    cycle { id name number }
                }
            }
        "#;

        let data: SingleIssueData = self
            .query(query, serde_json::json!({ "id": issue_id }))
            .await?;
        let issue_id = data.issue.id.clone();
        let (mut issue, _, _) = Self::convert_linear_issue(data.issue);
        let labels = self.fetch_all_issue_labels_remote(&issue_id).await?;
        let label_ids = apply_issue_labels(&mut issue, labels);
        let relations = self.fetch_all_issue_relations_remote(&issue_id).await?;
        Ok((issue, relations, label_ids))
    }

    /// Fetch a single issue from Linear by its identifier (e.g., "CUT-537").
    /// Parses the identifier into team key + number and queries via the issues filter.
    pub async fn fetch_issue_by_identifier(
        &self,
        identifier: &str,
    ) -> Result<Option<(db::Issue, Vec<db::Relation>, Vec<String>)>> {
        // Parse "CUT-537" into team_key="CUT", number=537
        let parts: Vec<&str> = identifier.rsplitn(2, '-').collect();
        if parts.len() != 2 {
            anyhow::bail!(
                "Invalid issue identifier '{}': expected format like 'ENG-123'",
                identifier
            );
        }
        let number: i32 = parts[0]
            .parse()
            .with_context(|| format!("Invalid issue number in '{}'", identifier))?;
        let team_key = parts[1];

        let query = format!(
            r#"query {{
                issues(
                    filter: {{
                        team: {{ key: {{ eq: "{}" }} }},
                        number: {{ eq: {} }}
                    }},
                    first: 1,
                    includeArchived: true
                ) {{
                    nodes {{
                        id identifier url title description priority branchName
                        createdAt updatedAt
                        state {{ name type }}
                        team {{ key }}
                        assignee {{ name }}
                        project {{ id name }}
                        projectMilestone {{ id name }}
                        cycle {{ id name number }}
                    }}
                    pageInfo {{ hasNextPage endCursor }}
                }}
            }}"#,
            team_key, number
        );

        let data: IssuesData = self.query(&query, serde_json::json!({})).await?;

        let Some(linear_issue) = data.issues.nodes.into_iter().next() else {
            return Ok(None);
        };
        let issue_id = linear_issue.id.clone();
        let (mut issue, _, _) = Self::convert_linear_issue(linear_issue);
        let labels = self.fetch_all_issue_labels_remote(&issue_id).await?;
        let label_ids = apply_issue_labels(&mut issue, labels);
        let relations = self.fetch_all_issue_relations_remote(&issue_id).await?;
        Ok(Some((issue, relations, label_ids)))
    }

    fn convert_linear_issue(i: LinearIssue) -> (db::Issue, Vec<db::Relation>, Vec<String>) {
        let labels: Vec<String> = i.labels.nodes.iter().map(|l| l.name.clone()).collect();
        let label_ids: Vec<String> = i.labels.nodes.iter().map(|l| l.id.clone()).collect();
        let labels_json = serde_json::to_string(&labels).unwrap_or_else(|_| "[]".to_string());

        let mut hasher = Sha256::new();
        hasher.update(&i.title);
        hasher.update(i.description.as_deref().unwrap_or(""));
        hasher.update(&labels_json);
        let content_hash = hex::encode(hasher.finalize());

        let relations = Self::extract_relations(&i.id, &i);

        let project_id = i.project.as_ref().map(|project| project.id.clone());
        let project_name = i.project.map(|project| project.name);
        let project_milestone_id = i
            .project_milestone
            .as_ref()
            .map(|milestone| milestone.id.clone());
        let project_milestone_name = i.project_milestone.map(|milestone| milestone.name);
        let cycle_id = i.cycle.as_ref().map(|cycle| cycle.id.clone());
        let cycle_name = i.cycle.map(|cycle| {
            cycle
                .name
                .unwrap_or_else(|| format!("Cycle {}", cycle.number))
        });

        let issue = db::Issue {
            id: i.id,
            identifier: i.identifier,
            url: i.url,
            team_key: i.team.key,
            title: i.title,
            description: i.description,
            state_name: i.state.name,
            state_type: i.state.state_type,
            priority: i.priority,
            assignee_name: i.assignee.map(|a| a.name),
            project_name,
            labels_json,
            created_at: i.created_at,
            updated_at: i.updated_at,
            content_hash,
            synced_at: None,
            branch_name: i.branch_name,
            workspace_id: "default".to_string(),
            project_id,
            project_milestone_id,
            project_milestone_name,
            cycle_id,
            cycle_name,
        };

        (issue, relations, label_ids)
    }

    fn convert_linear_comment(issue_id: &str, comment: LinearComment) -> db::Comment {
        let external_name = comment
            .external_user
            .and_then(|u| u.display_name.or(u.name));
        db::Comment {
            id: comment.id,
            issue_id: issue_id.to_string(),
            body: comment.body,
            user_name: comment.user.map(|u| u.name).or(external_name),
            created_at: comment.created_at,
            updated_at: Some(comment.updated_at),
            parent_id: comment.parent_id,
            url: Some(comment.url),
            workspace_id: "default".to_string(),
        }
    }

    /// Get a team's ID from its key
    pub async fn get_team_id(&self, team_key: &str) -> Result<String> {
        let teams = self.list_teams().await?;
        teams
            .iter()
            .find(|t| t.key.eq_ignore_ascii_case(team_key))
            .map(|t| t.id.clone())
            .with_context(|| format!("Team '{}' not found", team_key))
    }

    /// Look up a workflow state ID by name for a given team.
    /// Matches case-insensitively (e.g. "done", "cancelled", "duplicate").
    pub async fn get_state_id(&self, team_key: &str, state_name: &str) -> Result<String> {
        let team_id = self.get_team_id(team_key).await?;
        let query = r#"
            query($teamId: String!) {
                team(id: $teamId) {
                    states { nodes { id name type } }
                }
            }
        "#;

        let data: serde_json::Value = self
            .query(query, serde_json::json!({ "teamId": team_id }))
            .await?;

        let states = data["team"]["states"]["nodes"]
            .as_array()
            .context("No states in response")?;

        for state in states {
            if let Some(name) = state["name"].as_str() {
                if name.eq_ignore_ascii_case(state_name) {
                    return state["id"]
                        .as_str()
                        .map(|s| s.to_string())
                        .context("State has no id");
                }
            }
        }

        // Also try matching by type (e.g. "completed", "canceled")
        for state in states {
            if let Some(t) = state["type"].as_str() {
                if t.eq_ignore_ascii_case(state_name) {
                    return state["id"]
                        .as_str()
                        .map(|s| s.to_string())
                        .context("State has no id");
                }
            }
        }

        let available: Vec<&str> = states.iter().filter_map(|s| s["name"].as_str()).collect();
        anyhow::bail!(
            "State '{}' not found for team {}. Available: {}",
            state_name,
            team_key,
            available.join(", ")
        )
    }

    /// Resolve label names to IDs for a workspace.
    /// Linear labels are workspace-scoped, not team-scoped.
    /// Returns IDs for all matched labels and errors for any not found.
    pub async fn get_label_ids(&self, label_names: &[String]) -> Result<Vec<String>> {
        if label_names.is_empty() {
            return Ok(Vec::new());
        }

        let labels = self.fetch_labels().await?;

        let mut ids = Vec::new();
        for name in label_names {
            let found = labels
                .iter()
                .find(|label| label.name.eq_ignore_ascii_case(name));
            match found {
                Some(label) => ids.push(label.id.clone()),
                None => {
                    let available = labels
                        .iter()
                        .map(|label| label.name.as_str())
                        .collect::<Vec<_>>();
                    anyhow::bail!(
                        "Label '{}' not found. Available: {}",
                        name,
                        available.join(", ")
                    );
                }
            }
        }

        Ok(ids)
    }

    /// Resolve an assignee identifier to a Linear user id.
    ///
    /// - `"me"` (case-insensitive) → cached `viewer.id`.
    /// - `"none"` (case-insensitive) → empty string (caller decides whether that's allowed).
    /// - Anything else → case-insensitive `name` lookup against the workspace's users.
    ///   Errors if zero or multiple matches.
    pub async fn resolve_assignee_id(&self, input: &str) -> Result<String> {
        let trimmed = input.trim();
        if trimmed.eq_ignore_ascii_case("none") {
            return Ok(String::new());
        }
        if trimmed.eq_ignore_ascii_case("me") {
            if let Some(cached) = self.viewer_id.read().unwrap().clone() {
                return Ok(cached);
            }
            let data: serde_json::Value = self
                .query("query { viewer { id } }", serde_json::json!({}))
                .await?;
            let id = data["viewer"]["id"]
                .as_str()
                .context("viewer query returned no id")?
                .to_string();
            *self.viewer_id.write().unwrap() = Some(id.clone());
            return Ok(id);
        }

        // Name lookup. Linear's `users` query has no `eqIgnoreCase` filter; fetch and filter locally.
        let data: serde_json::Value = self
            .query(
                "query { users(first: 250) { nodes { id name } } }",
                serde_json::json!({}),
            )
            .await?;
        let nodes = data["users"]["nodes"]
            .as_array()
            .context("users query returned no nodes")?;
        let matches: Vec<(String, String)> = nodes
            .iter()
            .filter_map(|n| {
                let name = n["name"].as_str()?;
                if name.eq_ignore_ascii_case(trimmed) {
                    Some((n["id"].as_str()?.to_string(), name.to_string()))
                } else {
                    None
                }
            })
            .collect();

        match matches.len() {
            0 => anyhow::bail!("Assignee '{}' not found in Linear users.", trimmed),
            1 => Ok(matches.into_iter().next().unwrap().0),
            _ => {
                let names: Vec<&str> = matches.iter().map(|(_, n)| n.as_str()).collect();
                anyhow::bail!(
                    "Assignee '{}' matched multiple users: {}. Use a more specific name.",
                    trimmed,
                    names.join(", ")
                )
            }
        }
    }

    /// Fetch the full label catalog for the workspace (all pages).
    pub async fn fetch_labels(&self) -> Result<Vec<LabelCatalogEntry>> {
        let mut out = Vec::new();
        paginate(
            &self.sync_query_config,
            LinearOperation::Labels,
            None,
            |request| async move {
                self.fetch_labels_page(request.cursor.as_deref(), request.page_size)
                    .await
            },
            |nodes, _| {
                out.extend(nodes);
                ready(Ok(()))
            },
            |label| label.id.clone(),
            |event| self.observe_sync_event(event),
        )
        .await?;
        Ok(out)
    }

    async fn fetch_labels_page(
        &self,
        cursor: Option<&str>,
        page_size: usize,
    ) -> Result<ConnectionPage<LabelCatalogEntry>> {
        let query = r#"
            query($first: Int!, $after: String) {
                issueLabels(first: $first, after: $after, orderBy: updatedAt) {
                    nodes { id name color parent { id } }
                    pageInfo { hasNextPage endCursor }
                }
            }
        "#;
        let data: IssueLabelsData = self
            .query_operation(
                LinearOperation::Labels.name(),
                cursor,
                query,
                serde_json::json!({ "first": page_size, "after": cursor }),
            )
            .await?;
        Ok(ConnectionPage {
            nodes: data
                .issue_labels
                .nodes
                .into_iter()
                .map(|label| LabelCatalogEntry {
                    id: label.id,
                    name: label.name,
                    color: label.color,
                    parent_id: label.parent.map(|parent| parent.id),
                })
                .collect(),
            page_info: data.issue_labels.page_info,
        })
    }

    /// Sync the workspace's label catalog into the local database.
    /// Upserts labels by id and removes labels that no longer exist remotely.
    pub async fn sync_labels_catalog(&self, db: &Database, workspace_id: &str) -> Result<usize> {
        let sync_token = Uuid::new_v4().to_string();
        db.mark_sync_family_running(
            workspace_id,
            "*",
            "labels",
            None,
            Some(self.sync_query_config.page_size(LinearOperation::Labels)),
            &sync_token,
        )?;
        let result = paginate(
            &self.sync_query_config,
            LinearOperation::Labels,
            None,
            |request| async move {
                self.fetch_labels_page(request.cursor.as_deref(), request.page_size)
                    .await
            },
            |entries, context| {
                let result = (|| {
                    for entry in entries {
                        db.upsert_label(&db::Label {
                            id: entry.id.clone(),
                            workspace_id: workspace_id.to_string(),
                            name: entry.name,
                            color: entry.color,
                            parent_id: entry.parent_id,
                        })?;
                        db.mark_label_sync_token(&entry.id, &sync_token)?;
                    }
                    db.mark_sync_family_running(
                        workspace_id,
                        "*",
                        "labels",
                        context.cursor.as_deref(),
                        Some(context.page_size),
                        &sync_token,
                    )
                })();
                ready(result)
            },
            |label| label.id.clone(),
            |event| self.observe_sync_event(event),
        )
        .await;
        match result {
            Ok(stats) => {
                db.reconcile_label_sync(workspace_id, &sync_token)?;
                db.mark_sync_family_complete(
                    workspace_id,
                    "*",
                    "labels",
                    Some(self.sync_query_config.page_size(LinearOperation::Labels)),
                    &sync_token,
                )?;
                Ok(stats.nodes)
            }
            Err(error) => {
                let message = Self::redacted_error_message(&error);
                db.mark_sync_family_failed(
                    workspace_id,
                    "*",
                    "labels",
                    &sync_token,
                    &message,
                )?;
                Err(error)
            }
        }
    }

    /// Resolve a project name to its ID. Matches case-insensitively.
    pub async fn get_project_id(&self, project_name: &str) -> Result<String> {
        self.find_project_by_name(project_name).await
    }

    /// Create a relation between two issues.
    /// Linear API types: "blocks", "duplicate", "related".
    /// If relation_type is "blocked_by", we swap the issues and create a "blocks" relation.
    pub async fn create_relation(
        &self,
        issue_id: &str,
        related_issue_id: &str,
        relation_type: &str,
    ) -> Result<String> {
        let (actual_issue_id, actual_related_id, api_type) = if relation_type == "blocked_by" {
            (related_issue_id, issue_id, "blocks")
        } else {
            (issue_id, related_issue_id, relation_type)
        };

        let query = r#"
            mutation($input: IssueRelationCreateInput!) {
                issueRelationCreate(input: $input) {
                    success
                    issueRelation { id }
                }
            }
        "#;

        let input = serde_json::json!({
            "issueId": actual_issue_id,
            "relatedIssueId": actual_related_id,
            "type": api_type,
        });

        let data: CreateRelationData = self
            .query(query, serde_json::json!({ "input": input }))
            .await?;

        if !data.issue_relation_create.success {
            anyhow::bail!("Failed to create relation");
        }

        let relation = data
            .issue_relation_create
            .issue_relation
            .context("No relation returned")?;
        Ok(relation.id)
    }

    /// Delete a relation by its ID.
    pub async fn delete_relation(&self, relation_id: &str) -> Result<()> {
        let query = r#"
            mutation($id: String!) {
                issueRelationDelete(id: $id) {
                    success
                }
            }
        "#;

        let data: DeleteRelationData = self
            .query(query, serde_json::json!({ "id": relation_id }))
            .await?;

        if !data.issue_relation_delete.success {
            anyhow::bail!("Failed to delete relation");
        }

        Ok(())
    }
}

fn create_issue_value(create: &CreateIssueInput<'_>) -> serde_json::Value {
    let mut input = serde_json::json!({
        "teamId": create.team_id,
        "title": create.title,
    });
    if let Some(desc) = create.description {
        input["description"] = serde_json::Value::String(desc.to_string());
    }
    if let Some(priority) = create.priority {
        input["priority"] = serde_json::Value::Number(priority.into());
    }
    if !create.label_ids.is_empty() {
        input["labelIds"] = serde_json::json!(create.label_ids);
    }
    if let Some(assignee_id) = create.assignee_id {
        input["assigneeId"] = serde_json::Value::String(assignee_id.to_string());
    }
    if let Some(parent_id) = create.parent_id {
        input["parentId"] = serde_json::Value::String(parent_id.to_string());
    }
    if let Some(project_id) = create.project_id {
        input["projectId"] = serde_json::Value::String(project_id.to_string());
    }
    if let Some(milestone_id) = create.project_milestone_id {
        input["projectMilestoneId"] = serde_json::Value::String(milestone_id.to_string());
    }
    input
}

fn issue_page_variables(
    page_size: usize,
    cursor: Option<&str>,
    include_archived: bool,
) -> serde_json::Value {
    serde_json::json!({
        "first": page_size,
        "after": cursor,
        "includeArchived": include_archived,
    })
}

fn apply_issue_labels(issue: &mut db::Issue, labels: Vec<LinearLabel>) -> Vec<String> {
    let label_names = labels
        .iter()
        .map(|label| label.name.clone())
        .collect::<Vec<_>>();
    let label_ids = labels.into_iter().map(|label| label.id).collect::<Vec<_>>();
    issue.labels_json = serde_json::to_string(&label_names).unwrap_or_else(|_| "[]".to_string());
    let mut hasher = Sha256::new();
    hasher.update(&issue.title);
    hasher.update(issue.description.as_deref().unwrap_or(""));
    hasher.update(&issue.labels_json);
    issue.content_hash = hex::encode(hasher.finalize());
    label_ids
}

fn classify_http_status(status: u16) -> LinearErrorKind {
    match status {
        401 | 403 => LinearErrorKind::Authentication,
        429 => LinearErrorKind::RateLimit,
        _ => LinearErrorKind::Api,
    }
}

fn classify_graphql_message(message: &str) -> LinearErrorKind {
    let lower = message.to_lowercase();
    if lower.contains("complexity")
        || lower.contains("maximum allowed")
        || lower.contains("query cost")
    {
        LinearErrorKind::Complexity
    } else if lower.contains("rate limit") || lower.contains("too many requests") {
        LinearErrorKind::RateLimit
    } else if lower.contains("unauthorized")
        || lower.contains("forbidden")
        || lower.contains("authentication")
    {
        LinearErrorKind::Authentication
    } else if lower.contains("validation")
        || lower.contains("cannot query field")
        || lower.contains("unknown argument")
    {
        LinearErrorKind::Validation
    } else {
        LinearErrorKind::Api
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_create_serializes_project_and_milestone_relationships() {
        let labels = vec!["label-1".to_string()];
        let value = create_issue_value(&CreateIssueInput {
            team_id: "team-1",
            title: "Add request tracing",
            description: None,
            priority: Some(2),
            label_ids: &labels,
            assignee_id: None,
            parent_id: None,
            project_id: Some("project-1"),
            project_milestone_id: Some("milestone-1"),
        });
        assert_eq!(value["projectId"], serde_json::json!("project-1"));
        assert_eq!(
            value["projectMilestoneId"],
            serde_json::json!("milestone-1")
        );
        assert_eq!(value["labelIds"], serde_json::json!(["label-1"]));
    }

    #[test]
    fn issue_pages_explicitly_toggle_archived_records() {
        assert_eq!(
            issue_page_variables(50, None, false)["includeArchived"],
            serde_json::json!(false)
        );
        assert_eq!(
            issue_page_variables(50, Some("cursor-1"), true)["includeArchived"],
            serde_json::json!(true)
        );
    }

    #[test]
    fn graphql_errors_are_classified_without_stringly_typed_callers() {
        assert_eq!(
            classify_graphql_message("Query complexity: 72,400; maximum allowed: 10,000"),
            LinearErrorKind::Complexity
        );
        assert_eq!(
            classify_graphql_message("Cannot query field 'cycles'"),
            LinearErrorKind::Validation
        );
        assert_eq!(
            classify_graphql_message("Unauthorized"),
            LinearErrorKind::Authentication
        );
        assert_eq!(classify_http_status(429), LinearErrorKind::RateLimit);
    }
}
