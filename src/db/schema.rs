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

    Ok(())
}

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
