//! UniFFI facade — single entry point for Swift callers.
//!
//! Exposes a `RectilinearEngine` object with sync methods for database reads
//! and async methods for network operations. All types crossing the FFI
//! boundary use the `Rt` prefix to avoid collisions with Swift-side types.

use crate::config::Config;
use crate::db::Database;
use crate::linear::LinearClient;
use crate::search;
use std::path::Path;
use tokio::sync::OnceCell;

// ── Error ────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum RectilinearError {
    #[error("Database error: {message}")]
    Database { message: String },
    #[error("API error: {message}")]
    Api { message: String },
    #[error("Config error: {message}")]
    Config { message: String },
    #[error("Not found: {key}")]
    NotFound { key: String },
}

impl From<anyhow::Error> for RectilinearError {
    fn from(err: anyhow::Error) -> Self {
        RectilinearError::Database {
            message: err.to_string(),
        }
    }
}

// ── FFI Records ──────────────────────────────────────────────────────

#[derive(uniffi::Record)]
pub struct RtIssue {
    pub id: String,
    pub identifier: String,
    pub team_key: String,
    pub title: String,
    pub description: Option<String>,
    pub state_name: String,
    pub state_type: String,
    pub priority: i32,
    pub assignee_name: Option<String>,
    pub project_name: Option<String>,
    pub labels: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    pub url: String,
    pub branch_name: Option<String>,
}

impl From<crate::db::Issue> for RtIssue {
    fn from(issue: crate::db::Issue) -> Self {
        let labels: Vec<String> = serde_json::from_str(&issue.labels_json).unwrap_or_default();
        Self {
            id: issue.id,
            identifier: issue.identifier,
            team_key: issue.team_key,
            title: issue.title,
            description: issue.description,
            state_name: issue.state_name,
            state_type: issue.state_type,
            priority: issue.priority,
            assignee_name: issue.assignee_name,
            project_name: issue.project_name,
            labels,
            created_at: issue.created_at,
            updated_at: issue.updated_at,
            url: issue.url,
            branch_name: issue.branch_name,
        }
    }
}

#[derive(uniffi::Record)]
pub struct RtSearchResult {
    pub issue_id: String,
    pub identifier: String,
    pub title: String,
    pub state_name: String,
    pub priority: i32,
    pub score: f64,
    pub similarity: Option<f32>,
}

impl From<search::SearchResult> for RtSearchResult {
    fn from(sr: search::SearchResult) -> Self {
        Self {
            issue_id: sr.issue_id,
            identifier: sr.identifier,
            title: sr.title,
            state_name: sr.state_name,
            priority: sr.priority,
            score: sr.score,
            similarity: sr.similarity,
        }
    }
}

#[derive(uniffi::Record)]
pub struct RtRelation {
    pub relation_type: String,
    pub issue_identifier: String,
    pub issue_title: String,
    pub issue_state: String,
    pub issue_url: String,
}

impl From<crate::db::EnrichedRelation> for RtRelation {
    fn from(rel: crate::db::EnrichedRelation) -> Self {
        Self {
            relation_type: rel.relation_type,
            issue_identifier: rel.issue_identifier,
            issue_title: rel.issue_title,
            issue_state: rel.issue_state,
            issue_url: rel.issue_url,
        }
    }
}

#[derive(uniffi::Record)]
pub struct RtBlocker {
    pub identifier: String,
    pub title: String,
    pub state_name: String,
    pub is_terminal: bool,
}

#[derive(uniffi::Record)]
pub struct RtIssueEnriched {
    pub id: String,
    pub identifier: String,
    pub team_key: String,
    pub title: String,
    pub description: Option<String>,
    pub state_name: String,
    pub state_type: String,
    pub priority: i32,
    pub assignee_name: Option<String>,
    pub project_name: Option<String>,
    pub labels: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    pub url: String,
    pub branch_name: Option<String>,
    pub blocked_by: Vec<RtBlocker>,
}

#[derive(uniffi::Record)]
pub struct RtTeam {
    pub id: String,
    pub key: String,
    pub name: String,
}

#[derive(uniffi::Enum)]
pub enum RtSearchMode {
    Fts,
    Vector,
    Hybrid,
}

#[derive(uniffi::Record)]
pub struct RtFieldCompleteness {
    pub total: u64,
    pub with_description: u64,
    pub with_priority: u64,
    pub with_labels: u64,
    pub with_project: u64,
}

