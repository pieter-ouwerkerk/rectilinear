use std::future::ready;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::config::Config;
use crate::db::{self, Database};

mod cycles;
mod pagination;
mod progressive;
mod projects;
pub use pagination::{
    LinearErrorKind, LinearOperation, LinearOperationError, SyncEvent, SyncQueryConfig,
};
pub use progressive::*;
pub use projects::*;

use pagination::{operation_error, paginate, ConnectionPage, PageInfo};

const LINEAR_API_URL: &str = "https://api.linear.app/graphql";
const LINEAR_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const LINEAR_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

fn default_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(LINEAR_CONNECT_TIMEOUT)
        .timeout(LINEAR_REQUEST_TIMEOUT)
        .build()
        .expect("static Linear HTTP client configuration should be valid")
}

#[derive(Clone)]
pub struct LinearClient {
    client: reqwest::Client,
    api_key: String,
    api_url: String,
    viewer_id: std::sync::Arc<std::sync::RwLock<Option<String>>>,
    sync_query_config: SyncQueryConfig,
}

/// A token-leased cleanup guard for sync-family state. Dropping an in-flight
/// future (for example after an MCP cancellation) changes only the still-owned
/// `running` row to `partial`; completed, failed, or superseded attempts are
/// left untouched.
struct SyncFamilyRunGuard {
    db: Database,
    workspace_id: String,
    team_key: String,
    family: String,
    sync_token: String,
}

impl SyncFamilyRunGuard {
    fn new(
        db: &Database,
        workspace_id: &str,
        team_key: &str,
        family: &str,
        sync_token: &str,
    ) -> Self {
        Self {
            db: db.clone(),
            workspace_id: workspace_id.to_string(),
            team_key: team_key.to_string(),
            family: family.to_string(),
            sync_token: sync_token.to_string(),
        }
    }
}

impl Drop for SyncFamilyRunGuard {
    fn drop(&mut self) {
        let _ = self.db.mark_sync_family_interrupted(
            &self.workspace_id,
            &self.team_key,
            &self.family,
            &self.sync_token,
        );
    }
}

#[derive(Debug, Deserialize)]
struct GraphQLResponse<T> {
    data: Option<T>,
    errors: Option<Vec<GraphQLError>>,
}

#[derive(Debug, Deserialize)]
struct GraphQLError {
    message: String,
    #[serde(default)]
    extensions: Option<GraphQLErrorExtensions>,
}

#[derive(Debug, Deserialize)]
struct GraphQLErrorExtensions {
    #[serde(default)]
    code: Option<String>,
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
    #[serde(rename = "dueDate", default)]
    due_date: Option<String>,
    #[serde(rename = "createdAt")]
    created_at: String,
    #[serde(rename = "updatedAt")]
    updated_at: String,
    #[serde(rename = "archivedAt", default)]
    archived_at: Option<String>,
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
struct IssueInverseRelationsData {
    issue: IssueInverseRelationsNode,
}

#[derive(Debug, Deserialize)]
struct IssueInverseRelationsNode {
    #[serde(rename = "inverseRelations")]
    inverse_relations: PaginatedInverseRelationConnection,
}

#[derive(Debug, Deserialize)]
struct PaginatedInverseRelationConnection {
    nodes: Vec<LinearInverseRelation>,
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
}

/// An edge where the fetched issue is the TARGET. Linear stores "X blocked by
/// Y" as Y →blocks→ X, so blocked-by is only visible through this connection.
#[derive(Debug, Deserialize)]
struct LinearInverseRelation {
    id: String,
    #[serde(rename = "type")]
    relation_type: String,
    issue: LinearRelatedIssue,
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
    pub due_date: Option<&'a str>,
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
    pub due_date: Option<&'a str>,
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
        let client = default_http_client();
        Ok(Self {
            client,
            api_key,
            api_url: LINEAR_API_URL.to_string(),
            viewer_id: std::sync::Arc::new(std::sync::RwLock::new(None)),
            sync_query_config: SyncQueryConfig::from_environment(),
        })
    }

    /// Create a client with an explicit API key (for FFI callers).
    pub fn with_api_key(api_key: &str) -> Self {
        Self {
            client: default_http_client(),
            api_key: api_key.to_string(),
            api_url: LINEAR_API_URL.to_string(),
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
            api_url: LINEAR_API_URL.to_string(),
            viewer_id: std::sync::Arc::new(std::sync::RwLock::new(None)),
            sync_query_config: SyncQueryConfig::from_environment(),
        }
    }

    pub fn with_sync_query_config(mut self, sync_query_config: SyncQueryConfig) -> Self {
        self.sync_query_config = sync_query_config;
        self
    }

