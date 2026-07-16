use anyhow::Result;
use rusqlite::Connection;

pub fn run_migrations(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (
            version INTEGER PRIMARY KEY
        );",
    )?;

    let current_version: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_version",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    if current_version < 1 {
        conn.execute_batch(MIGRATION_1)?;
        conn.execute("INSERT INTO schema_version (version) VALUES (1)", [])?;
    }

    if current_version < 2 {
        conn.execute_batch(MIGRATION_2)?;
        conn.execute("INSERT INTO schema_version (version) VALUES (2)", [])?;
    }

    if current_version < 3 {
        conn.execute_batch(MIGRATION_3)?;
        conn.execute("INSERT INTO schema_version (version) VALUES (3)", [])?;
    }

    if current_version < 4 {
        conn.execute_batch(MIGRATION_4)?;
        conn.execute("INSERT INTO schema_version (version) VALUES (4)", [])?;
    }

    if current_version < 5 {
        conn.execute_batch(MIGRATION_5)?;
        conn.execute("INSERT INTO schema_version (version) VALUES (5)", [])?;
    }

    if current_version < 6 {
        conn.execute_batch(MIGRATION_6)?;
        conn.execute("INSERT INTO schema_version (version) VALUES (6)", [])?;
    }

    if current_version < 7 {
        conn.execute_batch(MIGRATION_7)?;
        conn.execute("INSERT INTO schema_version (version) VALUES (7)", [])?;
    }

    if current_version < 8 {
        conn.execute_batch(MIGRATION_8)?;
        conn.execute("INSERT INTO schema_version (version) VALUES (8)", [])?;
    }

    if current_version < 9 {
        run_migration_9(conn)?;
        conn.execute("INSERT INTO schema_version (version) VALUES (9)", [])?;
    }

    if current_version < 10 {
        run_migration_10(conn)?;
        conn.execute("INSERT INTO schema_version (version) VALUES (10)", [])?;
    }

    Ok(())
}

fn run_migration_10(conn: &Connection) -> Result<()> {
    conn.execute_batch(MIGRATION_10)?;
    add_column_if_missing(conn, "issues", "project_id", "TEXT")?;
    add_column_if_missing(conn, "issues", "project_milestone_id", "TEXT")?;
    add_column_if_missing(conn, "issues", "project_milestone_name", "TEXT")?;
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_issues_project ON issues(workspace_id, project_id);
         CREATE INDEX IF NOT EXISTS idx_issues_project_milestone
             ON issues(workspace_id, project_milestone_id);",
    )?;
    Ok(())
}

fn run_migration_9(conn: &Connection) -> Result<()> {
    add_column_if_missing(conn, "comments", "updated_at", "TEXT")?;
    add_column_if_missing(conn, "comments", "parent_id", "TEXT")?;
    add_column_if_missing(conn, "comments", "url", "TEXT")?;
    conn.execute_batch(MIGRATION_9)?;
    Ok(())
}

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for existing in columns {
        if existing? == column {
            return Ok(());
        }
    }
    conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {definition};"))?;
    Ok(())
}

