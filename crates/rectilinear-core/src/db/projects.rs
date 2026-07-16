use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{Database, Issue};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectTeam {
    pub id: String,
    pub key: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectMember {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectLabel {
    pub id: String,
    pub name: String,
    pub color: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Project {
    pub id: String,
    pub workspace_id: String,
    pub slug_id: String,
    pub name: String,
    pub description: String,
    pub content: Option<String>,
    pub icon: Option<String>,
    pub color: String,
    pub status_id: String,
    pub status_name: String,
    pub status_type: String,
    pub status_color: String,
    pub priority: i32,
    pub start_date: Option<String>,
    pub target_date: Option<String>,
    pub lead_id: Option<String>,
    pub lead_name: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub archived_at: Option<String>,
    pub url: String,
    pub progress: f64,
    pub synced_at: Option<String>,
    pub teams: Vec<ProjectTeam>,
    pub members: Vec<ProjectMember>,
    pub labels: Vec<ProjectLabel>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectMilestone {
    pub id: String,
    pub workspace_id: String,
    pub project_id: String,
    pub project_name: String,
    pub name: String,
    pub description: Option<String>,
    pub target_date: Option<String>,
    pub status: String,
    pub progress: f64,
    pub sort_order: f64,
    pub created_at: String,
    pub updated_at: String,
    pub archived_at: Option<String>,
    pub synced_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectBundle {
    pub project: Project,
    pub milestones: Vec<ProjectMilestone>,
    pub issues: Vec<Issue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMilestoneBundle {
    pub project: Project,
    pub milestone: ProjectMilestone,
    pub issues: Vec<Issue>,
}

const PROJECT_COLUMNS: &str =
    "id, workspace_id, slug_id, name, description, content, icon, color, \
     status_id, status_name, status_type, status_color, priority, start_date, \
     target_date, lead_id, lead_name, created_at, updated_at, archived_at, url, \
     progress, synced_at";

const ISSUE_COLUMNS: &str =
    "id, identifier, team_key, title, description, state_name, state_type, \
     priority, assignee_name, project_name, labels_json, created_at, updated_at, \
     content_hash, synced_at, url, branch_name, workspace_id, project_id, \
     project_milestone_id, project_milestone_name";

impl Database {
    pub fn upsert_project(&self, project: &Project) -> Result<()> {
        self.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "INSERT OR IGNORE INTO workspaces (id) VALUES (?1)",
                rusqlite::params![project.workspace_id],
            )?;
            tx.execute(
                "INSERT INTO projects (
                    id, workspace_id, slug_id, name, description, content, icon, color,
                    status_id, status_name, status_type, status_color, priority,
                    start_date, target_date, lead_id, lead_name, created_at, updated_at,
                    archived_at, url, progress, synced_at
                 ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                    ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, datetime('now')
                 ) ON CONFLICT(id) DO UPDATE SET
                    workspace_id=excluded.workspace_id, slug_id=excluded.slug_id,
                    name=excluded.name, description=excluded.description,
                    content=excluded.content, icon=excluded.icon, color=excluded.color,
                    status_id=excluded.status_id, status_name=excluded.status_name,
                    status_type=excluded.status_type, status_color=excluded.status_color,
                    priority=excluded.priority, start_date=excluded.start_date,
                    target_date=excluded.target_date, lead_id=excluded.lead_id,
                    lead_name=excluded.lead_name, created_at=excluded.created_at,
                    updated_at=excluded.updated_at, archived_at=excluded.archived_at,
                    url=excluded.url, progress=excluded.progress, synced_at=datetime('now')",
                rusqlite::params![
                    project.id,
                    project.workspace_id,
                    project.slug_id,
                    project.name,
                    project.description,
                    project.content,
                    project.icon,
                    project.color,
                    project.status_id,
                    project.status_name,
                    project.status_type,
                    project.status_color,
                    project.priority,
                    project.start_date,
                    project.target_date,
                    project.lead_id,
                    project.lead_name,
                    project.created_at,
                    project.updated_at,
                    project.archived_at,
                    project.url,
                    project.progress,
                ],
            )?;

            tx.execute(
                "DELETE FROM project_teams WHERE project_id = ?1",
                rusqlite::params![project.id],
            )?;
            for team in &project.teams {
                tx.execute(
                    "INSERT INTO project_teams (project_id, team_id, team_key, team_name)
                     VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![project.id, team.id, team.key, team.name],
                )?;
            }

            tx.execute(
                "DELETE FROM project_members WHERE project_id = ?1",
                rusqlite::params![project.id],
            )?;
            for member in &project.members {
                tx.execute(
                    "INSERT INTO project_members (project_id, user_id, user_name)
                     VALUES (?1, ?2, ?3)",
                    rusqlite::params![project.id, member.id, member.name],
                )?;
            }

            tx.execute(
                "DELETE FROM project_labels WHERE project_id = ?1",
                rusqlite::params![project.id],
            )?;
            for label in &project.labels {
                tx.execute(
                    "INSERT INTO project_labels (
                        project_id, label_id, label_name, color, description
                     ) VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![
                        project.id,
                        label.id,
                        label.name,
                        label.color,
                        label.description,
                    ],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
    }

    pub fn upsert_project_milestone(&self, milestone: &ProjectMilestone) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO project_milestones (
                    id, workspace_id, project_id, name, description, target_date,
                    status, progress, sort_order, created_at, updated_at, archived_at, synced_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, datetime('now'))
                 ON CONFLICT(id) DO UPDATE SET
                    workspace_id=excluded.workspace_id, project_id=excluded.project_id,
                    name=excluded.name, description=excluded.description,
                    target_date=excluded.target_date, status=excluded.status,
                    progress=excluded.progress, sort_order=excluded.sort_order,
                    created_at=excluded.created_at, updated_at=excluded.updated_at,
                    archived_at=excluded.archived_at, synced_at=datetime('now')",
                rusqlite::params![
                    milestone.id,
                    milestone.workspace_id,
                    milestone.project_id,
                    milestone.name,
                    milestone.description,
                    milestone.target_date,
                    milestone.status,
                    milestone.progress,
                    milestone.sort_order,
                    milestone.created_at,
                    milestone.updated_at,
                    milestone.archived_at,
                ],
            )?;
            Ok(())
        })
    }

    pub fn get_project(&self, workspace_id: &str, id_or_name: &str) -> Result<Option<Project>> {
        self.with_conn(|conn| {
            let sql = format!(
                "SELECT {PROJECT_COLUMNS} FROM projects
                 WHERE workspace_id = ?1 AND (id = ?2 OR slug_id = ?2 OR name = ?2 COLLATE NOCASE)
                 ORDER BY CASE WHEN id = ?2 OR slug_id = ?2 THEN 0 ELSE 1 END, updated_at DESC
                 LIMIT 1"
            );
            let mut stmt = conn.prepare(&sql)?;
            let mut rows = stmt.query(rusqlite::params![workspace_id, id_or_name])?;
            Ok(rows
                .next()?
                .map(|row| project_from_row(conn, row))
                .transpose()?)
        })
    }

    pub fn list_projects(
        &self,
        workspace_id: &str,
        include_archived: bool,
    ) -> Result<Vec<Project>> {
        self.with_conn(|conn| {
            let archive_filter = if include_archived {
                ""
            } else {
                "AND archived_at IS NULL"
            };
            let sql = format!(
                "SELECT {PROJECT_COLUMNS} FROM projects
                 WHERE workspace_id = ?1 {archive_filter}
                 ORDER BY updated_at DESC, name COLLATE NOCASE"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params![workspace_id], |row| {
                project_from_row(conn, row)
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    pub fn get_project_milestone(
        &self,
        workspace_id: &str,
        id_or_name: &str,
        project_id: Option<&str>,
    ) -> Result<Option<ProjectMilestone>> {
        self.with_conn(|conn| {
            let project_filter = if project_id.is_some() {
                "AND pm.project_id = ?3"
            } else {
                ""
            };
            let sql = format!(
                "SELECT pm.id, pm.workspace_id, pm.project_id, p.name, pm.name,
                        pm.description, pm.target_date, pm.status, pm.progress,
                        pm.sort_order, pm.created_at, pm.updated_at, pm.archived_at, pm.synced_at
                 FROM project_milestones pm
                 JOIN projects p ON p.id = pm.project_id
                 WHERE pm.workspace_id = ?1
                   AND (pm.id = ?2 OR pm.name = ?2 COLLATE NOCASE)
                   {project_filter}
                 ORDER BY CASE WHEN pm.id = ?2 THEN 0 ELSE 1 END, pm.updated_at DESC
                 LIMIT 1"
            );
            let mut stmt = conn.prepare(&sql)?;
            let mut rows = if let Some(pid) = project_id {
                stmt.query(rusqlite::params![workspace_id, id_or_name, pid])?
            } else {
                stmt.query(rusqlite::params![workspace_id, id_or_name])?
            };
            Ok(rows.next()?.map(milestone_from_row).transpose()?)
        })
    }

    pub fn list_project_milestones(&self, project_id: &str) -> Result<Vec<ProjectMilestone>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT pm.id, pm.workspace_id, pm.project_id, p.name, pm.name,
                        pm.description, pm.target_date, pm.status, pm.progress,
                        pm.sort_order, pm.created_at, pm.updated_at, pm.archived_at, pm.synced_at
                 FROM project_milestones pm
                 JOIN projects p ON p.id = pm.project_id
                 WHERE pm.project_id = ?1
                 ORDER BY pm.sort_order, pm.target_date, pm.created_at",
            )?;
            let rows = stmt.query_map(rusqlite::params![project_id], milestone_from_row)?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    pub fn get_project_bundle(
        &self,
        workspace_id: &str,
        id_or_name: &str,
    ) -> Result<Option<ProjectBundle>> {
        let Some(project) = self.get_project(workspace_id, id_or_name)? else {
            return Ok(None);
        };
        let milestones = self.list_project_milestones(&project.id)?;
        let issues = self.list_project_issues(workspace_id, &project.id)?;
        Ok(Some(ProjectBundle {
            project,
            milestones,
            issues,
        }))
    }

    pub fn get_project_milestone_bundle(
        &self,
        workspace_id: &str,
        id_or_name: &str,
        project_id: Option<&str>,
    ) -> Result<Option<ProjectMilestoneBundle>> {
        let Some(milestone) = self.get_project_milestone(workspace_id, id_or_name, project_id)?
        else {
            return Ok(None);
        };
        let Some(project) = self.get_project(workspace_id, &milestone.project_id)? else {
            return Ok(None);
        };
        let issues = self.list_project_milestone_issues(workspace_id, &milestone.id)?;
        Ok(Some(ProjectMilestoneBundle {
            project,
            milestone,
            issues,
        }))
    }

    pub fn list_project_issues(&self, workspace_id: &str, project_id: &str) -> Result<Vec<Issue>> {
        self.list_issues_for_hierarchy(workspace_id, "project_id", project_id)
    }

    pub fn list_project_milestone_issues(
        &self,
        workspace_id: &str,
        milestone_id: &str,
    ) -> Result<Vec<Issue>> {
        self.list_issues_for_hierarchy(workspace_id, "project_milestone_id", milestone_id)
    }

    pub fn reconcile_project_issue_membership(
        &self,
        workspace_id: &str,
        project_id: &str,
        keep_issue_ids: &[String],
    ) -> Result<usize> {
        self.clear_issue_membership_not_in(
            workspace_id,
            "project_id",
            project_id,
            "project_id = NULL, project_name = NULL, project_milestone_id = NULL, \
             project_milestone_name = NULL",
            keep_issue_ids,
        )
    }

    pub fn reconcile_project_milestone_issue_membership(
        &self,
        workspace_id: &str,
        milestone_id: &str,
        keep_issue_ids: &[String],
    ) -> Result<usize> {
        self.clear_issue_membership_not_in(
            workspace_id,
            "project_milestone_id",
            milestone_id,
            "project_milestone_id = NULL, project_milestone_name = NULL",
            keep_issue_ids,
        )
    }

    fn list_issues_for_hierarchy(
        &self,
        workspace_id: &str,
        column: &str,
        value: &str,
    ) -> Result<Vec<Issue>> {
        debug_assert!(matches!(column, "project_id" | "project_milestone_id"));
        self.with_conn(|conn| {
            let sql = format!(
                "SELECT {ISSUE_COLUMNS} FROM issues
                 WHERE workspace_id = ?1 AND {column} = ?2
                 ORDER BY team_key, identifier"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params![workspace_id, value], Issue::from_row)?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    fn clear_issue_membership_not_in(
        &self,
        workspace_id: &str,
        hierarchy_column: &str,
        hierarchy_id: &str,
        assignments: &str,
        keep_issue_ids: &[String],
    ) -> Result<usize> {
        debug_assert!(matches!(
            hierarchy_column,
            "project_id" | "project_milestone_id"
        ));
        self.with_conn(|conn| {
            let exclusion = if keep_issue_ids.is_empty() {
                String::new()
            } else {
                let placeholders = (0..keep_issue_ids.len())
                    .map(|index| format!("?{}", index + 3))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("AND id NOT IN ({placeholders})")
            };
            let sql = format!(
                "UPDATE issues SET {assignments}, synced_at = datetime('now')
                 WHERE workspace_id = ?1 AND {hierarchy_column} = ?2 {exclusion}"
            );
            let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![
                Box::new(workspace_id.to_string()),
                Box::new(hierarchy_id.to_string()),
            ];
            params.extend(
                keep_issue_ids
                    .iter()
                    .cloned()
                    .map(|id| Box::new(id) as Box<dyn rusqlite::types::ToSql>),
            );
            let refs = params
                .iter()
                .map(|value| value.as_ref())
                .collect::<Vec<_>>();
            Ok(conn.execute(&sql, refs.as_slice())?)
        })
    }

    pub fn delete_project_local(&self, project_id: &str) -> Result<usize> {
        self.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "UPDATE issues SET project_id = NULL, project_name = NULL,
                        project_milestone_id = NULL, project_milestone_name = NULL,
                        synced_at = datetime('now')
                 WHERE project_id = ?1",
                rusqlite::params![project_id],
            )?;
            let changed = tx.execute(
                "DELETE FROM projects WHERE id = ?1",
                rusqlite::params![project_id],
            )?;
            tx.commit()?;
            Ok(changed)
        })
    }

    pub fn delete_project_milestone_local(&self, milestone_id: &str) -> Result<usize> {
        self.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "UPDATE issues SET project_milestone_id = NULL,
                        project_milestone_name = NULL, synced_at = datetime('now')
                 WHERE project_milestone_id = ?1",
                rusqlite::params![milestone_id],
            )?;
            let changed = tx.execute(
                "DELETE FROM project_milestones WHERE id = ?1",
                rusqlite::params![milestone_id],
            )?;
            tx.commit()?;
            Ok(changed)
        })
    }

    pub fn delete_projects_for_workspace_not_in(
        &self,
        workspace_id: &str,
        keep_ids: &[String],
    ) -> Result<usize> {
        self.delete_hierarchy_not_in("projects", "workspace_id", workspace_id, keep_ids)
    }

    pub fn delete_milestones_for_workspace_not_in(
        &self,
        workspace_id: &str,
        keep_ids: &[String],
    ) -> Result<usize> {
        self.delete_hierarchy_not_in("project_milestones", "workspace_id", workspace_id, keep_ids)
    }

    pub fn delete_milestones_for_project_not_in(
        &self,
        project_id: &str,
        keep_ids: &[String],
    ) -> Result<usize> {
        self.delete_hierarchy_not_in("project_milestones", "project_id", project_id, keep_ids)
    }

    fn delete_hierarchy_not_in(
        &self,
        table: &str,
        scope_column: &str,
        scope: &str,
        keep_ids: &[String],
    ) -> Result<usize> {
        debug_assert!(matches!(table, "projects" | "project_milestones"));
        debug_assert!(matches!(scope_column, "workspace_id" | "project_id"));
        self.with_conn(|conn| {
            let exclusion = if keep_ids.is_empty() {
                String::new()
            } else {
                let placeholders = (0..keep_ids.len())
                    .map(|index| format!("?{}", index + 2))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("AND id NOT IN ({placeholders})")
            };
            let stale_filter = format!("{scope_column} = ?1 {exclusion}");
            let mut params: Vec<Box<dyn rusqlite::types::ToSql>> =
                vec![Box::new(scope.to_string())];
            params.extend(
                keep_ids
                    .iter()
                    .cloned()
                    .map(|id| Box::new(id) as Box<dyn rusqlite::types::ToSql>),
            );
            let refs = params
                .iter()
                .map(|value| value.as_ref())
                .collect::<Vec<_>>();
            let issue_update = if table == "projects" {
                format!(
                    "UPDATE issues SET project_id = NULL, project_name = NULL,
                            project_milestone_id = NULL, project_milestone_name = NULL,
                            synced_at = datetime('now')
                     WHERE project_id IN (SELECT id FROM projects WHERE {stale_filter})"
                )
            } else {
                format!(
                    "UPDATE issues SET project_milestone_id = NULL,
                            project_milestone_name = NULL, synced_at = datetime('now')
                     WHERE project_milestone_id IN (
                         SELECT id FROM project_milestones WHERE {stale_filter}
                     )"
                )
            };
            conn.execute(&issue_update, refs.as_slice())?;
            let sql = format!("DELETE FROM {table} WHERE {stale_filter}");
            Ok(conn.execute(&sql, refs.as_slice())?)
        })
    }
}

fn project_from_row(conn: &rusqlite::Connection, row: &rusqlite::Row) -> rusqlite::Result<Project> {
    let project_id: String = row.get(0)?;
    let teams = {
        let mut stmt = conn.prepare(
            "SELECT team_id, team_key, team_name FROM project_teams
             WHERE project_id = ?1 ORDER BY team_key",
        )?;
        let rows = stmt.query_map(rusqlite::params![project_id], |team_row| {
            Ok(ProjectTeam {
                id: team_row.get(0)?,
                key: team_row.get(1)?,
                name: team_row.get(2)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let members = {
        let mut stmt = conn.prepare(
            "SELECT user_id, user_name FROM project_members
             WHERE project_id = ?1 ORDER BY user_name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map(rusqlite::params![project_id], |member_row| {
            Ok(ProjectMember {
                id: member_row.get(0)?,
                name: member_row.get(1)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let labels = {
        let mut stmt = conn.prepare(
            "SELECT label_id, label_name, color, description FROM project_labels
             WHERE project_id = ?1 ORDER BY label_name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map(rusqlite::params![project_id], |label_row| {
            Ok(ProjectLabel {
                id: label_row.get(0)?,
                name: label_row.get(1)?,
                color: label_row.get(2)?,
                description: label_row.get(3)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    Ok(Project {
        id: project_id,
        workspace_id: row.get(1)?,
        slug_id: row.get(2)?,
        name: row.get(3)?,
        description: row.get(4)?,
        content: row.get(5)?,
        icon: row.get(6)?,
        color: row.get(7)?,
        status_id: row.get(8)?,
        status_name: row.get(9)?,
        status_type: row.get(10)?,
        status_color: row.get(11)?,
        priority: row.get(12)?,
        start_date: row.get(13)?,
        target_date: row.get(14)?,
        lead_id: row.get(15)?,
        lead_name: row.get(16)?,
        created_at: row.get(17)?,
        updated_at: row.get(18)?,
        archived_at: row.get(19)?,
        url: row.get(20)?,
        progress: row.get(21)?,
        synced_at: row.get(22)?,
        teams,
        members,
        labels,
    })
}

fn milestone_from_row(row: &rusqlite::Row) -> rusqlite::Result<ProjectMilestone> {
    Ok(ProjectMilestone {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        project_id: row.get(2)?,
        project_name: row.get(3)?,
        name: row.get(4)?,
        description: row.get(5)?,
        target_date: row.get(6)?,
        status: row.get(7)?,
        progress: row.get(8)?,
        sort_order: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
        archived_at: row.get(12)?,
        synced_at: row.get(13)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_helpers::{make_issue, test_db};

    fn project() -> Project {
        Project {
            id: "project-1".into(),
            workspace_id: "default".into(),
            slug_id: "api-reliability".into(),
            name: "API Reliability".into(),
            description: "Service resilience".into(),
            content: Some("Detailed rollout plan".into()),
            icon: Some("Cube".into()),
            color: "#f2994a".into(),
            status_id: "status-1".into(),
            status_name: "Backlog".into(),
            status_type: "backlog".into(),
            status_color: "#888888".into(),
            priority: 2,
            start_date: None,
            target_date: Some("2026-09-01".into()),
            lead_id: Some("user-1".into()),
            lead_name: Some("Alex Morgan".into()),
            created_at: "2026-07-01T00:00:00Z".into(),
            updated_at: "2026-07-16T00:00:00Z".into(),
            archived_at: None,
            url: "https://linear.app/acme/project/api-reliability".into(),
            progress: 0.25,
            synced_at: None,
            teams: vec![ProjectTeam {
                id: "team-1".into(),
                key: "ENG".into(),
                name: "Engineering".into(),
            }],
            members: vec![ProjectMember {
                id: "user-1".into(),
                name: "Alex Morgan".into(),
            }],
            labels: vec![ProjectLabel {
                id: "label-1".into(),
                name: "Infrastructure".into(),
                color: "#f2994a".into(),
                description: Some("Platform engineering".into()),
            }],
        }
    }

    fn milestone() -> ProjectMilestone {
        ProjectMilestone {
            id: "milestone-1".into(),
            workspace_id: "default".into(),
            project_id: "project-1".into(),
            project_name: "API Reliability".into(),
            name: "Request tracing".into(),
            description: Some("Instrument critical request paths".into()),
            target_date: Some("2026-08-15".into()),
            status: "next".into(),
            progress: 0.5,
            sort_order: 1.0,
            created_at: "2026-07-01T00:00:00Z".into(),
            updated_at: "2026-07-16T00:00:00Z".into(),
            archived_at: None,
            synced_at: None,
        }
    }

    #[test]
    fn project_round_trip_preserves_metadata_and_relationships() {
        let (db, _dir) = test_db();
        db.upsert_project(&project()).unwrap();
        db.upsert_project_milestone(&milestone()).unwrap();

        let stored = db
            .get_project("default", "api-reliability")
            .unwrap()
            .unwrap();
        assert_eq!(stored.name, "API Reliability");
        assert_eq!(stored.status_name, "Backlog");
        assert_eq!(stored.teams[0].key, "ENG");
        assert_eq!(stored.members[0].name, "Alex Morgan");
        assert_eq!(stored.labels[0].name, "Infrastructure");

        let milestones = db.list_project_milestones(&stored.id).unwrap();
        assert_eq!(milestones.len(), 1);
        assert_eq!(milestones[0].name, "Request tracing");
        assert!(milestones[0].synced_at.is_some());
    }

    #[test]
    fn project_and_milestone_bundles_include_linked_issues() {
        let (db, _dir) = test_db();
        db.upsert_project(&project()).unwrap();
        db.upsert_project_milestone(&milestone()).unwrap();
        let mut issue = make_issue("ENG-1", "ENG");
        issue.priority = 0;
        issue.project_id = Some("project-1".into());
        issue.project_milestone_id = Some("milestone-1".into());
        issue.project_milestone_name = Some("Request tracing".into());
        db.upsert_issue(&issue).unwrap();

        let bundle = db
            .get_project_bundle("default", "API Reliability")
            .unwrap()
            .unwrap();
        assert_eq!(bundle.milestones.len(), 1);
        assert_eq!(bundle.issues[0].identifier, "ENG-1");

        let milestone_bundle = db
            .get_project_milestone_bundle("default", "Request tracing", Some("project-1"))
            .unwrap()
            .unwrap();
        assert_eq!(
            milestone_bundle.issues[0].project_milestone_name.as_deref(),
            Some("Request tracing")
        );

        let triage_issues = db.get_unprioritized_issues(None, false, "default").unwrap();
        assert_eq!(triage_issues[0].project_id.as_deref(), Some("project-1"));
        assert_eq!(
            triage_issues[0].project_milestone_id.as_deref(),
            Some("milestone-1")
        );
    }

    #[test]
    fn deleting_project_cascades_milestones_but_not_issues() {
        let (db, _dir) = test_db();
        db.upsert_project(&project()).unwrap();
        db.upsert_project_milestone(&milestone()).unwrap();
        let mut issue = make_issue("ENG-1", "ENG");
        issue.project_id = Some("project-1".into());
        issue.project_milestone_id = Some("milestone-1".into());
        db.upsert_issue(&issue).unwrap();

        assert_eq!(db.delete_project_local("project-1").unwrap(), 1);
        assert!(db.list_project_milestones("project-1").unwrap().is_empty());
        let issue = db.get_issue("ENG-1").unwrap().unwrap();
        assert!(issue.project_id.is_none());
        assert!(issue.project_milestone_id.is_none());
    }

    #[test]
    fn deleting_milestone_clears_issue_milestone_but_keeps_project() {
        let (db, _dir) = test_db();
        db.upsert_project(&project()).unwrap();
        db.upsert_project_milestone(&milestone()).unwrap();
        let mut issue = make_issue("ENG-1", "ENG");
        issue.project_id = Some("project-1".into());
        issue.project_milestone_id = Some("milestone-1".into());
        db.upsert_issue(&issue).unwrap();

        assert_eq!(db.delete_project_milestone_local("milestone-1").unwrap(), 1);
        let issue = db.get_issue("ENG-1").unwrap().unwrap();
        assert_eq!(issue.project_id.as_deref(), Some("project-1"));
        assert!(issue.project_milestone_id.is_none());
    }

    #[test]
    fn hierarchy_reconciliation_clears_stale_issue_relationships() {
        let (db, _dir) = test_db();
        db.upsert_project(&project()).unwrap();
        db.upsert_project_milestone(&milestone()).unwrap();
        for identifier in ["ENG-1", "ENG-2"] {
            let mut issue = make_issue(identifier, "ENG");
            issue.project_id = Some("project-1".into());
            issue.project_name = Some("API Reliability".into());
            issue.project_milestone_id = Some("milestone-1".into());
            issue.project_milestone_name = Some("Request tracing".into());
            db.upsert_issue(&issue).unwrap();
        }

        db.reconcile_project_milestone_issue_membership(
            "default",
            "milestone-1",
            &["ENG-1".into()],
        )
        .unwrap();
        let stale = db.get_issue("ENG-2").unwrap().unwrap();
        assert!(stale.project_id.is_some());
        assert!(stale.project_milestone_id.is_none());

        db.reconcile_project_issue_membership("default", "project-1", &["ENG-1".into()])
            .unwrap();
        let stale = db.get_issue("ENG-2").unwrap().unwrap();
        assert!(stale.project_id.is_none());
        assert!(stale.project_milestone_name.is_none());
    }
}