    /// Override the GraphQL endpoint (tests and self-hosted proxies).
    pub fn with_api_url(mut self, api_url: impl Into<String>) -> Self {
        self.api_url = api_url.into();
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
            let failure = self.redacted_message(failure);
            let status = if failure.starts_with("retrying attempt ") {
                "retrying"
            } else {
                "failed"
            };
            eprintln!(
                "sync operation={}{} page={} nodes={} page_size={}{} status={} error={}",
                event.operation,
                parent,
                event.page_number,
                event.nodes_received,
                event.page_size,
                reduction,
                status,
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
                if event.completed {
                    "complete"
                } else {
                    "running"
                }
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
            .post(&self.api_url)
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
        let retry_after = retry_after_from_headers(resp.headers());
        let body = resp.bytes().await.map_err(|error| {
            LinearOperationError::new(
                LinearErrorKind::Transport,
                operation,
                cursor,
                format!("failed to read response: {error}"),
            )
        })?;
        let parsed: std::result::Result<GraphQLResponse<T>, _> = serde_json::from_slice(&body);

        if let Ok(response) = parsed {
            if let Some(errors) = response.errors {
                let message = errors
                    .iter()
                    .map(|error| error.message.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let message = self.redacted_message(message);
                let kind = classify_graphql_errors(&errors);
                return Err(LinearOperationError::new(kind, operation, cursor, message)
                    .with_retry_after(retry_after)
                    .into());
            }
            if !status.is_success() {
                return Err(LinearOperationError::new(
                    classify_http_status(status.as_u16()),
                    operation,
                    cursor,
                    format!("HTTP {status} (response body omitted)"),
                )
                .with_retry_after(retry_after)
                .into());
            }
            return response.data.ok_or_else(|| {
                LinearOperationError::new(
                    LinearErrorKind::Api,
                    operation,
                    cursor,
                    "response did not contain data",
                )
                .into()
            });
        }

        if !status.is_success() {
            return Err(LinearOperationError::new(
                classify_http_status(status.as_u16()),
                operation,
                cursor,
                format!("HTTP {status} (unparseable response body omitted)"),
            )
            .with_retry_after(retry_after)
            .into());
        }
        Err(LinearOperationError::new(
            LinearErrorKind::Api,
            operation,
            cursor,
            "failed to parse GraphQL response",
        )
        .into())
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

    /// Convert an inverse edge into a local row under the fetched issue.
    /// Only "blocks" is kept (as blocked_by): symmetric types like related
    /// would merely duplicate what the forward connection already reports.
    /// The synthetic ":inv" id suffix keeps the row from colliding with the
    /// forward-side row for the same Linear relation when both are synced.
    fn convert_inverse_relation(
        issue_id: &str,
        relation: LinearInverseRelation,
    ) -> Option<db::Relation> {
        if relation.relation_type != "blocks" {
            return None;
        }
        Some(db::Relation {
            id: format!("{}:inv", relation.id),
            issue_id: issue_id.to_string(),
            related_issue_id: relation.issue.id,
            related_issue_identifier: relation.issue.identifier,
            relation_type: "blocked_by".to_string(),
        })
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
    ) -> Result<(
        Vec<(db::Issue, Vec<db::Relation>, Vec<String>)>,
        bool,
        Option<String>,
    )> {
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
                        id identifier url title description priority dueDate branchName
                        createdAt updatedAt archivedAt
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

        Ok(ConnectionPage {
            nodes: issues,
            page_info: data.issues.page_info,
        })
    }

    /// Compatibility orchestration for existing clients. New clients should
    /// call `sync_team_index` and then bounded hydration explicitly.
    pub async fn sync_team(
        &self,
        db: &Database,
        team_key: &str,
        workspace_id: &str,
        full: bool,
        include_archived: bool,
        progress: Option<&SyncProgressCallback<'_>>,
    ) -> Result<usize> {
        self.sync_projects_for_team(db, workspace_id, team_key, include_archived)
            .await
            .with_context(|| {
                format!("project synchronization failed for workspace '{workspace_id}'")
            })?;
        self.sync_labels_catalog(db, workspace_id)
            .await
            .with_context(|| {
                format!("label synchronization failed for workspace '{workspace_id}'")
            })?;
        self.sync_cycles(db, team_key, workspace_id, include_archived)
            .await
            .with_context(|| format!("cycle synchronization failed for team '{team_key}'"))?;

        let index = self
            .sync_team_index(db, team_key, workspace_id, full, progress)
            .await?;

        if full {
            db.requeue_team_hydration(workspace_id, team_key, "legacy_full")?;
        } else {
            db.requeue_team_retryable_comments(workspace_id, team_key)?;
        }
        let issue_count = db.count_issues(Some(team_key), workspace_id)?;
        let hydration = self
            .hydrate_pending_issues(
                db,
                team_key,
                workspace_id,
                issue_count.max(1),
                db::HydrationPolicy::All,
                progress,
            )
            .await?;

        let family_token = Uuid::new_v4().to_string();
        if hydration.required_failures == 0 {
            db.mark_sync_family_complete(
                workspace_id,
                team_key,
                "issue labels",
                None,
                &family_token,
            )?;
            db.mark_sync_family_complete(workspace_id, team_key, "relations", None, &family_token)?;
        } else {
            let summary = format!(
                "{} required hydration resource(s) failed",
                hydration.required_failures
            );
            db.mark_sync_family_partial(
                workspace_id,
                team_key,
                "issue labels",
                &family_token,
                &summary,
            )?;
            db.mark_sync_family_partial(
                workspace_id,
                team_key,
                "relations",
                &family_token,
                &summary,
            )?;
        }
        if hydration.comment_failures == 0 {
            db.mark_sync_family_complete(workspace_id, team_key, "comments", None, &family_token)?;
        } else {
            let summary = format!("{} comment hydration(s) failed", hydration.comment_failures);
            db.mark_sync_family_partial(
                workspace_id,
                team_key,
                "comments",
                &family_token,
                &summary,
            )?;
        }
        if hydration.required_failures > 0 || hydration.rate_limited {
            anyhow::bail!(
                "issue hydration incomplete after the issue index committed ({} required failures, rate_limited={})",
                hydration.required_failures,
                hydration.rate_limited
            );
        }
        Ok(index.indexed)
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
                ready(db.upsert_comment_page(issue_id, workspace_id, &comments, &sync_token))
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
                let message = self.redacted_error_message(&error);
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

    async fn fetch_issue_inverse_relations_page(
        &self,
        issue_id: &str,
        cursor: Option<&str>,
        page_size: usize,
    ) -> Result<ConnectionPage<db::Relation>> {
        let query = r#"
            query($issueId: String!, $first: Int!, $after: String) {
                issue(id: $issueId) {
                    inverseRelations(first: $first, after: $after) {
                        nodes { id type issue { id identifier } }
                        pageInfo { hasNextPage endCursor }
                    }
                }
            }
        "#;
        let data: IssueInverseRelationsData = self
            .query_operation(
                LinearOperation::InverseRelations.name(),
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
                .inverse_relations
                .nodes
                .into_iter()
                .filter_map(|relation| Self::convert_inverse_relation(issue_id, relation))
                .collect(),
            page_info: data.issue.inverse_relations.page_info,
        })
    }

    pub async fn sync_issue_relations(&self, db: &Database, issue_id: &str) -> Result<usize> {
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
            |relations, _| ready(db.upsert_relation_page(issue_id, &relations, &sync_token)),
            |relation| relation.id.clone(),
            |event| self.observe_sync_event(event),
        )
        .await?;
        // Inverse edges land under the same sync token so the cleanup below
        // treats both connections as one atomic refresh of this issue's rows.
        let inverse_stats = paginate(
            &self.sync_query_config,
            LinearOperation::InverseRelations,
            Some(issue_id.to_string()),
            |request| async move {
                self.fetch_issue_inverse_relations_page(
                    issue_id,
                    request.cursor.as_deref(),
                    request.page_size,
                )
                .await
            },
            |relations, _| ready(db.upsert_relation_page(issue_id, &relations, &sync_token)),
            |relation| relation.id.clone(),
            |event| self.observe_sync_event(event),
        )
        .await?;
        db.complete_relation_sync(issue_id, &sync_token)?;
        Ok(stats.nodes + inverse_stats.nodes)
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
        let workspace_id = db
            .get_issue(issue_id)?
            .with_context(|| format!("issue '{issue_id}' disappeared before label sync"))?
            .workspace_id;
        self.sync_issue_labels_in_workspace(db, issue_id, &workspace_id)
            .await
    }

    pub async fn sync_issue_labels_in_workspace(
        &self,
        db: &Database,
        issue_id: &str,
        workspace_id: &str,
    ) -> Result<usize> {
        let sync_token = Uuid::new_v4().to_string();
        let mut names = Vec::new();
        let stats = paginate(
            &self.sync_query_config,
            LinearOperation::Labels,
            Some(issue_id.to_string()),
            |request| async move {
                self.fetch_issue_labels_page(issue_id, request.cursor.as_deref(), request.page_size)
                    .await
            },
            |labels, _| {
                let catalog_result = labels.iter().try_for_each(|label| {
                    db.upsert_label(&db::Label {
                        id: label.id.clone(),
                        workspace_id: workspace_id.to_string(),
                        name: label.name.clone(),
                        color: None,
                        parent_id: None,
                    })
                });
                if let Err(error) = catalog_result {
                    return ready(Err(error));
                }
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
                self.fetch_issue_labels_page(issue_id, request.cursor.as_deref(), request.page_size)
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

    async fn fetch_all_issue_relations_remote(&self, issue_id: &str) -> Result<Vec<db::Relation>> {
        let mut relations = Vec::new();
        let mut inverse_relations = Vec::new();
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
        paginate(
            &self.sync_query_config,
            LinearOperation::InverseRelations,
            Some(issue_id.to_string()),
            |request| async move {
                self.fetch_issue_inverse_relations_page(
                    issue_id,
                    request.cursor.as_deref(),
                    request.page_size,
                )
                .await
            },
            |nodes, _| {
                inverse_relations.extend(nodes);
                ready(Ok(()))
            },
            |relation| relation.id.clone(),
            |event| self.observe_sync_event(event),
        )
        .await?;
        relations.extend(inverse_relations);
        Ok(relations)
    }

    pub fn comment_error_status(error: &anyhow::Error) -> &'static str {
        if operation_error(error)
            .is_some_and(|classified| classified.kind == LinearErrorKind::Authentication)
        {
            return "permission_denied";
        }
        let message = format!("{error:#}").to_lowercase();
        if message.contains("permission")
            || message.contains("forbidden")
            || message.contains("unauthorized")
            || message.contains("access denied")
        {
            "permission_denied"
        } else {
            "unavailable"
        }
    }

    fn redacted_error_message(&self, error: &anyhow::Error) -> String {
        self.redacted_message(format!("{error:#}"))
    }

    fn redacted_message(&self, mut message: String) -> String {
        if !self.api_key.is_empty() {
            message = message.replace(&self.api_key, "[REDACTED]");
        }
        message = redact_sensitive_fragments(message);
        message.chars().take(500).collect()
    }

    pub async fn update_issue(&self, issue_id: &str, update: UpdateIssueInput<'_>) -> Result<()> {
        let input = update_issue_value(&update);

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
                    id identifier url title description priority dueDate branchName
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
                        id identifier url title description priority dueDate branchName
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
            due_date: i.due_date,
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
            archived_at: i.archived_at,
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
        let _family_guard = SyncFamilyRunGuard::new(db, workspace_id, "*", "labels", &sync_token);
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
                let message = self.redacted_error_message(&error);
                db.mark_sync_family_failed(workspace_id, "*", "labels", &sync_token, &message)?;
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
    if let Some(due_date) = create.due_date {
        input["dueDate"] = serde_json::Value::String(due_date.to_string());
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

fn update_issue_value(update: &UpdateIssueInput<'_>) -> serde_json::Map<String, serde_json::Value> {
    let mut input = serde_json::Map::new();
    if let Some(title) = update.title {
        input.insert("title".into(), serde_json::Value::String(title.to_string()));
    }
    if let Some(description) = update.description {
        input.insert(
            "description".into(),
            serde_json::Value::String(description.to_string()),
        );
    }
    if let Some(priority) = update.priority {
        input.insert(
            "priority".into(),
            serde_json::Value::Number(priority.into()),
        );
    }
    if let Some(due_date) = update.due_date {
        let value = if due_date.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::Value::String(due_date.to_string())
        };
        input.insert("dueDate".into(), value);
    }
    if let Some(state_id) = update.state_id {
        input.insert(
            "stateId".into(),
            serde_json::Value::String(state_id.to_string()),
        );
    }
    if let Some(label_ids) = update.label_ids {
        input.insert("labelIds".into(), serde_json::json!(label_ids));
    }
    for (key, value) in [
        ("projectId", update.project_id),
        ("assigneeId", update.assignee_id),
        ("projectMilestoneId", update.project_milestone_id),
    ] {
        if let Some(value) = value {
            let value = if value.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::Value::String(value.to_string())
            };
            input.insert(key.into(), value);
        }
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

fn redact_sensitive_fragments(mut message: String) -> String {
    message = redact_token_after_marker(message, "bearer ");
    for marker in [
        "authorization:",
        "authorization=",
        "api_key:",
        "api_key=",
        "api-key:",
        "api-key=",
        "access_token:",
        "access_token=",
        "password:",
        "password=",
        "secret:",
        "secret=",
    ] {
        message = redact_token_after_marker(message, marker);
    }
    message
}

fn redact_token_after_marker(mut message: String, marker: &str) -> String {
    let mut search_from = 0;
    loop {
        let lower = message.to_ascii_lowercase();
        let Some(relative_start) = lower[search_from..].find(marker) else {
            break;
        };
        let marker_end = search_from + relative_start + marker.len();
        let bytes = message.as_bytes();
        let mut value_start = marker_end;
        while value_start < bytes.len()
            && (bytes[value_start].is_ascii_whitespace()
                || matches!(bytes[value_start], b'\'' | b'"'))
        {
            value_start += 1;
        }
        let mut value_end = value_start;
        while value_end < bytes.len()
            && !bytes[value_end].is_ascii_whitespace()
            && !matches!(bytes[value_end], b',' | b';' | b'\'' | b'"' | b')')
        {
            value_end += 1;
        }
        if value_start == value_end {
            search_from = marker_end;
            continue;
        }
        message.replace_range(value_start..value_end, "[REDACTED]");
        search_from = value_start + "[REDACTED]".len();
    }
    message
}

fn classify_http_status(status: u16) -> LinearErrorKind {
    match status {
        401 | 403 => LinearErrorKind::Authentication,
        429 => LinearErrorKind::RateLimit,
        408 | 500..=599 => LinearErrorKind::Transient,
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
    } else if lower.contains("internal server")
        || lower.contains("internal error")
        || lower.contains("temporarily unavailable")
        || lower.contains("service unavailable")
        || lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("try again")
    {
        LinearErrorKind::Transient
    } else {
        LinearErrorKind::Api
    }
}

fn classify_graphql_errors(errors: &[GraphQLError]) -> LinearErrorKind {
    if errors.iter().any(|error| {
        error
            .extensions
            .as_ref()
            .and_then(|extensions| extensions.code.as_deref())
            .is_some_and(|code| code.eq_ignore_ascii_case("RATELIMITED"))
    }) {
        return LinearErrorKind::RateLimit;
    }
    let message = errors
        .iter()
        .map(|error| error.message.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    classify_graphql_message(&message)
}

fn retry_after_from_headers(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    if let Some(seconds) = headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
    {
        return Some(Duration::from_secs(seconds).min(Duration::from_secs(6 * 60 * 60)));
    }
    for name in ["x-ratelimit-requests-reset", "x-ratelimit-reset"] {
        let Some(value) = headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
        else {
            continue;
        };
        let value = if value > 1_000_000_000_000 {
            value / 1_000
        } else {
            value
        };
        let now = chrono::Utc::now().timestamp().max(0) as u64;
        let seconds = if value > now { value - now } else { value };
        return Some(Duration::from_secs(seconds).min(Duration::from_secs(6 * 60 * 60)));
    }
    None
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use crate::db::SyncFamilyState;

    use super::*;

    static SYNC_HTTP_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Serialize mock-HTTP tests without letting one test's panic poison the
    /// lock and cascade into every later test: each test owns its server and
    /// database, so a previous failure is irrelevant to the next one.
    fn serial_http_test() -> std::sync::MutexGuard<'static, ()> {
        SYNC_HTTP_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    struct MockResponse {
        status: u16,
        body: String,
    }

    impl MockResponse {
        fn json(body: serde_json::Value) -> Self {
            Self {
                status: 200,
                body: body.to_string(),
            }
        }

        fn status(status: u16) -> Self {
            Self {
                status,
                body: "{}".to_string(),
            }
        }
    }

    struct MockLinearServer {
        url: String,
        stop: Arc<AtomicBool>,
        worker: Option<thread::JoinHandle<()>>,
    }

    impl MockLinearServer {
        fn start<F>(mut handler: F) -> Self
        where
            F: FnMut(serde_json::Value) -> MockResponse + Send + 'static,
        {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let address = listener.local_addr().unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let worker_stop = Arc::clone(&stop);
            let worker = thread::spawn(move || {
                while !worker_stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            // Accepted sockets may inherit the listener's nonblocking mode.
                            stream.set_nonblocking(false).unwrap();
                            if let Some(request) = read_json_request(&mut stream) {
                                write_mock_response(&mut stream, handler(request));
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(_) => break,
                    }
                }
            });
            Self {
                url: format!("http://{address}/graphql"),
                stop,
                worker: Some(worker),
            }
        }
    }

    impl Drop for MockLinearServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    fn read_json_request(stream: &mut TcpStream) -> Option<serde_json::Value> {
        stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4096];
        let (header_end, content_length) = loop {
            let count = stream.read(&mut buffer).ok()?;
            if count == 0 {
                return None;
            }
            request.extend_from_slice(&buffer[..count]);
            if let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                break (header_end + 4, content_length);
            }
        };
        while request.len() < header_end + content_length {
            let count = stream.read(&mut buffer).ok()?;
            if count == 0 {
                return None;
            }
            request.extend_from_slice(&buffer[..count]);
        }
        serde_json::from_slice(&request[header_end..header_end + content_length]).ok()
    }

    fn write_mock_response(stream: &mut TcpStream, response: MockResponse) {
        let reason = match response.status {
            200 => "OK",
            403 => "Forbidden",
            500 => "Internal Server Error",
            _ => "Mock",
        };
        let headers = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            response.status,
            reason,
            response.body.len()
        );
        let _ = stream.write_all(headers.as_bytes());
        let _ = stream.write_all(response.body.as_bytes());
    }

    fn issue_node(id: &str, identifier: &str, updated_at: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "identifier": identifier,
            "url": format!("https://linear.app/issue/{identifier}"),
            "title": format!("Issue {identifier}"),
            "description": "Mock issue",
            "priority": 2,
            "dueDate": "2026-08-19",
            "branchName": null,
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": updated_at,
            "state": { "name": "Todo", "type": "unstarted" },
            "team": { "key": "CUT" },
            "assignee": null,
            "project": null,
            "projectMilestone": null,
            "cycle": null
        })
    }

    fn project_node(id: &str, name: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "slugId": id,
            "name": name,
            "description": "Project description",
            "content": null,
            "icon": null,
            "color": "#123456",
            "priority": 2,
            "startDate": null,
            "targetDate": null,
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-02-01T00:00:00Z",
            "archivedAt": null,
            "url": format!("https://linear.app/project/{id}"),
            "progress": 0.25,
            "status": { "id": "status-1", "name": "Planned", "type": "planned", "color": "#abcdef" },
            "lead": null
        })
    }

