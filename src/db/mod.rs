pub mod schema;

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};

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

    // --- Issue CRUD ---

    pub fn upsert_issue(&self, issue: &Issue) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO issues (id, identifier, team_key, title, description, state_name, state_type, priority, assignee_name, project_name, labels_json, created_at, updated_at, content_hash, synced_at, url)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, datetime('now'), ?15)
                 ON CONFLICT(id) DO UPDATE SET
                   identifier=excluded.identifier, team_key=excluded.team_key, title=excluded.title,
                   description=excluded.description, state_name=excluded.state_name, state_type=excluded.state_type,
                   priority=excluded.priority, assignee_name=excluded.assignee_name, project_name=excluded.project_name,
                   labels_json=excluded.labels_json, updated_at=excluded.updated_at,
                   content_hash=excluded.content_hash, url=excluded.url, synced_at=datetime('now')",
                rusqlite::params![
                    issue.id, issue.identifier, issue.team_key, issue.title, issue.description,
                    issue.state_name, issue.state_type, issue.priority, issue.assignee_name,
                    issue.project_name, issue.labels_json, issue.created_at, issue.updated_at,
                    issue.content_hash, issue.url,
                ],
            )?;
            Ok(())
        })
    }

    pub fn get_issue(&self, id_or_identifier: &str) -> Result<Option<Issue>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, identifier, team_key, title, description, state_name, state_type, priority, assignee_name, project_name, labels_json, created_at, updated_at, content_hash, synced_at, url
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

    pub fn get_unprioritized_issues(&self, team_key: Option<&str>) -> Result<Vec<Issue>> {
        self.with_conn(|conn| {
            let (sql, params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(team) = team_key {
                (
                    "SELECT id, identifier, team_key, title, description, state_name, state_type, priority, assignee_name, project_name, labels_json, created_at, updated_at, content_hash, synced_at, url
                     FROM issues WHERE priority = 0 AND state_type NOT IN ('completed', 'canceled') AND team_key = ?1
                     ORDER BY created_at DESC".to_string(),
                    vec![Box::new(team.to_string()) as Box<dyn rusqlite::types::ToSql>],
                )
            } else {
                (
                    "SELECT id, identifier, team_key, title, description, state_name, state_type, priority, assignee_name, project_name, labels_json, created_at, updated_at, content_hash, synced_at, url
                     FROM issues WHERE priority = 0 AND state_type NOT IN ('completed', 'canceled')
                     ORDER BY created_at DESC".to_string(),
                    vec![],
                )
            };
            let mut stmt = conn.prepare(&sql)?;
            let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
            let rows = stmt.query_map(param_refs.as_slice(), |row| {
                Ok(Issue::from_row(row).unwrap())
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    pub fn count_issues(&self, team_key: Option<&str>) -> Result<usize> {
        self.with_conn(|conn| {
            let count: usize = if let Some(team) = team_key {
                conn.query_row(
                    "SELECT COUNT(*) FROM issues WHERE team_key = ?1",
                    rusqlite::params![team],
                    |row| row.get(0),
                )?
            } else {
                conn.query_row("SELECT COUNT(*) FROM issues", [], |row| row.get(0))?
            };
            Ok(count)
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
                 WHERE r.issue_id = ?1"
            )?;
            let forward = stmt.query_map(rusqlite::params![issue_id], |row| {
                Ok(EnrichedRelation {
                    relation_id: row.get(0)?,
                    relation_type: row.get(1)?,
                    issue_identifier: row.get(2)?,
                    issue_title: row.get(3)?,
                    issue_state: row.get(4)?,
                    issue_url: row.get(5)?,
                })
            })?.collect::<std::result::Result<Vec<_>, _>>()?;

            // Relations where this issue is the target — flip direction
            let mut stmt2 = conn.prepare(
                "SELECT r.id, r.relation_type, i2.identifier,
                        COALESCE(i2.title, ''), COALESCE(i2.state_name, ''), COALESCE(i2.url, '')
                 FROM issue_relations r
                 JOIN issues i ON r.related_issue_id = i.id
                 JOIN issues i2 ON r.issue_id = i2.id
                 WHERE r.related_issue_id = i.id AND i.id = ?1"
            )?;
            let inverse = stmt2.query_map(rusqlite::params![issue_id], |row| {
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
            })?.collect::<std::result::Result<Vec<_>, _>>()?;

            let mut all = forward;
            all.extend(inverse);
            Ok(all)
        })
    }

    /// Look up a relation ID between two issues (by identifier) for deletion
    pub fn find_relation_id(&self, issue_id: &str, related_issue_id: &str, relation_type: &str) -> Result<Option<String>> {
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
        self.with_conn(|conn| {
            conn.execute(
                "DELETE FROM chunks WHERE issue_id = ?1",
                rusqlite::params![issue_id],
            )?;
            let mut stmt = conn.prepare(
                "INSERT INTO chunks (issue_id, chunk_index, chunk_text, embedding) VALUES (?1, ?2, ?3, ?4)"
            )?;
            for (idx, text, embedding) in chunks {
                stmt.execute(rusqlite::params![issue_id, idx, text, embedding])?;
            }
            Ok(())
        })
    }

    pub fn get_all_chunks(&self) -> Result<Vec<Chunk>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT c.issue_id, c.embedding, i.identifier
                 FROM chunks c JOIN issues i ON c.issue_id = i.id",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(Chunk {
                    issue_id: row.get(0)?,
                    embedding: row.get(1)?,
                    identifier: row.get(2)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    pub fn get_chunks_for_team(&self, team_key: &str) -> Result<Vec<Chunk>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT c.issue_id, c.embedding, i.identifier
                 FROM chunks c JOIN issues i ON c.issue_id = i.id
                 WHERE i.team_key = ?1",
            )?;
            let rows = stmt.query_map(rusqlite::params![team_key], |row| {
                Ok(Chunk {
                    issue_id: row.get(0)?,
                    embedding: row.get(1)?,
                    identifier: row.get(2)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
    }

    pub fn count_embedded_issues(&self, team_key: Option<&str>) -> Result<usize> {
        self.with_conn(|conn| {
            let count: usize = if let Some(team) = team_key {
                conn.query_row(
                    "SELECT COUNT(DISTINCT c.issue_id) FROM chunks c JOIN issues i ON c.issue_id = i.id WHERE i.team_key = ?1",
                    rusqlite::params![team],
                    |row| row.get(0),
                )?
            } else {
                conn.query_row("SELECT COUNT(DISTINCT issue_id) FROM chunks", [], |row| row.get(0))?
            };
            Ok(count)
        })
    }

    pub fn get_issues_needing_embedding(
        &self,
        team_key: Option<&str>,
        force: bool,
    ) -> Result<Vec<Issue>> {
        self.with_conn(|conn| {
            let sql = if force {
                if let Some(team) = team_key {
                    format!(
                        "SELECT id, identifier, team_key, title, description, state_name, state_type, priority, assignee_name, project_name, labels_json, created_at, updated_at, content_hash, synced_at, url
                         FROM issues WHERE team_key = '{}'", team
                    )
                } else {
                    "SELECT id, identifier, team_key, title, description, state_name, state_type, priority, assignee_name, project_name, labels_json, created_at, updated_at, content_hash, synced_at, url
                     FROM issues".to_string()
                }
            } else {
                let team_filter = if let Some(team) = team_key {
                    format!("AND i.team_key = '{}'", team)
                } else {
                    String::new()
                };
                format!(
                    "SELECT i.id, i.identifier, i.team_key, i.title, i.description, i.state_name, i.state_type, i.priority, i.assignee_name, i.project_name, i.labels_json, i.created_at, i.updated_at, i.content_hash, i.synced_at, i.url
                     FROM issues i
                     LEFT JOIN (SELECT DISTINCT issue_id FROM chunks) c ON i.id = c.issue_id
                     WHERE c.issue_id IS NULL {}",
                    team_filter
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

    pub fn get_sync_cursor(&self, team_key: &str) -> Result<Option<String>> {
        self.with_conn(|conn| {
            let mut stmt =
                conn.prepare("SELECT last_updated_at FROM sync_state WHERE team_key = ?1")?;
            let mut rows = stmt.query(rusqlite::params![team_key])?;
            if let Some(row) = rows.next()? {
                Ok(Some(row.get(0)?))
            } else {
                Ok(None)
            }
        })
    }

    pub fn set_sync_cursor(&self, team_key: &str, last_updated_at: &str) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO sync_state (team_key, last_updated_at, full_sync_done)
                 VALUES (?1, ?2, 1)
                 ON CONFLICT(team_key) DO UPDATE SET last_updated_at=excluded.last_updated_at, full_sync_done=1",
                rusqlite::params![team_key, last_updated_at],
            )?;
            Ok(())
        })
    }

    pub fn is_full_sync_done(&self, team_key: &str) -> Result<bool> {
        self.with_conn(|conn| {
            let mut stmt =
                conn.prepare("SELECT full_sync_done FROM sync_state WHERE team_key = ?1")?;
            let mut rows = stmt.query(rusqlite::params![team_key])?;
            if let Some(row) = rows.next()? {
                let done: bool = row.get(0)?;
                Ok(done)
            } else {
                Ok(false)
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

    pub fn fts_search(&self, query: &str, limit: usize) -> Result<Vec<FtsResult>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT i.id, i.identifier, i.title, i.state_name, i.priority, bm25(issues_fts) as rank
                 FROM issues_fts f
                 JOIN issues i ON f.rowid = i.rowid
                 WHERE issues_fts MATCH ?1
                 ORDER BY rank
                 LIMIT ?2"
            )?;
            let rows = stmt.query_map(rusqlite::params![query, limit], |row| {
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
