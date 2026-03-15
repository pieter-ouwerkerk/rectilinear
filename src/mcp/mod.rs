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

/// Scan a JSON value for issue identifiers (e.g. "CUT-42") and add a `referenced_issues`
/// field with their URLs so agents can render them as clickable links.
fn enrich_with_issue_links(value: &mut serde_json::Value, db: &Database) {
    let text = value.to_string();
    let identifiers = extract_issue_identifiers(&text);
    if identifiers.is_empty() {
        return;
    }

    let mut refs = serde_json::Map::new();
    for ident in &identifiers {
        if let Ok(Some(issue)) = db.get_issue(ident) {
            if !issue.url.is_empty() {
                refs.insert(
                    ident.clone(),
                    serde_json::json!({
                        "url": issue.url,
                        "title": issue.title,
                        "state": issue.state_name,
                    }),
                );
            }
        }
    }

    if !refs.is_empty() {
        if let serde_json::Value::Object(map) = value {
            map.insert(
                "referenced_issues".to_string(),
                serde_json::Value::Object(refs),
            );
        }
    }
}

/// Extract issue identifiers (e.g. "CUT-42", "ENG-123") from text.
/// Matches patterns like 1-4 uppercase letters followed by a dash and digits.
fn extract_issue_identifiers(text: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        // Look for uppercase letter start
        if bytes[i].is_ascii_uppercase() {
            let start = i;
            // Consume 1-6 uppercase letters
            while i < len && bytes[i].is_ascii_uppercase() {
                i += 1;
            }
            let key_len = i - start;
            if (1..=6).contains(&key_len) && i < len && bytes[i] == b'-' {
                i += 1; // skip dash
                let digit_start = i;
                while i < len && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                if i > digit_start {
                    // Make sure it's not part of a larger word
                    let before_ok = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
                    let after_ok = i >= len || !bytes[i].is_ascii_alphanumeric();
                    if before_ok && after_ok {
                        let ident = text[start..i].to_string();
                        if seen.insert(ident.clone()) {
                            result.push(ident);
                        }
                    }
                }
            }
        } else {
            i += 1;
        }
    }

    result
}

/// Extract markdown image references from text (e.g., `![alt](url)` or `![](url)`).
fn extract_image_references(text: &str) -> Vec<&str> {
    let mut images = Vec::new();
    let mut remaining = text;
    while let Some(start) = remaining.find("![") {
        if let Some(alt_end) = remaining[start + 2..].find("](") {
            let paren_start = start + 2 + alt_end + 2;
            if let Some(paren_end) = remaining[paren_start..].find(')') {
                let full_end = paren_start + paren_end + 1;
                images.push(&remaining[start..full_end]);
                remaining = &remaining[full_end..];
                continue;
            }
        }
        remaining = &remaining[start + 2..];
    }
    images
}

/// If `new_description` would drop image references present in `original`, append them.
fn preserve_images(original: &str, new_description: &str) -> String {
    let original_images = extract_image_references(original);
    if original_images.is_empty() {
        return new_description.to_string();
    }

    let mut missing: Vec<&str> = Vec::new();
    for img in &original_images {
        if !new_description.contains(img) {
            missing.push(img);
        }
    }

    if missing.is_empty() {
        return new_description.to_string();
    }

    let mut result = new_description.to_string();
    result.push_str("\n\n");
    result.push_str(&missing.join("\n"));
    result
}

