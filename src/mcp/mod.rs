use anyhow::Result;
use rmcp::model::*;
use rmcp::schemars::JsonSchema;
use rmcp::{tool, ServerHandler};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::db::Database;
use crate::embedding::{self, Embedder};
use crate::linear::LinearClient;
use crate::search::{self, SearchMode};

#[derive(Clone)]
pub struct RectilinearMcp {
    db: Database,
    config: Config,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct SearchArgs {
    /// Search query text
    query: String,
    /// Filter by team key (e.g., "ENG")
    team: Option<String>,
    /// Filter by state name
    state: Option<String>,
    /// Search mode: "fts", "vector", or "hybrid"
    mode: Option<String>,
    /// Maximum number of results
    limit: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct FindDuplicatesArgs {
    /// Title of the potential new issue
    title: String,
    /// Description of the potential new issue
    description: Option<String>,
    /// Filter by team key
    team: Option<String>,
    /// Minimum similarity threshold (0.0-1.0)
    threshold: Option<f32>,
    /// Maximum number of results
    limit: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct GetIssueArgs {
    /// Issue ID or identifier (e.g., "ENG-123")
    id: String,
    /// Whether to include comments
    include_comments: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct CreateIssueArgs {
    /// Team key (e.g., "ENG")
    team: String,
    /// Issue title
    title: String,
    /// Issue description
    description: Option<String>,
    /// Priority: 1=Urgent, 2=High, 3=Medium, 4=Low
    priority: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct UpdateIssueArgs {
    /// Issue ID or identifier
    id: String,
    /// New title
    title: Option<String>,
    /// New description
    description: Option<String>,
    /// New priority
    priority: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct AppendArgs {
    /// Issue ID or identifier
    id: String,
    /// Comment text to add
    comment: Option<String>,
    /// Text to append to description
    description: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct SyncTeamArgs {
    /// Team key to sync
    team: String,
    /// Whether to do a full re-sync
    full: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct IssueContextArgs {
    /// Issue ID or identifier
    id: String,
    /// Number of similar issues to return
    similar_count: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct GetTriageQueueArgs {
    /// Team key (e.g., "CUT")
    team: String,
    /// Max issues to return (default 10)
    limit: Option<usize>,
    /// Issue identifiers to skip (already triaged this session)
    exclude: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct MarkTriagedArgs {
    /// Issue identifier (e.g., "CUT-42")
    id: String,
    /// New priority (1=Urgent, 2=High, 3=Medium, 4=Low)
    priority: i32,
    /// Improved title (optional)
    title: Option<String>,
    /// Improved description (optional)
    description: Option<String>,
    /// Triage comment explaining the decision (optional)
    comment: Option<String>,
}

#[tool(tool_box)]
impl RectilinearMcp {
    pub fn new(db: Database, config: Config) -> Self {
        Self { db, config }
    }

    #[tool(
        name = "search_issues",
        description = "Search Linear issues using hybrid FTS + vector search. Supports filtering by team and state."
    )]
    async fn search_issues(&self, #[tool(aggr)] args: SearchArgs) -> Result<String, String> {
        let mode: SearchMode = args
            .mode
            .as_deref()
            .unwrap_or("hybrid")
            .parse()
            .map_err(|e: anyhow::Error| e.to_string())?;

        let limit = args.limit.unwrap_or(self.config.search.default_limit);

        let embedder = if mode != SearchMode::Fts {
            Embedder::new(&self.config).ok()
        } else {
            None
        };

        let results = search::search(
            &self.db,
            &args.query,
            mode,
            args.team.as_deref(),
            args.state.as_deref(),
            limit,
            embedder.as_ref(),
            self.config.search.rrf_k,
        )
        .await
        .map_err(|e| e.to_string())?;

        serde_json::to_string_pretty(&results).map_err(|e| e.to_string())
    }

    #[tool(
        name = "find_duplicates",
        description = "Find potential duplicate issues. Provide a title and optional description to find similar existing issues with similarity scores."
    )]
    async fn find_duplicates(
        &self,
        #[tool(aggr)] args: FindDuplicatesArgs,
    ) -> Result<String, String> {
        let embedder = Embedder::new(&self.config).map_err(|e| e.to_string())?;

        let search_text = if let Some(ref desc) = args.description {
            format!("{}\n\n{}", args.title, desc)
        } else {
            args.title.clone()
        };

        let threshold = args
            .threshold
            .unwrap_or(self.config.search.duplicate_threshold);
        let limit = args.limit.unwrap_or(10);

        let results = search::find_duplicates(
            &self.db,
            &search_text,
            args.team.as_deref(),
            threshold,
            limit,
            &embedder,
            self.config.search.rrf_k,
        )
        .await
        .map_err(|e| e.to_string())?;

        serde_json::to_string_pretty(&results).map_err(|e| e.to_string())
    }

    #[tool(
        name = "get_issue",
        description = "Get full details of an issue by ID or identifier (e.g., 'ENG-123'). Includes description, state, priority, labels, and optionally comments."
    )]
    async fn get_issue(&self, #[tool(aggr)] args: GetIssueArgs) -> Result<String, String> {
        let issue = self
            .db
            .get_issue(&args.id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Issue '{}' not found", args.id))?;

        let mut value = serde_json::to_value(&issue).map_err(|e| e.to_string())?;

        if args.include_comments.unwrap_or(false) {
            let comments = self.db.get_comments(&issue.id).map_err(|e| e.to_string())?;
            value["comments"] = serde_json::to_value(&comments).map_err(|e| e.to_string())?;
        }

        serde_json::to_string_pretty(&value).map_err(|e| e.to_string())
    }

    #[tool(
        name = "create_issue",
        description = "Create a new issue in Linear. Specify team (key like 'ENG'), title, and optionally description, priority (1=Urgent, 2=High, 3=Medium, 4=Low)."
    )]
    async fn create_issue(&self, #[tool(aggr)] args: CreateIssueArgs) -> Result<String, String> {
        let client = LinearClient::new(&self.config).map_err(|e| e.to_string())?;

        let team_id = client
            .get_team_id(&args.team)
            .await
            .map_err(|e| e.to_string())?;

        let (issue_id, identifier) = client
            .create_issue(
                &team_id,
                &args.title,
                args.description.as_deref(),
                args.priority,
                &[],
            )
            .await
            .map_err(|e| e.to_string())?;

        let issue = client
            .fetch_single_issue(&issue_id)
            .await
            .map_err(|e| e.to_string())?;
        self.db.upsert_issue(&issue).map_err(|e| e.to_string())?;

        Ok(serde_json::json!({
            "id": issue_id,
            "identifier": identifier,
            "status": "created"
        })
        .to_string())
    }

    #[tool(
        name = "update_issue",
        description = "Update an existing Linear issue. Provide the issue ID/identifier and fields to update."
    )]
    async fn update_issue(&self, #[tool(aggr)] args: UpdateIssueArgs) -> Result<String, String> {
        let issue = self
            .db
            .get_issue(&args.id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Issue '{}' not found", args.id))?;

        let client = LinearClient::new(&self.config).map_err(|e| e.to_string())?;

        client
            .update_issue(
                &issue.id,
                args.title.as_deref(),
                args.description.as_deref(),
                args.priority,
                None,
            )
            .await
            .map_err(|e| e.to_string())?;

        let updated = client
            .fetch_single_issue(&issue.id)
            .await
            .map_err(|e| e.to_string())?;
        self.db.upsert_issue(&updated).map_err(|e| e.to_string())?;

        Ok(serde_json::json!({
            "identifier": issue.identifier,
            "status": "updated"
        })
        .to_string())
    }

    #[tool(
        name = "append_to_issue",
        description = "Add a comment to an issue or append text to its description."
    )]
    async fn append_to_issue(&self, #[tool(aggr)] args: AppendArgs) -> Result<String, String> {
        let issue = self
            .db
            .get_issue(&args.id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Issue '{}' not found", args.id))?;

        let client = LinearClient::new(&self.config).map_err(|e| e.to_string())?;
        let mut actions: Vec<&str> = Vec::new();

        if let Some(ref comment_text) = args.comment {
            client
                .add_comment(&issue.id, comment_text)
                .await
                .map_err(|e| e.to_string())?;
            actions.push("comment_added");
        }

        if let Some(ref desc_text) = args.description {
            let new_desc = match &issue.description {
                Some(existing) => format!("{}\n\n{}", existing, desc_text),
                None => desc_text.clone(),
            };
            client
                .update_issue(&issue.id, None, Some(&new_desc), None, None)
                .await
                .map_err(|e| e.to_string())?;
            actions.push("description_updated");
        }

        let updated = client
            .fetch_single_issue(&issue.id)
            .await
            .map_err(|e| e.to_string())?;
        self.db.upsert_issue(&updated).map_err(|e| e.to_string())?;

        Ok(serde_json::json!({
            "identifier": issue.identifier,
            "actions": actions
        })
        .to_string())
    }

    #[tool(
        name = "sync_team",
        description = "Sync issues from Linear for a specific team. Use full=true for a complete re-sync."
    )]
    async fn sync_team(&self, #[tool(aggr)] args: SyncTeamArgs) -> Result<String, String> {
        let client = LinearClient::new(&self.config).map_err(|e| e.to_string())?;
        let full = args.full.unwrap_or(false);

        let count = client
            .sync_team(&self.db, &args.team, full, false, None)
            .await
            .map_err(|e| e.to_string())?;

        let total = self
            .db
            .count_issues(Some(&args.team))
            .map_err(|e| e.to_string())?;

        Ok(serde_json::json!({
            "synced": count,
            "total": total,
            "team": args.team
        })
        .to_string())
    }

    #[tool(
        name = "issue_context",
        description = "Get an issue along with its N most similar issues, useful for understanding context and related work."
    )]
    async fn issue_context(&self, #[tool(aggr)] args: IssueContextArgs) -> Result<String, String> {
        let issue = self
            .db
            .get_issue(&args.id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Issue '{}' not found", args.id))?;

        let similar_count = args.similar_count.unwrap_or(5);

        let search_text = format!(
            "{}\n\n{}",
            issue.title,
            issue.description.as_deref().unwrap_or("")
        );

        let similar = if let Ok(embedder) = Embedder::new(&self.config) {
            search::find_duplicates(
                &self.db,
                &search_text,
                Some(&issue.team_key),
                0.3,
                similar_count + 1,
                &embedder,
                self.config.search.rrf_k,
            )
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.issue_id != issue.id)
            .take(similar_count)
            .collect::<Vec<_>>()
        } else {
            Vec::new()
        };

        let comments = self.db.get_comments(&issue.id).map_err(|e| e.to_string())?;

        let result = serde_json::json!({
            "issue": issue,
            "comments": comments,
            "similar_issues": similar,
        });

        serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
    }

    #[tool(
        name = "get_triage_queue",
        description = "Get a batch of unprioritized issues for triage. Returns enriched issues with similar issues for context. Use exclude to skip already-processed issues."
    )]
    async fn get_triage_queue(
        &self,
        #[tool(aggr)] args: GetTriageQueueArgs,
    ) -> Result<String, String> {
        // Incremental sync to pick up changes made by other users
        if let Ok(client) = LinearClient::new(&self.config) {
            let _ = client.sync_team(&self.db, &args.team, false, false, None).await;
        }

        let all_issues = self
            .db
            .get_unprioritized_issues(Some(&args.team))
            .map_err(|e| e.to_string())?;

        let exclude_set: std::collections::HashSet<&str> = args
            .exclude
            .as_ref()
            .map(|v| v.iter().map(|s| s.as_str()).collect())
            .unwrap_or_default();

        let filtered: Vec<_> = all_issues
            .into_iter()
            .filter(|i| !exclude_set.contains(i.identifier.as_str()))
            .collect();

        let limit = args.limit.unwrap_or(10);
        let batch: Vec<_> = filtered.iter().take(limit).collect();

        let total_remaining = self
            .db
            .count_unprioritized_issues(Some(&args.team))
            .map_err(|e| e.to_string())?
            .saturating_sub(exclude_set.len());

        let embedder = Embedder::new(&self.config).ok();

        let mut enriched = Vec::new();
        for issue in &batch {
            let description = issue
                .description
                .as_deref()
                .map(|d| {
                    if d.len() > 2000 {
                        let mut end = 2000;
                        while end > 0 && !d.is_char_boundary(end) {
                            end -= 1;
                        }
                        &d[..end]
                    } else {
                        d
                    }
                });

            let similar = if let Some(ref embedder) = embedder {
                let search_text = format!(
                    "{}\n\n{}",
                    issue.title,
                    description.unwrap_or("")
                );
                search::find_duplicates(
                    &self.db,
                    &search_text,
                    Some(&args.team),
                    0.3,
                    4,
                    embedder,
                    self.config.search.rrf_k,
                )
                .await
                .unwrap_or_default()
                .into_iter()
                .filter(|r| r.issue_id != issue.id)
                .take(3)
                .collect::<Vec<_>>()
            } else {
                Vec::new()
            };

            enriched.push(serde_json::json!({
                "identifier": issue.identifier,
                "title": issue.title,
                "description": description,
                "state_name": issue.state_name,
                "assignee_name": issue.assignee_name,
                "project_name": issue.project_name,
                "labels": issue.labels(),
                "created_at": issue.created_at,
                "similar_issues": similar,
            }));
        }

        let result = serde_json::json!({
            "queue": enriched,
            "total_remaining": total_remaining,
            "team": args.team,
        });

        serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
    }

    #[tool(
        name = "mark_triaged",
        description = "Mark an issue as triaged by setting priority and optionally updating title, description, and adding a triage comment. Combines update + comment into one call."
    )]
    async fn mark_triaged(
        &self,
        #[tool(aggr)] args: MarkTriagedArgs,
    ) -> Result<String, String> {
        if args.priority < 1 || args.priority > 4 {
            return Err("Priority must be 1 (Urgent), 2 (High), 3 (Medium), or 4 (Low)".into());
        }

        // Resolve from local DB to get the Linear UUID
        let local_issue = self
            .db
            .get_issue(&args.id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Issue '{}' not found", args.id))?;

        let client = LinearClient::new(&self.config).map_err(|e| e.to_string())?;

        // Re-fetch from Linear to get the latest version
        let issue = client
            .fetch_single_issue(&local_issue.id)
            .await
            .map_err(|e| e.to_string())?;
        self.db.upsert_issue(&issue).map_err(|e| e.to_string())?;

        // If someone else already prioritized it, let the caller know
        if issue.priority != 0 {
            return Ok(serde_json::json!({
                "identifier": issue.identifier,
                "status": "already_triaged",
                "current_priority": issue.priority,
                "current_priority_label": issue.priority_label(),
                "message": format!(
                    "{} was already prioritized as {} — skipping",
                    issue.identifier, issue.priority_label()
                ),
            })
            .to_string());
        }

        // Flag if the issue was modified since we last saw it
        let was_modified = issue.content_hash != local_issue.content_hash;
        if was_modified {
            let mut changes = Vec::new();
            if issue.title != local_issue.title {
                changes.push(format!("title changed: \"{}\" → \"{}\"", local_issue.title, issue.title));
            }
            if issue.description != local_issue.description {
                changes.push("description was updated".to_string());
            }
            if issue.state_name != local_issue.state_name {
                changes.push(format!("state changed: {} → {}", local_issue.state_name, issue.state_name));
            }
            if issue.assignee_name != local_issue.assignee_name {
                changes.push(format!(
                    "assignee changed: {} → {}",
                    local_issue.assignee_name.as_deref().unwrap_or("unassigned"),
                    issue.assignee_name.as_deref().unwrap_or("unassigned")
                ));
            }
            // Re-embed with the updated content
            self.reembed_issue(&issue).await;

            return Ok(serde_json::json!({
                "identifier": issue.identifier,
                "status": "modified_since_queued",
                "changes": changes,
                "current_title": issue.title,
                "current_description": issue.description,
                "current_state": issue.state_name,
                "message": format!(
                    "{} was modified since the queue was fetched — review the latest version before triaging",
                    issue.identifier
                ),
            })
            .to_string());
        }

        client
            .update_issue(
                &issue.id,
                args.title.as_deref(),
                args.description.as_deref(),
                Some(args.priority),
                None,
            )
            .await
            .map_err(|e| e.to_string())?;

        if let Some(ref comment_text) = args.comment {
            client
                .add_comment(&issue.id, comment_text)
                .await
                .map_err(|e| e.to_string())?;
        }

        let updated = client
            .fetch_single_issue(&issue.id)
            .await
            .map_err(|e| e.to_string())?;
        self.db.upsert_issue(&updated).map_err(|e| e.to_string())?;

        // Re-embed if title or description changed
        if args.title.is_some() || args.description.is_some() {
            self.reembed_issue(&updated).await;
        }

        let priority_label = match args.priority {
            1 => "Urgent",
            2 => "High",
            3 => "Medium",
            4 => "Low",
            _ => "Unknown",
        };

        Ok(serde_json::json!({
            "identifier": issue.identifier,
            "priority": args.priority,
            "priority_label": priority_label,
            "title": args.title.as_deref().unwrap_or(&issue.title),
            "status": "triaged",
        })
        .to_string())
    }
}

