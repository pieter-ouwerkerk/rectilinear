use std::future::ready;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::db::{
    self, comment_refresh_cutoff, recent_cutoff, Database, HydrationPolicy, HydrationResource,
    HydrationStatus, IndexUpsertOutcome, IssueIndexEntry,
};

use super::pagination::{operation_error, LinearErrorKind};
use super::{
    paginate, ConnectionPage, LinearClient, LinearIssue, LinearOperation, PageInfo, SingleIssueData,
};

const INDEX_OVERLAP_SECONDS: i64 = 300;
const INDEX_SAFETY_LAG_SECONDS: i64 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncProgressPhase {
    IndexingIssues,
    IndexComplete,
    HydratingIssueDetails,
    HydratingLabels,
    HydratingRelations,
    HydratingComments,
    WaitingForRateLimitRetry,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncProgressUpdate {
    pub phase: SyncProgressPhase,
    pub completed: usize,
    pub total: Option<usize>,
    pub issue_id: Option<String>,
}

pub type SyncProgressCallback<'a> = dyn Fn(SyncProgressUpdate) + Send + Sync + 'a;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IssueIndexSyncResult {
    pub indexed: usize,
    pub inserted: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub queued_for_hydration: usize,
    pub committed_checkpoint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueHydrationResult {
    pub issue_id: String,
    pub status: HydrationStatus,
    pub hydrated_resources: usize,
    pub retryable_failures: usize,
    pub permanent_failures: usize,
    pub rate_limited: bool,
    pub resources: Vec<db::HydrationResourceState>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HydrationBatchResult {
    pub requested: usize,
    pub hydrated: usize,
    pub partial: usize,
    pub deferred: usize,
    pub retryable_failures: usize,
    pub permanent_failures: usize,
    pub required_failures: usize,
    pub comment_failures: usize,
    pub rate_limited: bool,
}

#[derive(Debug, Deserialize)]
struct IndexIssuesData {
    issues: IndexIssueConnection,
}

#[derive(Debug, Deserialize)]
struct IndexIssueConnection {
    nodes: Vec<LinearIssueIndex>,
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
}

#[derive(Debug, Deserialize)]
struct LinearIssueIndex {
    id: String,
    identifier: String,
    title: String,
    url: String,
    team: super::LinearTeam,
    state: super::LinearState,
    #[serde(rename = "createdAt")]
    created_at: String,
    #[serde(rename = "updatedAt")]
    updated_at: String,
    #[serde(rename = "archivedAt", default)]
    archived_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResourceRun {
    Hydrated,
    Retryable { rate_limited: bool },
    Permanent,
}

impl LinearClient {
    /// Synchronize only the authoritative issue list fields. Relay cursors are
    /// used only inside this fixed timestamp window and are never persisted as
    /// the durable checkpoint.
    pub async fn sync_team_index(
        &self,
        db: &Database,
        team_key: &str,
        workspace_id: &str,
        full: bool,
        progress: Option<&SyncProgressCallback<'_>>,
    ) -> Result<IssueIndexSyncResult> {
        let upper = Utc::now() - chrono::Duration::seconds(INDEX_SAFETY_LAG_SECONDS);
        self.sync_team_index_window(db, team_key, workspace_id, full, upper, progress)
            .await
    }

    pub(crate) async fn sync_team_index_window(
        &self,
        db: &Database,
        team_key: &str,
        workspace_id: &str,
        full: bool,
        upper: DateTime<Utc>,
        progress: Option<&SyncProgressCallback<'_>>,
    ) -> Result<IssueIndexSyncResult> {
        let committed = db.get_synced_through_at(workspace_id, team_key)?;
        let lower = if full {
            None
        } else {
            committed
                .as_deref()
                .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                .map(|value| {
                    (value.with_timezone(&Utc) - chrono::Duration::seconds(INDEX_OVERLAP_SECONDS))
                        .to_rfc3339()
                })
        };
        let upper = upper.to_rfc3339();
        let sync_token = Uuid::new_v4().to_string();
        let mut result = IssueIndexSyncResult {
            committed_checkpoint: upper.clone(),
            ..Default::default()
        };

        db.mark_sync_family_running(
            workspace_id,
            team_key,
            "issue index",
            None,
            Some(self.sync_query_config().page_size(LinearOperation::Issues)),
            &sync_token,
        )?;

        let pagination = paginate(
            self.sync_query_config(),
            LinearOperation::Issues,
            Some(team_key.to_string()),
            |request| {
                let lower = lower.clone();
                let upper = upper.clone();
                async move {
                    self.fetch_issue_index_page(
                        team_key,
                        request.cursor.as_deref(),
                        lower.as_deref(),
                        &upper,
                        request.page_size,
                    )
                    .await
                }
            },
            |issues, context| {
                let persisted = (|| {
                    for issue in issues {
                        match db.upsert_issue_index(
                            &IssueIndexEntry {
                                id: issue.id,
                                identifier: issue.identifier,
                                team_key: issue.team.key,
                                title: issue.title,
                                state_name: issue.state.name,
                                state_type: issue.state.state_type,
                                created_at: issue.created_at,
                                updated_at: issue.updated_at,
                                archived_at: issue.archived_at,
                                url: issue.url,
                            },
                            workspace_id,
                            &sync_token,
                        )? {
                            IndexUpsertOutcome::Inserted => result.inserted += 1,
                            IndexUpsertOutcome::Updated => result.updated += 1,
                            IndexUpsertOutcome::Unchanged => result.unchanged += 1,
                        }
                        result.indexed += 1;
                    }
                    result.queued_for_hydration = result.inserted + result.updated;
                    db.mark_sync_family_running(
                        workspace_id,
                        team_key,
                        "issue index",
                        context.cursor.as_deref(),
                        Some(context.page_size),
                        &sync_token,
                    )?;
                    if let Some(callback) = progress {
                        callback(SyncProgressUpdate {
                            phase: SyncProgressPhase::IndexingIssues,
                            completed: result.indexed,
                            total: None,
                            issue_id: None,
                        });
                    }
                    Ok(())
                })();
                ready(persisted)
            },
            |issue| issue.id.clone(),
            |event| self.observe_sync_event(event),
        )
        .await;

        if let Err(error) = pagination {
            let message = self.redacted_error_message(&error);
            db.mark_sync_family_failed(
                workspace_id,
                team_key,
                "issue index",
                &sync_token,
                &message,
            )?;
            return Err(error);
        }

        if full {
            db.reconcile_full_issue_index(workspace_id, team_key, &sync_token, &upper)?;
        }
        db.mark_sync_family_complete(
            workspace_id,
            team_key,
            "issue index",
            Some(self.sync_query_config().page_size(LinearOperation::Issues)),
            &sync_token,
        )?;
        // This is deliberately the final durable write. Any fetch or page
        // persistence error above leaves the previously committed value intact.
        db.set_sync_cursor(workspace_id, team_key, &upper)?;
        if let Some(callback) = progress {
            callback(SyncProgressUpdate {
                phase: SyncProgressPhase::IndexComplete,
                completed: result.indexed,
                total: Some(result.indexed),
                issue_id: None,
            });
        }
        Ok(result)
    }

    async fn fetch_issue_index_page(
        &self,
        team_key: &str,
        cursor: Option<&str>,
        lower: Option<&str>,
        upper: &str,
        page_size: usize,
    ) -> Result<ConnectionPage<LinearIssueIndex>> {
        let updated_filter = if lower.is_some() {
            "updatedAt: { gte: $lower, lte: $upper }"
        } else {
            "updatedAt: { lte: $upper }"
        };
        let lower_declaration = if lower.is_some() {
            ", $lower: DateTime"
        } else {
            ""
        };
        let query = format!(
            r#"query($first: Int!, $after: String, $teamKey: String!{lower_declaration}, $upper: DateTime!) {{
                issues(
                    first: $first,
                    after: $after,
                    filter: {{ team: {{ key: {{ eq: $teamKey }} }}, {updated_filter} }},
                    includeArchived: true,
                    orderBy: updatedAt
                ) {{
                    nodes {{
                        id identifier title url createdAt updatedAt archivedAt
                        team {{ key }}
                        state {{ name type }}
                    }}
                    pageInfo {{ hasNextPage endCursor }}
                }}
            }}"#
        );
        let mut variables = serde_json::json!({
            "first": page_size,
            "after": cursor,
            "teamKey": team_key,
            "upper": upper,
        });
        if let Some(lower) = lower {
            variables["lower"] = serde_json::Value::String(lower.to_string());
        }
        let data: IndexIssuesData = self
            .query_operation("issue index", cursor, &query, variables)
            .await?;
        Ok(ConnectionPage {
            nodes: data.issues.nodes,
            page_info: data.issues.page_info,
        })
    }

    /// Hydrate one selected issue immediately. Explicit requests re-queue
    /// permanent failures once, allowing a user to retry after permissions
    /// change without creating a background hot loop.
    pub async fn hydrate_issue(
        &self,
        db: &Database,
        issue_id: &str,
        workspace_id: &str,
        progress: Option<&SyncProgressCallback<'_>>,
    ) -> Result<IssueHydrationResult> {
        let issue = db
            .get_issue(issue_id)?
            .with_context(|| format!("issue '{issue_id}' is not present in the local index"))?;
        if issue.workspace_id != workspace_id {
            anyhow::bail!("issue '{issue_id}' is not in workspace '{workspace_id}'");
        }
        db.requeue_issue_hydration(workspace_id, &issue.id, "explicit")?;
        self.hydrate_one(db, &issue.id, workspace_id, progress)
            .await
    }

    /// Hydrate a deterministic bounded batch. Execution is intentionally
    /// sequential (concurrency bound of one) to avoid task fan-out and to stop
    /// immediately when Linear asks the client to back off.
    pub async fn hydrate_pending_issues(
        &self,
        db: &Database,
        team_key: &str,
        workspace_id: &str,
        limit: usize,
        policy: HydrationPolicy,
        progress: Option<&SyncProgressCallback<'_>>,
    ) -> Result<HydrationBatchResult> {
        if limit == 0 {
            return Ok(HydrationBatchResult::default());
        }
        let now = Utc::now();
        let now_text = now.to_rfc3339();
        let recent_after = recent_cutoff(now);
        let stale_comments = comment_refresh_cutoff(now);
        db.queue_stale_comment_hydration(
            workspace_id,
            team_key,
            policy,
            &stale_comments,
            &recent_after,
        )?;
        let candidates = db.list_hydration_candidates(
            workspace_id,
            team_key,
            limit,
            policy,
            &now_text,
            &recent_after,
        )?;
        let mut batch = HydrationBatchResult {
            requested: candidates.len(),
            ..Default::default()
        };
        for (index, candidate) in candidates.iter().enumerate() {
            let result = self
                .hydrate_one(db, &candidate.id, workspace_id, progress)
                .await?;
            batch.retryable_failures += result.retryable_failures;
            batch.permanent_failures += result.permanent_failures;
            for resource in &result.resources {
                if resource.status != HydrationStatus::Hydrated {
                    if resource.resource == HydrationResource::Comments {
                        batch.comment_failures += 1;
                    } else {
                        batch.required_failures += 1;
                    }
                }
            }
            match result.status {
                HydrationStatus::Hydrated => batch.hydrated += 1,
                _ => batch.partial += 1,
            }
            if result.rate_limited {
                batch.rate_limited = true;
                batch.deferred = candidates.len().saturating_sub(index + 1);
                break;
            }
        }
        Ok(batch)
    }

    async fn hydrate_one(
        &self,
        db: &Database,
        issue_id: &str,
        workspace_id: &str,
        progress: Option<&SyncProgressCallback<'_>>,
    ) -> Result<IssueHydrationResult> {
        let mut source_updated_at = db
            .get_issue(issue_id)?
            .with_context(|| format!("issue '{issue_id}' disappeared during hydration"))?
            .updated_at;
        let initial = db.get_issue_hydration_state(workspace_id, issue_id)?;
        let mut hydrated_resources = 0;
        let mut retryable_failures = 0;
        let mut permanent_failures = 0;
        let mut rate_limited = false;

        for resource_state in initial.resources {
            if !matches!(
                resource_state.status,
                HydrationStatus::Pending | HydrationStatus::Retryable
            ) {
                continue;
            }
            let phase = match resource_state.resource {
                HydrationResource::Details => SyncProgressPhase::HydratingIssueDetails,
                HydrationResource::Labels => SyncProgressPhase::HydratingLabels,
                HydrationResource::Relations => SyncProgressPhase::HydratingRelations,
                HydrationResource::Comments => SyncProgressPhase::HydratingComments,
            };
            if let Some(callback) = progress {
                callback(SyncProgressUpdate {
                    phase,
                    completed: 0,
                    total: Some(1),
                    issue_id: Some(issue_id.to_string()),
                });
            }
            let attempted_at = Utc::now().to_rfc3339();
            let attempts = db.mark_hydration_running(
                workspace_id,
                issue_id,
                resource_state.resource,
                &attempted_at,
            )?;
            let operation = match resource_state.resource {
                HydrationResource::Details => match self.fetch_issue_details_only(issue_id).await {
                    Ok(mut issue) => {
                        issue.workspace_id = workspace_id.to_string();
                        source_updated_at = issue.updated_at.clone();
                        db.upsert_issue_preserving_labels(&issue).map(|_| 1)
                    }
                    Err(error) => Err(error),
                },
                HydrationResource::Labels => {
                    self.sync_issue_labels_in_workspace(db, issue_id, workspace_id)
                        .await
                }
                HydrationResource::Relations => self.sync_issue_relations(db, issue_id).await,
                HydrationResource::Comments => {
                    self.sync_issue_comments(db, issue_id, workspace_id).await
                }
            };
            let run = match operation {
                Ok(_) => {
                    db.mark_hydration_complete(
                        workspace_id,
                        issue_id,
                        resource_state.resource,
                        &source_updated_at,
                        &Utc::now().to_rfc3339(),
                    )?;
                    hydrated_resources += 1;
                    ResourceRun::Hydrated
                }
                Err(error) => {
                    let classified = operation_error(&error);
                    let (status, next_retry, is_rate_limit) = match classified.map(|e| e.kind) {
                        Some(
                            LinearErrorKind::RateLimit
                            | LinearErrorKind::Transport
                            | LinearErrorKind::Transient,
                        ) => {
                            let delay = retry_delay(
                                issue_id,
                                resource_state.resource,
                                attempts,
                                classified,
                            );
                            (
                                HydrationStatus::Retryable,
                                Some(
                                    (Utc::now() + chrono::Duration::from_std(delay)?).to_rfc3339(),
                                ),
                                classified.is_some_and(|e| e.kind == LinearErrorKind::RateLimit),
                            )
                        }
                        Some(LinearErrorKind::Authentication) => {
                            (HydrationStatus::PermissionDenied, None, false)
                        }
                        _ => (HydrationStatus::Unavailable, None, false),
                    };
                    let message = self.redacted_error_message(&error);
                    db.mark_hydration_failed(
                        workspace_id,
                        issue_id,
                        resource_state.resource,
                        status,
                        next_retry.as_deref(),
                        &message,
                    )?;
                    if status == HydrationStatus::Retryable {
                        retryable_failures += 1;
                        if is_rate_limit {
                            rate_limited = true;
                            if let Some(callback) = progress {
                                callback(SyncProgressUpdate {
                                    phase: SyncProgressPhase::WaitingForRateLimitRetry,
                                    completed: 0,
                                    total: None,
                                    issue_id: Some(issue_id.to_string()),
                                });
                            }
                        }
                        ResourceRun::Retryable {
                            rate_limited: is_rate_limit,
                        }
                    } else {
                        permanent_failures += 1;
                        ResourceRun::Permanent
                    }
                }
            };
            if let Some(callback) = progress {
                callback(SyncProgressUpdate {
                    phase,
                    completed: usize::from(run == ResourceRun::Hydrated),
                    total: Some(1),
                    issue_id: Some(issue_id.to_string()),
                });
            }
            if matches!(run, ResourceRun::Retryable { rate_limited: true }) {
                break;
            }
        }
        let state = db.get_issue_hydration_state(workspace_id, issue_id)?;
        Ok(IssueHydrationResult {
            issue_id: issue_id.to_string(),
            status: state.status,
            hydrated_resources,
            retryable_failures,
            permanent_failures,
            rate_limited,
            resources: state.resources,
        })
    }

    async fn fetch_issue_details_only(&self, issue_id: &str) -> Result<db::Issue> {
        let query = r#"
            query($id: String!) {
                issue(id: $id) {
                    id identifier url title description priority branchName
                    createdAt updatedAt archivedAt
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
            .query_operation(
                "issue details",
                None,
                query,
                serde_json::json!({ "id": issue_id }),
            )
            .await?;
        Ok(Self::convert_linear_issue(data.issue).0)
    }
}

fn retry_delay(
    issue_id: &str,
    resource: HydrationResource,
    attempts: u32,
    classified: Option<&super::LinearOperationError>,
) -> Duration {
    if let Some(delay) = classified.and_then(|error| error.retry_after) {
        return delay.min(Duration::from_secs(6 * 60 * 60));
    }
    let exponent = attempts.saturating_sub(1).min(10);
    let base = Duration::from_secs(30_u64.saturating_mul(1_u64 << exponent))
        .min(Duration::from_secs(6 * 60 * 60));
    let mut hasher = Sha256::new();
    hasher.update(issue_id.as_bytes());
    hasher.update(resource.as_str().as_bytes());
    hasher.update(attempts.to_le_bytes());
    let digest = hasher.finalize();
    let jitter_basis = u16::from_le_bytes([digest[0], digest[1]]) as u64;
    let jitter_max = (base.as_secs() / 4).max(1);
    base.saturating_add(Duration::from_secs(jitter_basis % jitter_max))
}

#[allow(dead_code)]
fn _assert_linear_issue_is_reachable(_: LinearIssue) {}