/// Extract code-relevant search hints from issue title, description, and labels.
/// Returns terms that Claude should search for in the codebase.
fn extract_code_hints(title: &str, description: &str, labels: &[String]) -> Vec<String> {
    let mut hints = Vec::new();
    let combined = format!("{} {}", title, description);

    // Extract file paths (e.g. src/foo.rs, Components/Bar.swift)
    for word in combined.split_whitespace() {
        let word = word.trim_matches(|c: char| {
            !c.is_alphanumeric() && c != '/' && c != '.' && c != '_' && c != '-'
        });
        if (word.contains('/') && word.contains('.'))
            || word.ends_with(".rs")
            || word.ends_with(".ts")
            || word.ends_with(".swift")
        {
            hints.push(word.to_string());
        }
    }

    // Extract backtick-quoted identifiers (e.g. `WorktreeManager`, `cleanup()`)
    for cap in combined.split('`').collect::<Vec<_>>().chunks(2) {
        if cap.len() == 2 && !cap[1].is_empty() && cap[1].len() < 80 {
            hints.push(cap[1].trim().to_string());
        }
    }

    // Extract PascalCase and snake_case identifiers from title
    for word in title.split_whitespace() {
        let word = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
        // PascalCase: at least 2 uppercase letters with lowercase between
        let upper_count = word.chars().filter(|c| c.is_uppercase()).count();
        if upper_count >= 2
            && word.len() >= 4
            && word.chars().next().is_some_and(|c| c.is_uppercase())
        {
            hints.push(word.to_string());
        }
        // snake_case
        if word.contains('_')
            && word.chars().all(|c| c.is_alphanumeric() || c == '_')
            && word.len() >= 4
        {
            hints.push(word.to_string());
        }
    }

    // Add labels as search terms
    for label in labels {
        if !label.is_empty() {
            hints.push(label.clone());
        }
    }

    // Deduplicate
    hints.sort();
    hints.dedup();
    hints
}

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
    /// Parent issue identifier to create as sub-issue (e.g., "CUT-42")
    parent: Option<String>,
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
    /// Set issue state by name (e.g., "Done", "Cancelled", "In Progress")
    state: Option<String>,
    /// Set labels by name (replaces all existing labels)
    labels: Option<Vec<String>>,
    /// Set project by name (or "none" to remove from project)
    project: Option<String>,
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
    /// Randomize issue order instead of chronological (default false)
    shuffle: Option<bool>,
    /// Include completed/canceled issues (default false). Useful for archival prioritization.
    include_completed: Option<bool>,
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
    /// Set issue state (e.g., "Done", "Cancelled", "Duplicate", "Backlog"). Looked up by name for the issue's team.
    state: Option<String>,
    /// Set labels by name (replaces all existing labels)
    labels: Option<Vec<String>>,
    /// Set project by name (or "none" to remove from project)
    project: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct ManageRelationArgs {
    /// Action: "add" or "remove"
    action: String,
    /// Source issue identifier (e.g., "CUT-42")
    issue: String,
    /// Related issue identifier (e.g., "CUT-99")
    related_issue: String,
    /// Relation type: "blocks", "blocked_by", "related", "duplicate"
    relation_type: String,
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
        description = "Get full details of an issue by ID or identifier (e.g., 'ENG-123'). Includes description, state, priority, labels, and optionally comments. Falls back to fetching from Linear API if not found locally."
    )]
    async fn get_issue(&self, #[tool(aggr)] args: GetIssueArgs) -> Result<String, String> {
        let issue = match self.db.get_issue(&args.id).map_err(|e| e.to_string())? {
            Some(issue) => issue,
            None => {
                // Not found locally — try fetching from Linear API by identifier
                let client = LinearClient::new(&self.config).map_err(|e| e.to_string())?;
                let result = client
                    .fetch_issue_by_identifier(&args.id)
                    .await
                    .map_err(|e| e.to_string())?;
                match result {
                    Some((issue, relations)) => {
                        self.db.upsert_issue(&issue).map_err(|e| e.to_string())?;
                        self.db
                            .upsert_relations(&issue.id, &relations)
                            .map_err(|e| e.to_string())?;
                        issue
                    }
                    None => return Err(format!("Issue '{}' not found", args.id)),
                }
            }
        };

        let mut value = serde_json::to_value(&issue).map_err(|e| e.to_string())?;

        let relations = self
            .db
            .get_relations_enriched(&issue.id)
            .map_err(|e| e.to_string())?;
        if !relations.is_empty() {
            value["relations"] = serde_json::to_value(&relations).map_err(|e| e.to_string())?;
        }

        if args.include_comments.unwrap_or(false) {
            let comments = self.db.get_comments(&issue.id).map_err(|e| e.to_string())?;
            value["comments"] = serde_json::to_value(&comments).map_err(|e| e.to_string())?;
        }

        enrich_with_issue_links(&mut value, &self.db);
        serde_json::to_string_pretty(&value).map_err(|e| e.to_string())
    }

    #[tool(
        name = "create_issue",
        description = "Create a new issue in Linear. Specify team (key like 'ENG'), title, and optionally description, priority (1=Urgent, 2=High, 3=Medium, 4=Low).