impl RectilinearMcp {
    /// Re-chunk and re-embed a single issue. Best-effort — failures are silently ignored.
    async fn reembed_issue(&self, issue: &crate::db::Issue) {
        let Ok(embedder) = Embedder::new(&self.config) else {
            return;
        };
        let chunks = embedding::chunk_text(
            &issue.title,
            issue.description.as_deref().unwrap_or(""),
            512,
            64,
        );
        if let Ok(embeddings) = embedder.embed_batch(&chunks).await {
            let chunk_data: Vec<(usize, String, Vec<u8>)> = chunks
                .into_iter()
                .zip(embeddings)
                .enumerate()
                .map(|(i, (text, emb))| (i, text, embedding::embedding_to_bytes(&emb)))
                .collect();
            let _ = self.db.upsert_chunks(&issue.id, &chunk_data);
        }
    }
}

#[tool(tool_box)]
impl ServerHandler for RectilinearMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities {
                tools: Some(ToolsCapability {
                    list_changed: None,
                }),
                ..Default::default()
            },
            server_info: Implementation {
                name: "rectilinear".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
            instructions: Some(
                "Rectilinear provides Linear issue intelligence with search, duplicate detection, and triage.\n\n\
                 ## Triage Workflow\n\
                 When the user asks to triage issues:\n\
                 1. Call get_triage_queue with the team key\n\
                 2. For each issue, present a brief summary and ask 2-4 clarifying questions focused on impact, frequency, severity, and business context. Suggest best-guess answers.\n\
                 3. Based on answers, propose: priority (1-4), improved title, improved description\n\
                 4. After user confirms, call mark_triaged\n\
                 5. Move to next issue. When batch exhausted, call get_triage_queue again with processed identifiers in exclude.\n\n\
                 ## Priority Framework\n\
                 1=Urgent (production down, data loss, security)\n\
                 2=High (major feature broken, significant user impact, no workaround)\n\
                 3=Medium (degraded experience, workarounds exist)\n\
                 4=Low (minor polish, nice-to-have)\n\n\
                 ## Duplicate Handling\n\
                 If similar_issues show >0.8 similarity, flag as potential duplicate. Ask user whether to merge, close as dup, or keep separate."
                    .into(),
            ),
        }
    }
}

pub async fn serve(db: Database, config: Config) -> Result<()> {
    eprintln!("MCP server ready (stdio transport)");
    let handler = RectilinearMcp::new(db, config);
    let transport = rmcp::transport::io::stdio();
    let server = rmcp::serve_server(handler, transport).await?;
    server.waiting().await?;
    Ok(())
}