    fn milestone_node(project_id: &str) -> serde_json::Value {
        serde_json::json!({
            "id": format!("milestone-{project_id}"),
            "name": format!("Milestone {project_id}"),
            "description": null,
            "targetDate": null,
            "status": "next",
            "progress": 0.5,
            "sortOrder": 1.0,
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-02-01T00:00:00Z",
            "archivedAt": null,
            "project": { "id": project_id, "name": format!("Project {project_id}") }
        })
    }

    fn project_sync_response(
        request: &serde_json::Value,
        team_project_count: usize,
    ) -> Option<MockResponse> {
        let query = request["query"].as_str().unwrap_or_default();
        if query.contains("teams(first:") && !query.contains("project(id:") {
            return Some(MockResponse::json(serde_json::json!({
                "data": { "teams": {
                    "nodes": [{ "id": "team-cut", "key": "CUT", "name": "Cuttlefish" }],
                    "pageInfo": { "hasNextPage": false, "endCursor": null }
                }}
            })));
        }
        if query.contains("projects(") {
            let team_scoped = request["variables"]["teamId"].is_string();
            let count = if team_scoped { team_project_count } else { 2 };
            let nodes = (1..=count)
                .map(|number| {
                    project_node(&format!("project-{number}"), &format!("Project {number}"))
                })
                .collect::<Vec<_>>();
            return Some(MockResponse::json(serde_json::json!({
                "data": { "projects": {
                    "nodes": nodes,
                    "pageInfo": { "hasNextPage": false, "endCursor": null }
                }}
            })));
        }
        let project_id = request["variables"]["id"].as_str().unwrap_or("project-1");
        if query.contains("projectMilestones(") {
            return Some(MockResponse::json(serde_json::json!({
                "data": { "project": { "projectMilestones": {
                    "nodes": [milestone_node(project_id)],
                    "pageInfo": { "hasNextPage": false, "endCursor": null }
                }}}
            })));
        }
        if query.contains("teams(first:") {
            return Some(MockResponse::json(serde_json::json!({
                "data": { "project": { "teams": {
                    "nodes": [{ "id": "team-cut", "key": "CUT", "name": "Cuttlefish" }],
                    "pageInfo": { "hasNextPage": false, "endCursor": null }
                }}}
            })));
        }
        if query.contains("members(first:") {
            return Some(MockResponse::json(serde_json::json!({
                "data": { "project": { "members": {
                    "nodes": [],
                    "pageInfo": { "hasNextPage": false, "endCursor": null }
                }}}
            })));
        }
        if query.contains("labels(first:") {
            return Some(MockResponse::json(serde_json::json!({
                "data": { "project": { "labels": {
                    "nodes": [],
                    "pageInfo": { "hasNextPage": false, "endCursor": null }
                }}}
            })));
        }
        None
    }

