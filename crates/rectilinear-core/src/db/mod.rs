pub mod schema;
#[cfg(test)]
mod test_helpers;

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};

pub struct BlockerRow {
    pub issue_id: String,
    pub identifier: String,
    pub title: String,
    pub state_name: String,
    pub state_type: String,
}

#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("Failed to open database at {}", path.display()))?;

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;

        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        schema::run_migrations(&conn)?;
        Ok(())
    }

    pub fn with_conn<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T>,
    {
        let conn = self.conn.lock().unwrap();
        f(&conn)
    }

    // --- Workspace CRUD ---

    pub fn upsert_workspace(
        &self,
        id: &str,
        linear_org_id: Option<&str>,
        display_name: Option<&str>,
    ) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO workspaces (id, linear_org_id, display_name)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(id) DO UPDATE SET
                   linear_org_id=excluded.linear_org_id,
                   display_name=excluded.display_name",
                rusqlite::params![id, linear_org_id, display_name],
            )?;
            Ok(())
        })
    }

    pub fn get_workspace(&self, id: &str) -> Result<Option<WorkspaceRow>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, linear_org_id, display_name, created_at FROM workspaces WHERE id = ?1",
            )?;
            let mut rows = stmt.query(rusqlite::params![id])?;
            if let Some(row) = rows.next()? {
                Ok(Some(WorkspaceRow {
                    id: row.get(0)?,
                    linear_org_id: row.get(1)?,
                    display_name: row.get(2)?,
                    created_at: row.get(3)?,
                }))
            } else {
                Ok(None)
            }
        })
    }

    pub fn list_workspaces(&self) -> Result<Vec<WorkspaceRow>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, linear_org_id, display_name, created_at FROM workspaces ORDER BY id",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(WorkspaceRow {
                    id: row.get(0)?,
                    linear_org_id: row.get(1)?,
                    display_name: row.get(2)?,
                    created_at: row.get(3)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    /// Delete a workspace and all its associated data (issues, chunks, comments, sync state).
    pub fn delete_workspace(&self, id: &str) -> Result<usize> {
        self.with_conn(|conn| {
            // Chunks and issue_relations cascade from issues via ON DELETE CASCADE
            let issue_count: usize = conn.query_row(
                "SELECT COUNT(*) FROM issues WHERE workspace_id = ?1",
                rusqlite::params![id],
                |row| row.get(0),
            )?;
            conn.execute(
                "DELETE FROM issues WHERE workspace_id = ?1",
                rusqlite::params![id],
            )?;
            conn.execute(
                "DELETE FROM comments WHERE workspace_id = ?1",
                rusqlite::params![id],
            )?;
            conn.execute(
                "DELETE FROM sync_state WHERE workspace_id = ?1",
                rusqlite::params![id],
            )?;
            conn.execute(
                "DELETE FROM labels WHERE workspace_id = ?1",
                rusqlite::params![id],
            )?;
            conn.execute(
                "DELETE FROM workspaces WHERE id = ?1",
                rusqlite::params![id],
            )?;
            Ok(issue_count)
        })
    }

    // --- Label CRUD ---

    pub fn upsert_label(&self, label: &Label) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO labels (id, workspace_id, name, color, parent_id)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(id) DO UPDATE SET
                   workspace_id=excluded.workspace_id,
                   name=excluded.name,
                   color=excluded.color,
                   parent_id=excluded.parent_id",
                rusqlite::params![label.id, label.workspace_id, label.name, label.color, label.parent_id],
            )?;
            Ok(())
        })
    }

    pub fn list_labels(&self, workspace_id: &str) -> Result<Vec<Label>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, workspace_id, name, color, parent_id
                 FROM labels WHERE workspace_id = ?1
                 ORDER BY name COLLATE NOCASE ASC",
            )?;
            let rows = stmt.query_map(rusqlite::params![workspace_id], |row| {
                Ok(Label {
                    id: row.get(0)?,
                    workspace_id: row.get(1)?,
                    name: row.get(2)?,
                    color: row.get(3)?,
                    parent_id: row.get(4)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    /// Delete labels in `workspace_id` whose id is NOT in `keep_ids`.
    /// Returns the number of rows deleted. Cascades to `issue_labels`.
    pub fn delete_labels_for_workspace_not_in(
        &self,
        workspace_id: &str,
        keep_ids: &[String],
    ) -> Result<usize> {
        self.with_conn(|conn| {
            if keep_ids.is_empty() {
                let n = conn.execute(
                    "DELETE FROM labels WHERE workspace_id = ?1",
                    rusqlite::params![workspace_id],
                )?;
                return Ok(n);
            }
            let placeholders = (0..keep_ids.len())
                .map(|i| format!("?{}", i + 2))
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                "DELETE FROM labels WHERE workspace_id = ?1 AND id NOT IN ({placeholders})"
            );
            let mut params: Vec<Box<dyn rusqlite::types::ToSql>> =
                vec![Box::new(workspace_id.to_string())];
            for id in keep_ids {
                params.push(Box::new(id.clone()));
            }
            let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                params.iter().map(|p| p.as_ref()).collect();
            let n = conn.execute(&sql, param_refs.as_slice())?;
            Ok(n)
        })
    }

    /// Resolve label names to ids using the local catalog (case-insensitive).
    /// Returns (resolved_ids, unknown_names). Order of resolved_ids is not guaranteed.
    pub fn resolve_label_ids_local(
        &self,
        workspace_id: &str,
        names: &[String],
    ) -> Result<(Vec<String>, Vec<String>)> {
        if names.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        self.with_conn(|conn| {
            let mut resolved = Vec::new();
            let mut unknown = Vec::new();
            let mut stmt = conn.prepare(
                "SELECT id FROM labels WHERE workspace_id = ?1 AND name = ?2 COLLATE NOCASE",
            )?;
            for name in names {
                let mut rows = stmt.query(rusqlite::params![workspace_id, name])?;
                if let Some(row) = rows.next()? {
                    resolved.push(row.get::<_, String>(0)?);
                } else {
                    unknown.push(name.clone());
                }
            }
            Ok((resolved, unknown))
        })
    }

    // --- Issue-Label Join CRUD ---

    /// Replace the label set for an issue. Atomic via transaction.
    /// Skips any label_ids not present in the `labels` table (logged at warn level via eprintln).
    pub fn replace_issue_labels(&self, issue_id: &str, label_ids: &[String]) -> Result<()> {
        self.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "DELETE FROM issue_labels WHERE issue_id = ?1",
                rusqlite::params![issue_id],
            )?;
            for lid in label_ids {
                let exists: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM labels WHERE id = ?1",
                    rusqlite::params![lid],
                    |r| r.get(0),
                )?;
                if exists == 0 {
                    eprintln!(
                        "warning: skipping unknown label id '{}' for issue '{}'",
                        lid, issue_id
                    );
                    continue;
                }
                tx.execute(
                    "INSERT OR IGNORE INTO issue_labels (issue_id, label_id) VALUES (?1, ?2)",
                    rusqlite::params![issue_id, lid],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
    }

    pub fn get_issue_label_ids(&self, issue_id: &str) -> Result<Vec<String>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT label_id FROM issue_labels WHERE issue_id = ?1 ORDER BY label_id",
            )?;
            let rows = stmt.query_map(rusqlite::params![issue_id], |row| row.get::<_, String>(0))?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    // --- Issue CRUD ---

    pub fn upsert_issue(&self, issue: &Issue) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO issues (id, identifier, team_key, title, description, state_name, state_type, priority, assignee_name, project_name, labels_json, created_at, updated_at, content_hash, synced_at, url, branch_name, workspace_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, datetime('now'), ?15, ?16, ?17)
                 ON CONFLICT(id) DO UPDATE SET
                   identifier=excluded.identifier, team_key=excluded.team_key, title=excluded.title,
                   description=excluded.description, state_name=excluded.state_name, state_type=excluded.state_type,
                   priority=excluded.priority, assignee_name=excluded.assignee_name, project_name=excluded.project_name,
                   labels_json=excluded.labels_json, updated_at=excluded.updated_at,
                   content_hash=excluded.content_hash, url=excluded.url, branch_name=excluded.branch_name,
                   workspace_id=excluded.workspace_id, synced_at=datetime('now')",
                rusqlite::params![
                    issue.id, issue.identifier, issue.team_key, issue.title, issue.description,
                    issue.state_name, issue.state_type, issue.priority, issue.assignee_name,
                    issue.project_name, issue.labels_json, issue.created_at, issue.updated_at,
                    issue.content_hash, issue.url, issue.branch_name, issue.workspace_id,
                ],
            )?;
            Ok(())
        })
    }

    pub fn get_issue(&self, id_or_identifier: &str) -> Result<Option<Issue>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, identifier, team_key, title, description, state_name, state_type, priority, assignee_name, project_name, labels_json, created_at, updated_at, content_hash, synced_at, url, branch_name, workspace_id
                 FROM issues WHERE id = ?1 OR identifier = ?1"
            )?;
            let mut rows = stmt.query(rusqlite::params![id_or_identifier])?;
            if let Some(row) = rows.next()? {
                Ok(Some(Issue::from_row(row)?))
            } else {
                Ok(None)
            }
        })
    }

    /// Build a SQL fragment "<table_alias>.id IN (SELECT issue_id FROM issue_labels ...)"
    /// for AND-matching all of `label_ids`. Returns the fragment + bound params.
    /// Caller is responsible for prepending " AND " before splicing in.
    /// `param_offset` is the next free `?N` index (1-based).
    /// `table_alias` is the alias used by the outer query (e.g. "issues" or "i").
    fn label_filter_fragment(
        label_ids: &[String],
        param_offset: usize,
        table_alias: &str,
    ) -> (String, Vec<Box<dyn rusqlite::types::ToSql>>) {
        let n = label_ids.len();
        let placeholders = (0..n)
            .map(|i| format!("?{}", param_offset + i))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "{table_alias}.id IN (\
                SELECT issue_id FROM issue_labels \
                WHERE label_id IN ({placeholders}) \
                GROUP BY issue_id \
                HAVING COUNT(DISTINCT label_id) = {n}\
             )"
        );
        let params: Vec<Box<dyn rusqlite::types::ToSql>> =
            label_ids.iter().map(|s| Box::new(s.clone()) as Box<dyn rusqlite::types::ToSql>).collect();
        (sql, params)
    }

    pub fn get_unprioritized_issues(
        &self,
        team_key: Option<&str>,
        include_completed: bool,
        workspace_id: &str,
    ) -> Result<Vec<Issue>> {
        self.get_unprioritized_issues_filtered(team_key, include_completed, workspace_id, None)
    }

    pub fn get_unprioritized_issues_filtered(
        &self,
        team_key: Option<&str>,
        include_completed: bool,
        workspace_id: &str,
        label_ids: Option<&[String]>,
    ) -> Result<Vec<Issue>> {
        self.with_conn(|conn| {
            let state_filter = if include_completed {
                ""
            } else {
                " AND state_type NOT IN ('completed', 'canceled')"
            };

            // Required base params come first; label-filter params (if any) are appended.
            let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
            let base_where: String = if let Some(team) = team_key {
                params.push(Box::new(team.to_string()));
                params.push(Box::new(workspace_id.to_string()));
                "team_key = ?1 AND workspace_id = ?2".to_string()
            } else {
                params.push(Box::new(workspace_id.to_string()));
                "workspace_id = ?1".to_string()
            };

            let label_clause = if let Some(ids) = label_ids.filter(|ids| !ids.is_empty()) {
                let (frag, mut lp) = Self::label_filter_fragment(ids, params.len() + 1, "issues");
                params.append(&mut lp);
                format!(" AND {frag}")
            } else {
                String::new()
            };

            let sql = format!(
                "SELECT id, identifier, team_key, title, description, state_name, state_type, priority, assignee_name, project_name, labels_json, created_at, updated_at, content_hash, synced_at, url, branch_name, workspace_id
                 FROM issues WHERE priority = 0{state_filter} AND {base_where}{label_clause}
                 ORDER BY created_at DESC"
            );

            let mut stmt = conn.prepare(&sql)?;
            let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
            let rows = stmt.query_map(param_refs.as_slice(), |row| Ok(Issue::from_row(row).unwrap()))?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    pub fn get_issues_by_state_types(
        &self,
        team_key: &str,
        state_types: &[String],
        workspace_id: &str,
    ) -> Result<Vec<Issue>> {
        self.with_conn(|conn| {
            let placeholders: String = state_types
                .iter()
                .enumerate()
                .map(|(i, _)| format!("?{}", i + 3))
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                "SELECT id, identifier, team_key, title, description, state_name, state_type, \
                 priority, assignee_name, project_name, labels_json, created_at, updated_at, \
                 content_hash, synced_at, url, branch_name, workspace_id \
                 FROM issues WHERE team_key = ?1 AND workspace_id = ?2 AND state_type IN ({placeholders}) \
                 ORDER BY priority ASC, created_at DESC"
            );
            let mut stmt = conn.prepare(&sql)?;
            let mut params: Vec<Box<dyn rusqlite::types::ToSql>> =
                vec![Box::new(team_key.to_string()), Box::new(workspace_id.to_string())];
            for st in state_types {
                params.push(Box::new(st.clone()));
            }
            let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                params.iter().map(|p| p.as_ref()).collect();
            let rows = stmt.query_map(param_refs.as_slice(), |row| {
                Ok(Issue::from_row(row).unwrap())
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    /// For a set of issue IDs, return all `blocked_by` relations with resolved state info.
    /// Returns (issue_id, blocker_identifier, blocker_title, blocker_state_name, blocker_state_type).
    pub fn get_blockers_for_issues(&self, issue_ids: &[String]) -> Result<Vec<BlockerRow>> {
        if issue_ids.is_empty() {
            return Ok(vec![]);
        }
        self.with_conn(|conn| {
            let placeholders: String = issue_ids
                .iter()
                .enumerate()
                .map(|(i, _)| format!("?{}", i + 1))
                .collect::<Vec<_>>()
                .join(", ");

            // Forward: issue has a "blocked_by" relation
            let sql_fwd = format!(
                "SELECT r.issue_id, COALESCE(i.identifier, r.related_issue_identifier),
                        COALESCE(i.title, ''), COALESCE(i.state_name, ''), COALESCE(i.state_type, '')
                 FROM issue_relations r
                 LEFT JOIN issues i ON r.related_issue_id = i.id
                 WHERE r.issue_id IN ({placeholders}) AND r.relation_type = 'blocked_by'"
            );

            // Inverse: another issue has a "blocks" relation pointing at this issue
            let sql_inv = format!(
                "SELECT r.related_issue_id, i2.identifier,
                        COALESCE(i2.title, ''), COALESCE(i2.state_name, ''), COALESCE(i2.state_type, '')
                 FROM issue_relations r
                 JOIN issues i ON r.related_issue_id = i.id
                 JOIN issues i2 ON r.issue_id = i2.id
                 WHERE r.related_issue_id IN ({placeholders}) AND r.relation_type = 'blocks'"
            );

            let mut results = Vec::new();
            let params: Vec<Box<dyn rusqlite::types::ToSql>> =
                issue_ids.iter().map(|id| Box::new(id.clone()) as _).collect();
            let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                params.iter().map(|p| p.as_ref()).collect();

            for sql in [&sql_fwd, &sql_inv] {
                let mut stmt = conn.prepare(sql)?;
                let rows = stmt.query_map(param_refs.as_slice(), |row| {
                    Ok(BlockerRow {
                        issue_id: row.get(0)?,
                        identifier: row.get(1)?,
                        title: row.get(2)?,
                        state_name: row.get(3)?,
                        state_type: row.get(4)?,
                    })
                })?;
                for row in rows {
                    results.push(row?);
                }
            }
            Ok(results)
        })
    }

    pub fn count_issues(&self, team_key: Option<&str>, workspace_id: &str) -> Result<usize> {
        self.with_conn(|conn| {
            let count: usize = if let Some(team) = team_key {
                conn.query_row(
                    "SELECT COUNT(*) FROM issues WHERE team_key = ?1 AND workspace_id = ?2",
                    rusqlite::params![team, workspace_id],
                    |row| row.get(0),
                )?
            } else {
                conn.query_row(
                    "SELECT COUNT(*) FROM issues WHERE workspace_id = ?1",
                    rusqlite::params![workspace_id],
                    |row| row.get(0),
                )?
            };
            Ok(count)
        })
    }

    /// Count issues with each optional field populated. Returns (total, with_description, with_priority, with_labels, with_project).
    pub fn get_field_completeness(
        &self,
        team_key: Option<&str>,
        workspace_id: &str,
    ) -> Result<(usize, usize, usize, usize, usize)> {
        self.with_conn(|conn| {
            let (sql, params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) =
                if let Some(team) = team_key {
                    (
                        "SELECT COUNT(*),
                                SUM(CASE WHEN description IS NOT NULL AND description != '' THEN 1 ELSE 0 END),
                                SUM(CASE WHEN priority > 0 THEN 1 ELSE 0 END),
                                SUM(CASE WHEN labels_json != '[]' THEN 1 ELSE 0 END),
                                SUM(CASE WHEN project_name IS NOT NULL AND project_name != '' THEN 1 ELSE 0 END)
                         FROM issues WHERE team_key = ?1 AND workspace_id = ?2"
                            .to_string(),
                        vec![Box::new(team.to_string()) as Box<dyn rusqlite::types::ToSql>, Box::new(workspace_id.to_string())],
                    )
                } else {
                    (
                        "SELECT COUNT(*),
                                SUM(CASE WHEN description IS NOT NULL AND description != '' THEN 1 ELSE 0 END),
                                SUM(CASE WHEN priority > 0 THEN 1 ELSE 0 END),
                                SUM(CASE WHEN labels_json != '[]' THEN 1 ELSE 0 END),
                                SUM(CASE WHEN project_name IS NOT NULL AND project_name != '' THEN 1 ELSE 0 END)
                         FROM issues WHERE workspace_id = ?1"
                            .to_string(),
                        vec![Box::new(workspace_id.to_string()) as Box<dyn rusqlite::types::ToSql>],
                    )
                };
            let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                params.iter().map(|p| p.as_ref()).collect();
            let row = conn.query_row(&sql, param_refs.as_slice(), |row| {
                Ok((
                    row.get::<_, usize>(0)?,
                    row.get::<_, Option<usize>>(1)?.unwrap_or(0),
                    row.get::<_, Option<usize>>(2)?.unwrap_or(0),
                    row.get::<_, Option<usize>>(3)?.unwrap_or(0),
                    row.get::<_, Option<usize>>(4)?.unwrap_or(0),
                ))
            })?;
            Ok(row)
        })
    }

    /// List all issues with summary info (no description text). Supports pagination,
    /// optional team filter, and optional text filter on identifier/title.
    #[allow(unused_assignments)]
    pub fn list_all_issues(
        &self,
        team_key: Option<&str>,
        filter: Option<&str>,
        limit: usize,
        offset: usize,
        workspace_id: &str,
    ) -> Result<Vec<IssueSummary>> {
        self.with_conn(|conn| {
            let mut conditions = Vec::new();
            let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
            let mut param_idx = 1;

            // Always filter by workspace
            conditions.push(format!("i.workspace_id = ?{param_idx}"));
            params.push(Box::new(workspace_id.to_string()));
            param_idx += 1;

            if let Some(team) = team_key {
                conditions.push(format!("i.team_key = ?{param_idx}"));
                params.push(Box::new(team.to_string()));
                param_idx += 1;
            }

            if let Some(text) = filter {
                let like = format!("%{text}%");
                conditions.push(format!(
                    "(i.identifier LIKE ?{} OR i.title LIKE ?{})",
                    param_idx,
                    param_idx + 1
                ));
                params.push(Box::new(like.clone()));
                params.push(Box::new(like));
                param_idx += 2;
            }

            let _ = param_idx;

            let where_clause = if conditions.is_empty() {
                String::new()
            } else {
                format!("WHERE {}", conditions.join(" AND "))
            };

            let limit_idx = params.len() + 1;
            let offset_idx = params.len() + 2;

            let sql = format!(
                "SELECT i.id, i.identifier, i.team_key, i.title, i.state_name, i.state_type,
                        i.priority, i.project_name, i.labels_json, i.updated_at, i.url,
                        i.description IS NOT NULL AND i.description != '' AS has_desc,
                        EXISTS(SELECT 1 FROM chunks c WHERE c.issue_id = i.id) AS has_emb
                 FROM issues i
                 {where_clause}
                 ORDER BY i.updated_at DESC
                 LIMIT ?{limit_idx} OFFSET ?{offset_idx}"
            );
            params.push(Box::new(limit as i64));
            params.push(Box::new(offset as i64));

            let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                params.iter().map(|p| p.as_ref()).collect();
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(param_refs.as_slice(), |row| {
                let labels_json: String = row.get(8)?;
                let labels: Vec<String> = serde_json::from_str(&labels_json).unwrap_or_default();
                Ok(IssueSummary {
                    id: row.get(0)?,
                    identifier: row.get(1)?,
                    team_key: row.get(2)?,
                    title: row.get(3)?,
                    state_name: row.get(4)?,
                    state_type: row.get(5)?,
                    priority: row.get(6)?,
                    project_name: row.get(7)?,
                    labels,
                    updated_at: row.get(9)?,
                    url: row.get(10)?,
                    has_description: row.get(11)?,
                    has_embedding: row.get(12)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    // --- Relations ---

    pub fn upsert_relations(&self, issue_id: &str, relations: &[Relation]) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "DELETE FROM issue_relations WHERE issue_id = ?1",
                rusqlite::params![issue_id],
            )?;
            let mut stmt = conn.prepare(
                "INSERT OR IGNORE INTO issue_relations (id, issue_id, related_issue_id, related_issue_identifier, relation_type)
                 VALUES (?1, ?2, ?3, ?4, ?5)"
            )?;
            for rel in relations {
                stmt.execute(rusqlite::params![
                    rel.id, rel.issue_id, rel.related_issue_id,
                    rel.related_issue_identifier, rel.relation_type,
                ])?;
            }
            Ok(())
        })
    }

    pub fn get_relations_enriched(&self, issue_id: &str) -> Result<Vec<EnrichedRelation>> {
        self.with_conn(|conn| {
            // Relations where this issue is the source
            let mut stmt = conn.prepare(
                "SELECT r.id, r.relation_type, r.related_issue_identifier,
                        COALESCE(i.title, ''), COALESCE(i.state_name, ''), COALESCE(i.url, '')
                 FROM issue_relations r
                 LEFT JOIN issues i ON r.related_issue_id = i.id
                 WHERE r.issue_id = ?1",
            )?;
            let forward = stmt
                .query_map(rusqlite::params![issue_id], |row| {
                    Ok(EnrichedRelation {
                        relation_id: row.get(0)?,
                        relation_type: row.get(1)?,
                        issue_identifier: row.get(2)?,
                        issue_title: row.get(3)?,
                        issue_state: row.get(4)?,
                        issue_url: row.get(5)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;

            // Relations where this issue is the target — flip direction
            let mut stmt2 = conn.prepare(
                "SELECT r.id, r.relation_type, i2.identifier,
                        COALESCE(i2.title, ''), COALESCE(i2.state_name, ''), COALESCE(i2.url, '')
                 FROM issue_relations r
                 JOIN issues i ON r.related_issue_id = i.id
                 JOIN issues i2 ON r.issue_id = i2.id
                 WHERE r.related_issue_id = i.id AND i.id = ?1",
            )?;
            let inverse = stmt2
                .query_map(rusqlite::params![issue_id], |row| {
                    let raw_type: String = row.get(1)?;
                    let flipped = match raw_type.as_str() {
                        "blocks" => "blocked_by".to_string(),
                        "blocked_by" => "blocks".to_string(),
                        other => other.to_string(), // related, duplicate are symmetric
                    };
                    Ok(EnrichedRelation {
                        relation_id: row.get(0)?,
                        relation_type: flipped,
                        issue_identifier: row.get(2)?,
                        issue_title: row.get(3)?,
                        issue_state: row.get(4)?,
                        issue_url: row.get(5)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;

            let mut all = forward;
            all.extend(inverse);
            Ok(all)
        })
    }

    /// Look up a relation ID between two issues (by identifier) for deletion
    pub fn find_relation_id(
        &self,
        issue_id: &str,
        related_issue_id: &str,
        relation_type: &str,
    ) -> Result<Option<String>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id FROM issue_relations WHERE issue_id = ?1 AND related_issue_id = ?2 AND relation_type = ?3"
            )?;
            let mut rows = stmt.query(rusqlite::params![issue_id, related_issue_id, relation_type])?;
            if let Some(row) = rows.next()? {
                Ok(Some(row.get(0)?))
            } else {
                Ok(None)
            }
        })
    }

    // --- Chunks (embeddings) ---

    pub fn upsert_chunks(&self, issue_id: &str, chunks: &[(usize, String, Vec<u8>)]) -> Result<()> {
        self.upsert_chunks_with_model(issue_id, chunks, "")
    }

    pub fn upsert_chunks_with_model(
        &self,
        issue_id: &str,
        chunks: &[(usize, String, Vec<u8>)],
        model_name: &str,
    ) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "DELETE FROM chunks WHERE issue_id = ?1",
                rusqlite::params![issue_id],
            )?;
            let mut stmt = conn.prepare(
                "INSERT INTO chunks (issue_id, chunk_index, chunk_text, embedding, model_name) VALUES (?1, ?2, ?3, ?4, ?5)"
            )?;
            for (idx, text, embedding) in chunks {
                stmt.execute(rusqlite::params![issue_id, idx, text, embedding, model_name])?;
            }
            Ok(())
        })
    }

    /// Get the embedding model name for an issue's chunks, if any exist.
    pub fn get_embedding_model(&self, issue_id: &str) -> Result<Option<String>> {
        self.with_conn(|conn| {
            let mut stmt =
                conn.prepare("SELECT model_name FROM chunks WHERE issue_id = ?1 LIMIT 1")?;
            let mut rows = stmt.query(rusqlite::params![issue_id])?;
            if let Some(row) = rows.next()? {
                let name: String = row.get(0)?;
                Ok(if name.is_empty() { None } else { Some(name) })
            } else {
                Ok(None)
            }
        })
    }

    pub fn get_all_chunks(&self, workspace_id: &str) -> Result<Vec<Chunk>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT c.issue_id, c.embedding, i.identifier
                 FROM chunks c JOIN issues i ON c.issue_id = i.id
                 WHERE i.workspace_id = ?1",
            )?;
            let rows = stmt.query_map(rusqlite::params![workspace_id], |row| {
                Ok(Chunk {
                    issue_id: row.get(0)?,
                    embedding: row.get(1)?,
                    identifier: row.get(2)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    pub fn get_chunks_for_team(&self, team_key: &str, workspace_id: &str) -> Result<Vec<Chunk>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT c.issue_id, c.embedding, i.identifier
                 FROM chunks c JOIN issues i ON c.issue_id = i.id
                 WHERE i.team_key = ?1 AND i.workspace_id = ?2",
            )?;
            let rows = stmt.query_map(rusqlite::params![team_key, workspace_id], |row| {
                Ok(Chunk {
                    issue_id: row.get(0)?,
                    embedding: row.get(1)?,
                    identifier: row.get(2)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    pub fn count_embedded_issues(
        &self,
        team_key: Option<&str>,
        workspace_id: &str,
    ) -> Result<usize> {
        self.with_conn(|conn| {
            let count: usize = if let Some(team) = team_key {
                conn.query_row(
                    "SELECT COUNT(DISTINCT c.issue_id) FROM chunks c JOIN issues i ON c.issue_id = i.id WHERE i.team_key = ?1 AND i.workspace_id = ?2",
                    rusqlite::params![team, workspace_id],
                    |row| row.get(0),
                )?
            } else {
                conn.query_row(
                    "SELECT COUNT(DISTINCT c.issue_id) FROM chunks c JOIN issues i ON c.issue_id = i.id WHERE i.workspace_id = ?1",
                    rusqlite::params![workspace_id],
                    |row| row.get(0),
                )?
            };
            Ok(count)
        })
    }

    pub fn get_issues_needing_embedding(
        &self,
        team_key: Option<&str>,
        force: bool,
        workspace_id: &str,
    ) -> Result<Vec<Issue>> {
        self.with_conn(|conn| {
            let sql = if force {
                if let Some(team) = team_key {
                    format!(
                        "SELECT id, identifier, team_key, title, description, state_name, state_type, priority, assignee_name, project_name, labels_json, created_at, updated_at, content_hash, synced_at, url, branch_name, workspace_id
                         FROM issues WHERE team_key = '{}' AND workspace_id = '{}'", team, workspace_id
                    )
                } else {
                    format!(
                        "SELECT id, identifier, team_key, title, description, state_name, state_type, priority, assignee_name, project_name, labels_json, created_at, updated_at, content_hash, synced_at, url, branch_name, workspace_id
                         FROM issues WHERE workspace_id = '{}'", workspace_id
                    )
                }
            } else {
                let team_filter = if let Some(team) = team_key {
                    format!("AND i.team_key = '{}'", team)
                } else {
                    String::new()
                };
                format!(
                    "SELECT i.id, i.identifier, i.team_key, i.title, i.description, i.state_name, i.state_type, i.priority, i.assignee_name, i.project_name, i.labels_json, i.created_at, i.updated_at, i.content_hash, i.synced_at, i.url, i.branch_name, i.workspace_id
                     FROM issues i
                     LEFT JOIN (SELECT DISTINCT issue_id FROM chunks) c ON i.id = c.issue_id
                     WHERE c.issue_id IS NULL AND i.workspace_id = '{}' {}",
                    workspace_id, team_filter
                )
            };
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map([], |row| {
                Ok(Issue::from_row(row).unwrap())
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    // --- Comments ---

    pub fn get_comments(&self, issue_id: &str) -> Result<Vec<Comment>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, issue_id, body, user_name, created_at FROM comments WHERE issue_id = ?1 ORDER BY created_at"
            )?;
            let rows = stmt.query_map(rusqlite::params![issue_id], |row| {
                Ok(Comment {
                    id: row.get(0)?,
                    issue_id: row.get(1)?,
                    body: row.get(2)?,
                    user_name: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    // --- Sync state ---

    pub fn get_sync_cursor(&self, workspace_id: &str, team_key: &str) -> Result<Option<String>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT last_updated_at FROM sync_state WHERE workspace_id = ?1 AND team_key = ?2",
            )?;
            let mut rows = stmt.query(rusqlite::params![workspace_id, team_key])?;
            if let Some(row) = rows.next()? {
                Ok(Some(row.get(0)?))
            } else {
                Ok(None)
            }
        })
    }

    pub fn set_sync_cursor(
        &self,
        workspace_id: &str,
        team_key: &str,
        last_updated_at: &str,
    ) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO sync_state (workspace_id, team_key, last_updated_at, full_sync_done, last_synced_at)
                 VALUES (?1, ?2, ?3, 1, datetime('now'))
                 ON CONFLICT(workspace_id, team_key) DO UPDATE SET last_updated_at=excluded.last_updated_at, full_sync_done=1, last_synced_at=datetime('now')",
                rusqlite::params![workspace_id, team_key, last_updated_at],
            )?;
            Ok(())
        })
    }

    pub fn is_full_sync_done(&self, workspace_id: &str, team_key: &str) -> Result<bool> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT full_sync_done FROM sync_state WHERE workspace_id = ?1 AND team_key = ?2",
            )?;
            let mut rows = stmt.query(rusqlite::params![workspace_id, team_key])?;
            if let Some(row) = rows.next()? {
                let done: bool = row.get(0)?;
                Ok(done)
            } else {
                Ok(false)
            }
        })
    }

    /// Get the wall-clock time of the last sync for a team.
    pub fn get_last_synced_at(&self, workspace_id: &str, team_key: &str) -> Result<Option<String>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT last_synced_at FROM sync_state WHERE workspace_id = ?1 AND team_key = ?2",
            )?;
            let mut rows = stmt.query(rusqlite::params![workspace_id, team_key])?;
            if let Some(row) = rows.next()? {
                Ok(row.get(0)?)
            } else {
                Ok(None)
            }
        })
    }

    // --- Metadata ---

    pub fn get_metadata(&self, key: &str) -> Result<Option<String>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT value FROM metadata WHERE key = ?1")?;
            let mut rows = stmt.query(rusqlite::params![key])?;
            if let Some(row) = rows.next()? {
                Ok(Some(row.get(0)?))
            } else {
                Ok(None)
            }
        })
    }

    pub fn set_metadata(&self, key: &str, value: &str) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO metadata (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                rusqlite::params![key, value],
            )?;
            Ok(())
        })
    }

    // --- FTS search ---

    /// List teams that have synced issues, with issue and embedding counts.
    /// Local-only query — no network required.
    pub fn list_synced_teams(&self, workspace_id: &str) -> Result<Vec<TeamSummary>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT i.team_key,
                        COUNT(DISTINCT i.id) AS issue_count,
                        COUNT(DISTINCT c.issue_id) AS embedded_count,
                        s.last_synced_at
                 FROM issues i
                 LEFT JOIN chunks c ON i.id = c.issue_id
                 LEFT JOIN sync_state s ON i.team_key = s.team_key AND s.workspace_id = ?1
                 WHERE i.workspace_id = ?1
                 GROUP BY i.team_key
                 ORDER BY i.team_key",
            )?;
            let rows = stmt.query_map(rusqlite::params![workspace_id], |row| {
                Ok(TeamSummary {
                    key: row.get(0)?,
                    issue_count: row.get(1)?,
                    embedded_count: row.get(2)?,
                    last_synced_at: row.get(3)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    pub fn fts_search(
        &self,
        query: &str,
        limit: usize,
        workspace_id: &str,
    ) -> Result<Vec<FtsResult>> {
        self.fts_search_filtered(query, limit, workspace_id, None)
    }

    pub fn fts_search_filtered(
        &self,
        query: &str,
        limit: usize,
        workspace_id: &str,
        label_ids: Option<&[String]>,
    ) -> Result<Vec<FtsResult>> {
        self.with_conn(|conn| {
            let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![
                Box::new(query.to_string()),
                Box::new(limit as i64),
                Box::new(workspace_id.to_string()),
            ];

            let label_clause = if let Some(ids) = label_ids.filter(|ids| !ids.is_empty()) {
                let (frag, mut lp) = Self::label_filter_fragment(ids, params.len() + 1, "i");
                params.append(&mut lp);
                format!(" AND {frag}")
            } else {
                String::new()
            };

            let sql = format!(
                "SELECT i.id, i.identifier, i.title, i.state_name, i.priority, bm25(issues_fts) as rank
                 FROM issues_fts f
                 JOIN issues i ON f.rowid = i.rowid
                 WHERE issues_fts MATCH ?1 AND i.workspace_id = ?3{label_clause}
                 ORDER BY rank
                 LIMIT ?2"
            );
            let mut stmt = conn.prepare(&sql)?;
            let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
            let rows = stmt.query_map(param_refs.as_slice(), |row| {
                Ok(FtsResult {
                    issue_id: row.get(0)?,
                    identifier: row.get(1)?,
                    title: row.get(2)?,
                    state_name: row.get(3)?,
                    priority: row.get(4)?,
                    bm25_score: row.get(5)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }
}

// --- Data types ---

fn default_workspace_id() -> String {
    "default".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
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
    pub labels_json: String,
    pub created_at: String,
    pub updated_at: String,
    pub content_hash: String,
    pub synced_at: Option<String>,
    pub url: String,
    pub branch_name: Option<String>,
    #[serde(default = "default_workspace_id")]
    pub workspace_id: String,
}

impl Issue {
    pub fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            identifier: row.get(1)?,
            team_key: row.get(2)?,
            title: row.get(3)?,
            description: row.get(4)?,
            state_name: row.get(5)?,
            state_type: row.get(6)?,
            priority: row.get(7)?,
            assignee_name: row.get(8)?,
            project_name: row.get(9)?,
            labels_json: row.get(10)?,
            created_at: row.get(11)?,
            updated_at: row.get(12)?,
            content_hash: row.get(13)?,
            synced_at: row.get(14)?,
            url: row.get(15)?,
            branch_name: row.get(16).unwrap_or(None),
            workspace_id: row.get(17).unwrap_or_else(|_| "default".to_string()),
        })
    }

    pub fn labels(&self) -> Vec<String> {
        serde_json::from_str(&self.labels_json).unwrap_or_default()
    }

    pub fn priority_label(&self) -> &str {
        match self.priority {
            0 => "No priority",
            1 => "Urgent",
            2 => "High",
            3 => "Medium",
            4 => "Low",
            _ => "Unknown",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relation {
    pub id: String,
    pub issue_id: String,
    pub related_issue_id: String,
    pub related_issue_identifier: String,
    pub relation_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichedRelation {
    pub relation_id: String,
    pub relation_type: String,
    pub issue_identifier: String,
    pub issue_title: String,
    pub issue_state: String,
    pub issue_url: String,
}

#[derive(Debug, Clone)]
pub struct Chunk {
    pub issue_id: String,
    pub embedding: Vec<u8>,
    pub identifier: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    pub issue_id: String,
    pub body: String,
    pub user_name: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct FtsResult {
    pub issue_id: String,
    pub identifier: String,
    pub title: String,
    pub state_name: String,
    pub priority: i32,
    pub bm25_score: f64,
}

#[derive(Debug, Clone)]
pub struct IssueSummary {
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

#[derive(Debug, Clone)]
pub struct TeamSummary {
    pub key: String,
    pub issue_count: usize,
    pub embedded_count: usize,
    pub last_synced_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceRow {
    pub id: String,
    pub linear_org_id: Option<String>,
    pub display_name: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Label {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub color: Option<String>,
    pub parent_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::test_helpers::*;

    #[test]
    fn count_embedded_issues_empty_db() {
        let (db, _dir) = test_db();
        assert_eq!(db.count_embedded_issues(None, "default").unwrap(), 0);
    }

    #[test]
    fn count_embedded_issues_with_data() {
        let (db, _dir) = test_db();

        let issue1 = make_issue("TST-1", "TST");
        let issue2 = make_issue("TST-2", "TST");
        let issue3 = make_issue("OTH-1", "OTH");
        db.upsert_issue(&issue1).unwrap();
        db.upsert_issue(&issue2).unwrap();
        db.upsert_issue(&issue3).unwrap();

        // Only issue1 and issue3 have embeddings
        db.upsert_chunks(&issue1.id, &[(0, "chunk".into(), fake_embedding(768))])
            .unwrap();
        db.upsert_chunks(&issue3.id, &[(0, "chunk".into(), fake_embedding(768))])
            .unwrap();

        // Global count
        assert_eq!(db.count_embedded_issues(None, "default").unwrap(), 2);
        // Team filter
        assert_eq!(db.count_embedded_issues(Some("TST"), "default").unwrap(), 1);
        assert_eq!(db.count_embedded_issues(Some("OTH"), "default").unwrap(), 1);
        assert_eq!(
            db.count_embedded_issues(Some("NONE"), "default").unwrap(),
            0
        );
    }

    #[test]
    fn get_field_completeness_empty_db() {
        let (db, _dir) = test_db();
        let (total, desc, pri, labels, proj) = db.get_field_completeness(None, "default").unwrap();
        assert_eq!(total, 0);
        assert_eq!(desc, 0);
        assert_eq!(pri, 0);
        assert_eq!(labels, 0);
        assert_eq!(proj, 0);
    }

    #[test]
    fn get_field_completeness_with_data() {
        let (db, _dir) = test_db();

        // Issue with all fields
        let mut full = make_issue("TST-1", "TST");
        full.description = Some("Has desc".into());
        full.priority = 2;
        full.labels_json = r#"["bug"]"#.into();
        full.project_name = Some("Proj".into());
        db.upsert_issue(&full).unwrap();

        // Issue with no optional fields
        let mut sparse = make_issue("TST-2", "TST");
        sparse.description = None;
        sparse.priority = 0;
        sparse.labels_json = "[]".into();
        sparse.project_name = None;
        db.upsert_issue(&sparse).unwrap();

        // Issue on different team
        let mut other = make_issue("OTH-1", "OTH");
        other.description = Some("Has desc".into());
        other.priority = 0;
        other.labels_json = "[]".into();
        other.project_name = None;
        db.upsert_issue(&other).unwrap();

        // Global
        let (total, desc, pri, labels, proj) = db.get_field_completeness(None, "default").unwrap();
        assert_eq!(total, 3);
        assert_eq!(desc, 2); // full + other
        assert_eq!(pri, 1); // full only
        assert_eq!(labels, 1); // full only
        assert_eq!(proj, 1); // full only

        // Team filter
        let (total, desc, pri, labels, proj) =
            db.get_field_completeness(Some("TST"), "default").unwrap();
        assert_eq!(total, 2);
        assert_eq!(desc, 1);
        assert_eq!(pri, 1);
        assert_eq!(labels, 1);
        assert_eq!(proj, 1);
    }

    #[test]
    fn list_all_issues_pagination_and_filter() {
        let (db, _dir) = test_db();

        for i in 1..=5 {
            let mut issue = make_issue(&format!("TST-{i}"), "TST");
            issue.updated_at = format!("2026-01-0{i}T00:00:00Z");
            db.upsert_issue(&issue).unwrap();
        }
        let mut other = make_issue("OTH-1", "OTH");
        other.updated_at = "2026-01-06T00:00:00Z".to_string();
        db.upsert_issue(&other).unwrap();

        // All issues, first page
        let page1 = db.list_all_issues(None, None, 3, 0, "default").unwrap();
        assert_eq!(page1.len(), 3);
        // Ordered by updated_at DESC — OTH-1 is newest
        assert_eq!(page1[0].identifier, "OTH-1");

        // Second page
        let page2 = db.list_all_issues(None, None, 3, 3, "default").unwrap();
        assert_eq!(page2.len(), 3);

        // Third page (empty)
        let page3 = db.list_all_issues(None, None, 3, 6, "default").unwrap();
        assert_eq!(page3.len(), 0);

        // Team filter
        let tst = db
            .list_all_issues(Some("TST"), None, 10, 0, "default")
            .unwrap();
        assert_eq!(tst.len(), 5);

        // Text filter
        let filtered = db
            .list_all_issues(None, Some("TST-3"), 10, 0, "default")
            .unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].identifier, "TST-3");

        // Title filter
        let title_match = db
            .list_all_issues(None, Some("Test issue OTH"), 10, 0, "default")
            .unwrap();
        assert_eq!(title_match.len(), 1);
    }

    #[test]
    fn list_all_issues_has_embedding_flag() {
        let (db, _dir) = test_db();

        let issue1 = make_issue("TST-1", "TST");
        let issue2 = make_issue("TST-2", "TST");
        db.upsert_issue(&issue1).unwrap();
        db.upsert_issue(&issue2).unwrap();

        // Only issue1 gets an embedding
        db.upsert_chunks(&issue1.id, &[(0, "chunk".into(), fake_embedding(768))])
            .unwrap();

        let issues = db.list_all_issues(None, None, 10, 0, "default").unwrap();
        let by_id: std::collections::HashMap<_, _> =
            issues.iter().map(|i| (i.identifier.as_str(), i)).collect();

        assert!(by_id["TST-1"].has_embedding);
        assert!(!by_id["TST-2"].has_embedding);
    }

    #[test]
    fn list_synced_teams_empty_db() {
        let (db, _dir) = test_db();
        let teams = db.list_synced_teams("default").unwrap();
        assert!(teams.is_empty());
    }

    #[test]
    fn list_synced_teams_with_data() {
        let (db, _dir) = test_db();

        // 3 issues on TST, 1 on OTH
        for i in 1..=3 {
            let issue = make_issue(&format!("TST-{i}"), "TST");
            db.upsert_issue(&issue).unwrap();
            if i <= 2 {
                // Embed first 2
                db.upsert_chunks(&issue.id, &[(0, "chunk".into(), fake_embedding(768))])
                    .unwrap();
            }
        }
        let other = make_issue("OTH-1", "OTH");
        db.upsert_issue(&other).unwrap();

        let teams = db.list_synced_teams("default").unwrap();
        assert_eq!(teams.len(), 2);

        // Sorted by team_key
        let by_key: std::collections::HashMap<_, _> =
            teams.iter().map(|t| (t.key.as_str(), t)).collect();

        assert_eq!(by_key["TST"].issue_count, 3);
        assert_eq!(by_key["TST"].embedded_count, 2);
        assert_eq!(by_key["OTH"].issue_count, 1);
        assert_eq!(by_key["OTH"].embedded_count, 0);
    }

    #[test]
    fn list_synced_teams_includes_last_synced_at() {
        let (db, _dir) = test_db();

        let issue = make_issue("TST-1", "TST");
        db.upsert_issue(&issue).unwrap();

        // Before any sync, last_synced_at should be None
        let teams = db.list_synced_teams("default").unwrap();
        assert_eq!(teams.len(), 1);
        assert!(teams[0].last_synced_at.is_none());

        // After setting sync cursor, last_synced_at should be set
        db.set_sync_cursor("default", "TST", "2026-01-01T00:00:00Z")
            .unwrap();
        let teams = db.list_synced_teams("default").unwrap();
        assert!(teams[0].last_synced_at.is_some());
    }

    #[test]
    fn list_synced_teams_multi_chunk_issue() {
        let (db, _dir) = test_db();

        let issue = make_issue("TST-1", "TST");
        db.upsert_issue(&issue).unwrap();
        // Insert multiple chunks for the same issue — count should still be 1
        db.upsert_chunks(
            &issue.id,
            &[
                (0, "chunk0".into(), fake_embedding(768)),
                (1, "chunk1".into(), fake_embedding(768)),
                (2, "chunk2".into(), fake_embedding(768)),
            ],
        )
        .unwrap();

        let teams = db.list_synced_teams("default").unwrap();
        assert_eq!(teams.len(), 1);
        assert_eq!(teams[0].issue_count, 1); // not 3
        assert_eq!(teams[0].embedded_count, 1);
    }

    #[test]
    fn workspace_crud() {
        let (db, _dir) = test_db();

        // Default workspace exists from migration
        let ws = db.get_workspace("default").unwrap();
        assert!(ws.is_some());

        // Upsert a new workspace
        db.upsert_workspace("work", None, None).unwrap();
        let ws = db.get_workspace("work").unwrap().unwrap();
        assert_eq!(ws.id, "work");
        assert!(ws.linear_org_id.is_none());

        // Update with org info
        db.upsert_workspace("work", Some("org-123"), Some("Work Org"))
            .unwrap();
        let ws = db.get_workspace("work").unwrap().unwrap();
        assert_eq!(ws.linear_org_id.as_deref(), Some("org-123"));
        assert_eq!(ws.display_name.as_deref(), Some("Work Org"));

        // List all
        let all = db.list_workspaces().unwrap();
        assert_eq!(all.len(), 2);

        // Delete
        db.delete_workspace("work").unwrap();
        let ws = db.get_workspace("work").unwrap();
        assert!(ws.is_none());
    }

    #[test]
    fn issues_isolated_by_workspace() {
        let (db, _dir) = test_db();

        // Create second workspace
        db.upsert_workspace("work", None, None).unwrap();

        // Insert issue in default workspace
        let mut issue1 = make_issue("TST-1", "TST");
        issue1.workspace_id = "default".to_string();
        issue1.priority = 0;
        db.upsert_issue(&issue1).unwrap();

        // Insert issue in work workspace
        let mut issue2 = make_issue("TST-2", "TST");
        issue2.id = "id-2".to_string();
        issue2.workspace_id = "work".to_string();
        issue2.priority = 0;
        db.upsert_issue(&issue2).unwrap();

        // Count scoped to each workspace
        assert_eq!(db.count_issues(None, "default").unwrap(), 1);
        assert_eq!(db.count_issues(None, "work").unwrap(), 1);

        // Unprioritized scoped
        let default_unpri = db.get_unprioritized_issues(None, false, "default").unwrap();
        assert_eq!(default_unpri.len(), 1);
        assert_eq!(default_unpri[0].identifier, "TST-1");

        let work_unpri = db.get_unprioritized_issues(None, false, "work").unwrap();
        assert_eq!(work_unpri.len(), 1);
        assert_eq!(work_unpri[0].identifier, "TST-2");
    }

    #[test]
    fn sync_state_isolated_by_workspace() {
        let (db, _dir) = test_db();
        db.upsert_workspace("work", None, None).unwrap();

        // Set cursor for same team in different workspaces
        db.set_sync_cursor("default", "TST", "2024-01-01T00:00:00Z")
            .unwrap();
        db.set_sync_cursor("work", "TST", "2024-06-01T00:00:00Z")
            .unwrap();

        assert_eq!(
            db.get_sync_cursor("default", "TST").unwrap().as_deref(),
            Some("2024-01-01T00:00:00Z")
        );
        assert_eq!(
            db.get_sync_cursor("work", "TST").unwrap().as_deref(),
            Some("2024-06-01T00:00:00Z")
        );

        assert!(db.is_full_sync_done("default", "TST").unwrap());
        assert!(db.is_full_sync_done("work", "TST").unwrap());
        assert!(!db.is_full_sync_done("default", "OTHER").unwrap());
    }

    #[test]
    fn list_synced_teams_workspace_scoped() {
        let (db, _dir) = test_db();
        db.upsert_workspace("work", None, None).unwrap();

        let mut issue1 = make_issue("TST-1", "TST");
        issue1.workspace_id = "default".to_string();
        db.upsert_issue(&issue1).unwrap();

        let mut issue2 = make_issue("WRK-1", "WRK");
        issue2.id = "id-wrk".to_string();
        issue2.workspace_id = "work".to_string();
        db.upsert_issue(&issue2).unwrap();

        let default_teams = db.list_synced_teams("default").unwrap();
        assert_eq!(default_teams.len(), 1);
        assert_eq!(default_teams[0].key, "TST");

        let work_teams = db.list_synced_teams("work").unwrap();
        assert_eq!(work_teams.len(), 1);
        assert_eq!(work_teams[0].key, "WRK");
    }

    #[test]
    fn migration_8_creates_label_tables_and_resets_sync_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let conn = rusqlite::Connection::open(&path).unwrap();

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .unwrap();

        // Run migrations 1-7 only
        crate::db::schema::run_migrations(&conn).unwrap();

        // Delete from schema_version to simulate being at version 7
        conn.execute("DELETE FROM schema_version WHERE version = 8", [])
            .unwrap();

        // Seed sync_state as if a prior sync had completed
        conn.execute(
            "INSERT INTO sync_state (workspace_id, team_key, last_updated_at, full_sync_done, last_synced_at)
             VALUES ('default', 'ENG', '2026-04-01T00:00:00Z', 1, '2026-04-01T00:00:00Z')",
            [],
        )
        .unwrap();

        // Verify the sync_state row before migration 8
        let full_done_before: i64 = conn
            .query_row(
                "SELECT full_sync_done FROM sync_state WHERE workspace_id='default' AND team_key='ENG'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(full_done_before, 1);

        // Run migrations again — migration 8 should now run
        crate::db::schema::run_migrations(&conn).unwrap();

        // Tables exist
        let labels_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='labels'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(labels_count, 1);
        let join_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='issue_labels'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(join_count, 1);

        // sync_state reset
        let full_done: i64 = conn
            .query_row(
                "SELECT full_sync_done FROM sync_state WHERE workspace_id='default' AND team_key='ENG'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(full_done, 0);
        let last_updated: String = conn
            .query_row(
                "SELECT last_updated_at FROM sync_state WHERE workspace_id='default' AND team_key='ENG'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(last_updated, "1970-01-01T00:00:00Z");
    }

    #[test]
    fn upsert_label_inserts_and_renames_in_place() {
        use super::test_helpers::{test_db, make_label};
        let (db, _dir) = test_db();

        let mut l = make_label("lbl_1", "Vanta", "default");
        db.upsert_label(&l).unwrap();

        let listed = db.list_labels("default").unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "Vanta");

        // Rename — same id, new name
        l.name = "Compliance".to_string();
        db.upsert_label(&l).unwrap();

        let listed = db.list_labels("default").unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "Compliance");
    }

    #[test]
    fn list_labels_is_workspace_scoped_and_sorted() {
        use super::test_helpers::{test_db, make_label};
        let (db, _dir) = test_db();
        db.upsert_workspace("work", None, None).unwrap();

        db.upsert_label(&make_label("a", "Zebra", "default")).unwrap();
        db.upsert_label(&make_label("b", "Apple", "default")).unwrap();
        db.upsert_label(&make_label("c", "OnlyInWork", "work")).unwrap();

        let default_labels = db.list_labels("default").unwrap();
        assert_eq!(default_labels.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(),
                   vec!["Apple", "Zebra"]);
        let work_labels = db.list_labels("work").unwrap();
        assert_eq!(work_labels.len(), 1);
        assert_eq!(work_labels[0].name, "OnlyInWork");
    }

    #[test]
    fn delete_labels_for_workspace_not_in_removes_orphans() {
        use super::test_helpers::{test_db, make_label};
        let (db, _dir) = test_db();

        db.upsert_label(&make_label("keep", "Keep", "default")).unwrap();
        db.upsert_label(&make_label("drop", "Drop", "default")).unwrap();

        let kept = db.delete_labels_for_workspace_not_in("default", &["keep".to_string()]).unwrap();
        assert_eq!(kept, 1, "should report 1 deleted");

        let listed = db.list_labels("default").unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "Keep");
    }

    #[test]
    fn replace_issue_labels_overwrites_existing() {
        use super::test_helpers::{test_db, make_issue, make_label};
        let (db, _dir) = test_db();

        let issue = make_issue("ENG-1", "ENG");
        db.upsert_issue(&issue).unwrap();
        db.upsert_label(&make_label("l1", "Bug", "default")).unwrap();
        db.upsert_label(&make_label("l2", "UI", "default")).unwrap();
        db.upsert_label(&make_label("l3", "Backend", "default")).unwrap();

        db.replace_issue_labels(&issue.id, &["l1".to_string(), "l2".to_string()]).unwrap();
        let labels = db.get_issue_label_ids(&issue.id).unwrap();
        assert_eq!(labels, vec!["l1".to_string(), "l2".to_string()]);

        // Replace overwrites
        db.replace_issue_labels(&issue.id, &["l3".to_string()]).unwrap();
        let labels = db.get_issue_label_ids(&issue.id).unwrap();
        assert_eq!(labels, vec!["l3".to_string()]);
    }

    #[test]
    fn deleting_issue_cascades_to_issue_labels() {
        use super::test_helpers::{test_db, make_issue, make_label};
        let (db, _dir) = test_db();

        let issue = make_issue("ENG-2", "ENG");
        db.upsert_issue(&issue).unwrap();
        db.upsert_label(&make_label("l1", "Bug", "default")).unwrap();
        db.replace_issue_labels(&issue.id, &["l1".to_string()]).unwrap();

        db.with_conn(|conn| {
            conn.execute("DELETE FROM issues WHERE id = ?1", rusqlite::params![&issue.id])?;
            let n: i64 = conn.query_row(
                "SELECT COUNT(*) FROM issue_labels WHERE issue_id = ?1",
                rusqlite::params![&issue.id], |r| r.get(0))?;
            assert_eq!(n, 0);
            Ok(())
        }).unwrap();
    }

    #[test]
    fn deleting_label_cascades_to_issue_labels() {
        use super::test_helpers::{test_db, make_issue, make_label};
        let (db, _dir) = test_db();

        let issue = make_issue("ENG-3", "ENG");
        db.upsert_issue(&issue).unwrap();
        db.upsert_label(&make_label("l1", "Bug", "default")).unwrap();
        db.replace_issue_labels(&issue.id, &["l1".to_string()]).unwrap();

        db.delete_labels_for_workspace_not_in("default", &[]).unwrap();
        let labels = db.get_issue_label_ids(&issue.id).unwrap();
        assert!(labels.is_empty());
    }

    #[test]
    fn resolve_label_ids_local_matches_case_insensitive_and_returns_unknowns() {
        use super::test_helpers::{test_db, make_label};
        let (db, _dir) = test_db();

        db.upsert_label(&make_label("l1", "Vanta", "default")).unwrap();
        db.upsert_label(&make_label("l2", "Security", "default")).unwrap();

        let (resolved, unknown) = db
            .resolve_label_ids_local("default", &["vanta".to_string(), "secURity".to_string(), "missing".to_string()])
            .unwrap();
        assert_eq!(resolved.len(), 2);
        assert!(resolved.contains(&"l1".to_string()));
        assert!(resolved.contains(&"l2".to_string()));
        assert_eq!(unknown, vec!["missing".to_string()]);
    }

    #[test]
    fn resolve_label_ids_local_is_workspace_scoped() {
        use super::test_helpers::{test_db, make_label};
        let (db, _dir) = test_db();
        db.upsert_workspace("work", None, None).unwrap();

        db.upsert_label(&make_label("l1", "Vanta", "default")).unwrap();
        db.upsert_label(&make_label("l2", "Vanta", "work")).unwrap();

        let (resolved, _) = db.resolve_label_ids_local("work", &["vanta".to_string()]).unwrap();
        assert_eq!(resolved, vec!["l2".to_string()]);
    }

    #[test]
    fn get_unprioritized_issues_filters_by_labels_with_and_semantics() {
        use super::test_helpers::{test_db, make_issue, make_label};
        let (db, _dir) = test_db();

        let mut a = make_issue("ENG-10", "ENG"); a.priority = 0;
        let mut b = make_issue("ENG-11", "ENG"); b.priority = 0;
        let mut c = make_issue("ENG-12", "ENG"); c.priority = 0;
        db.upsert_issue(&a).unwrap();
        db.upsert_issue(&b).unwrap();
        db.upsert_issue(&c).unwrap();

        db.upsert_label(&make_label("vanta", "Vanta", "default")).unwrap();
        db.upsert_label(&make_label("sec",   "Security", "default")).unwrap();

        db.replace_issue_labels(&a.id, &["vanta".to_string(), "sec".to_string()]).unwrap();
        db.replace_issue_labels(&b.id, &["vanta".to_string()]).unwrap();
        db.replace_issue_labels(&c.id, &["sec".to_string()]).unwrap();

        // Filter by both labels (AND) → only `a`
        let result = db.get_unprioritized_issues_filtered(
            Some("ENG"), false, "default",
            Some(&["vanta".to_string(), "sec".to_string()]),
        ).unwrap();
        let idents: Vec<_> = result.iter().map(|i| i.identifier.as_str()).collect();
        assert_eq!(idents, vec!["ENG-10"]);

        // Filter by single label → `a` and `b`
        let result = db.get_unprioritized_issues_filtered(
            Some("ENG"), false, "default",
            Some(&["vanta".to_string()]),
        ).unwrap();
        let idents: Vec<_> = result.iter().map(|i| i.identifier.as_str()).collect();
        assert!(idents.contains(&"ENG-10"));
        assert!(idents.contains(&"ENG-11"));
        assert!(!idents.contains(&"ENG-12"));

        // No filter → all three
        let result = db.get_unprioritized_issues_filtered(Some("ENG"), false, "default", None).unwrap();
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn fts_search_with_label_filter_intersects() {
        use super::test_helpers::{test_db, make_issue, make_label};
        let (db, _dir) = test_db();

        let mut a = make_issue("ENG-20", "ENG");
        a.title = "Audit logging gap".to_string();
        let mut b = make_issue("ENG-21", "ENG");
        b.title = "Audit something else".to_string();
        db.upsert_issue(&a).unwrap();
        db.upsert_issue(&b).unwrap();

        db.upsert_label(&make_label("vanta", "Vanta", "default")).unwrap();
        db.replace_issue_labels(&a.id, &["vanta".to_string()]).unwrap();

        // Without filter, both match "audit"
        let r = db.fts_search_filtered("\"audit\"", 10, "default", None).unwrap();
        assert_eq!(r.len(), 2);

        // With Vanta filter, only `a`
        let r = db.fts_search_filtered("\"audit\"", 10, "default", Some(&["vanta".to_string()])).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].identifier, "ENG-20");
    }

    #[test]
    fn delete_workspace_cleans_up_labels() {
        use super::test_helpers::{test_db, make_label};
        let (db, _dir) = test_db();
        db.upsert_workspace("doomed", None, None).unwrap();
        db.upsert_label(&make_label("l1", "Lab", "doomed")).unwrap();
        db.delete_workspace("doomed").unwrap();
        let listed = db.list_labels("doomed").unwrap();
        assert!(listed.is_empty());
    }
}
