use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{Comment, Database, ProjectLabel, ProjectMember, ProjectTeam, Relation};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Cycle {
    pub id: String,
    pub workspace_id: String,
    pub team_id: String,
    pub team_key: String,
    pub number: i32,
    pub name: Option<String>,
    pub starts_at: Option<String>,
    pub ends_at: Option<String>,
    pub completed_at: Option<String>,
    pub archived_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueSyncRef {
    pub id: String,
    pub identifier: String,
}

struct SyncFamilyUpdate<'a> {
    workspace_id: &'a str,
    team_key: &'a str,
    family: &'a str,
    status: &'a str,
    cursor: Option<&'a str>,
    page_size: Option<usize>,
    sync_token: &'a str,
    error: Option<&'a str>,
}

impl Database {
    pub fn upsert_issue_label_page(
        &self,
        issue_id: &str,
        label_ids: &[String],
        sync_token: &str,
    ) -> Result<()> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "INSERT INTO issue_labels (issue_id, label_id, sync_token)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(issue_id, label_id) DO UPDATE SET
                    sync_token=excluded.sync_token",
            )?;
            for label_id in label_ids {
                let exists: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM labels WHERE id = ?1",
                    rusqlite::params![label_id],
                    |row| row.get(0),
                )?;
                if exists > 0 {
                    stmt.execute(rusqlite::params![issue_id, label_id, sync_token])?;
                }
            }
            Ok(())
        })
    }

    pub fn complete_issue_label_sync(&self, issue_id: &str, sync_token: &str) -> Result<usize> {
        self.with_conn(|conn| {
            Ok(conn.execute(
                "DELETE FROM issue_labels
                 WHERE issue_id = ?1 AND COALESCE(sync_token, '') <> ?2",
                rusqlite::params![issue_id, sync_token],
            )?)
        })
    }

    pub fn mark_project_sync_token(&self, project_id: &str, sync_token: &str) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE projects SET sync_token = ?2 WHERE id = ?1",
                rusqlite::params![project_id, sync_token],
            )?;
            Ok(())
        })
    }

    pub fn list_project_ids_for_sync_token(
        &self,
        workspace_id: &str,
        sync_token: &str,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<String>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id FROM projects
                 WHERE workspace_id = ?1 AND sync_token = ?2
                   AND (?3 IS NULL OR id > ?3)
                 ORDER BY id LIMIT ?4",
            )?;
            let rows = stmt.query_map(
                rusqlite::params![workspace_id, sync_token, after_id, limit as i64],
                |row| row.get::<_, String>(0),
            )?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    pub fn reconcile_workspace_projects(
        &self,
        workspace_id: &str,
        sync_token: &str,
    ) -> Result<usize> {
        self.with_conn(|conn| {
            Ok(conn.execute(
                "DELETE FROM projects
                 WHERE workspace_id = ?1 AND COALESCE(sync_token, '') <> ?2",
                rusqlite::params![workspace_id, sync_token],
            )?)
        })
    }

    pub fn reconcile_team_projects(
        &self,
        workspace_id: &str,
        team_key: &str,
        sync_token: &str,
    ) -> Result<usize> {
        self.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "DELETE FROM project_teams
                 WHERE team_key = ?1 AND COALESCE(sync_token, '') <> ?2",
                rusqlite::params![team_key, sync_token],
            )?;
            let changed = tx.execute(
                "DELETE FROM projects
                 WHERE workspace_id = ?1
                   AND NOT EXISTS (
                       SELECT 1 FROM project_teams pt WHERE pt.project_id = projects.id
                   )",
                rusqlite::params![workspace_id],
            )?;
            tx.commit()?;
            Ok(changed)
        })
    }

    pub fn upsert_project_team_page(
        &self,
        project_id: &str,
        teams: &[ProjectTeam],
        sync_token: &str,
    ) -> Result<()> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "INSERT INTO project_teams
                    (project_id, team_id, team_key, team_name, sync_token)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(project_id, team_id) DO UPDATE SET
                    team_key=excluded.team_key,
                    team_name=excluded.team_name,
                    sync_token=excluded.sync_token",
            )?;
            for team in teams {
                stmt.execute(rusqlite::params![
                    project_id, team.id, team.key, team.name, sync_token,
                ])?;
            }
            Ok(())
        })
    }

    pub fn complete_project_team_sync(&self, project_id: &str, sync_token: &str) -> Result<usize> {
        self.with_conn(|conn| {
            Ok(conn.execute(
                "DELETE FROM project_teams
                 WHERE project_id = ?1 AND COALESCE(sync_token, '') <> ?2",
                rusqlite::params![project_id, sync_token],
            )?)
        })
    }

    pub fn upsert_project_member_page(
        &self,
        project_id: &str,
        members: &[ProjectMember],
        sync_token: &str,
    ) -> Result<()> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "INSERT INTO project_members (project_id, user_id, user_name, sync_token)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(project_id, user_id) DO UPDATE SET
                    user_name=excluded.user_name,
                    sync_token=excluded.sync_token",
            )?;
            for member in members {
                stmt.execute(rusqlite::params![
                    project_id,
                    member.id,
                    member.name,
                    sync_token,
                ])?;
            }
            Ok(())
        })
    }

    pub fn complete_project_member_sync(
        &self,
        project_id: &str,
        sync_token: &str,
    ) -> Result<usize> {
        self.with_conn(|conn| {
            Ok(conn.execute(
                "DELETE FROM project_members
                 WHERE project_id = ?1 AND COALESCE(sync_token, '') <> ?2",
                rusqlite::params![project_id, sync_token],
            )?)
        })
    }

    pub fn upsert_project_label_page(
        &self,
        project_id: &str,
        labels: &[ProjectLabel],
        sync_token: &str,
    ) -> Result<()> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "INSERT INTO project_labels
                    (project_id, label_id, label_name, color, description, sync_token)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(project_id, label_id) DO UPDATE SET
                    label_name=excluded.label_name,
                    color=excluded.color,
                    description=excluded.description,
                    sync_token=excluded.sync_token",
            )?;
            for label in labels {
                stmt.execute(rusqlite::params![
                    project_id,
                    label.id,
                    label.name,
                    label.color,
                    label.description,
                    sync_token,
                ])?;
            }
            Ok(())
        })
    }

    pub fn complete_project_label_sync(&self, project_id: &str, sync_token: &str) -> Result<usize> {
        self.with_conn(|conn| {
            Ok(conn.execute(
                "DELETE FROM project_labels
                 WHERE project_id = ?1 AND COALESCE(sync_token, '') <> ?2",
                rusqlite::params![project_id, sync_token],
            )?)
        })
    }

    pub fn mark_project_milestone_sync_token(
        &self,
        milestone_id: &str,
        sync_token: &str,
    ) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE project_milestones SET sync_token = ?2 WHERE id = ?1",
                rusqlite::params![milestone_id, sync_token],
            )?;
            Ok(())
        })
    }

    pub fn reconcile_project_milestones_by_token(
        &self,
        project_id: &str,
        sync_token: &str,
    ) -> Result<usize> {
        self.with_conn(|conn| {
            Ok(conn.execute(
                "DELETE FROM project_milestones
                 WHERE project_id = ?1 AND COALESCE(sync_token, '') <> ?2",
                rusqlite::params![project_id, sync_token],
            )?)
        })
    }

    pub fn mark_label_sync_token(&self, label_id: &str, sync_token: &str) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE labels SET sync_token = ?2 WHERE id = ?1",
                rusqlite::params![label_id, sync_token],
            )?;
            Ok(())
        })
    }

    pub fn reconcile_label_sync(&self, workspace_id: &str, sync_token: &str) -> Result<usize> {
        self.with_conn(|conn| {
            Ok(conn.execute(
                "DELETE FROM labels
                 WHERE workspace_id = ?1 AND COALESCE(sync_token, '') <> ?2",
                rusqlite::params![workspace_id, sync_token],
            )?)
        })
    }

    pub fn mark_issue_sync_token(&self, issue_id: &str, sync_token: &str) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE issues SET sync_token = ?2 WHERE id = ?1",
                rusqlite::params![issue_id, sync_token],
            )?;
            Ok(())
        })
    }

    pub fn list_issue_sync_refs(
        &self,
        workspace_id: &str,
        team_key: &str,
        sync_token: &str,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<IssueSyncRef>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, identifier FROM issues
                 WHERE workspace_id = ?1 AND team_key = ?2 AND sync_token = ?3
                   AND (?4 IS NULL OR id > ?4)
                 ORDER BY id LIMIT ?5",
            )?;
            let rows = stmt.query_map(
                rusqlite::params![workspace_id, team_key, sync_token, after_id, limit as i64],
                |row| {
                    Ok(IssueSyncRef {
                        id: row.get(0)?,
                        identifier: row.get(1)?,
                    })
                },
            )?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    pub fn reconcile_full_issue_sync(
        &self,
        workspace_id: &str,
        team_key: &str,
        sync_token: &str,
    ) -> Result<usize> {
        self.with_conn(|conn| {
            Ok(conn.execute(
                "DELETE FROM issues
                 WHERE workspace_id = ?1 AND team_key = ?2
                   AND COALESCE(sync_token, '') <> ?3",
                rusqlite::params![workspace_id, team_key, sync_token],
            )?)
        })
    }

    pub fn upsert_relation_page(
        &self,
        issue_id: &str,
        relations: &[Relation],
        sync_token: &str,
    ) -> Result<()> {
        self.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            let mut stmt = tx.prepare(
                "INSERT INTO issue_relations
                    (id, issue_id, related_issue_id, related_issue_identifier, relation_type, sync_token)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(id) DO UPDATE SET
                    issue_id=excluded.issue_id,
                    related_issue_id=excluded.related_issue_id,
                    related_issue_identifier=excluded.related_issue_identifier,
                    relation_type=excluded.relation_type,
                    sync_token=excluded.sync_token",
            )?;
            for relation in relations {
                stmt.execute(rusqlite::params![
                    relation.id,
                    issue_id,
                    relation.related_issue_id,
                    relation.related_issue_identifier,
                    relation.relation_type,
                    sync_token,
                ])?;
            }
            drop(stmt);
            tx.commit()?;
            Ok(())
        })
    }

    pub fn complete_relation_sync(&self, issue_id: &str, sync_token: &str) -> Result<usize> {
        self.with_conn(|conn| {
            Ok(conn.execute(
                "DELETE FROM issue_relations
                 WHERE issue_id = ?1 AND COALESCE(sync_token, '') <> ?2",
                rusqlite::params![issue_id, sync_token],
            )?)
        })
    }

    pub fn upsert_comment_page(
        &self,
        issue_id: &str,
        workspace_id: &str,
        comments: &[Comment],
        sync_token: &str,
    ) -> Result<()> {
        self.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            let mut stmt = tx.prepare(
                "INSERT INTO comments
                    (id, issue_id, body, user_name, created_at, workspace_id,
                     updated_at, parent_id, url, sync_token)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(id) DO UPDATE SET
                    issue_id=excluded.issue_id,
                    body=excluded.body,
                    user_name=excluded.user_name,
                    created_at=excluded.created_at,
                    workspace_id=excluded.workspace_id,
                    updated_at=excluded.updated_at,
                    parent_id=excluded.parent_id,
                    url=excluded.url,
                    sync_token=excluded.sync_token",
            )?;
            for comment in comments {
                stmt.execute(rusqlite::params![
                    comment.id,
                    issue_id,
                    comment.body,
                    comment.user_name,
                    comment.created_at,
                    workspace_id,
                    comment.updated_at,
                    comment.parent_id,
                    comment.url,
                    sync_token,
                ])?;
            }
            drop(stmt);
            tx.commit()?;
            Ok(())
        })
    }

    pub fn complete_comment_sync(
        &self,
        issue_id: &str,
        workspace_id: &str,
        sync_token: &str,
    ) -> Result<usize> {
        self.with_conn(|conn| {
            Ok(conn.execute(
                "DELETE FROM comments
                 WHERE issue_id = ?1 AND workspace_id = ?2
                   AND COALESCE(sync_token, '') <> ?3",
                rusqlite::params![issue_id, workspace_id, sync_token],
            )?)
        })
    }

    pub fn upsert_cycle(&self, cycle: &Cycle, sync_token: &str) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO cycles (
                    id, workspace_id, team_id, team_key, number, name, starts_at,
                    ends_at, completed_at, archived_at, created_at, updated_at,
                    sync_token, synced_at
                 ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                    datetime('now')
                 ) ON CONFLICT(id) DO UPDATE SET
                    workspace_id=excluded.workspace_id,
                    team_id=excluded.team_id,
                    team_key=excluded.team_key,
                    number=excluded.number,
                    name=excluded.name,
                    starts_at=excluded.starts_at,
                    ends_at=excluded.ends_at,
                    completed_at=excluded.completed_at,
                    archived_at=excluded.archived_at,
                    created_at=excluded.created_at,
                    updated_at=excluded.updated_at,
                    sync_token=excluded.sync_token,
                    synced_at=datetime('now')",
                rusqlite::params![
                    cycle.id,
                    cycle.workspace_id,
                    cycle.team_id,
                    cycle.team_key,
                    cycle.number,
                    cycle.name,
                    cycle.starts_at,
                    cycle.ends_at,
                    cycle.completed_at,
                    cycle.archived_at,
                    cycle.created_at,
                    cycle.updated_at,
                    sync_token,
                ],
            )?;
            Ok(())
        })
    }

    pub fn reconcile_cycles(
        &self,
        workspace_id: &str,
        team_key: &str,
        sync_token: &str,
    ) -> Result<usize> {
        self.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "UPDATE issues SET cycle_id = NULL, cycle_name = NULL
                 WHERE workspace_id = ?1 AND cycle_id IN (
                    SELECT id FROM cycles
                    WHERE workspace_id = ?1 AND team_key = ?2
                      AND COALESCE(sync_token, '') <> ?3
                 )",
                rusqlite::params![workspace_id, team_key, sync_token],
            )?;
            let changed = tx.execute(
                "DELETE FROM cycles
                 WHERE workspace_id = ?1 AND team_key = ?2
                   AND COALESCE(sync_token, '') <> ?3",
                rusqlite::params![workspace_id, team_key, sync_token],
            )?;
            tx.commit()?;
            Ok(changed)
        })
    }

    pub fn mark_sync_family_running(
        &self,
        workspace_id: &str,
        team_key: &str,
        family: &str,
        cursor: Option<&str>,
        page_size: Option<usize>,
        sync_token: &str,
    ) -> Result<()> {
        self.set_sync_family_state(SyncFamilyUpdate {
            workspace_id,
            team_key,
            family,
            status: "running",
            cursor,
            page_size,
            sync_token,
            error: None,
        })
    }

    pub fn mark_sync_family_complete(
        &self,
        workspace_id: &str,
        team_key: &str,
        family: &str,
        page_size: Option<usize>,
        sync_token: &str,
    ) -> Result<()> {
        self.set_sync_family_state(SyncFamilyUpdate {
            workspace_id,
            team_key,
            family,
            status: "complete",
            cursor: None,
            page_size,
            sync_token,
            error: None,
        })
    }

    pub fn mark_sync_family_failed(
        &self,
        workspace_id: &str,
        team_key: &str,
        family: &str,
        sync_token: &str,
        error: &str,
    ) -> Result<()> {
        self.set_sync_family_state(SyncFamilyUpdate {
            workspace_id,
            team_key,
            family,
            status: "failed",
            cursor: None,
            page_size: None,
            sync_token,
            error: Some(error),
        })
    }

    fn set_sync_family_state(&self, state: SyncFamilyUpdate<'_>) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO sync_family_state (
                    workspace_id, team_key, family, status, cursor, page_size,
                    sync_token, error, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, datetime('now'))
                 ON CONFLICT(workspace_id, team_key, family) DO UPDATE SET
                    status=excluded.status,
                    cursor=excluded.cursor,
                    page_size=excluded.page_size,
                    sync_token=excluded.sync_token,
                    error=excluded.error,
                    updated_at=datetime('now')",
                rusqlite::params![
                    state.workspace_id,
                    state.team_key,
                    state.family,
                    state.status,
                    state.cursor,
                    state.page_size.map(|value| value as i64),
                    state.sync_token,
                    state.error,
                ],
            )?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_helpers::{make_issue, test_db};

    #[test]
    fn page_tokens_preserve_old_comments_until_completion() {
        let (db, _dir) = test_db();
        let issue = make_issue("ENG-1", "ENG");
        db.upsert_issue(&issue).unwrap();
        let old = Comment {
            id: "old".into(),
            issue_id: issue.id.clone(),
            body: "old body".into(),
            user_name: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: None,
            parent_id: None,
            url: None,
            workspace_id: "default".into(),
        };
        db.replace_issue_comments(&issue.id, "default", &[old])
            .unwrap();
        let new = Comment {
            id: "new".into(),
            issue_id: issue.id.clone(),
            body: "new body".into(),
            user_name: None,
            created_at: "2026-01-02T00:00:00Z".into(),
            updated_at: None,
            parent_id: None,
            url: None,
            workspace_id: "default".into(),
        };
        db.upsert_comment_page(&issue.id, "default", &[new], "run-1")
            .unwrap();
        assert_eq!(db.get_comments(&issue.id).unwrap().len(), 2);
        db.complete_comment_sync(&issue.id, "default", "run-1")
            .unwrap();
        let comments = db.get_comments(&issue.id).unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].id, "new");
    }

    #[test]
    fn restarting_after_partial_persistence_is_idempotent() {
        let (db, _dir) = test_db();
        let issue = make_issue("ENG-2", "ENG");
        db.upsert_issue(&issue).unwrap();
        let comment = |id: &str| Comment {
            id: id.into(),
            issue_id: issue.id.clone(),
            body: id.into(),
            user_name: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: None,
            parent_id: None,
            url: None,
            workspace_id: "default".into(),
        };

        db.upsert_comment_page(&issue.id, "default", &[comment("one")], "interrupted")
            .unwrap();
        db.upsert_comment_page(
            &issue.id,
            "default",
            &[comment("one"), comment("two")],
            "resumed",
        )
        .unwrap();
        db.complete_comment_sync(&issue.id, "default", "resumed")
            .unwrap();

        let comments = db.get_comments(&issue.id).unwrap();
        assert_eq!(
            comments
                .iter()
                .map(|comment| comment.id.as_str())
                .collect::<Vec<_>>(),
            ["one", "two"]
        );
    }

    #[test]
    fn cycle_membership_round_trips_and_reconciles_after_complete_sync() {
        let (db, _dir) = test_db();
        let cycle = Cycle {
            id: "cycle-1".into(),
            workspace_id: "default".into(),
            team_id: "team-1".into(),
            team_key: "ENG".into(),
            number: 42,
            name: Some("Launch".into()),
            starts_at: Some("2026-01-01T00:00:00Z".into()),
            ends_at: Some("2026-01-14T00:00:00Z".into()),
            completed_at: None,
            archived_at: Some("2026-02-01T00:00:00Z".into()),
            created_at: "2025-12-01T00:00:00Z".into(),
            updated_at: "2026-02-01T00:00:00Z".into(),
        };
        db.upsert_cycle(&cycle, "complete-run").unwrap();
        let mut issue = make_issue("ENG-3", "ENG");
        issue.cycle_id = Some(cycle.id.clone());
        issue.cycle_name = cycle.name.clone();
        db.upsert_issue(&issue).unwrap();

        let stored = db.get_issue(&issue.id).unwrap().unwrap();
        assert_eq!(stored.cycle_id.as_deref(), Some("cycle-1"));
        assert_eq!(stored.cycle_name.as_deref(), Some("Launch"));

        db.reconcile_cycles("default", "ENG", "next-complete-run")
            .unwrap();
        let stored = db.get_issue(&issue.id).unwrap().unwrap();
        assert!(stored.cycle_id.is_none());
        assert!(stored.cycle_name.is_none());
    }

    #[test]
    fn migration_11_forces_exactly_one_membership_refresh() {
        let (db, _dir) = test_db();
        db.set_sync_cursor("default", "ENG", "2026-01-01T00:00:00Z")
            .unwrap();
        db.with_conn(|conn| {
            conn.execute("DELETE FROM schema_version WHERE version = 11", [])?;
            crate::db::schema::run_migrations(conn)
        })
        .unwrap();
        assert!(!db.is_full_sync_done("default", "ENG").unwrap());

        db.set_sync_cursor("default", "ENG", "2026-01-02T00:00:00Z")
            .unwrap();
        db.with_conn(crate::db::schema::run_migrations).unwrap();
        assert!(db.is_full_sync_done("default", "ENG").unwrap());
    }
}