IMPORTANT — Before calling this tool, you MUST:

1. **Disambiguate the request.** Ask the user 2-4 clarifying questions to sharpen scope, acceptance criteria, and edge cases. Think like a principal engineer: what assumptions are you making? What could go wrong? What's in vs. out of scope? Do not create the issue until the user has answered.

2. **Check for duplicates.** Call find_duplicates with the intended title/description to verify this issue doesn't already exist. If a match is found (>0.8 similarity), show it to the user and ask whether to proceed, update the existing issue, or cancel.

3. **Write a clear title and description.** The title should be imperative and specific (e.g. 'Add rate limiting to /api/upload endpoint' not 'rate limiting'). The description should include: what the desired behavior is, why it matters, and any constraints or acceptance criteria surfaced during disambiguation."
    )]
    async fn create_issue(&self, #[tool(aggr)] args: CreateIssueArgs) -> Result<String, String> {
        let client = LinearClient::new(&self.config).map_err(|e| e.to_string())?;

        let team_id = client
            .get_team_id(&args.team)
            .await
            .map_err(|e| e.to_string())?;

        let parent_id = if let Some(ref parent_ident) = args.parent {
            let parent = self
                .db
                .get_issue(parent_ident)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("Parent issue '{}' not found", parent_ident))?;
            Some(parent.id)
        } else {
            None
        };

        let (issue_id, identifier) = client
            .create_issue(
                &team_id,
                &args.title,
                args.description.as_deref(),
                args.priority,
                &[],
                parent_id.as_deref(),
            )
            .await
            .map_err(|e| e.to_string())?;

        let (issue, relations) = client
            .fetch_single_issue(&issue_id)
            .await
            .map_err(|e| e.to_string())?;
        self.db.upsert_issue(&issue).map_err(|e| e.to_string())?;
        self.db
            .upsert_relations(&issue.id, &relations)
            .map_err(|e| e.to_string())?;

        Ok(serde_json::json!({
            "id": issue_id,
            "identifier": identifier,
            "url": issue.url,
            "status": "created"
        })
        .to_string())
    }

    #[tool(
        name = "update_issue",
        description = "Update an existing Linear issue. Provide the issue ID/identifier and fields to update. Prefer append_to_issue for adding context. Image references in the original description are automatically preserved when updating."
    )]
    async fn update_issue(&self, #[tool(aggr)] args: UpdateIssueArgs) -> Result<String, String> {
        let issue = self
            .db
            .get_issue(&args.id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Issue '{}' not found", args.id))?;

        let client = LinearClient::new(&self.config).map_err(|e| e.to_string())?;

        let state_id = if let Some(ref state_name) = args.state {
            Some(
                client
                    .get_state_id(&issue.team_key, state_name)
                    .await
                    .map_err(|e| e.to_string())?,
            )
        } else {
            None
        };

        let label_ids = if let Some(ref label_names) = args.labels {
            Some(
                client
                    .get_label_ids(label_names)
                    .await
                    .map_err(|e| e.to_string())?,
            )
        } else {
            None
        };

        let project_id = if let Some(ref project_name) = args.project {
            if project_name.eq_ignore_ascii_case("none") {
                Some(String::new()) // Empty string removes project in Linear
            } else {
                Some(
                    client
                        .get_project_id(project_name)
                        .await
                        .map_err(|e| e.to_string())?,
                )
            }
        } else {
            None
        };

        // If updating description, re-fetch from Linear to preserve any image references
        let safe_description = if args.description.is_some() {
            let (latest, _) = client
                .fetch_single_issue(&issue.id)
                .await
                .map_err(|e| e.to_string())?;
            args.description
                .as_ref()
                .map(|new_desc| match &latest.description {
                    Some(original) => preserve_images(original, new_desc),
                    None => new_desc.clone(),
                })
        } else {
            None
        };

        client
            .update_issue(
                &issue.id,
                args.title.as_deref(),
                safe_description.as_deref(),
                args.priority,
                state_id.as_deref(),
                label_ids.as_deref(),
                project_id.as_deref(),
            )
            .await
            .map_err(|e| e.to_string())?;

        let (updated, relations) = client
            .fetch_single_issue(&issue.id)
            .await
            .map_err(|e| e.to_string())?;
        self.db.upsert_issue(&updated).map_err(|e| e.to_string())?;
        self.db
            .upsert_relations(&updated.id, &relations)
            .map_err(|e| e.to_string())?;

        Ok(serde_json::json!({
            "identifier": issue.identifier,
            "url": issue.url,
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
                .update_issue(&issue.id, None, Some(&new_desc), None, None, None, None)
                .await
                .map_err(|e| e.to_string())?;
            actions.push("description_updated");
        }

        let (updated, relations) = client
            .fetch_single_issue(&issue.id)
            .await
            .map_err(|e| e.to_string())?;
        self.db.upsert_issue(&updated).map_err(|e| e.to_string())?;
        self.db
            .upsert_relations(&updated.id, &relations)
            .map_err(|e| e.to_string())?;

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
        let relations = self
            .db
            .get_relations_enriched(&issue.id)
            .map_err(|e| e.to_string())?;

        let mut result = serde_json::json!({
            "issue": issue,
            "comments": comments,
            "similar_issues": similar,
            "relations": relations,
        });

        enrich_with_issue_links(&mut result, &self.db);
        serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
    }

    #[tool(
        name = "get_triage_queue",
        description = "Get a batch of unprioritized issues for triage. Returns enriched issues with similar issues and code_search_hints. IMPORTANT: For each issue, BEFORE presenting it to the user, you MUST search the codebase using the code_search_hints (via Grep, Glob, Read, or Cuttlefish MCP tools like get_symbols/find_references). Spend 2-4 tool calls per issue exploring relevant code, then include your findings when asking the user questions."
    )]
    async fn get_triage_queue(
        &self,
        #[tool(aggr)] args: GetTriageQueueArgs,
    ) -> Result<String, String> {
        // Incremental sync to pick up changes made by other users
        if let Ok(client) = LinearClient::new(&self.config) {
            let _ = client
                .sync_team(&self.db, &args.team, false, false, None)
                .await;
        }

        let all_issues = self
            .db
            .get_unprioritized_issues(Some(&args.team), args.include_completed.unwrap_or(false))
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
        let batch: Vec<_> = if args.shuffle.unwrap_or(false) {
            use rand::seq::SliceRandom;
            let mut indices: Vec<usize> = (0..filtered.len()).collect();
            indices.shuffle(&mut rand::rng());
            indices
                .into_iter()
                .take(limit)
                .map(|i| &filtered[i])
                .collect()
        } else {
            filtered.iter().take(limit).collect()
        };

        let total_remaining = filtered.len();

        let embedder = Embedder::new(&self.config).ok();

        let mut enriched = Vec::new();
        for issue in &batch {
            let description = issue.description.as_deref().map(|d| {
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
                let search_text = format!("{}\n\n{}", issue.title, description.unwrap_or(""));
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

            let relations = self
                .db
                .get_relations_enriched(&issue.id)
                .unwrap_or_default();

            // Extract search hints from title and description for code exploration
            let code_search_hints =
                extract_code_hints(&issue.title, description.unwrap_or(""), &issue.labels());

            enriched.push(serde_json::json!({
                "identifier": issue.identifier,
                "url": issue.url,
                "title": issue.title,
                "description": description,
                "state_name": issue.state_name,
                "assignee_name": issue.assignee_name,
                "project_name": issue.project_name,
                "labels": issue.labels(),
                "created_at": issue.created_at,
                "similar_issues": similar,
                "relations": relations,
                "code_search_hints": code_search_hints,
            }));
        }

        let mut result = serde_json::json!({
            "instruction": "IMPORTANT: For each issue below, BEFORE asking the user any questions, search the codebase using the code_search_hints. Use Grep, Glob, Read, or Cuttlefish MCP tools (get_symbols, find_references) to understand the current code state. Then present your code findings alongside the issue summary. Always include the issue's Linear URL as a clickable markdown link [IDENTIFIER](url). Assume the perspective of a principal staff software engineer who has been tasked to implement this issue. Ask 2-4 thoughtful clarifying questions that would help elucidate any ambiguity or uncertainty in the issue description — the kind of questions an experienced engineer asks before writing code.",
            "queue": enriched,
            "total_remaining": total_remaining,
            "team": args.team,
        });

        enrich_with_issue_links(&mut result, &self.db);
        serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
    }

    #[tool(
        name = "mark_triaged",
        description = "Mark an issue as triaged by setting priority and optionally updating title, description, and adding a triage comment. Combines update + comment into one call. Prefer using the comment field over description for adding context — description updates risk losing formatting. Image references in the original description are automatically preserved."
    )]
    async fn mark_triaged(&self, #[tool(aggr)] args: MarkTriagedArgs) -> Result<String, String> {
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
        let (issue, issue_relations) = client
            .fetch_single_issue(&local_issue.id)
            .await
            .map_err(|e| e.to_string())?;
        self.db.upsert_issue(&issue).map_err(|e| e.to_string())?;
        self.db
            .upsert_relations(&issue.id, &issue_relations)
            .map_err(|e| e.to_string())?;

        // If someone else already prioritized it, let the caller know
        if issue.priority != 0 {
            return Ok(serde_json::json!({
                "identifier": issue.identifier,
                "url": issue.url,
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
                changes.push(format!(
                    "title changed: \"{}\" → \"{}\"",
                    local_issue.title, issue.title
                ));
            }
            if issue.description != local_issue.description {
                changes.push("description was updated".to_string());
            }
            if issue.state_name != local_issue.state_name {
                changes.push(format!(
                    "state changed: {} → {}",
                    local_issue.state_name, issue.state_name
                ));
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
                "url": issue.url,
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

        // Resolve state name to ID if provided
        let state_id = if let Some(ref state_name) = args.state {
            Some(
                client
                    .get_state_id(&issue.team_key, state_name)
                    .await
                    .map_err(|e| e.to_string())?,
            )
        } else {
            None
        };

        let label_ids = if let Some(ref label_names) = args.labels {
            Some(
                client
                    .get_label_ids(label_names)
                    .await
                    .map_err(|e| e.to_string())?,
            )
        } else {
            None
        };

        let project_id = if let Some(ref project_name) = args.project {
            if project_name.eq_ignore_ascii_case("none") {
                Some(String::new())
            } else {
                Some(
                    client
                        .get_project_id(project_name)
                        .await
                        .map_err(|e| e.to_string())?,
                )
            }
        } else {
            None
        };

        // Preserve any image references from the original description
        let safe_description = args
            .description
            .as_ref()
            .map(|new_desc| match &issue.description {
                Some(original) => preserve_images(original, new_desc),
                None => new_desc.clone(),
            });

        client
            .update_issue(
                &issue.id,
                args.title.as_deref(),
                safe_description.as_deref(),
                Some(args.priority),
                state_id.as_deref(),
                label_ids.as_deref(),
                project_id.as_deref(),
            )
            .await
            .map_err(|e| e.to_string())?;

        if let Some(ref comment_text) = args.comment {
            client
                .add_comment(&issue.id, comment_text)
                .await
                .map_err(|e| e.to_string())?;
        }

        let (updated, updated_relations) = client
            .fetch_single_issue(&issue.id)
            .await
            .map_err(|e| e.to_string())?;
        self.db.upsert_issue(&updated).map_err(|e| e.to_string())?;
        self.db
            .upsert_relations(&updated.id, &updated_relations)
            .map_err(|e| e.to_string())?;

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
            "url": issue.url,
            "priority": args.priority,
            "priority_label": priority_label,
            "title": args.title.as_deref().unwrap_or(&issue.title),
            "status": "triaged",
        })
        .to_string())
    }

    #[tool(
        name = "manage_relation",
        description = "Add or remove a relation between two issues. Relation types: 'blocks', 'blocked_by', 'related', 'duplicate'. Use action 'add' to create or 'remove' to delete a relation."
    )]
    async fn manage_relation(
        &self,
        #[tool(aggr)] args: ManageRelationArgs,
    ) -> Result<String, String> {
        let valid_types = ["blocks", "blocked_by", "related", "duplicate"];
        if !valid_types.contains(&args.relation_type.as_str()) {
            return Err(format!(
                "Invalid relation_type '{}'. Must be one of: {}",
                args.relation_type,
                valid_types.join(", ")
            ));
        }

        let source = self
            .db
            .get_issue(&args.issue)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Issue '{}' not found", args.issue))?;
        let target = self
            .db
            .get_issue(&args.related_issue)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Issue '{}' not found", args.related_issue))?;

        let client = LinearClient::new(&self.config).map_err(|e| e.to_string())?;

        match args.action.as_str() {
            "add" => {
                let relation_id = client
                    .create_relation(&source.id, &target.id, &args.relation_type)
                    .await
                    .map_err(|e| e.to_string())?;

                // Re-fetch to update local relations
                let (updated, relations) = client
                    .fetch_single_issue(&source.id)
                    .await
                    .map_err(|e| e.to_string())?;
                self.db.upsert_issue(&updated).map_err(|e| e.to_string())?;
                self.db
                    .upsert_relations(&updated.id, &relations)
                    .map_err(|e| e.to_string())?;

                Ok(serde_json::json!({
                    "status": "added",
                    "relation_id": relation_id,
                    "issue": source.identifier,
                    "related_issue": target.identifier,
                    "relation_type": args.relation_type,
                })
                .to_string())
            }
            "remove" => {
                // For blocked_by, the stored relation is reversed
                let (db_source, db_target, db_type) = if args.relation_type == "blocked_by" {
                    (&target.id, &source.id, "blocks")
                } else {
                    (&source.id, &target.id, args.relation_type.as_str())
                };

                let relation_id = self
                    .db
                    .find_relation_id(db_source, db_target, db_type)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| {
                        format!(
                            "No '{}' relation found between {} and {}",
                            args.relation_type, args.issue, args.related_issue
                        )
                    })?;

                client
                    .delete_relation(&relation_id)
                    .await
                    .map_err(|e| e.to_string())?;

                // Re-fetch to update local relations
                let (updated, relations) = client
                    .fetch_single_issue(&source.id)
                    .await
                    .map_err(|e| e.to_string())?;
                self.db.upsert_issue(&updated).map_err(|e| e.to_string())?;
                self.db
                    .upsert_relations(&updated.id, &relations)
                    .map_err(|e| e.to_string())?;

                Ok(serde_json::json!({
                    "status": "removed",
                    "issue": source.identifier,
                    "related_issue": target.identifier,
                    "relation_type": args.relation_type,
                })
                .to_string())
            }
            _ => Err("action must be 'add' or 'remove'".into()),
        }
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
                 IMPORTANT: Present exactly ONE issue at a time. Wait for the user's response and call mark_triaged before presenting the next issue. \
                 Never batch multiple issues into a single message.\n\n\
                 When the user asks to triage issues:\n\
                 1. Call get_triage_queue with the team key. IMPORTANT: Always use get_triage_queue from rectilinear — \
                 never use Linear's list_issues as a substitute. If the user asks to include completed issues, \
                 pass include_completed: true.\n\
                 2. Take the FIRST issue from the queue. BEFORE presenting it to the user, use the code_search_hints field to explore the codebase. \
                 Search for the mentioned files, symbols, and keywords using Grep, Glob, Read, or Cuttlefish MCP tools (get_symbols, find_references, get_hover_info). \
                 Spend 2-4 tool calls understanding the current code state for THIS issue.\n\
                 3. Present a brief summary of the issue AND what you found in the code. \
                 Assume the perspective of a principal staff software engineer who has been tasked to implement this issue. \
                 Ask 2-4 thoughtful clarifying questions that would help elucidate any ambiguity or uncertainty in the issue description — \
                 the kind of questions an experienced engineer asks before writing code \
                 (e.g. \"I found WorktreeManager.cleanup() at src/worktree.rs:142 — it already handles orphaned worktrees. Is this issue about a gap in that logic, or something else entirely?\"). \
                 Suggest best-guess answers.\n\
                 4. WAIT for the user to respond. Based on their answers, propose: priority (1-4), improved title, \
                 triage comment (use the comment field for adding context, code references, and file paths — prefer comments over description changes to avoid losing images or formatting), \
                 state change if appropriate (e.g. Done, Cancelled, Duplicate), and any label or project changes. \
                 Only update the description if the original is genuinely wrong or missing key information.\n\
                 5. WAIT for user confirmation, then call mark_triaged with all agreed changes.\n\
                 6. Only after mark_triaged succeeds, move to the NEXT issue. Repeat from step 2. \
                 When the batch is exhausted, call get_triage_queue again with processed identifiers in exclude.\n\n\
                 ## Archival Mode\n\
                 To triage completed/canceled issues (for archival prioritization), pass include_completed: true to get_triage_queue. \
                 This surfaces Done/Canceled/Duplicate issues that have no priority set. The workflow is the same — set a priority for historical record.\n\n\
                 ## Priority Framework\n\
                 1=Urgent (production down, data loss, security)\n\
                 2=High (major feature broken, significant user impact, no workaround)\n\
                 3=Medium (degraded experience, workarounds exist)\n\
                 4=Low (minor polish, nice-to-have)\n\n\
                 ## Duplicate Handling\n\
                 If similar_issues show >0.8 similarity, flag as potential duplicate. Ask user whether to merge, close as dup, or keep separate. \
                 Use the state field in mark_triaged to set the status (e.g. state: \"Duplicate\", state: \"Done\", state: \"Cancelled\").\n\n\
                 ## Labels and Projects\n\
                 When triaging, consider whether the issue should be labeled or assigned to a project. \
                 Use the labels and project fields in mark_triaged to set these. Pass project: \"none\" to remove an issue from its current project.\n\n\
                 ## Issue Relations\n\
                 Issues may have relations (blocks, blocked_by, related, duplicate) visible in the `relations` field. \
                 When triaging, surface blocking relationships — they affect priority. Use manage_relation to add/remove relations. \
                 If an issue blocks or is blocked by another, always mention this prominently.\n\n\
                 ## Linear Links\n\
                 ALWAYS include the Linear issue URL (from the `url` field) as a clickable markdown link when presenting issues to the user. \
                 Format as [IDENTIFIER](url) so the user can click through to Linear directly. \
                 Do this for the main issue being discussed AND for any related, blocking, or similar issues referenced. \
                 Tool responses include a `referenced_issues` field that maps any issue identifiers found in descriptions/comments \
                 to their URLs and titles — use these to render all mentioned issues as clickable links."
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_image_references() {
        let text = "Some text ![screenshot](https://uploads.linear.app/abc.png) more text";
        let images = extract_image_references(text);
        assert_eq!(
            images,
            vec!["![screenshot](https://uploads.linear.app/abc.png)"]
        );
    }

    #[test]
    fn test_extract_multiple_images() {
        let text = "![a](url1) text ![b](url2)";
        let images = extract_image_references(text);
        assert_eq!(images, vec!["![a](url1)", "![b](url2)"]);
    }

    #[test]
    fn test_extract_empty_alt() {
        let text = "![](https://example.com/img.png)";
        let images = extract_image_references(text);
        assert_eq!(images, vec!["![](https://example.com/img.png)"]);
    }

    #[test]
    fn test_no_images() {
        let text = "Just some regular text with [a link](url)";
        let images = extract_image_references(text);
        assert!(images.is_empty());
    }

    #[test]
    fn test_preserve_images_no_originals() {
        let result = preserve_images("no images here", "new description");
        assert_eq!(result, "new description");
    }

    #[test]
    fn test_preserve_images_keeps_missing() {
        let original = "Text ![img](https://uploads.linear.app/abc.png) more";
        let new_desc = "Rewritten description";
        let result = preserve_images(original, new_desc);
        assert!(result.starts_with("Rewritten description"));
        assert!(result.contains("![img](https://uploads.linear.app/abc.png)"));
    }

    #[test]
    fn test_preserve_images_already_present() {
        let original = "Text ![img](url)";
        let new_desc = "New text ![img](url)";
        let result = preserve_images(original, new_desc);
        assert_eq!(result, "New text ![img](url)");
    }
}