#[derive(uniffi::Record)]
pub struct RtIssueSummary {
    pub id: String,
    pub identifier: String,
    pub team_key: String,
    pub title: String,
    pub state_name: String,
    pub state_type: String,
    pub priority: i32,
    pub project_name: Option<String>,
    pub labels: Vec<String>,
    pub updated_at: String,
    pub url: String,
    pub has_description: bool,
    pub has_embedding: bool,
}

impl From<crate::db::IssueSummary> for RtIssueSummary {
    fn from(s: crate::db::IssueSummary) -> Self {
        Self {
            id: s.id,
            identifier: s.identifier,
            team_key: s.team_key,
            title: s.title,
            state_name: s.state_name,
            state_type: s.state_type,
            priority: s.priority,
            project_name: s.project_name,
            labels: s.labels,
            updated_at: s.updated_at,
            url: s.url,
            has_description: s.has_description,
            has_embedding: s.has_embedding,
        }
    }
}

#[derive(uniffi::Record)]
pub struct RtTeamSummary {
    pub key: String,
    pub issue_count: u64,
    pub embedded_count: u64,
    pub last_synced_at: Option<String>,
}

impl From<crate::db::TeamSummary> for RtTeamSummary {
    fn from(t: crate::db::TeamSummary) -> Self {
        Self {
            key: t.key,
            issue_count: t.issue_count as u64,
            embedded_count: t.embedded_count as u64,
            last_synced_at: t.last_synced_at,
        }
    }
}

impl From<RtSearchMode> for search::SearchMode {
    fn from(mode: RtSearchMode) -> Self {
        match mode {
            RtSearchMode::Fts => search::SearchMode::Fts,
            RtSearchMode::Vector => search::SearchMode::Vector,
            RtSearchMode::Hybrid => search::SearchMode::Hybrid,
        }
    }
}

// ── Engine ───────────────────────────────────────────────────────────

#[derive(uniffi::Object)]
pub struct RectilinearEngine {
    db: Database,
    linear_api_key: String,
    gemini_api_key: Option<String>,
    /// Lazily initialized on first async call so it's created inside
    /// UniFFI's Tokio runtime, binding hyper's DNS resolver to a live reactor.
    http_client: OnceCell<reqwest::Client>,
}

