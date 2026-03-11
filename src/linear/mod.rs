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

    pub async fn fetch_issues(
        &self,
        team_key: &str,
        after_cursor: Option<&str>,
        updated_after: Option<&str>,
        include_archived: bool,
    ) -> Result<(Vec<db::Issue>, bool, Option<String>)> {
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
                        id identifier title description priority
                        createdAt updatedAt
                        state {{ name type }}
                        team {{ key }}
                        assignee {{ name }}
                        project {{ name }}
                        labels {{ nodes {{ name }} }}
                    }}
                    pageInfo {{ hasNextPage endCursor }}
                }}
            }}"#,
            filter, include_archive, after_param
        );

        let data: IssuesData = self.query(&query, serde_json::json!({})).await?;

        let issues: Vec<db::Issue> = data
            .issues
            .nodes
            .into_iter()
            .map(|i| {
                let labels: Vec<String> = i.labels.nodes.iter().map(|l| l.name.clone()).collect();
                let labels_json =
                    serde_json::to_string(&labels).unwrap_or_else(|_| "[]".to_string());

                let mut hasher = Sha256::new();
                hasher.update(&i.title);
                hasher.update(i.description.as_deref().unwrap_or(""));
                hasher.update(&labels_json);
                let content_hash = hex::encode(hasher.finalize());

                db::Issue {
                    id: i.id,
                    identifier: i.identifier,
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
                }
            })
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
        full: bool,
        include_archived: bool,
        progress: Option<&indicatif::ProgressBar>,
    ) -> Result<usize> {
        let updated_after = if full {
            None
        } else {
            db.get_sync_cursor(team_key)?
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
            for issue in &issues {
                if max_updated.is_none() || Some(&issue.updated_at) > max_updated.as_ref() {
                    max_updated = Some(issue.updated_at.clone());
                }
                db.upsert_issue(issue)?;
            }
            total += count;

            if let Some(pb) = progress {
                pb.set_message(format!("{} issues synced", total));
            }

            if !has_next || count == 0 {
                break;
            }
            cursor = next_cursor;
        }

        if let Some(max) = max_updated {
            db.set_sync_cursor(team_key, &max)?;
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
        _state_name: Option<&str>,
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

    pub async fn fetch_single_issue(&self, issue_id: &str) -> Result<db::Issue> {
        let query = r#"
            query($id: String!) {
                issue(id: $id) {
                    id identifier title description priority
                    createdAt updatedAt
                    state { name type }
                    team { key }
                    assignee { name }
                    project { name }
                    labels { nodes { name } }
                }
            }
        "#;

        let data: SingleIssueData = self
            .query(query, serde_json::json!({ "id": issue_id }))
            .await?;

        let i = data.issue;
        let labels: Vec<String> = i.labels.nodes.iter().map(|l| l.name.clone()).collect();
        let labels_json = serde_json::to_string(&labels).unwrap_or_else(|_| "[]".to_string());

        let mut hasher = Sha256::new();
        hasher.update(&i.title);
        hasher.update(i.description.as_deref().unwrap_or(""));
        hasher.update(&labels_json);
        let content_hash = hex::encode(hasher.finalize());

        Ok(db::Issue {
            id: i.id,
            identifier: i.identifier,
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
        })
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
}
