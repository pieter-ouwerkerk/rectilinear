//! Offline sync-status reporting: queue depths and retry timing read
//! directly from the database, usable while a sync runs elsewhere.

use anyhow::Result;
use serde::Serialize;

use super::Database;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ResourceQueueStatus {
    pub resource: String,
    pub pending: u64,
    pub running: u64,
    pub hydrated: u64,
    pub partial: u64,
    pub retryable: u64,
    pub permission_denied: u64,
    pub unavailable: u64,
}

impl ResourceQueueStatus {
    fn empty(resource: String) -> Self {
        Self {
            resource,
            pending: 0,
            running: 0,
            hydrated: 0,
            partial: 0,
            retryable: 0,
            permission_denied: 0,
            unavailable: 0,
        }
    }

    pub fn total(&self) -> u64 {
        self.pending
            + self.running
            + self.hydrated
            + self.partial
            + self.retryable
            + self.permission_denied
            + self.unavailable
    }

    pub fn outstanding(&self) -> u64 {
        self.pending + self.running + self.retryable
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TeamSyncStatus {
    pub team_key: String,
    pub issue_count: u64,
    pub last_synced_at: Option<String>,
    /// Earliest scheduled retry among retryable resources, if any.
    pub next_retry_at: Option<String>,
    pub resources: Vec<ResourceQueueStatus>,
}

impl Database {
    /// Report hydration queue depths per team, grouped by resource.
    /// Pass a team key to restrict the report to that team.
    pub fn sync_status(
        &self,
        workspace_id: &str,
        team_key: Option<&str>,
    ) -> Result<Vec<TeamSyncStatus>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT i.team_key, h.resource, h.status, COUNT(*)
                 FROM issue_hydration_state h
                 JOIN issues i ON i.workspace_id = h.workspace_id AND i.id = h.issue_id
                 WHERE h.workspace_id = ?1 AND (?2 IS NULL OR i.team_key = ?2)
                 GROUP BY i.team_key, h.resource, h.status
                 ORDER BY i.team_key",
            )?;
            let rows = stmt.query_map(rusqlite::params![workspace_id, team_key], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, u64>(3)?,
                ))
            })?;

            let mut teams: Vec<TeamSyncStatus> = Vec::new();
            for row in rows {
                let (team, resource, status, count) = row?;
                if teams.last().is_none_or(|t| t.team_key != team) {
                    teams.push(TeamSyncStatus {
                        team_key: team,
                        issue_count: 0,
                        last_synced_at: None,
                        next_retry_at: None,
                        resources: Vec::new(),
                    });
                }
                let team_entry = teams.last_mut().unwrap();
                let entry = match team_entry
                    .resources
                    .iter_mut()
                    .find(|r| r.resource == resource)
                {
                    Some(entry) => entry,
                    None => {
                        team_entry
                            .resources
                            .push(ResourceQueueStatus::empty(resource));
                        team_entry.resources.last_mut().unwrap()
                    }
                };
                match status.as_str() {
                    "pending" => entry.pending += count,
                    "running" => entry.running += count,
                    "hydrated" => entry.hydrated += count,
                    "partial" => entry.partial += count,
                    "retryable" => entry.retryable += count,
                    "permission_denied" => entry.permission_denied += count,
                    "unavailable" => entry.unavailable += count,
                    other => anyhow::bail!("unknown hydration status '{other}'"),
                }
            }

            for team in &mut teams {
                team.resources.sort_by(|a, b| a.resource.cmp(&b.resource));
                team.issue_count = conn.query_row(
                    "SELECT COUNT(*) FROM issues WHERE workspace_id = ?1 AND team_key = ?2",
                    rusqlite::params![workspace_id, team.team_key],
                    |row| row.get(0),
                )?;
                team.last_synced_at = conn
                    .query_row(
                        "SELECT last_synced_at FROM sync_state
                         WHERE workspace_id = ?1 AND team_key = ?2",
                        rusqlite::params![workspace_id, team.team_key],
                        |row| row.get(0),
                    )
                    .unwrap_or(None);
                team.next_retry_at = conn.query_row(
                    "SELECT MIN(h.next_retry_at) FROM issue_hydration_state h
                     JOIN issues i ON i.workspace_id = h.workspace_id AND i.id = h.issue_id
                     WHERE h.workspace_id = ?1 AND i.team_key = ?2
                       AND h.status = 'retryable' AND h.next_retry_at IS NOT NULL",
                    rusqlite::params![workspace_id, team.team_key],
                    |row| row.get(0),
                )?;
            }
            Ok(teams)
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::db::test_helpers::{make_issue, test_db};
    use crate::db::{HydrationResource, HydrationStatus};

    #[test]
    fn sync_status_reports_queue_depths_per_team_and_resource() {
        let (db, _dir) = test_db();

        let a1 = make_issue("ENG-1", "ENG");
        let a2 = make_issue("ENG-2", "ENG");
        let b1 = make_issue("OPS-1", "OPS");
        for issue in [&a1, &a2, &b1] {
            db.upsert_issue(issue).unwrap();
            db.ensure_hydration_state_for_issue("default", issue, "initial")
                .unwrap();
        }

        db.mark_hydration_complete(
            "default",
            &a1.id,
            HydrationResource::Relations,
            "2026-08-01T00:00:00Z",
            "2026-08-02T00:00:00Z",
        )
        .unwrap();
        db.mark_hydration_failed(
            "default",
            &a1.id,
            HydrationResource::Comments,
            HydrationStatus::Retryable,
            Some("2026-08-21T10:00:00.000Z"),
            "rate limited",
        )
        .unwrap();
        db.mark_hydration_failed(
            "default",
            &a2.id,
            HydrationResource::Comments,
            HydrationStatus::Retryable,
            Some("2026-08-21T09:00:00.000Z"),
            "rate limited",
        )
        .unwrap();
        db.mark_hydration_failed(
            "default",
            &a2.id,
            HydrationResource::Details,
            HydrationStatus::Unavailable,
            None,
            "gone",
        )
        .unwrap();

        let teams = db.sync_status("default", Some("ENG")).unwrap();
        assert_eq!(teams.len(), 1);
        let eng = &teams[0];
        assert_eq!(eng.team_key, "ENG");
        assert_eq!(eng.issue_count, 2);
        // Earliest retry wins across the team.
        assert_eq!(
            eng.next_retry_at.as_deref(),
            Some("2026-08-21T09:00:00.000Z")
        );

        let relations = eng
            .resources
            .iter()
            .find(|r| r.resource == "relations")
            .unwrap();
        assert_eq!(relations.hydrated, 1);
        assert_eq!(relations.pending, 1);
        assert_eq!(relations.total(), 2);
        assert_eq!(relations.outstanding(), 1);

        let comments = eng
            .resources
            .iter()
            .find(|r| r.resource == "comments")
            .unwrap();
        assert_eq!(comments.retryable, 2);
        assert_eq!(comments.pending, 0);

        let details = eng
            .resources
            .iter()
            .find(|r| r.resource == "details")
            .unwrap();
        assert_eq!(details.unavailable, 1);
        assert_eq!(details.pending, 1);
    }

    #[test]
    fn sync_status_without_team_filter_lists_all_teams() {
        let (db, _dir) = test_db();
        let a = make_issue("ENG-1", "ENG");
        let b = make_issue("OPS-1", "OPS");
        for issue in [&a, &b] {
            db.upsert_issue(issue).unwrap();
            db.ensure_hydration_state_for_issue("default", issue, "initial")
                .unwrap();
        }
        db.set_sync_cursor("default", "ENG", "2026-08-20T00:00:00Z")
            .unwrap();

        let teams = db.sync_status("default", None).unwrap();
        let keys: Vec<&str> = teams.iter().map(|t| t.team_key.as_str()).collect();
        assert_eq!(keys, ["ENG", "OPS"]);
        assert!(teams[0].last_synced_at.is_some());
        assert!(teams[1].last_synced_at.is_none());
        assert!(teams[0].next_retry_at.is_none());
    }

    #[test]
    fn sync_status_is_empty_when_no_hydration_state_exists() {
        let (db, _dir) = test_db();
        let teams = db.sync_status("default", None).unwrap();
        assert!(teams.is_empty());
    }
}