impl RectilinearEngine {
    /// Get or create the HTTP client. Lazily initialized so it's created
    /// inside the caller's Tokio runtime (UniFFI's), binding hyper's DNS
    /// resolver to a live reactor.
    async fn client(&self) -> &reqwest::Client {
        self.http_client
            .get_or_init(|| async { reqwest::Client::new() })
            .await
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl RectilinearEngine {
    /// Create a new engine with an explicit database path and API keys.
    #[uniffi::constructor]
    pub fn new(
        db_path: String,
        linear_api_key: String,
        gemini_api_key: Option<String>,
    ) -> Result<Self, RectilinearError> {
        let path = Path::new(&db_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| RectilinearError::Config {
                message: format!("Failed to create database directory: {e}"),
            })?;
        }

        let db = Database::open(path)?;

        Ok(Self {
            db,
            linear_api_key,
            gemini_api_key,
            http_client: OnceCell::new(),
        })
    }

    // ── Sync methods (database reads, fast) ──────────────────────

    /// Look up an issue by UUID or identifier (e.g. "CUT-123").
    pub fn get_issue(&self, id_or_identifier: String) -> Result<Option<RtIssue>, RectilinearError> {
        Ok(self.db.get_issue(&id_or_identifier)?.map(RtIssue::from))
    }

    /// Get unprioritized issues for triage.
    pub fn get_triage_queue(
        &self,
        team: Option<String>,
        include_completed: bool,
    ) -> Result<Vec<RtIssue>, RectilinearError> {
        let issues = self
            .db
            .get_unprioritized_issues(team.as_deref(), include_completed)?;
        Ok(issues.into_iter().map(RtIssue::from).collect())
    }

    /// Full-text search (FTS5, BM25 ranking). Synchronous — hits local SQLite only.
    pub fn search_fts(
        &self,
        query: String,
        limit: u32,
    ) -> Result<Vec<RtSearchResult>, RectilinearError> {
        let results = self.db.fts_search(&query, limit as usize)?;
        Ok(results
            .into_iter()
            .map(|fts| RtSearchResult {
                issue_id: fts.issue_id,
                identifier: fts.identifier,
                title: fts.title,
                state_name: fts.state_name,
                priority: fts.priority,
                score: fts.bm25_score,
                similarity: None,
            })
            .collect())
    }

    /// Count issues in the local database.
    pub fn count_issues(&self, team: Option<String>) -> Result<u64, RectilinearError> {
        Ok(self.db.count_issues(team.as_deref())? as u64)
    }

    /// Count issues that have at least one embedding chunk.
    pub fn count_embedded_issues(&self, team: Option<String>) -> Result<u64, RectilinearError> {
        Ok(self.db.count_embedded_issues(team.as_deref())? as u64)
    }

    /// Get field completeness counts in a single query.
    pub fn get_field_completeness(
        &self,
        team: Option<String>,
    ) -> Result<RtFieldCompleteness, RectilinearError> {
        let (total, desc, pri, labels, proj) = self.db.get_field_completeness(team.as_deref())?;
        Ok(RtFieldCompleteness {
            total: total as u64,
            with_description: desc as u64,
            with_priority: pri as u64,
            with_labels: labels as u64,
            with_project: proj as u64,
        })
    }

    /// List all issues with lightweight summary data. Supports pagination and filtering.
    pub fn list_all_issues(
        &self,
        team: Option<String>,
        filter: Option<String>,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<RtIssueSummary>, RectilinearError> {
        let issues = self.db.list_all_issues(
            team.as_deref(),
            filter.as_deref(),
            limit as usize,
            offset as usize,
        )?;
        Ok(issues.into_iter().map(RtIssueSummary::from).collect())
    }

    /// List teams with synced issues and their embedding coverage. Local-only, no network.
    pub fn list_synced_teams(&self) -> Result<Vec<RtTeamSummary>, RectilinearError> {
        Ok(self
            .db
            .list_synced_teams()?
            .into_iter()
            .map(RtTeamSummary::from)
            .collect())
    }

    /// Get enriched relations for an issue.
    pub fn get_relations(&self, issue_id: String) -> Result<Vec<RtRelation>, RectilinearError> {
        Ok(self
            .db
            .get_relations_enriched(&issue_id)?
            .into_iter()
            .map(RtRelation::from)
            .collect())
    }

    /// Get issues filtered by team and state types, enriched with blocker info.
    pub fn get_active_issues(
        &self,
        team: String,
        state_types: Vec<String>,
    ) -> Result<Vec<RtIssueEnriched>, RectilinearError> {
        let issues = self.db.get_issues_by_state_types(&team, &state_types)?;
        let issue_ids: Vec<String> = issues.iter().map(|i| i.id.clone()).collect();
        let blockers = self.db.get_blockers_for_issues(&issue_ids)?;

        // Group blockers by issue ID
        let mut blocker_map: std::collections::HashMap<String, Vec<RtBlocker>> =
            std::collections::HashMap::new();
        for b in blockers {
            let is_terminal = matches!(b.state_type.as_str(), "completed" | "canceled");
            blocker_map.entry(b.issue_id).or_default().push(RtBlocker {
                identifier: b.identifier,
                title: b.title,
                state_name: b.state_name,
                is_terminal,
            });
        }

        Ok(issues
            .into_iter()
            .map(|issue| {
                let labels: Vec<String> =
                    serde_json::from_str(&issue.labels_json).unwrap_or_default();
                let blocked_by = blocker_map.remove(&issue.id).unwrap_or_default();
                RtIssueEnriched {
                    id: issue.id,
                    identifier: issue.identifier,
                    team_key: issue.team_key,
                    title: issue.title,
                    description: issue.description,
                    state_name: issue.state_name,
                    state_type: issue.state_type,
                    priority: issue.priority,
                    assignee_name: issue.assignee_name,
                    project_name: issue.project_name,
                    labels,
                    created_at: issue.created_at,
                    updated_at: issue.updated_at,
                    url: issue.url,
                    branch_name: issue.branch_name,
                    blocked_by,
                }
            })
            .collect())
    }

    // ── Async methods (network I/O) ─────────────────────────────

    /// List all teams from Linear.
    pub async fn list_teams(&self) -> Result<Vec<RtTeam>, RectilinearError> {
        let client =
            LinearClient::with_http_client(self.client().await.clone(), &self.linear_api_key);
        let teams = client
            .list_teams()
            .await
            .map_err(|e| RectilinearError::Api {
                message: e.to_string(),
            })?;
        Ok(teams
            .into_iter()
            .map(|t| RtTeam {
                id: t.id,
                key: t.key,
                name: t.name,
            })
            .collect())
    }

    /// Sync issues from Linear for a team. Returns the number of issues synced.
    pub async fn sync_team(&self, team_key: String, full: bool) -> Result<u64, RectilinearError> {
        let client =
            LinearClient::with_http_client(self.client().await.clone(), &self.linear_api_key);
        let count = client
            .sync_team(&self.db, &team_key, full, false, None)
            .await
            .map_err(|e| RectilinearError::Api {
                message: e.to_string(),
            })?;
        Ok(count as u64)
    }

    /// Hybrid search (FTS + vector via RRF). Requires embedder for vector component.
    pub async fn search_hybrid(
        &self,
        query: String,
        team: Option<String>,
        limit: u32,
    ) -> Result<Vec<RtSearchResult>, RectilinearError> {
        let config = Config::load().unwrap_or_default();
        let embedder = self.make_embedder(&config).await?;

        let results = search::search(
            &self.db,
            &query,
            search::SearchMode::Hybrid,
            team.as_deref(),
            None,
            limit as usize,
            embedder.as_ref(),
            config.search.rrf_k,
        )
        .await?;

        Ok(results.into_iter().map(RtSearchResult::from).collect())
    }

    /// Find potential duplicate issues by semantic similarity.
    pub async fn find_duplicates(
        &self,
        text: String,
        team: Option<String>,
        threshold: f32,
    ) -> Result<Vec<RtSearchResult>, RectilinearError> {
        let config = Config::load().unwrap_or_default();
        let embedder =
            self.make_embedder(&config)
                .await?
                .ok_or_else(|| RectilinearError::Config {
                    message:
                        "Embedder not available — set GEMINI_API_KEY or enable local embeddings"
                            .into(),
                })?;

        let results = search::find_duplicates(
            &self.db,
            &text,
            team.as_deref(),
            threshold,
            10,
            &embedder,
            config.search.rrf_k,
        )
        .await?;

        Ok(results.into_iter().map(RtSearchResult::from).collect())
    }

    /// Update an issue in Linear (title, priority, state).
    pub async fn save_issue(
        &self,
        issue_id: String,
        title: Option<String>,
        priority: Option<i32>,
        state: Option<String>,
    ) -> Result<(), RectilinearError> {
        let client =
            LinearClient::with_http_client(self.client().await.clone(), &self.linear_api_key);

        let state_id = if let Some(ref state_name) = state {
            // Need to resolve state name → ID. Get team from issue first.
            if let Some(issue) = self.db.get_issue(&issue_id)? {
                Some(
                    client
                        .get_state_id(&issue.team_key, state_name)
                        .await
                        .map_err(|e| RectilinearError::Api {
                            message: e.to_string(),
                        })?,
                )
            } else {
                None
            }
        } else {
            None
        };

        client
            .update_issue(
                &issue_id,
                title.as_deref(),
                None,
                priority,
                state_id.as_deref(),
                None,
                None,
            )
            .await
            .map_err(|e| RectilinearError::Api {
                message: e.to_string(),
            })?;

        // Re-sync the updated issue back to local DB
        if let Ok((issue, relations)) = client.fetch_single_issue(&issue_id).await {
            let _ = self.db.upsert_issue(&issue);
            let _ = self.db.upsert_relations(&issue.id, &relations);
        }

        Ok(())
    }

    /// Add a comment to a Linear issue.
    pub async fn add_comment(
        &self,
        issue_id: String,
        body: String,
    ) -> Result<(), RectilinearError> {
        let client =
            LinearClient::with_http_client(self.client().await.clone(), &self.linear_api_key);
        client
            .add_comment(&issue_id, &body)
            .await
            .map_err(|e| RectilinearError::Api {
                message: e.to_string(),
            })
    }

    /// Validate the configured Gemini API key without generating embeddings.
    pub async fn test_gemini_api_key(&self) -> Result<(), RectilinearError> {
        let api_key = self
            .gemini_api_key
            .as_deref()
            .ok_or_else(|| RectilinearError::Config {
                message: "Gemini API key not configured".into(),
            })?;

        crate::embedding::Embedder::new_api_with_http_client(self.client().await.clone(), api_key)
            .map_err(|e| RectilinearError::Config {
                message: e.to_string(),
            })?
            .test_api_key()
            .await
            .map_err(|e| RectilinearError::Api {
                message: e.to_string(),
            })
    }

    /// Fetch a single issue live from Linear and upsert into local DB.
    /// Accepts either a UUID or identifier (e.g. "CUT-123").
    pub async fn refresh_issue(
        &self,
        id_or_identifier: String,
    ) -> Result<Option<RtIssue>, RectilinearError> {
        let client =
            LinearClient::with_http_client(self.client().await.clone(), &self.linear_api_key);

        let result = if id_or_identifier.contains('-')
            && id_or_identifier
                .chars()
                .last()
                .map_or(false, |c| c.is_ascii_digit())
        {
            client
                .fetch_issue_by_identifier(&id_or_identifier)
                .await
                .map_err(|e| RectilinearError::Api {
                    message: e.to_string(),
                })?
        } else {
            Some(
                client
                    .fetch_single_issue(&id_or_identifier)
                    .await
                    .map_err(|e| RectilinearError::Api {
                        message: e.to_string(),
                    })?,
            )
        };

        if let Some((issue, relations)) = result {
            self.db.upsert_issue(&issue)?;
            self.db.upsert_relations(&issue.id, &relations)?;
            Ok(Some(RtIssue::from(issue)))
        } else {
            Ok(None)
        }
    }

    /// Generate embeddings for issues that don't have them yet.
    /// Returns the number of issues embedded.
    pub async fn embed_issues(
        &self,
        team: Option<String>,
        limit: u32,
    ) -> Result<u64, RectilinearError> {
        let config = Config::load().unwrap_or_default();
        let embedder = self.make_embedder(&config).await?.ok_or_else(|| {
            RectilinearError::Config {
                message:
                    "No embedding backend available — set GEMINI_API_KEY or enable local embeddings"
                        .into(),
            }
        })?;

        let model_name = embedder.backend_name().to_string();
        let issues = self
            .db
            .get_issues_needing_embedding(team.as_deref(), false)?;

        let to_process = if limit > 0 {
            &issues[..std::cmp::min(issues.len(), limit as usize)]
        } else {
            &issues
        };

        let mut count = 0u64;
        for issue in to_process {
            // Skip if already embedded with the same model and content hasn't changed
            if let Some(existing_model) = self.db.get_embedding_model(&issue.id)? {
                if existing_model == model_name {
                    continue;
                }
            }

            let chunks = crate::embedding::chunk_text(
                &issue.title,
                issue.description.as_deref().unwrap_or(""),
                512,
                64,
            );
            let embeddings =
                embedder
                    .embed_batch(&chunks)
                    .await
                    .map_err(|e| RectilinearError::Api {
                        message: e.to_string(),
                    })?;

            let chunk_data: Vec<(usize, String, Vec<u8>)> = chunks
                .into_iter()
                .zip(embeddings.iter())
                .enumerate()
                .map(|(idx, (text, emb))| (idx, text, crate::embedding::embedding_to_bytes(emb)))
                .collect();

            self.db
                .upsert_chunks_with_model(&issue.id, &chunk_data, &model_name)?;
            count += 1;
        }

        Ok(count)
    }
}

// ── Private helpers ──────────────────────────────────────────────────

impl RectilinearEngine {
    async fn make_embedder(
        &self,
        config: &Config,
    ) -> Result<Option<crate::embedding::Embedder>, RectilinearError> {
        let key = self
            .gemini_api_key
            .as_deref()
            .or(config.embedding.gemini_api_key.as_deref());

        if let Some(api_key) = key {
            Ok(Some(
                crate::embedding::Embedder::new_api_with_http_client(
                    self.client().await.clone(),
                    api_key,
                )
                .map_err(|e| RectilinearError::Config {
                    message: e.to_string(),
                })?,
            ))
        } else {
            #[cfg(feature = "local-embeddings")]
            {
                let models_dir = Config::models_dir().map_err(|e| RectilinearError::Config {
                    message: e.to_string(),
                })?;
                Ok(Some(
                    crate::embedding::Embedder::new_local(&models_dir).map_err(|e| {
                        RectilinearError::Config {
                            message: e.to_string(),
                        }
                    })?,
                ))
            }
            #[cfg(not(feature = "local-embeddings"))]
            {
                Ok(None)
            }
        }
    }
}