    fn standard_sync_response(request: &serde_json::Value) -> Option<MockResponse> {
        let query = request["query"].as_str().unwrap_or_default();
        if query.contains("issue(id: $id)") && query.contains("description priority") {
            let issue_id = request["variables"]["id"].as_str().unwrap_or("issue-1");
            let (identifier, updated_at) = if issue_id == "issue-2" {
                ("CUT-2", "2026-02-02T00:00:00Z")
            } else {
                ("CUT-1", "2026-02-01T00:00:00Z")
            };
            return Some(MockResponse::json(serde_json::json!({
                "data": { "issue": issue_node(issue_id, identifier, updated_at) }
            })));
        }
        if query.contains("teams(first:") {
            return Some(MockResponse::json(serde_json::json!({
                "data": { "teams": {
                    "nodes": [{ "id": "team-1", "key": "CUT", "name": "Cuttlefish" }],
                    "pageInfo": { "hasNextPage": false, "endCursor": null }
                }}
            })));
        }
        if query.contains("projects(") {
            return Some(MockResponse::json(serde_json::json!({
                "data": { "projects": {
                    "nodes": [],
                    "pageInfo": { "hasNextPage": false, "endCursor": null }
                }}
            })));
        }
        if query.contains("issueLabels(") {
            return Some(MockResponse::json(serde_json::json!({
                "data": { "issueLabels": {
                    "nodes": [],
                    "pageInfo": { "hasNextPage": false, "endCursor": null }
                }}
            })));
        }
        if query.contains("cycles(") {
            return Some(MockResponse::json(serde_json::json!({
                "data": { "cycles": {
                    "nodes": [],
                    "pageInfo": { "hasNextPage": false, "endCursor": null }
                }}
            })));
        }
        if query.contains("issues(") {
            let nodes = if request["variables"]["lower"].is_string()
                || query.contains("updatedAt: { gt:")
            {
                Vec::new()
            } else {
                vec![
                    issue_node("issue-1", "CUT-1", "2026-02-01T00:00:00Z"),
                    issue_node("issue-2", "CUT-2", "2026-02-02T00:00:00Z"),
                ]
            };
            return Some(MockResponse::json(serde_json::json!({
                "data": { "issues": {
                    "nodes": nodes,
                    "pageInfo": { "hasNextPage": false, "endCursor": null }
                }}
            })));
        }
        if query.contains("labels(first:") {
            return Some(MockResponse::json(serde_json::json!({
                "data": { "issue": { "labels": {
                    "nodes": [],
                    "pageInfo": { "hasNextPage": false, "endCursor": null }
                }}}
            })));
        }
        if query.contains("inverseRelations(first:") {
            return Some(MockResponse::json(serde_json::json!({
                "data": { "issue": { "inverseRelations": {
                    "nodes": [],
                    "pageInfo": { "hasNextPage": false, "endCursor": null }
                }}}
            })));
        }
        if query.contains("relations(first:") {
            return Some(MockResponse::json(serde_json::json!({
                "data": { "issue": { "relations": {
                    "nodes": [],
                    "pageInfo": { "hasNextPage": false, "endCursor": null }
                }}}
            })));
        }
        None
    }

    fn successful_comments() -> MockResponse {
        MockResponse::json(serde_json::json!({
            "data": { "comments": {
                "nodes": [],
                "pageInfo": { "hasNextPage": false, "endCursor": null }
            }}
        }))
    }

    fn comment_issue_id(request: &serde_json::Value) -> Option<&str> {
        request["query"]
            .as_str()
            .is_some_and(|query| query.contains("comments("))
            .then(|| request["variables"]["issueId"].as_str())
            .flatten()
    }