const MIGRATION_9: &str = "
-- Preserve Linear comment thread metadata and track whether comments were
-- actually refreshed for an issue.
CREATE TABLE IF NOT EXISTS comment_sync_state (
    issue_id TEXT PRIMARY KEY REFERENCES issues(id) ON DELETE CASCADE,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    status TEXT NOT NULL,
    sync_error TEXT,
    synced_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_comment_sync_workspace ON comment_sync_state(workspace_id);
";

const MIGRATION_10: &str = "
-- Projects and their milestones are first-class cached resources. Linear is
-- still the source of truth; these tables make the hierarchy importable by
-- downstream clients without reconstructing it from issue names.
CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    slug_id TEXT NOT NULL DEFAULT '',
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    content TEXT,
    icon TEXT,
    color TEXT NOT NULL DEFAULT '',
    status_id TEXT NOT NULL DEFAULT '',
    status_name TEXT NOT NULL DEFAULT '',
    status_type TEXT NOT NULL DEFAULT '',
    status_color TEXT NOT NULL DEFAULT '',
    priority INTEGER NOT NULL DEFAULT 0,
    start_date TEXT,
    target_date TEXT,
    lead_id TEXT,
    lead_name TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    archived_at TEXT,
    url TEXT NOT NULL DEFAULT '',
    progress REAL NOT NULL DEFAULT 0,
    synced_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_projects_workspace ON projects(workspace_id);
CREATE INDEX IF NOT EXISTS idx_projects_workspace_name
    ON projects(workspace_id, name COLLATE NOCASE);

CREATE TABLE IF NOT EXISTS project_teams (
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    team_id TEXT NOT NULL,
    team_key TEXT NOT NULL,
    team_name TEXT NOT NULL,
    PRIMARY KEY (project_id, team_id)
);
CREATE INDEX IF NOT EXISTS idx_project_teams_key ON project_teams(team_key);

CREATE TABLE IF NOT EXISTS project_members (
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL,
    user_name TEXT NOT NULL,
    PRIMARY KEY (project_id, user_id)
);

CREATE TABLE IF NOT EXISTS project_labels (
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    label_id TEXT NOT NULL,
    label_name TEXT NOT NULL,
    color TEXT NOT NULL DEFAULT '',
    description TEXT,
    PRIMARY KEY (project_id, label_id)
);
CREATE INDEX IF NOT EXISTS idx_project_labels_name
    ON project_labels(label_name COLLATE NOCASE);

CREATE TABLE IF NOT EXISTS project_milestones (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    description TEXT,
    target_date TEXT,
    status TEXT NOT NULL DEFAULT '',
    progress REAL NOT NULL DEFAULT 0,
    sort_order REAL NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    archived_at TEXT,
    synced_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_project_milestones_project
    ON project_milestones(project_id, sort_order, target_date);
CREATE INDEX IF NOT EXISTS idx_project_milestones_workspace
    ON project_milestones(workspace_id);
";

const MIGRATION_8: &str = "
-- Workspace label catalog
CREATE TABLE IF NOT EXISTS labels (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    name TEXT NOT NULL,
    color TEXT,
    parent_id TEXT,
    UNIQUE (workspace_id, name COLLATE NOCASE)
);
CREATE INDEX IF NOT EXISTS idx_labels_workspace ON labels(workspace_id);

-- Issue ↔ label join table
CREATE TABLE IF NOT EXISTS issue_labels (
    issue_id TEXT NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    label_id TEXT NOT NULL REFERENCES labels(id) ON DELETE CASCADE,
    PRIMARY KEY (issue_id, label_id)
);
CREATE INDEX IF NOT EXISTS idx_issue_labels_label ON issue_labels(label_id);

-- Force full re-sync so issue_labels gets populated for existing issues.
UPDATE sync_state SET full_sync_done = 0, last_updated_at = '1970-01-01T00:00:00Z';
";

const MIGRATION_7: &str = "
-- Workspace registry
CREATE TABLE IF NOT EXISTS workspaces (
    id TEXT PRIMARY KEY,
    linear_org_id TEXT,
    display_name TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Seed the default workspace for existing data
INSERT OR IGNORE INTO workspaces (id) VALUES ('default');

-- Add workspace_id to issues
ALTER TABLE issues ADD COLUMN workspace_id TEXT NOT NULL DEFAULT 'default';

-- Add workspace_id to sync_state (recreate since altering PK is not supported)
CREATE TABLE sync_state_new (
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    team_key TEXT NOT NULL,
    last_updated_at TEXT NOT NULL,
    full_sync_done INTEGER NOT NULL DEFAULT 0,
    last_synced_at TEXT,
    PRIMARY KEY (workspace_id, team_key)
);
INSERT INTO sync_state_new (workspace_id, team_key, last_updated_at, full_sync_done, last_synced_at)
    SELECT 'default', team_key, last_updated_at, full_sync_done, last_synced_at FROM sync_state;
DROP TABLE sync_state;
ALTER TABLE sync_state_new RENAME TO sync_state;

-- Add workspace_id to comments
ALTER TABLE comments ADD COLUMN workspace_id TEXT NOT NULL DEFAULT 'default';

-- New indices
CREATE INDEX IF NOT EXISTS idx_issues_workspace ON issues(workspace_id);
CREATE INDEX IF NOT EXISTS idx_issues_workspace_team ON issues(workspace_id, team_key);
CREATE INDEX IF NOT EXISTS idx_comments_workspace ON comments(workspace_id);
";

// Add branch_name to issues, model_name to chunks
const MIGRATION_6: &str = "
ALTER TABLE issues ADD COLUMN branch_name TEXT;
ALTER TABLE chunks ADD COLUMN model_name TEXT NOT NULL DEFAULT '';
";

// Add last_synced_at to sync_state
const MIGRATION_5: &str = "
ALTER TABLE sync_state ADD COLUMN last_synced_at TEXT;
";

// Add issue_relations table
const MIGRATION_4: &str = "
CREATE TABLE IF NOT EXISTS issue_relations (
    id TEXT PRIMARY KEY,
    issue_id TEXT NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    related_issue_id TEXT NOT NULL,
    related_issue_identifier TEXT NOT NULL,
    relation_type TEXT NOT NULL,
    UNIQUE(issue_id, related_issue_id, relation_type)
);
CREATE INDEX IF NOT EXISTS idx_relations_issue ON issue_relations(issue_id);
CREATE INDEX IF NOT EXISTS idx_relations_related ON issue_relations(related_issue_id);
";

// Add url column to issues
const MIGRATION_3: &str = "
ALTER TABLE issues ADD COLUMN url TEXT NOT NULL DEFAULT '';
";

// Fix contentless FTS5 triggers: use 'delete' command instead of DELETE FROM
const MIGRATION_2: &str = "
DROP TRIGGER IF EXISTS issues_au;
DROP TRIGGER IF EXISTS issues_ad;

-- Rebuild FTS index from scratch (old triggers may have left it corrupt)
DELETE FROM issues_fts;
INSERT INTO issues_fts(rowid, title, description, labels_text)
    SELECT rowid, title, COALESCE(description, ''), labels_json FROM issues;

CREATE TRIGGER issues_au AFTER UPDATE ON issues BEGIN
    INSERT INTO issues_fts(issues_fts, rowid, title, description, labels_text)
    VALUES ('delete', old.rowid, old.title, COALESCE(old.description, ''), old.labels_json);
    INSERT INTO issues_fts(rowid, title, description, labels_text)
    VALUES (new.rowid, new.title, COALESCE(new.description, ''), new.labels_json);
END;

CREATE TRIGGER issues_ad AFTER DELETE ON issues BEGIN
    INSERT INTO issues_fts(issues_fts, rowid, title, description, labels_text)
    VALUES ('delete', old.rowid, old.title, COALESCE(old.description, ''), old.labels_json);
END;
";

const MIGRATION_1: &str = "
CREATE TABLE IF NOT EXISTS issues (
    id TEXT PRIMARY KEY,
    identifier TEXT NOT NULL UNIQUE,
    team_key TEXT NOT NULL,
    title TEXT NOT NULL,
    description TEXT,
    state_name TEXT NOT NULL DEFAULT '',
    state_type TEXT NOT NULL DEFAULT '',
    priority INTEGER NOT NULL DEFAULT 0,
    assignee_name TEXT,
    project_name TEXT,
    labels_json TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    content_hash TEXT NOT NULL DEFAULT '',
    synced_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_issues_team ON issues(team_key);
CREATE INDEX IF NOT EXISTS idx_issues_updated ON issues(updated_at);
CREATE INDEX IF NOT EXISTS idx_issues_identifier ON issues(identifier);

CREATE VIRTUAL TABLE IF NOT EXISTS issues_fts USING fts5(
    title,
    description,
    labels_text,
    content='',
    tokenize='porter unicode61'
);

-- Triggers to keep FTS in sync
CREATE TRIGGER IF NOT EXISTS issues_ai AFTER INSERT ON issues BEGIN
    INSERT INTO issues_fts(rowid, title, description, labels_text)
    VALUES (new.rowid, new.title, COALESCE(new.description, ''), new.labels_json);
END;

CREATE TRIGGER IF NOT EXISTS issues_au AFTER UPDATE ON issues BEGIN
    INSERT INTO issues_fts(issues_fts, rowid, title, description, labels_text)
    VALUES ('delete', old.rowid, old.title, COALESCE(old.description, ''), old.labels_json);
    INSERT INTO issues_fts(rowid, title, description, labels_text)
    VALUES (new.rowid, new.title, COALESCE(new.description, ''), new.labels_json);
END;

CREATE TRIGGER IF NOT EXISTS issues_ad AFTER DELETE ON issues BEGIN
    INSERT INTO issues_fts(issues_fts, rowid, title, description, labels_text)
    VALUES ('delete', old.rowid, old.title, COALESCE(old.description, ''), old.labels_json);
END;

CREATE TABLE IF NOT EXISTS chunks (
    issue_id TEXT NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    chunk_index INTEGER NOT NULL,
    chunk_text TEXT NOT NULL,
    embedding BLOB NOT NULL,
    PRIMARY KEY (issue_id, chunk_index)
);

CREATE TABLE IF NOT EXISTS comments (
    id TEXT PRIMARY KEY,
    issue_id TEXT NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    body TEXT NOT NULL,
    user_name TEXT,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_comments_issue ON comments(issue_id);

CREATE TABLE IF NOT EXISTS sync_state (
    team_key TEXT PRIMARY KEY,
    last_updated_at TEXT NOT NULL,
    full_sync_done INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS metadata (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";
