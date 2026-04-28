use anyhow::{Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::db::{self, Database};

const LINEAR_API_URL: &str = "https://api.linear.app/graphql";

#[derive(Clone)]
pub struct LinearClient {
    client: reqwest::Client,
    api_key: String,
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
struct PageInfo {
    #[serde(rename = "hasNextPage")]
    has_next_page: bool,
    #[serde(rename = "endCursor")]
    end_cursor: Option<String>,
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
struct LinearProject {
    name: String,
}

#[derive(Debug, Deserialize)]
struct LinearLabelConnection {
    nodes: Vec<LinearLabel>,
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
        Ok(Self { client, api_key })
    }

    /// Create a client with an explicit API key (for FFI callers).
    pub fn with_api_key(api_key: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.to_string(),
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
        }
    }

    async fn query<T: serde::de::DeserializeOwned>(
        &self,
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
            .context("Failed to send request to Linear API")?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Linear API returned {}: {}", status, text);
        }

        let response: GraphQLResponse<T> = resp
            .json()
            .await
            .context("Failed to parse Linear response")?;

        if let Some(errors) = response.errors {
            let msgs: Vec<_> = errors.iter().map(|e| e.message.as_str()).collect();
            anyhow::bail!("Linear API errors: {}", msgs.join(", "));
        }

        response.data.context("No data in Linear response")
    }

    pub async fn list_teams(&self) -> Result<Vec<TeamNode>> {
        let data: TeamsData = self
            .query(
                "query { teams { nodes { id key name } } }",
                serde_json::json!({}),
            )
            .await?;
        Ok(data.teams.nodes)
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

    pub async fn fetch_issues(
        &self,
        team_key: &str,
        after_cursor: Option<&str>,
        updated_after: Option<&str>,
        include_archived: bool,
    ) -> Result<(Vec<(db::Issue, Vec<db::Relation>, Vec<String>)>, bool, Option<String>)> {
        let mut filter_parts = vec![format!("team: {{ key: {{ eq: \"{}\" }} }}", team_key)];
        if let Some(after) = updated_after {
            filter_parts.push(format!("updatedAt: {{ gt: \"{}\" }}", after));
        }
        let filter = filter_parts.join(", ");

        let after_param = if let Some(c) = after_cursor {
            format!(", after: \"{}\"", c)
        } else {
            String::new()
        };

        let include_archive = if include_archived { "true" } else { "false" };

        let query = format!(
            r#"query {{
                issues(
                    first: 250,
                    filter: {{ {} }},
                    includeArchived: {}
                    orderBy: updatedAt
                    {}
                ) {{
                    nodes {{
                        id identifier url title description priority branchName
                        createdAt updatedAt
                        state {{ name type }}
                        team {{ key }}
                        assignee {{ name }}
                        project {{ name }}
                        labels {{ nodes {{ id name }} }}
                        relations {{ nodes {{ id type relatedIssue {{ id identifier }} }} }}
                    }}
                    pageInfo {{ hasNextPage endCursor }}
                }}
            }}"#,
            filter, include_archive, after_param
        );

        let data: IssuesData = self.query(&query, serde_json::json!({})).await?;

        let issues: Vec<(db::Issue, Vec<db::Relation>, Vec<String>)> = data
            .issues
            .nodes
            .into_iter()
            .map(Self::convert_linear_issue)
            .collect();

        Ok((
            issues,
            data.issues.page_info.has_next_page,
            data.issues.page_info.end_cursor,
        ))
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
        // Refresh workspace label catalog before syncing issues so issue_labels
        // can be populated. Linear labels are workspace-scoped, so this runs
        // per-call (cheap: one paginated query).
        if let Err(e) = self.sync_labels_catalog(db, workspace_id).await {
            eprintln!("warning: failed to sync label catalog for workspace '{}': {}", workspace_id, e);
        }

        let updated_after = if full {
            None
        } else {
            db.get_sync_cursor(workspace_id, team_key)?
        };

        let mut total = 0;
        let mut cursor: Option<String> = None;
        let mut max_updated: Option<String> = None;

        loop {
            let (issues, has_next, next_cursor) = self
                .fetch_issues(
                    team_key,
                    cursor.as_deref(),
                    updated_after.as_deref(),
                    include_archived,
                )
                .await?;

            let count = issues.len();
            for (mut issue, relations, label_ids) in issues {
                issue.workspace_id = workspace_id.to_string();
                if max_updated.is_none() || Some(&issue.updated_at) > max_updated.as_ref() {
                    max_updated = Some(issue.updated_at.clone());
                }
                db.upsert_issue(&issue)?;
                db.upsert_relations(&issue.id, &relations)?;
                db.replace_issue_labels(&issue.id, &label_ids)?;
            }
            total += count;

            if let Some(cb) = progress {
                cb(total);
            }

            if !has_next || count == 0 {
                break;
            }
            cursor = next_cursor;
        }

        if let Some(max) = max_updated {
            db.set_sync_cursor(workspace_id, team_key, &max)?;
        }

        Ok(total)
    }

    pub async fn create_issue(
        &self,
        team_id: &str,
        title: &str,
        description: Option<&str>,
        priority: Option<i32>,
        label_ids: &[String],
        parent_id: Option<&str>,
    ) -> Result<(String, String)> {
        let mut input = serde_json::json!({
            "teamId": team_id,
            "title": title,
        });

        if let Some(desc) = description {
            input["description"] = serde_json::Value::String(desc.to_string());
        }
        if let Some(p) = priority {
            input["priority"] = serde_json::Value::Number(p.into());
        }
        if !label_ids.is_empty() {
            input["labelIds"] = serde_json::json!(label_ids);
        }
        if let Some(pid) = parent_id {
            input["parentId"] = serde_json::Value::String(pid.to_string());
        }

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

    pub async fn update_issue(
        &self,
        issue_id: &str,
        title: Option<&str>,
        description: Option<&str>,
        priority: Option<i32>,
        state_id: Option<&str>,
        label_ids: Option<&[String]>,
        project_id: Option<&str>,
    ) -> Result<()> {
        let mut input = serde_json::Map::new();
        if let Some(t) = title {
            input.insert("title".into(), serde_json::Value::String(t.to_string()));
        }
        if let Some(d) = description {
            input.insert(
                "description".into(),
                serde_json::Value::String(d.to_string()),
            );
        }
        if let Some(p) = priority {
            input.insert("priority".into(), serde_json::Value::Number(p.into()));
        }
        if let Some(sid) = state_id {
            input.insert("stateId".into(), serde_json::Value::String(sid.to_string()));
        }
        if let Some(lids) = label_ids {
            input.insert("labelIds".into(), serde_json::json!(lids));
        }
        if let Some(pid) = project_id {
            input.insert(
                "projectId".into(),
                serde_json::Value::String(pid.to_string()),
            );
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
                    project { name }
                    labels { nodes { id name } }
                    relations { nodes { id type relatedIssue { id identifier } } }
                }
            }
        "#;

        let data: SingleIssueData = self
            .query(query, serde_json::json!({ "id": issue_id }))
            .await?;

        Ok(Self::convert_linear_issue(data.issue))
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
                    first: 1
                ) {{
                    nodes {{
                        id identifier url title description priority branchName
                        createdAt updatedAt
                        state {{ name type }}
                        team {{ key }}
                        assignee {{ name }}
                        project {{ name }}
                        labels {{ nodes {{ id name }} }}
                        relations {{ nodes {{ id type relatedIssue {{ id identifier }} }} }}
                    }}
                    pageInfo {{ hasNextPage endCursor }}
                }}
            }}"#,
            team_key, number
        );

        let data: IssuesData = self.query(&query, serde_json::json!({})).await?;

        Ok(data
            .issues
            .nodes
            .into_iter()
            .next()
            .map(Self::convert_linear_issue))
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
            project_name: i.project.map(|p| p.name),
            labels_json,
            created_at: i.created_at,
            updated_at: i.updated_at,
            content_hash,
            synced_at: None,
            branch_name: i.branch_name,
            workspace_id: "default".to_string(),
        };

        (issue, relations, label_ids)
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

        let query = r#"
            query {
                issueLabels(first: 250) {
                    nodes { id name }
                }
            }
        "#;

        let data: serde_json::Value = self.query(query, serde_json::json!({})).await?;

        let labels = data["issueLabels"]["nodes"]
            .as_array()
            .context("No labels in response")?;

        let mut ids = Vec::new();
        for name in label_names {
            let found = labels.iter().find(|l| {
                l["name"]
                    .as_str()
                    .is_some_and(|n| n.eq_ignore_ascii_case(name))
            });
            match found {
                Some(l) => {
                    ids.push(l["id"].as_str().context("Label has no id")?.to_string());
                }
                None => {
                    let available: Vec<&str> =
                        labels.iter().filter_map(|l| l["name"].as_str()).collect();
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

    /// Fetch the full label catalog for the workspace (all pages).
    pub async fn fetch_labels(&self) -> Result<Vec<LabelCatalogEntry>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let after_param = match cursor {
                Some(ref c) => format!(", after: \"{}\"", c),
                None => String::new(),
            };
            let query = format!(
                r#"query {{
                    issueLabels(first: 250{}) {{
                        nodes {{ id name color parent {{ id }} }}
                        pageInfo {{ hasNextPage endCursor }}
                    }}
                }}"#,
                after_param
            );
            let data: serde_json::Value = self.query(&query, serde_json::json!({})).await?;
            let nodes = data["issueLabels"]["nodes"]
                .as_array()
                .context("No issueLabels.nodes in response")?;
            for n in nodes {
                let id = n["id"].as_str().context("label has no id")?.to_string();
                let name = n["name"].as_str().unwrap_or("").to_string();
                let color = n["color"].as_str().map(|s| s.to_string());
                let parent_id = n["parent"]["id"].as_str().map(|s| s.to_string());
                out.push(LabelCatalogEntry { id, name, color, parent_id });
            }
            let has_next = data["issueLabels"]["pageInfo"]["hasNextPage"].as_bool().unwrap_or(false);
            if !has_next { break; }
            cursor = data["issueLabels"]["pageInfo"]["endCursor"].as_str().map(|s| s.to_string());
            if cursor.is_none() { break; }
        }
        Ok(out)
    }

    /// Sync the workspace's label catalog into the local database.
    /// Upserts labels by id and removes labels that no longer exist remotely.
    pub async fn sync_labels_catalog(&self, db: &Database, workspace_id: &str) -> Result<usize> {
        let entries = self.fetch_labels().await?;
        let keep_ids: Vec<String> = entries.iter().map(|e| e.id.clone()).collect();
        for e in &entries {
            db.upsert_label(&db::Label {
                id: e.id.clone(),
                workspace_id: workspace_id.to_string(),
                name: e.name.clone(),
                color: e.color.clone(),
                parent_id: e.parent_id.clone(),
            })?;
        }
        db.delete_labels_for_workspace_not_in(workspace_id, &keep_ids)?;
        Ok(entries.len())
    }

    /// Resolve a project name to its ID. Matches case-insensitively.
    pub async fn get_project_id(&self, project_name: &str) -> Result<String> {
        let query = r#"
            query {
                projects(first: 250) {
                    nodes { id name }
                }
            }
        "#;

        let data: serde_json::Value = self.query(query, serde_json::json!({})).await?;

        let projects = data["projects"]["nodes"]
            .as_array()
            .context("No projects in response")?;

        for project in projects {
            if let Some(name) = project["name"].as_str() {
                if name.eq_ignore_ascii_case(project_name) {
                    return project["id"]
                        .as_str()
                        .map(|s| s.to_string())
                        .context("Project has no id");
                }
            }
        }

        let available: Vec<&str> = projects.iter().filter_map(|p| p["name"].as_str()).collect();
        anyhow::bail!(
            "Project '{}' not found. Available: {}",
            project_name,
            available.join(", ")
        )
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