    fn test_client(api_url: &str) -> LinearClient {
        let mut config = SyncQueryConfig::default();
        config.max_retry_attempts = 2;
        config.retry_base_delay = Duration::ZERO;
        // no_proxy: workspace builds unify reqwest features with the binary
        // crate, which enables macOS system-proxy support. A proxy configured
        // on the machine (e.g. a debugging proxy on 127.0.0.1:8080) would
        // swallow every request to the mock server. Tests always dial direct.
        let http = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("test HTTP client configuration should be valid");
        LinearClient::with_http_client(http, "test-api-key")
            .with_api_url(api_url)
            .with_sync_query_config(config)
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn test_db() -> (Database, tempfile::TempDir) {
        let directory = tempfile::tempdir().unwrap();
        let db = Database::open(&directory.path().join("test.db")).unwrap();
        (db, directory)
    }

    fn family_status(db: &Database, family: &str) -> SyncFamilyState {
        db.get_sync_family_state("default", "CUT", family)
            .unwrap()
            .unwrap()
    }

    #[test]
    fn sync_team_reports_batch_hydration_progress() {
        let _serial = serial_http_test();
        let server = MockLinearServer::start(|request| {
            let query = request["query"].as_str().unwrap_or_default();
            if query.contains("comments(") {
                return successful_comments();
            }
            standard_sync_response(&request).expect("unexpected GraphQL operation")
        });
        let client = test_client(&server.url);
        let (db, _dir) = test_db();

        let updates = Mutex::new(Vec::<SyncProgressUpdate>::new());
        let progress = |update: SyncProgressUpdate| {
            updates.lock().unwrap().push(update);
        };
        let count = runtime()
            .block_on(client.sync_team(&db, "CUT", "default", true, true, Some(&progress)))
            .unwrap();
        assert_eq!(count, 2);

        let updates = updates.into_inner().unwrap();
        let batch: Vec<(usize, Option<usize>)> = updates
            .iter()
            .filter(|u| u.phase == SyncProgressPhase::HydratingIssues)
            .map(|u| (u.completed, u.total))
            .collect();
        assert_eq!(batch, vec![(1, Some(2)), (2, Some(2))]);
        assert!(updates
            .iter()
            .any(|u| u.phase == SyncProgressPhase::HydratingRelations));
        assert!(updates
            .iter()
            .any(|u| u.phase == SyncProgressPhase::IndexingIssues));
    }

    #[test]
    fn issue_create_serializes_project_and_milestone_relationships() {
        let labels = vec!["label-1".to_string()];
        let value = create_issue_value(&CreateIssueInput {
            team_id: "team-1",
            title: "Add request tracing",
            description: None,
            priority: Some(2),
            due_date: Some("2026-08-19"),
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
        assert_eq!(value["dueDate"], serde_json::json!("2026-08-19"));
    }

    #[test]
    fn issue_create_omits_due_date_when_not_provided() {
        let value = create_issue_value(&CreateIssueInput {
            team_id: "team-1",
            title: "No deadline",
            description: None,
            priority: None,
            due_date: None,
            label_ids: &[],
            assignee_id: None,
            parent_id: None,
            project_id: None,
            project_milestone_id: None,
        });
        assert!(value.get("dueDate").is_none());
    }

    #[test]
    fn issue_update_sets_and_clears_due_date() {
        let set = update_issue_value(&UpdateIssueInput {
            due_date: Some("2026-08-19"),
            ..Default::default()
        });
        assert_eq!(set["dueDate"], serde_json::json!("2026-08-19"));

        let clear = update_issue_value(&UpdateIssueInput {
            due_date: Some(""),
            ..Default::default()
        });
        assert_eq!(clear["dueDate"], serde_json::Value::Null);
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
        assert_eq!(
            classify_graphql_message("Internal server error; try again"),
            LinearErrorKind::Transient
        );
        assert_eq!(classify_http_status(429), LinearErrorKind::RateLimit);
        assert_eq!(classify_http_status(500), LinearErrorKind::Transient);
    }

    #[test]
    fn team_sync_retries_http_500_comments_then_completes() {
        let _serial = serial_http_test();
        let attempts = Arc::new(Mutex::new(HashMap::<String, usize>::new()));
        let server_attempts = Arc::clone(&attempts);
        let server = MockLinearServer::start(move |request| {
            if let Some(issue_id) = comment_issue_id(&request) {
                let mut attempts = server_attempts.lock().unwrap();
                let count = attempts.entry(issue_id.to_string()).or_default();
                *count += 1;
                if issue_id == "issue-1" && *count == 1 {
                    return MockResponse::status(500);
                }
                return successful_comments();
            }
            standard_sync_response(&request).expect("unexpected GraphQL operation")
        });
        let client = test_client(&server.url);
        let (db, _dir) = test_db();

        let count = runtime()
            .block_on(client.sync_team(&db, "CUT", "default", true, true, None))
            .unwrap();

        assert_eq!(count, 2);
        assert_eq!(attempts.lock().unwrap().get("issue-1"), Some(&2));
        assert_eq!(
            db.get_comment_sync_state("issue-1").unwrap().status,
            "none_found"
        );
        assert_eq!(family_status(&db, "comments").status, "complete");
    }

    #[test]
    fn exhausted_comment_retries_are_partial_and_do_not_block_later_issues_or_cursor() {
        let _serial = serial_http_test();
        let attempts = Arc::new(Mutex::new(HashMap::<String, usize>::new()));
        let server_attempts = Arc::clone(&attempts);
        let server = MockLinearServer::start(move |request| {
            if let Some(issue_id) = comment_issue_id(&request) {
                let mut attempts = server_attempts.lock().unwrap();
                *attempts.entry(issue_id.to_string()).or_default() += 1;
                return if issue_id == "issue-1" {
                    MockResponse::status(500)
                } else {
                    successful_comments()
                };
            }
            standard_sync_response(&request).expect("unexpected GraphQL operation")
        });
        let client = test_client(&server.url);
        let (db, _dir) = test_db();

        let count = runtime()
            .block_on(client.sync_team(&db, "CUT", "default", true, true, None))
            .unwrap();

        assert_eq!(count, 2);
        assert_eq!(attempts.lock().unwrap().get("issue-1"), Some(&3));
        assert_eq!(attempts.lock().unwrap().get("issue-2"), Some(&1));
        let failed = db.get_comment_sync_state("issue-1").unwrap();
        assert_eq!(failed.status, "unavailable");
        let diagnostic = failed.sync_error.unwrap();
        assert!(diagnostic.contains("failed to paginate comments at cursor None"));
        assert!(diagnostic.contains("HTTP 500"));
        assert!(diagnostic.chars().count() <= 500);
        assert_eq!(
            db.get_comment_sync_state("issue-2").unwrap().status,
            "none_found"
        );
        assert_eq!(family_status(&db, "issue labels").status, "complete");
        assert_eq!(family_status(&db, "relations").status, "complete");
        let comments = family_status(&db, "comments");
        assert_eq!(comments.status, "partial");
        assert_eq!(
            comments.error.as_deref(),
            Some("1 comment hydration(s) failed")
        );
        assert!(db.get_sync_cursor("default", "CUT").unwrap().is_some());
    }

    #[test]
    fn permission_comment_failure_is_not_retried_and_other_issues_continue() {
        let _serial = serial_http_test();
        let attempts = Arc::new(Mutex::new(HashMap::<String, usize>::new()));
        let server_attempts = Arc::clone(&attempts);
        let server = MockLinearServer::start(move |request| {
            if let Some(issue_id) = comment_issue_id(&request) {
                let mut attempts = server_attempts.lock().unwrap();
                *attempts.entry(issue_id.to_string()).or_default() += 1;
                if issue_id == "issue-1" {
                    return MockResponse::json(serde_json::json!({
                        "errors": [{
                            "message": "Forbidden: Authorization: Bearer super-secret-token"
                        }]
                    }));
                }
                return successful_comments();
            }
            standard_sync_response(&request).expect("unexpected GraphQL operation")
        });
        let client = test_client(&server.url);
        let (db, _dir) = test_db();

        runtime()
            .block_on(client.sync_team(&db, "CUT", "default", true, true, None))
            .unwrap();

        assert_eq!(attempts.lock().unwrap().get("issue-1"), Some(&1));
        assert_eq!(attempts.lock().unwrap().get("issue-2"), Some(&1));
        let failed = db.get_comment_sync_state("issue-1").unwrap();
        assert_eq!(failed.status, "permission_denied");
        let diagnostic = failed.sync_error.unwrap();
        assert!(diagnostic.contains("Forbidden"));
        assert!(!diagnostic.contains("super-secret-token"));
        assert_eq!(
            db.get_comment_sync_state("issue-2").unwrap().status,
            "none_found"
        );
    }

    #[test]
    fn later_incremental_sync_recovers_failed_comment_state_and_clears_error() {
        let _serial = serial_http_test();
        let should_fail = Arc::new(AtomicBool::new(true));
        let server_should_fail = Arc::clone(&should_fail);
        let attempts = Arc::new(Mutex::new(HashMap::<String, usize>::new()));
        let server_attempts = Arc::clone(&attempts);
        let server = MockLinearServer::start(move |request| {
            if let Some(issue_id) = comment_issue_id(&request) {
                let mut attempts = server_attempts.lock().unwrap();
                *attempts.entry(issue_id.to_string()).or_default() += 1;
                if issue_id == "issue-1" && server_should_fail.load(Ordering::Relaxed) {
                    return MockResponse::status(500);
                }
                return successful_comments();
            }
            standard_sync_response(&request).expect("unexpected GraphQL operation")
        });
        let client = test_client(&server.url);
        let (db, _dir) = test_db();
        let rt = runtime();

        rt.block_on(client.sync_team(&db, "CUT", "default", true, true, None))
            .unwrap();
        assert_eq!(
            db.get_comment_sync_state("issue-1").unwrap().status,
            "unavailable"
        );
        let first_cursor = db.get_sync_cursor("default", "CUT").unwrap();
        should_fail.store(false, Ordering::Relaxed);

        let count = rt
            .block_on(client.sync_team(&db, "CUT", "default", false, false, None))
            .unwrap();

        assert_eq!(count, 0);
        let recovered = db.get_comment_sync_state("issue-1").unwrap();
        assert_eq!(recovered.status, "none_found");
        assert!(recovered.sync_error.is_none());
        assert_eq!(family_status(&db, "comments").status, "complete");
        assert_ne!(db.get_sync_cursor("default", "CUT").unwrap(), first_cursor);
        assert_eq!(attempts.lock().unwrap().get("issue-1"), Some(&4));
    }

    #[test]
    fn index_only_traverses_bounded_pages_without_supplemental_requests() {
        let _serial = serial_http_test();
        let requests = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let server_requests = Arc::clone(&requests);
        let server = MockLinearServer::start(move |request| {
            server_requests.lock().unwrap().push(request.clone());
            let after = request["variables"]["after"].as_str();
            let (nodes, has_next_page, end_cursor) = if after.is_none() {
                (
                    vec![issue_node("issue-1", "CUT-1", "2026-02-01T00:00:00Z")],
                    true,
                    Some("cursor-1"),
                )
            } else {
                (
                    vec![issue_node("issue-2", "CUT-2", "2026-02-02T00:00:00Z")],
                    false,
                    None,
                )
            };
            MockResponse::json(serde_json::json!({
                "data": { "issues": {
                    "nodes": nodes,
                    "pageInfo": {
                        "hasNextPage": has_next_page,
                        "endCursor": end_cursor
                    }
                }}
            }))
        });
        let client = test_client(&server.url);
        let (db, _dir) = test_db();
        let upper = chrono::DateTime::parse_from_rfc3339("2026-03-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);

        let result = runtime()
            .block_on(client.sync_team_index_window(&db, "CUT", "default", true, upper, None))
            .unwrap();

        assert_eq!(result.indexed, 2);
        assert_eq!(result.inserted, 2);
        assert_eq!(result.queued_for_hydration, 2);
        assert_eq!(result.committed_checkpoint, "2026-03-01T00:00:00+00:00");
        assert_eq!(
            db.list_all_issues(Some("CUT"), None, 10, 0, "default")
                .unwrap()
                .len(),
            2
        );
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|request| {
            let query = request["query"].as_str().unwrap();
            query.contains("$upper: DateTimeOrDuration!")
                && query.contains("orderBy: updatedAt")
                && !query.contains("description")
                && !query.contains("comments(")
                && !query.contains("relations(")
                && !query.contains("labels(")
        }));
    }

    #[test]
    fn team_project_sync_is_scoped_and_returns_project_and_milestone_counts() {
        let _serial = serial_http_test();
        let project_requests = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let server_requests = Arc::clone(&project_requests);
        let server = MockLinearServer::start(move |request| {
            if request["query"]
                .as_str()
                .is_some_and(|query| query.contains("projects("))
            {
                server_requests.lock().unwrap().push(request.clone());
            }
            project_sync_response(&request, 1).expect("unexpected GraphQL operation")
        });
        let client = test_client(&server.url);
        let (db, _dir) = test_db();

        let result = runtime()
            .block_on(client.sync_team_projects(&db, "CUT", "default"))
            .unwrap();

        assert_eq!(result.projects, 1);
        assert_eq!(result.milestones, 1);
        let projects = db.list_projects("default", true).unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].teams[0].key, "CUT");
        let requests = project_requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["variables"]["teamId"], "team-cut");
    }

    #[test]
    fn interrupted_team_project_sync_does_not_reconcile_missing_projects() {
        let _serial = serial_http_test();
        let fail_members = Arc::new(AtomicBool::new(false));
        let server_fail_members = Arc::clone(&fail_members);
        let server = MockLinearServer::start(move |request| {
            let query = request["query"].as_str().unwrap_or_default();
            if server_fail_members.load(Ordering::Relaxed) && query.contains("members(first:") {
                return MockResponse::status(500);
            }
            let count = if server_fail_members.load(Ordering::Relaxed) {
                1
            } else {
                2
            };
            project_sync_response(&request, count).expect("unexpected GraphQL operation")
        });
        let client = test_client(&server.url);
        let (db, _dir) = test_db();
        let rt = runtime();

        let initial = rt
            .block_on(client.sync_team_projects(&db, "CUT", "default"))
            .unwrap();
        assert_eq!(initial.projects, 2);
        fail_members.store(true, Ordering::Relaxed);
        assert!(rt
            .block_on(client.sync_team_projects(&db, "CUT", "default"))
            .is_err());
        let projects = db.list_projects("default", true).unwrap();
        assert_eq!(projects.len(), 2);
        assert!(projects.iter().any(|project| project.id == "project-2"));
    }

    #[test]
    fn workspace_project_sync_compatibility_api_still_traverses_all_projects() {
        let _serial = serial_http_test();
        let server = MockLinearServer::start(move |request| {
            project_sync_response(&request, 1).expect("unexpected GraphQL operation")
        });
        let client = test_client(&server.url);
        let (db, _dir) = test_db();

        let (projects, milestones) = runtime()
            .block_on(client.sync_projects(&db, "default"))
            .unwrap();
        assert_eq!((projects, milestones), (2, 2));
        let stored = db.list_projects("default", true).unwrap();
        assert_eq!(stored.len(), 2);
        assert!(stored.iter().any(|project| project.teams[0].key == "CUT"));
    }

    #[test]
    fn failed_index_page_leaves_checkpoint_unchanged() {
        let _serial = serial_http_test();
        let server = MockLinearServer::start(move |request| {
            if request["variables"]["after"].is_null() {
                MockResponse::json(serde_json::json!({
                    "data": { "issues": {
                        "nodes": [issue_node("issue-1", "CUT-1", "2026-02-01T00:00:00Z")],
                        "pageInfo": { "hasNextPage": true, "endCursor": "cursor-1" }
                    }}
                }))
            } else {
                MockResponse::status(500)
            }
        });
        let client = test_client(&server.url);
        let (db, _dir) = test_db();
        let mut stale = db::test_helpers::make_issue("CUT-99", "CUT");
        stale.id = "stale-issue".into();
        db.upsert_issue(&stale).unwrap();
        db.set_sync_cursor("default", "CUT", "2026-01-01T00:00:00Z")
            .unwrap();
        let upper = chrono::DateTime::parse_from_rfc3339("2026-03-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);

        assert!(runtime()
            .block_on(client.sync_team_index_window(&db, "CUT", "default", true, upper, None,))
            .is_err());
        assert_eq!(
            db.get_synced_through_at("default", "CUT")
                .unwrap()
                .as_deref(),
            Some("2026-01-01T00:00:00Z")
        );
        assert!(db.get_issue("CUT-1").unwrap().is_some());
        assert!(db.get_issue("CUT-99").unwrap().is_some());
    }

    #[test]
    fn empty_index_window_advances_checkpoint_and_uses_overlap() {
        let _serial = serial_http_test();
        let lower = Arc::new(Mutex::new(None::<String>));
        let query = Arc::new(Mutex::new(None::<String>));
        let server_lower = Arc::clone(&lower);
        let server_query = Arc::clone(&query);
        let server = MockLinearServer::start(move |request| {
            *server_lower.lock().unwrap() = request["variables"]["lower"]
                .as_str()
                .map(ToString::to_string);
            *server_query.lock().unwrap() = request["query"].as_str().map(ToString::to_string);
            MockResponse::json(serde_json::json!({
                "data": { "issues": {
                    "nodes": [],
                    "pageInfo": { "hasNextPage": false, "endCursor": null }
                }}
            }))
        });
        let client = test_client(&server.url);
        let (db, _dir) = test_db();
        db.set_sync_cursor("default", "CUT", "2026-02-01T00:00:00Z")
            .unwrap();
        let upper = chrono::DateTime::parse_from_rfc3339("2026-03-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);

        let result = runtime()
            .block_on(client.sync_team_index_window(&db, "CUT", "default", false, upper, None))
            .unwrap();
        assert_eq!(result.indexed, 0);
        assert_eq!(
            lower.lock().unwrap().as_deref(),
            Some("2026-01-31T23:55:00+00:00")
        );
        let query = query.lock().unwrap();
        let query = query.as_deref().unwrap();
        assert!(query.contains("$lower: DateTimeOrDuration"));
        assert!(query.contains("$upper: DateTimeOrDuration!"));
        assert_eq!(
            db.get_synced_through_at("default", "CUT")
                .unwrap()
                .as_deref(),
            Some("2026-03-01T00:00:00+00:00")
        );
    }

    #[test]
    fn subsequent_overlap_finds_an_issue_at_the_previous_upper_boundary() {
        let _serial = serial_http_test();
        let calls = Arc::new(AtomicUsize::new(0));
        let server_calls = Arc::clone(&calls);
        let server = MockLinearServer::start(move |_request| {
            let call = server_calls.fetch_add(1, Ordering::Relaxed);
            let nodes = if call == 0 {
                Vec::new()
            } else {
                vec![issue_node(
                    "issue-boundary",
                    "CUT-9",
                    "2026-01-01T00:00:00Z",
                )]
            };
            MockResponse::json(serde_json::json!({
                "data": { "issues": {
                    "nodes": nodes,
                    "pageInfo": { "hasNextPage": false, "endCursor": null }
                }}
            }))
        });
        let client = test_client(&server.url);
        let (db, _dir) = test_db();
        let first_upper = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let second_upper = chrono::DateTime::parse_from_rfc3339("2026-01-02T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let rt = runtime();

        let first = rt
            .block_on(client.sync_team_index_window(
                &db,
                "CUT",
                "default",
                false,
                first_upper,
                None,
            ))
            .unwrap();
        let second = rt
            .block_on(client.sync_team_index_window(
                &db,
                "CUT",
                "default",
                false,
                second_upper,
                None,
            ))
            .unwrap();

        assert_eq!(first.indexed, 0);
        assert_eq!(second.indexed, 1);
        let stored = db.get_issue("CUT-9").unwrap().unwrap();
        assert_eq!(stored.due_date.as_deref(), Some("2026-08-19"));
    }

    #[test]
    fn cancelled_hydration_requeues_the_inflight_resource() {
        let _serial = serial_http_test();
        let details_started = Arc::new(AtomicBool::new(false));
        let server_details_started = Arc::clone(&details_started);
        let server = MockLinearServer::start(move |request| {
            let query = request["query"].as_str().unwrap_or_default();
            if query.contains("comments(") {
                return successful_comments();
            }
            if query.contains("issue(id: $id)") && query.contains("description priority") {
                server_details_started.store(true, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(500));
            }
            standard_sync_response(&request).expect("unexpected GraphQL operation")
        });
        let client = test_client(&server.url);
        let (db, _dir) = test_db();
        let mut issue = db::test_helpers::make_issue("CUT-1", "CUT");
        issue.id = "issue-1".into();
        db.upsert_issue(&issue).unwrap();
        db.ensure_hydration_state_for_issue("default", &issue, "initial")
            .unwrap();

        runtime().block_on(async {
            let task_client = client.clone();
            let task_db = db.clone();
            let task = tokio::spawn(async move {
                task_client
                    .hydrate_pending_issues(
                        &task_db,
                        "CUT",
                        "default",
                        1,
                        db::HydrationPolicy::All,
                        None,
                    )
                    .await
            });

            tokio::time::timeout(Duration::from_secs(10), async {
                while !details_started.load(Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("detail hydration did not start");

            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        });

        let state = db.get_issue_hydration_state("default", "issue-1").unwrap();
        let details = state
            .resources
            .iter()
            .find(|resource| resource.resource == db::HydrationResource::Details)
            .unwrap();
        assert_eq!(details.status, db::HydrationStatus::Retryable);
        assert!(details.next_retry_at.is_none());
        assert_eq!(
            details.last_error.as_deref(),
            Some("hydration attempt interrupted before completion")
        );

        let retry = runtime()
            .block_on(client.hydrate_pending_issues(
                &db,
                "CUT",
                "default",
                1,
                db::HydrationPolicy::All,
                None,
            ))
            .unwrap();
        assert_eq!(retry.hydrated, 1);
        let retried = db.get_issue_hydration_state("default", "issue-1").unwrap();
        assert!(retried
            .resources
            .iter()
            .all(|resource| resource.status == db::HydrationStatus::Hydrated));
    }

    #[test]
    fn cancelled_index_sync_marks_the_owned_family_partial() {
        let _serial = serial_http_test();
        let index_started = Arc::new(AtomicBool::new(false));
        let server_index_started = Arc::clone(&index_started);
        let server = MockLinearServer::start(move |request| {
            let query = request["query"].as_str().unwrap_or_default();
            if query.contains("issues(") {
                server_index_started.store(true, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(500));
            }
            standard_sync_response(&request).expect("unexpected GraphQL operation")
        });
        let client = test_client(&server.url);
        let (db, _dir) = test_db();

        runtime().block_on(async {
            let task_client = client.clone();
            let task_db = db.clone();
            let task = tokio::spawn(async move {
                task_client
                    .sync_team_index(&task_db, "CUT", "default", false, None)
                    .await
            });

            tokio::time::timeout(Duration::from_secs(1), async {
                while !index_started.load(Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("issue index synchronization did not start");

            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        });

        let state = family_status(&db, "issue index");
        assert_eq!(state.status, "partial");
        assert_eq!(
            state.error.as_deref(),
            Some("sync interrupted before completion")
        );
        assert!(db.get_sync_cursor("default", "CUT").unwrap().is_none());
    }

    #[test]
    fn issue_hydration_families_complete_independently() {
        let _serial = serial_http_test();
        let server = MockLinearServer::start(move |request| {
            let query = request["query"].as_str().unwrap_or_default();
            if query.contains("labels(first:") {
                return MockResponse::json(serde_json::json!({
                    "errors": [{ "message": "Forbidden" }]
                }));
            }
            if query.contains("inverseRelations(first:") {
                return MockResponse::json(serde_json::json!({
                    "data": { "issue": { "inverseRelations": {
                        "nodes": [],
                        "pageInfo": { "hasNextPage": false, "endCursor": null }
                    }}}
                }));
            }
            if query.contains("relations(first:") {
                return MockResponse::json(serde_json::json!({
                    "data": { "issue": { "relations": {
                        "nodes": [],
                        "pageInfo": { "hasNextPage": false, "endCursor": null }
                    }}}
                }));
            }
            if query.contains("comments(") {
                return successful_comments();
            }
            standard_sync_response(&request).expect("unexpected GraphQL operation")
        });
        let client = test_client(&server.url);
        let (db, _dir) = test_db();
        let mut issue = db::test_helpers::make_issue("CUT-1", "CUT");
        issue.id = "issue-1".into();
        db.upsert_issue(&issue).unwrap();
        db.ensure_hydration_state_for_issue("default", &issue, "initial")
            .unwrap();
        db.mark_hydration_complete(
            "default",
            "issue-1",
            db::HydrationResource::Details,
            &issue.updated_at,
            "2026-01-03T00:00:00Z",
        )
        .unwrap();

        let batch = runtime()
            .block_on(client.hydrate_pending_issues(
                &db,
                "CUT",
                "default",
                1,
                db::HydrationPolicy::OpenOnly,
                None,
            ))
            .unwrap();
        let result = db.get_issue_hydration_state("default", "issue-1").unwrap();
        assert_eq!(
            result.status,
            db::HydrationStatus::Partial,
            "{:?}",
            result.resources
        );
        assert_eq!(batch.partial, 1);
        assert_eq!(batch.permanent_failures, 1);
        let status = |resource| {
            result
                .resources
                .iter()
                .find(|state| state.resource == resource)
                .unwrap()
                .status
        };
        assert_eq!(
            status(db::HydrationResource::Details),
            db::HydrationStatus::Hydrated
        );
        assert_eq!(
            status(db::HydrationResource::Labels),
            db::HydrationStatus::PermissionDenied
        );
        assert_eq!(
            status(db::HydrationResource::Relations),
            db::HydrationStatus::Hydrated
        );
        assert_eq!(
            status(db::HydrationResource::Comments),
            db::HydrationStatus::Hydrated
        );
    }

    #[test]
    fn http_400_ratelimited_is_persisted_and_stops_background_batch() {
        let _serial = serial_http_test();
        let calls = Arc::new(AtomicUsize::new(0));
        let server_calls = Arc::clone(&calls);
        let server = MockLinearServer::start(move |_request| {
            server_calls.fetch_add(1, Ordering::Relaxed);
            MockResponse {
                status: 400,
                body: serde_json::json!({
                    "errors": [{
                        "message": "request budget exhausted",
                        "extensions": { "code": "RATELIMITED" }
                    }]
                })
                .to_string(),
            }
        });
        let client = test_client(&server.url);
        let (db, _dir) = test_db();
        for number in 1..=2 {
            let mut issue = db::test_helpers::make_issue(&format!("CUT-{number}"), "CUT");
            issue.id = format!("issue-{number}");
            db.upsert_issue(&issue).unwrap();
            db.ensure_hydration_state_for_issue("default", &issue, "initial")
                .unwrap();
        }

        let result = runtime()
            .block_on(client.hydrate_pending_issues(
                &db,
                "CUT",
                "default",
                2,
                db::HydrationPolicy::OpenOnly,
                None,
            ))
            .unwrap();
        assert!(result.rate_limited);
        assert_eq!(result.deferred, 1);
        assert!((1..=3).contains(&calls.load(Ordering::Relaxed)));
        let state = db.get_issue_hydration_state("default", "issue-1").unwrap();
        assert_eq!(
            state
                .resources
                .iter()
                .find(|resource| resource.resource == db::HydrationResource::Details)
                .unwrap()
                .status,
            db::HydrationStatus::Retryable
        );
        assert!(state.resources[0].next_retry_at.is_some());
    }

    #[test]
    fn permanent_permission_failure_is_not_background_retried() {
        let _serial = serial_http_test();
        let calls = Arc::new(AtomicUsize::new(0));
        let server_calls = Arc::clone(&calls);
        let server = MockLinearServer::start(move |_request| {
            server_calls.fetch_add(1, Ordering::Relaxed);
            MockResponse::json(serde_json::json!({
                "errors": [{ "message": "Forbidden" }]
            }))
        });
        let client = test_client(&server.url);
        let (db, _dir) = test_db();
        let mut issue = db::test_helpers::make_issue("CUT-1", "CUT");
        issue.id = "issue-1".into();
        db.upsert_issue(&issue).unwrap();
        db.ensure_hydration_state_for_issue("default", &issue, "initial")
            .unwrap();
        for resource in [
            db::HydrationResource::Labels,
            db::HydrationResource::Relations,
            db::HydrationResource::Comments,
        ] {
            db.mark_hydration_complete(
                "default",
                "issue-1",
                resource,
                &issue.updated_at,
                "2026-01-03T00:00:00Z",
            )
            .unwrap();
        }

        let rt = runtime();
        rt.block_on(client.hydrate_pending_issues(
            &db,
            "CUT",
            "default",
            1,
            db::HydrationPolicy::OpenOnly,
            None,
        ))
        .unwrap();
        let first_calls = calls.load(Ordering::Relaxed);
        let second = rt
            .block_on(client.hydrate_pending_issues(
                &db,
                "CUT",
                "default",
                1,
                db::HydrationPolicy::OpenOnly,
                None,
            ))
            .unwrap();
        assert_eq!(
            db.get_issue_hydration_state("default", "issue-1")
                .unwrap()
                .resources
                .iter()
                .find(|resource| resource.resource == db::HydrationResource::Details)
                .unwrap()
                .attempt_count,
            1
        );
        assert_eq!(calls.load(Ordering::Relaxed), first_calls);
        assert_eq!(second.requested, 0);
    }

    #[test]
    fn if_needed_selected_hydration_skips_fresh_and_deferred_resources() {
        let _serial = serial_http_test();
        let calls = Arc::new(AtomicUsize::new(0));
        let server_calls = Arc::clone(&calls);
        let server = MockLinearServer::start(move |_request| {
            server_calls.fetch_add(1, Ordering::Relaxed);
            MockResponse::status(500)
        });
        let client = test_client(&server.url);
        let (db, _dir) = test_db();
        let mut issue = db::test_helpers::make_issue("CUT-1", "CUT");
        issue.id = "issue-1".into();
        db.upsert_issue(&issue).unwrap();
        db.ensure_hydration_state_for_issue("default", &issue, "initial")
            .unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        for resource in db::HYDRATION_RESOURCES {
            db.mark_hydration_complete("default", &issue.id, resource, &issue.updated_at, &now)
                .unwrap();
        }
        db.mark_hydration_failed(
            "default",
            &issue.id,
            db::HydrationResource::Relations,
            db::HydrationStatus::Retryable,
            Some("2999-01-01T00:00:00Z"),
            "later",
        )
        .unwrap();
        db.mark_hydration_failed(
            "default",
            &issue.id,
            db::HydrationResource::Labels,
            db::HydrationStatus::PermissionDenied,
            None,
            "forbidden",
        )
        .unwrap();

        let rt = runtime();
        for _ in 0..2 {
            rt.block_on(client.hydrate_issue_with_mode(
                &db,
                &issue.id,
                "default",
                db::HydrationMode::IfNeeded,
                None,
            ))
            .unwrap();
        }
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        let state = db.get_issue_hydration_state("default", &issue.id).unwrap();
        assert_eq!(
            state
                .resources
                .iter()
                .find(|resource| resource.resource == db::HydrationResource::Relations)
                .unwrap()
                .attempt_count,
            0
        );
    }

    #[test]
    fn if_needed_hydrates_pending_resources_without_refreshing_fresh_ones() {
        let _serial = serial_http_test();
        let requests = Arc::new(Mutex::new(Vec::<String>::new()));
        let server_requests = Arc::clone(&requests);
        let server = MockLinearServer::start(move |request| {
            let query = request["query"].as_str().unwrap_or_default().to_string();
            server_requests.lock().unwrap().push(query);
            standard_sync_response(&request).expect("unexpected GraphQL operation")
        });
        let client = test_client(&server.url);
        let (db, _dir) = test_db();
        let mut issue = db::test_helpers::make_issue("CUT-1", "CUT");
        issue.id = "issue-1".into();
        db.upsert_issue(&issue).unwrap();
        db.ensure_hydration_state_for_issue("default", &issue, "initial")
            .unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        for resource in [
            db::HydrationResource::Details,
            db::HydrationResource::Relations,
            db::HydrationResource::Comments,
        ] {
            db.mark_hydration_complete("default", &issue.id, resource, &issue.updated_at, &now)
                .unwrap();
        }

        runtime()
            .block_on(client.hydrate_issue_with_mode(
                &db,
                &issue.id,
                "default",
                db::HydrationMode::IfNeeded,
                None,
            ))
            .unwrap();
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].contains("labels(first:"));
    }

    #[test]
    fn force_refresh_requeues_all_resources_and_recovers_permanent_failures() {
        let _serial = serial_http_test();
        let requests = Arc::new(AtomicUsize::new(0));
        let server_requests = Arc::clone(&requests);
        let server = MockLinearServer::start(move |request| {
            server_requests.fetch_add(1, Ordering::Relaxed);
            if request["query"]
                .as_str()
                .is_some_and(|query| query.contains("comments("))
            {
                return successful_comments();
            }
            standard_sync_response(&request).expect("unexpected GraphQL operation")
        });
        let client = test_client(&server.url);
        let (db, _dir) = test_db();
        let mut issue = db::test_helpers::make_issue("CUT-1", "CUT");
        issue.id = "issue-1".into();
        db.upsert_issue(&issue).unwrap();
        db.ensure_hydration_state_for_issue("default", &issue, "initial")
            .unwrap();
        for resource in db::HYDRATION_RESOURCES {
            db.mark_hydration_failed(
                "default",
                &issue.id,
                resource,
                db::HydrationStatus::PermissionDenied,
                None,
                "forbidden",
            )
            .unwrap();
        }

        let result = runtime()
            .block_on(client.hydrate_issue_with_mode(
                &db,
                &issue.id,
                "default",
                db::HydrationMode::ForceRefresh,
                None,
            ))
            .unwrap();
        assert_eq!(result.status, db::HydrationStatus::Hydrated);
        assert_eq!(result.hydrated_resources, 4);
        assert!(requests.load(Ordering::Relaxed) >= 4);
    }

    #[test]
    fn error_diagnostics_keep_chains_bounded_and_redact_credentials() {
        let client = LinearClient::with_api_key("top-secret-api-key");
        let error: anyhow::Error = LinearOperationError::new(
            LinearErrorKind::Transient,
            "comments",
            None,
            format!(
                "upstream timeout Authorization: Bearer top-secret-api-key {}",
                "x".repeat(700)
            ),
        )
        .into();
        let error = error.context("comment synchronization failed for CUT-249");

        let diagnostic = client.redacted_error_message(&error);

        assert!(diagnostic.contains("comment synchronization failed for CUT-249"));
        assert!(diagnostic.contains("upstream timeout"));
        assert!(diagnostic.contains("[REDACTED]"));
        assert!(!diagnostic.contains("top-secret-api-key"));
        assert!(diagnostic.chars().count() <= 500);
    }

    #[test]
    fn sync_issue_relations_captures_inverse_blocks_as_blocked_by() {
        let _serial = serial_http_test();
        let server = MockLinearServer::start(move |request| {
            let query = request["query"].as_str().unwrap_or("");
            if query.contains("inverseRelations(first:") {
                return MockResponse::json(serde_json::json!({
                    "data": { "issue": { "inverseRelations": {
                        "nodes": [
                            { "id": "rel-9", "type": "blocks",
                              "issue": { "id": "eng-9-uuid", "identifier": "ENG-9" } },
                            { "id": "rel-10", "type": "related",
                              "issue": { "id": "eng-10-uuid", "identifier": "ENG-10" } }
                        ],
                        "pageInfo": { "hasNextPage": false, "endCursor": null }
                    }}}
                }));
            }
            if query.contains("inverseRelations(first:") {
                return MockResponse::json(serde_json::json!({
                    "data": { "issue": { "inverseRelations": {
                        "nodes": [],
                        "pageInfo": { "hasNextPage": false, "endCursor": null }
                    }}}
                }));
            }
            if query.contains("relations(first:") {
                return MockResponse::json(serde_json::json!({
                    "data": { "issue": { "relations": {
                        "nodes": [
                            { "id": "rel-1", "type": "related",
                              "relatedIssue": { "id": "cut-2-uuid", "identifier": "CUT-2" } }
                        ],
                        "pageInfo": { "hasNextPage": false, "endCursor": null }
                    }}}
                }));
            }
            panic!("unexpected GraphQL operation: {query}");
        });
        let client = test_client(&server.url);
        let (db, _dir) = crate::db::test_helpers::test_db();
        let mut issue = crate::db::test_helpers::make_issue("CUT-1", "CUT");
        issue.id = "cut-1-uuid".to_string();
        db.upsert_issue(&issue).unwrap();

        runtime()
            .block_on(client.sync_issue_relations(&db, "cut-1-uuid"))
            .unwrap();

        let mut relations = db.get_relations_enriched("cut-1-uuid").unwrap();
        relations.sort_by(|a, b| a.issue_identifier.cmp(&b.issue_identifier));
        // Forward "related" edge plus the inverse "blocks" edge as blocked_by.
        // The inverse "related" edge is skipped: symmetric types would only
        // duplicate what the forward connection already reports.
        assert_eq!(relations.len(), 2, "got {relations:?}");
        assert_eq!(relations[0].relation_type, "related");
        assert_eq!(relations[0].issue_identifier, "CUT-2");
        assert_eq!(relations[1].relation_type, "blocked_by");
        assert_eq!(relations[1].issue_identifier, "ENG-9");
        assert_eq!(relations[1].relation_id, "rel-9:inv");

        // Re-running must not strand stale rows: same data, same two relations.
        runtime()
            .block_on(client.sync_issue_relations(&db, "cut-1-uuid"))
            .unwrap();
        assert_eq!(db.get_relations_enriched("cut-1-uuid").unwrap().len(), 2);
    }
}
