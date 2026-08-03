use std::fmt;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use super::{Database, Issue};

pub const HYDRATION_RESOURCES: [HydrationResource; 4] = [
    HydrationResource::Details,
    HydrationResource::Labels,
    HydrationResource::Relations,
    HydrationResource::Comments,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HydrationResource {
    Details,
    Labels,
    Relations,
    Comments,
}

impl HydrationResource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Details => "details",
            Self::Labels => "labels",
            Self::Relations => "relations",
            Self::Comments => "comments",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "details" => Ok(Self::Details),
            "labels" => Ok(Self::Labels),
            "relations" => Ok(Self::Relations),
            "comments" => Ok(Self::Comments),
            _ => anyhow::bail!("unknown hydration resource '{value}'"),
        }
    }
}

impl fmt::Display for HydrationResource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HydrationStatus {
    Pending,
    Running,
    Hydrated,
    Partial,
    Retryable,
    PermissionDenied,
    Unavailable,
}

impl HydrationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Hydrated => "hydrated",
            Self::Partial => "partial",
            Self::Retryable => "retryable",
            Self::PermissionDenied => "permission_denied",
            Self::Unavailable => "unavailable",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "hydrated" => Ok(Self::Hydrated),
            "partial" => Ok(Self::Partial),
            "retryable" => Ok(Self::Retryable),
            "permission_denied" => Ok(Self::PermissionDenied),
            "unavailable" => Ok(Self::Unavailable),
            _ => anyhow::bail!("unknown hydration status '{value}'"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HydrationPolicy {
    OpenOnly,
    OpenAndRecent,
    All,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HydrationResourceState {
    pub resource: HydrationResource,
    pub status: HydrationStatus,
    pub source_updated_at: String,
    pub last_attempted_at: Option<String>,
    pub hydrated_at: Option<String>,
    pub attempt_count: u32,
    pub next_retry_at: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueHydrationState {
    pub issue_id: String,
    pub status: HydrationStatus,
    pub resources: Vec<HydrationResourceState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HydrationCandidate {
    pub id: String,
    pub identifier: String,
    pub updated_at: String,
    pub state_type: String,
}

#[derive(Debug, Clone)]
pub struct IssueIndexEntry {
    pub id: String,
    pub identifier: String,
    pub team_key: String,
    pub title: String,
    pub state_name: String,
    pub state_type: String,
    pub created_at: String,
    pub updated_at: String,
    pub archived_at: Option<String>,
    pub url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexUpsertOutcome {
    Inserted,
    Updated,
    Unchanged,
}

impl Database {
    /// Merge the authoritative list/index fields without replacing rich fields
    /// populated by an earlier hydration.
    pub fn upsert_issue_index(
        &self,
        issue: &IssueIndexEntry,
        workspace_id: &str,
        sync_token: &str,
    ) -> Result<IndexUpsertOutcome> {
        self.with_conn(|conn| {
            let existing = conn
                .query_row(
                    "SELECT identifier, team_key, title, state_name, state_type,
                            created_at, updated_at, archived_at, url
                     FROM issues WHERE id = ?1",
                    rusqlite::params![issue.id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, String>(5)?,
                            row.get::<_, String>(6)?,
                            row.get::<_, Option<String>>(7)?,
                            row.get::<_, String>(8)?,
                        ))
                    },
                )
                .optional()?;

            let outcome = match &existing {
                None => IndexUpsertOutcome::Inserted,
                Some(current)
                    if current
                        == &(
                            issue.identifier.clone(),
                            issue.team_key.clone(),
                            issue.title.clone(),
                            issue.state_name.clone(),
                            issue.state_type.clone(),
                            issue.created_at.clone(),
                            issue.updated_at.clone(),
                            issue.archived_at.clone(),
                            issue.url.clone(),
                        ) =>
                {
                    IndexUpsertOutcome::Unchanged
                }
                Some(_) => IndexUpsertOutcome::Updated,
            };

            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "INSERT INTO issues (
                    id, identifier, team_key, title, description, state_name,
                    state_type, priority, assignee_name, project_name,
                    labels_json, created_at, updated_at, content_hash, synced_at,
                    url, branch_name, workspace_id, archived_at, sync_token
                 ) VALUES (
                    ?1, ?2, ?3, ?4, NULL, ?5, ?6, 0, NULL, NULL, '[]', ?7,
                    ?8, '', datetime('now'), ?9, NULL, ?10, ?11, ?12
                 ) ON CONFLICT(id) DO UPDATE SET
                    identifier=excluded.identifier,
                    team_key=excluded.team_key,
                    title=excluded.title,
                    state_name=excluded.state_name,
                    state_type=excluded.state_type,
                    created_at=excluded.created_at,
                    updated_at=excluded.updated_at,
                    archived_at=excluded.archived_at,
                    url=excluded.url,
                    workspace_id=excluded.workspace_id,
                    sync_token=excluded.sync_token,
                    synced_at=datetime('now')",
                rusqlite::params![
                    issue.id,
                    issue.identifier,
                    issue.team_key,
                    issue.title,
                    issue.state_name,
                    issue.state_type,
                    issue.created_at,
                    issue.updated_at,
                    issue.url,
                    workspace_id,
                    issue.archived_at,
                    sync_token,
                ],
            )?;

            if outcome != IndexUpsertOutcome::Unchanged {
                for resource in HYDRATION_RESOURCES {
                    tx.execute(
                        "INSERT INTO issue_hydration_state (
                            workspace_id, issue_id, resource, status,
                            source_updated_at, queue_reason, index_sync_token, attempt_count,
                            next_retry_at, last_error
                         ) VALUES (?1, ?2, ?3, 'pending', ?4, 'index_changed', ?5, 0, NULL, NULL)
                         ON CONFLICT(workspace_id, issue_id, resource) DO UPDATE SET
                            status=CASE
                                WHEN issue_hydration_state.source_updated_at <> excluded.source_updated_at
                                THEN 'pending' ELSE issue_hydration_state.status END,
                            source_updated_at=excluded.source_updated_at,
                            queue_reason=CASE
                                WHEN issue_hydration_state.source_updated_at <> excluded.source_updated_at
                                THEN 'index_changed' ELSE issue_hydration_state.queue_reason END,
                            index_sync_token=CASE
                                WHEN issue_hydration_state.source_updated_at <> excluded.source_updated_at
                                THEN excluded.index_sync_token ELSE issue_hydration_state.index_sync_token END,
                            attempt_count=CASE
                                WHEN issue_hydration_state.source_updated_at <> excluded.source_updated_at
                                THEN 0 ELSE issue_hydration_state.attempt_count END,
                            next_retry_at=CASE
                                WHEN issue_hydration_state.source_updated_at <> excluded.source_updated_at
                                THEN NULL ELSE issue_hydration_state.next_retry_at END,
                            last_error=CASE
                                WHEN issue_hydration_state.source_updated_at <> excluded.source_updated_at
                                THEN NULL ELSE issue_hydration_state.last_error END",
                        rusqlite::params![
                            workspace_id,
                            issue.id,
                            resource.as_str(),
                            issue.updated_at,
                            sync_token,
                        ],
                    )?;
                }
            }
            tx.commit()?;
            Ok(outcome)
        })
    }

    pub fn mark_hydration_running(
        &self,
        workspace_id: &str,
        issue_id: &str,
        resource: HydrationResource,
        attempted_at: &str,
    ) -> Result<u32> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE issue_hydration_state
                 SET status='running', last_attempted_at=?4,
                     attempt_count=attempt_count + 1
                 WHERE workspace_id=?1 AND issue_id=?2 AND resource=?3",
                rusqlite::params![workspace_id, issue_id, resource.as_str(), attempted_at],
            )?;
            let attempts = conn.query_row(
                "SELECT attempt_count FROM issue_hydration_state
                 WHERE workspace_id=?1 AND issue_id=?2 AND resource=?3",
                rusqlite::params![workspace_id, issue_id, resource.as_str()],
                |row| row.get::<_, u32>(0),
            )?;
            Ok(attempts)
        })
    }

    pub fn requeue_issue_hydration(
        &self,
        workspace_id: &str,
        issue_id: &str,
        reason: &str,
    ) -> Result<()> {
        let issue = self
            .get_issue(issue_id)?
            .with_context(|| format!("issue '{issue_id}' not found"))?;
        self.ensure_hydration_state_for_issue(workspace_id, &issue, reason)?;
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE issue_hydration_state
                 SET status='pending', queue_reason=?3, next_retry_at=NULL,
                     last_error=NULL
                 WHERE workspace_id=?1 AND issue_id=?2",
                rusqlite::params![workspace_id, issue.id, reason],
            )?;
            Ok(())
        })
    }

    pub fn requeue_team_hydration(
        &self,
        workspace_id: &str,
        team_key: &str,
        reason: &str,
    ) -> Result<usize> {
        self.with_conn(|conn| {
            Ok(conn.execute(
                "UPDATE issue_hydration_state
                 SET status='pending', queue_reason=?3, next_retry_at=NULL,
                     last_error=NULL
                 WHERE workspace_id=?1 AND issue_id IN (
                    SELECT id FROM issues WHERE workspace_id=?1 AND team_key=?2
                 )",
                rusqlite::params![workspace_id, team_key, reason],
            )?)
        })
    }

    pub fn requeue_team_retryable_comments(
        &self,
        workspace_id: &str,
        team_key: &str,
    ) -> Result<usize> {
        self.with_conn(|conn| {
            Ok(conn.execute(
                "UPDATE issue_hydration_state
                 SET status='pending', queue_reason='legacy_retry', next_retry_at=NULL
                 WHERE workspace_id=?1 AND resource='comments'
                   AND status='retryable' AND issue_id IN (
                     SELECT id FROM issues WHERE workspace_id=?1 AND team_key=?2
                   )",
                rusqlite::params![workspace_id, team_key],
            )?)
        })
    }

    pub fn mark_hydration_complete(
        &self,
        workspace_id: &str,
        issue_id: &str,
        resource: HydrationResource,
        source_updated_at: &str,
        hydrated_at: &str,
    ) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE issue_hydration_state
                 SET status='hydrated', source_updated_at=?4, hydrated_at=?5,
                     next_retry_at=NULL, last_error=NULL, queue_reason='complete'
                 WHERE workspace_id=?1 AND issue_id=?2 AND resource=?3",
                rusqlite::params![
                    workspace_id,
                    issue_id,
                    resource.as_str(),
                    source_updated_at,
                    hydrated_at,
                ],
            )?;
            Ok(())
        })
    }

    pub fn mark_hydration_failed(
        &self,
        workspace_id: &str,
        issue_id: &str,
        resource: HydrationResource,
        status: HydrationStatus,
        next_retry_at: Option<&str>,
        error: &str,
    ) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE issue_hydration_state
                 SET status=?4, next_retry_at=?5, last_error=?6,
                     queue_reason=CASE WHEN ?4='retryable' THEN 'retry' ELSE queue_reason END
                 WHERE workspace_id=?1 AND issue_id=?2 AND resource=?3",
                rusqlite::params![
                    workspace_id,
                    issue_id,
                    resource.as_str(),
                    status.as_str(),
                    next_retry_at,
                    error,
                ],
            )?;
            Ok(())
        })
    }

    pub fn get_issue_hydration_state(
        &self,
        workspace_id: &str,
        issue_id: &str,
    ) -> Result<IssueHydrationState> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT resource, status, source_updated_at, last_attempted_at,
                        hydrated_at, attempt_count, next_retry_at, last_error
                 FROM issue_hydration_state
                 WHERE workspace_id=?1 AND issue_id=?2
                 ORDER BY CASE resource
                    WHEN 'details' THEN 1 WHEN 'labels' THEN 2
                    WHEN 'relations' THEN 3 ELSE 4 END",
            )?;
            let rows = stmt.query_map(rusqlite::params![workspace_id, issue_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, u32>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            })?;
            let resources = rows
                .map(|row| {
                    let row = row?;
                    Ok(HydrationResourceState {
                        resource: HydrationResource::parse(&row.0)?,
                        status: HydrationStatus::parse(&row.1)?,
                        source_updated_at: row.2,
                        last_attempted_at: row.3,
                        hydrated_at: row.4,
                        attempt_count: row.5,
                        next_retry_at: row.6,
                        last_error: row.7,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            if resources.is_empty() {
                anyhow::bail!("no hydration state for issue '{issue_id}'");
            }
            let status = aggregate_hydration_status(&resources);
            Ok(IssueHydrationState {
                issue_id: issue_id.to_string(),
                status,
                resources,
            })
        })
    }

    pub fn queue_stale_comment_hydration(
        &self,
        workspace_id: &str,
        team_key: &str,
        policy: HydrationPolicy,
        stale_before: &str,
        recent_after: &str,
    ) -> Result<usize> {
        self.with_conn(|conn| {
            let policy_filter = match policy {
                HydrationPolicy::OpenOnly => "i.state_type NOT IN ('completed', 'canceled')",
                HydrationPolicy::OpenAndRecent => {
                    "(i.state_type NOT IN ('completed', 'canceled') OR julianday(i.updated_at) >= julianday(?5))"
                }
                HydrationPolicy::All => "1=1",
            };
            let sql = format!(
                "UPDATE issue_hydration_state AS h
                 SET status='pending', queue_reason='comment_refresh'
                 WHERE h.workspace_id=?1 AND h.resource='comments'
                   AND h.status='hydrated'
                   AND julianday(h.hydrated_at) <= julianday(?3)
                   AND EXISTS (
                       SELECT 1 FROM issues i
                       WHERE i.id=h.issue_id AND i.workspace_id=?1
                         AND i.team_key=?2 AND {policy_filter}
                   )"
            );
            let changed = match policy {
                HydrationPolicy::OpenAndRecent => conn.execute(
                    &sql,
                    rusqlite::params![workspace_id, team_key, stale_before, 0, recent_after],
                )?,
                _ => conn.execute(
                    &sql,
                    rusqlite::params![workspace_id, team_key, stale_before],
                )?,
            };
            Ok(changed)
        })
    }

    pub fn list_hydration_candidates(
        &self,
        workspace_id: &str,
        team_key: &str,
        limit: usize,
        policy: HydrationPolicy,
        now: &str,
        recent_after: &str,
    ) -> Result<Vec<HydrationCandidate>> {
        self.with_conn(|conn| {
            let policy_filter = match policy {
                HydrationPolicy::OpenOnly => "i.state_type NOT IN ('completed', 'canceled')",
                HydrationPolicy::OpenAndRecent => {
                    "(i.state_type NOT IN ('completed', 'canceled') OR julianday(i.updated_at) >= julianday(?4))"
                }
                HydrationPolicy::All => "1=1",
            };
            let sql = format!(
                "SELECT i.id, i.identifier, i.updated_at, i.state_type
                 FROM issues i
                 WHERE i.workspace_id=?1 AND i.team_key=?2
                   AND {policy_filter}
                   AND EXISTS (
                       SELECT 1 FROM issue_hydration_state h
                       WHERE h.workspace_id=i.workspace_id AND h.issue_id=i.id
                         AND (h.status='pending' OR (
                             h.status='retryable' AND (
                               h.next_retry_at IS NULL OR julianday(h.next_retry_at) <= julianday(?3)
                             )
                         ))
                   )
                 ORDER BY
                   CASE
                     WHEN i.state_type NOT IN ('completed', 'canceled') AND EXISTS (
                       SELECT 1 FROM issue_hydration_state h
                       WHERE h.workspace_id=i.workspace_id AND h.issue_id=i.id
                         AND h.status='pending' AND h.queue_reason='index_changed'
                         AND h.index_sync_token=(
                           SELECT latest.sync_token FROM sync_family_state latest
                           WHERE latest.workspace_id=i.workspace_id
                             AND latest.team_key=i.team_key
                             AND latest.family='issue index'
                         )
                     ) THEN 2
                     WHEN EXISTS (
                       SELECT 1 FROM issue_hydration_state h
                       WHERE h.workspace_id=i.workspace_id AND h.issue_id=i.id
                         AND h.status='retryable'
                         AND (h.next_retry_at IS NULL OR julianday(h.next_retry_at) <= julianday(?3))
                     ) THEN 3
                     WHEN i.state_type NOT IN ('completed', 'canceled') THEN 4
                     WHEN julianday(i.updated_at) >= julianday(?4) THEN 5
                     ELSE 6
                   END,
                   julianday(i.updated_at) DESC, i.identifier ASC
                 LIMIT ?5"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(
                rusqlite::params![workspace_id, team_key, now, recent_after, limit as i64],
                |row| {
                    Ok(HydrationCandidate {
                        id: row.get(0)?,
                        identifier: row.get(1)?,
                        updated_at: row.get(2)?,
                        state_type: row.get(3)?,
                    })
                },
            )?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    pub fn ensure_hydration_state_for_issue(
        &self,
        workspace_id: &str,
        issue: &Issue,
        reason: &str,
    ) -> Result<()> {
        self.with_conn(|conn| {
            for resource in HYDRATION_RESOURCES {
                conn.execute(
                    "INSERT INTO issue_hydration_state (
                        workspace_id, issue_id, resource, status,
                        source_updated_at, queue_reason
                     ) VALUES (?1, ?2, ?3, 'pending', ?4, ?5)
                     ON CONFLICT(workspace_id, issue_id, resource) DO NOTHING",
                    rusqlite::params![
                        workspace_id,
                        issue.id,
                        resource.as_str(),
                        issue.updated_at,
                        reason,
                    ],
                )?;
            }
            Ok(())
        })
    }
}

fn aggregate_hydration_status(resources: &[HydrationResourceState]) -> HydrationStatus {
    if resources
        .iter()
        .all(|state| state.status == HydrationStatus::Hydrated)
    {
        return HydrationStatus::Hydrated;
    }
    if resources
        .iter()
        .any(|state| state.status == HydrationStatus::Running)
    {
        return HydrationStatus::Running;
    }
    if resources
        .iter()
        .any(|state| state.status == HydrationStatus::Pending)
    {
        return HydrationStatus::Pending;
    }
    if resources
        .iter()
        .any(|state| state.status == HydrationStatus::Retryable)
    {
        return HydrationStatus::Retryable;
    }
    let hydrated = resources
        .iter()
        .any(|state| state.status == HydrationStatus::Hydrated);
    if hydrated {
        HydrationStatus::Partial
    } else if resources
        .iter()
        .all(|state| state.status == HydrationStatus::PermissionDenied)
    {
        HydrationStatus::PermissionDenied
    } else {
        HydrationStatus::Unavailable
    }
}

pub fn recent_cutoff(now: DateTime<Utc>) -> String {
    (now - Duration::days(30)).to_rfc3339()
}

pub fn comment_refresh_cutoff(now: DateTime<Utc>) -> String {
    (now - Duration::minutes(15)).to_rfc3339()
}

trait OptionalRow<T> {
    fn optional(self) -> rusqlite::Result<Option<T>>;
}

impl<T> OptionalRow<T> for rusqlite::Result<T> {
    fn optional(self) -> rusqlite::Result<Option<T>> {
        match self {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

pub fn parse_timestamp(value: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .with_context(|| format!("invalid RFC3339 timestamp '{value}'"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_helpers::make_issue;

    #[test]
    fn index_upsert_preserves_hydrated_fields_and_requeues_on_change() {
        let (db, _dir) = crate::db::test_helpers::test_db();
        let mut rich = make_issue("CUT-1", "CUT");
        rich.id = "issue-1".into();
        rich.description = Some("hydrated description".into());
        rich.labels_json = r#"["bug"]"#.into();
        db.upsert_issue(&rich).unwrap();
        db.ensure_hydration_state_for_issue("default", &rich, "test")
            .unwrap();

        let outcome = db
            .upsert_issue_index(
                &IssueIndexEntry {
                    id: rich.id.clone(),
                    identifier: rich.identifier.clone(),
                    team_key: rich.team_key.clone(),
                    title: "new index title".into(),
                    state_name: "In Progress".into(),
                    state_type: "started".into(),
                    created_at: rich.created_at.clone(),
                    updated_at: "2026-01-03T00:00:00Z".into(),
                    archived_at: None,
                    url: rich.url.clone(),
                },
                "default",
                "run-1",
            )
            .unwrap();

        assert_eq!(outcome, IndexUpsertOutcome::Updated);
        let stored = db.get_issue("issue-1").unwrap().unwrap();
        assert_eq!(stored.title, "new index title");
        assert_eq!(stored.description.as_deref(), Some("hydrated description"));
        assert_eq!(stored.labels_json, r#"["bug"]"#);
        let state = db.get_issue_hydration_state("default", "issue-1").unwrap();
        assert!(state
            .resources
            .iter()
            .all(|resource| resource.status == HydrationStatus::Pending));
    }

    #[test]
    fn open_changed_issues_are_prioritized_over_old_completed_issues() {
        let (db, _dir) = crate::db::test_helpers::test_db();
        let mut completed = make_issue("CUT-1", "CUT");
        completed.id = "completed".into();
        completed.state_type = "completed".into();
        completed.updated_at = "2020-01-01T00:00:00Z".into();
        db.upsert_issue(&completed).unwrap();
        db.ensure_hydration_state_for_issue("default", &completed, "initial")
            .unwrap();

        let mut open = make_issue("CUT-2", "CUT");
        open.id = "open".into();
        open.updated_at = "2025-01-01T00:00:00Z".into();
        db.upsert_issue(&open).unwrap();
        db.ensure_hydration_state_for_issue("default", &open, "index_changed")
            .unwrap();

        let candidates = db
            .list_hydration_candidates(
                "default",
                "CUT",
                10,
                HydrationPolicy::All,
                "2026-01-01T00:00:00Z",
                "2025-12-01T00:00:00Z",
            )
            .unwrap();
        assert_eq!(candidates[0].id, "open");
        assert_eq!(candidates[1].id, "completed");
    }

    #[test]
    fn explicit_request_requeues_a_permanent_failure() {
        let (db, _dir) = crate::db::test_helpers::test_db();
        let mut issue = make_issue("CUT-1", "CUT");
        issue.id = "issue-1".into();
        db.upsert_issue(&issue).unwrap();
        db.ensure_hydration_state_for_issue("default", &issue, "initial")
            .unwrap();
        db.mark_hydration_failed(
            "default",
            "issue-1",
            HydrationResource::Comments,
            HydrationStatus::PermissionDenied,
            None,
            "forbidden",
        )
        .unwrap();

        db.requeue_issue_hydration("default", "issue-1", "explicit")
            .unwrap();
        let state = db.get_issue_hydration_state("default", "issue-1").unwrap();
        assert!(state
            .resources
            .iter()
            .all(|resource| resource.status == HydrationStatus::Pending));
    }

    #[test]
    fn retry_state_survives_reopening_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("retry.db");
        {
            let db = Database::open(&path).unwrap();
            let mut issue = make_issue("CUT-1", "CUT");
            issue.id = "issue-1".into();
            db.upsert_issue(&issue).unwrap();
            db.ensure_hydration_state_for_issue("default", &issue, "initial")
                .unwrap();
            db.mark_hydration_running(
                "default",
                "issue-1",
                HydrationResource::Relations,
                "2026-01-01T00:00:00Z",
            )
            .unwrap();
        }
        let reopened = Database::open(&path).unwrap();
        let state = reopened
            .get_issue_hydration_state("default", "issue-1")
            .unwrap();
        let relations = state
            .resources
            .iter()
            .find(|resource| resource.resource == HydrationResource::Relations)
            .unwrap();
        assert_eq!(relations.status, HydrationStatus::Retryable);
        assert_eq!(relations.attempt_count, 1);
        assert!(relations.next_retry_at.is_some());
    }

    #[test]
    fn migration_12_preserves_issues_comments_and_comment_sync_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("migration.db");
        {
            let db = Database::open(&path).unwrap();
            let mut issue = make_issue("CUT-1", "CUT");
            issue.id = "issue-1".into();
            db.upsert_issue(&issue).unwrap();
            db.replace_issue_comments(
                "issue-1",
                "default",
                &[crate::db::Comment {
                    id: "comment-1".into(),
                    issue_id: "issue-1".into(),
                    body: "preserved".into(),
                    user_name: None,
                    created_at: "2026-01-01T00:00:00Z".into(),
                    updated_at: None,
                    parent_id: None,
                    url: None,
                    workspace_id: "default".into(),
                }],
            )
            .unwrap();
            db.mark_comments_synced("issue-1", "default", 1).unwrap();
            db.set_sync_cursor("default", "CUT", "2026-01-02T00:00:00Z")
                .unwrap();
            db.with_conn(|conn| {
                conn.execute_batch(
                    "DROP TABLE issue_hydration_state;
                     DELETE FROM schema_version WHERE version = 12;",
                )?;
                Ok(())
            })
            .unwrap();
        }

        let migrated = Database::open(&path).unwrap();
        assert!(migrated.get_issue("CUT-1").unwrap().is_some());
        assert_eq!(migrated.get_comments("issue-1").unwrap().len(), 1);
        assert_eq!(
            migrated
                .get_issue_hydration_state("default", "issue-1")
                .unwrap()
                .resources
                .iter()
                .find(|resource| resource.resource == HydrationResource::Comments)
                .unwrap()
                .status,
            HydrationStatus::Hydrated
        );
        assert_eq!(
            migrated
                .get_synced_through_at("default", "CUT")
                .unwrap()
                .as_deref(),
            Some("2026-01-02T00:00:00Z")
        );
    }
}
