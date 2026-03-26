# Multi-Workspace Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Support multiple Linear workspaces in a single rectilinear installation with context-based switching and a shared SQLite database.

**Architecture:** Add a `workspace_id` column to all data tables, restructure config to support named workspaces with independent API keys, and add a workspace context resolution layer (env var → persisted state file → config default). The CLI gets a global `--workspace` flag; MCP tools require an explicit `workspace` parameter.

**Tech Stack:** Rust, SQLite (rusqlite 0.32), TOML (toml 0.8), clap 4, rmcp 0.1.5, schemars 0.8

**Spec:** `docs/superpowers/specs/2026-03-25-multi-workspace-design.md`

---

## File Structure

### rectilinear-core (library crate)

| File | Action | Responsibility |
|------|--------|---------------|
| `crates/rectilinear-core/src/config.rs` | Modify | New `WorkspaceConfig` struct, `workspaces` map, backward-compat `[linear]` parsing, workspace resolution |
| `crates/rectilinear-core/src/db/schema.rs` | Modify | Migration 7: `workspaces` table, `workspace_id` column on all data tables |
| `crates/rectilinear-core/src/db/mod.rs` | Modify | Add `workspace_id` param to all query methods, new workspace CRUD methods |
| `crates/rectilinear-core/src/linear/mod.rs` | Modify | Change `LinearClient::new` to accept API key directly, add `fetch_organization` method |
| `crates/rectilinear-core/src/search/mod.rs` | Modify | Thread `workspace_id` through search functions |

### rectilinear (binary crate)

| File | Action | Responsibility |
|------|--------|---------------|
| `src/main.rs` | Modify | Add global `--workspace` flag, workspace resolution, pass workspace context to commands |
| `src/cli/config_cmd.rs` | Modify | Add `add-workspace` / `remove-workspace` subcommands, update `show` and `interactive` |
| `src/cli/workspace_cmd.rs` | Create | `assume`, `list`, `current` subcommands |
| `src/cli/sync_cmd.rs` | Modify | Accept workspace context, pass to LinearClient and DB |
| `src/cli/search_cmd.rs` | Modify | Accept workspace context |
| `src/cli/show_cmd.rs` | Modify | Accept workspace context (for get_issue) |
| `src/cli/create_cmd.rs` | Modify | Accept workspace context |
| `src/cli/append_cmd.rs` | Modify | Accept workspace context |
| `src/cli/embed_cmd.rs` | Modify | Accept workspace context |
| `src/cli/triage_cmd.rs` | Modify | Accept workspace context |
| `src/cli/mark_triaged_cmd.rs` | Modify | Accept workspace context |
| `src/cli/teams_cmd.rs` | Modify | Accept workspace context |
| `src/cli/mod.rs` | Modify | Add `workspace_cmd` module |
| `src/mcp/mod.rs` | Modify | Add `workspace` param to all tool args, add `list_workspaces` tool, require workspace on all others |

---

## Task 1: Config — WorkspaceConfig Struct and Parsing

**Files:**
- Modify: `crates/rectilinear-core/src/config.rs`

This task adds the new config structures and parsing logic. The old `[linear]` section is preserved for backward compatibility.

- [ ] **Step 1: Write tests for new config parsing**

Add these tests at the bottom of `crates/rectilinear-core/src/config.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_multi_workspace_config() {
        let toml_str = r#"
default_workspace = "personal"

[workspaces.personal]
api_key = "lin_api_personal"
default_team = "CUT"

[workspaces.work]
api_key = "lin_api_work"
default_team = "ENG"

[embedding]
backend = "local"

[search]
default_limit = 10
duplicate_threshold = 0.7
rrf_k = 60
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.default_workspace, Some("personal".to_string()));
        assert_eq!(config.workspaces.len(), 2);
        assert_eq!(
            config.workspaces["personal"].api_key,
            Some("lin_api_personal".to_string())
        );
        assert_eq!(
            config.workspaces["personal"].default_team,
            Some("CUT".to_string())
        );
        assert_eq!(
            config.workspaces["work"].api_key,
            Some("lin_api_work".to_string())
        );
    }

    #[test]
    fn parse_legacy_linear_config() {
        let toml_str = r#"
[linear]
api_key = "lin_api_legacy"
default_team = "CUT"

[embedding]
backend = "local"

[search]
default_limit = 10
duplicate_threshold = 0.7
rrf_k = 60
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        // Legacy config should be accessible via workspaces
        assert!(config.workspaces.is_empty());
        assert_eq!(
            config.linear.api_key,
            Some("lin_api_legacy".to_string())
        );
    }

    #[test]
    fn resolve_workspace_config_multi() {
        let toml_str = r#"
default_workspace = "work"

[workspaces.personal]
api_key = "lin_api_personal"
default_team = "CUT"

[workspaces.work]
api_key = "lin_api_work"
default_team = "ENG"

[embedding]
backend = "local"

[search]
default_limit = 10
duplicate_threshold = 0.7
rrf_k = 60
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let ws = config.workspace_config("personal").unwrap();
        assert_eq!(ws.api_key.as_deref(), Some("lin_api_personal"));
        assert_eq!(ws.default_team.as_deref(), Some("CUT"));
    }

    #[test]
    fn resolve_workspace_config_legacy() {
        let toml_str = r#"
[linear]
api_key = "lin_api_legacy"
default_team = "CUT"

[embedding]
backend = "local"

[search]
default_limit = 10
duplicate_threshold = 0.7
rrf_k = 60
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let ws = config.workspace_config("default").unwrap();
        assert_eq!(ws.api_key.as_deref(), Some("lin_api_legacy"));
    }

    #[test]
    fn resolve_workspace_config_unknown() {
        let config = Config::default();
        assert!(config.workspace_config("nonexistent").is_err());
    }

    #[test]
    fn workspace_names_returns_all() {
        let toml_str = r#"
[workspaces.a]
api_key = "key_a"

[workspaces.b]
api_key = "key_b"

[embedding]
backend = "local"

[search]
default_limit = 10
duplicate_threshold = 0.7
rrf_k = 60
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let mut names = config.workspace_names();
        names.sort();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn workspace_names_legacy_only() {
        let toml_str = r#"
[linear]
api_key = "lin_api_legacy"

[embedding]
backend = "local"

[search]
default_limit = 10
duplicate_threshold = 0.7
rrf_k = 60
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let names = config.workspace_names();
        assert_eq!(names, vec!["default"]);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd /Users/pieter/Documents/rectilinear && cargo test -p rectilinear-core --lib config::tests -- --nocapture 2>&1 | head -40`
Expected: Compilation errors — `WorkspaceConfig`, `default_workspace`, `workspaces`, `workspace_config`, `workspace_names` don't exist yet.

- [ ] **Step 3: Add WorkspaceConfig struct and update Config**

In `crates/rectilinear-core/src/config.rs`, add the `WorkspaceConfig` struct and new fields to `Config`:

```rust
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkspaceConfig {
    pub api_key: Option<String>,
    pub default_team: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    /// Default workspace name (used when no override is set)
    pub default_workspace: Option<String>,
    /// Named workspaces (new format)
    #[serde(default)]
    pub workspaces: HashMap<String, WorkspaceConfig>,
    /// Legacy single-workspace config (deprecated, backward compat)
    #[serde(default)]
    pub linear: LinearConfig,
    #[serde(default)]
    pub embedding: EmbeddingConfig,
    #[serde(default)]
    pub search: SearchConfig,
    #[serde(default)]
    pub anthropic: AnthropicConfig,
    #[serde(default)]
    pub triage: TriageConfig,
}
```

Add these methods to the `impl Config` block:

```rust
    /// Get the config for a specific workspace by name.
    /// Falls back to legacy `[linear]` section for the "default" workspace.
    pub fn workspace_config(&self, name: &str) -> Result<&WorkspaceConfig> {
        if let Some(ws) = self.workspaces.get(name) {
            return Ok(ws);
        }
        if name == "default" && self.linear.api_key.is_some() {
            // Can't return a reference to a temporary, so we need a different approach.
            // We'll handle this differently — see step 4.
            anyhow::bail!("Workspace 'default' not found");
        }
        anyhow::bail!(
            "Workspace '{}' not found. Available: {}",
            name,
            self.workspace_names().join(", ")
        )
    }

    /// List all configured workspace names.
    pub fn workspace_names(&self) -> Vec<String> {
        if !self.workspaces.is_empty() {
            self.workspaces.keys().cloned().collect()
        } else if self.linear.api_key.is_some() {
            vec!["default".to_string()]
        } else {
            vec![]
        }
    }
```

- [ ] **Step 4: Handle legacy "default" workspace properly**

The `workspace_config` method can't return a reference to the legacy config since `WorkspaceConfig` and `LinearConfig` are different types. Instead, return an owned `WorkspaceConfig`:

Replace the `workspace_config` method with:

```rust
    /// Get the config for a specific workspace by name.
    /// Falls back to legacy `[linear]` section for the "default" workspace.
    pub fn workspace_config(&self, name: &str) -> Result<WorkspaceConfig> {
        if let Some(ws) = self.workspaces.get(name) {
            return Ok(ws.clone());
        }
        if name == "default" && self.linear.api_key.is_some() {
            return Ok(WorkspaceConfig {
                api_key: self.linear.api_key.clone(),
                default_team: self.linear.default_team.clone(),
            });
        }
        anyhow::bail!(
            "Workspace '{}' not found. Available: {}",
            name,
            self.workspace_names().join(", ")
        )
    }

    /// Get the API key for a specific workspace.
    pub fn workspace_api_key(&self, workspace: &str) -> Result<String> {
        let ws = self.workspace_config(workspace)?;
        ws.api_key.context(format!(
            "No API key configured for workspace '{}'. Run: rectilinear config add-workspace",
            workspace
        ))
    }

    /// Get the default team for a specific workspace.
    pub fn workspace_default_team(&self, workspace: &str) -> Result<Option<String>> {
        let ws = self.workspace_config(workspace)?;
        Ok(ws.default_team)
    }
```

- [ ] **Step 5: Add active workspace resolution**

Add this method to `impl Config`:

```rust
    /// Resolve the active workspace name.
    /// Priority: RECTILINEAR_WORKSPACE env var → persisted state file → default_workspace config.
    pub fn resolve_active_workspace(&self) -> Result<String> {
        // 1. Environment variable
        if let Ok(ws) = std::env::var("RECTILINEAR_WORKSPACE") {
            if !ws.is_empty() {
                return Ok(ws);
            }
        }

        // 2. Persisted state file
        if let Ok(data_dir) = Self::data_dir() {
            let state_path = data_dir.join("active_workspace");
            if let Ok(contents) = std::fs::read_to_string(&state_path) {
                let name = contents.trim().to_string();
                if !name.is_empty() {
                    return Ok(name);
                }
            }
        }

        // 3. Config default
        if let Some(ref default) = self.default_workspace {
            return Ok(default.clone());
        }

        // 4. Single workspace shortcut
        let names = self.workspace_names();
        if names.len() == 1 {
            return Ok(names.into_iter().next().unwrap());
        }

        anyhow::bail!(
            "No active workspace set. Run: rectilinear workspace assume <name>\nAvailable: {}",
            names.join(", ")
        )
    }

    /// Persist the active workspace choice to disk.
    pub fn set_active_workspace(name: &str) -> Result<()> {
        let data_dir = Self::data_dir()?;
        let state_path = data_dir.join("active_workspace");
        std::fs::write(&state_path, name)?;
        Ok(())
    }

    /// Read the persisted active workspace (if any).
    pub fn get_persisted_workspace() -> Option<String> {
        let data_dir = Self::data_dir().ok()?;
        let state_path = data_dir.join("active_workspace");
        let contents = std::fs::read_to_string(&state_path).ok()?;
        let name = contents.trim().to_string();
        if name.is_empty() { None } else { Some(name) }
    }
```

- [ ] **Step 6: Update `load()` to apply env var overrides to workspaces**

In the `Config::load()` method, after loading the TOML, add env var handling for workspace API keys:

```rust
        // Env vars override config file
        if let Ok(key) = std::env::var("LINEAR_API_KEY") {
            config.linear.api_key = Some(key.clone());
            // Also override the active workspace's key if workspaces are configured
            if let Ok(active) = config.resolve_active_workspace() {
                if let Some(ws) = config.workspaces.get_mut(&active) {
                    ws.api_key = Some(key);
                }
            }
        }
```

- [ ] **Step 7: Run tests to verify they pass**

Run: `cd /Users/pieter/Documents/rectilinear && cargo test -p rectilinear-core --lib config::tests -- --nocapture`
Expected: All tests PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/rectilinear-core/src/config.rs
git commit -m "Add WorkspaceConfig struct and multi-workspace config parsing

Support named workspaces in config.toml with backward-compatible
legacy [linear] section. Add workspace resolution (env → state → config)."
```

---

## Task 2: Database Schema — Migration 7

**Files:**
- Modify: `crates/rectilinear-core/src/db/schema.rs`

- [ ] **Step 1: Write the migration**

Add migration 7 to `crates/rectilinear-core/src/db/schema.rs`. Add the constant:

```rust
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
ALTER TABLE issues ADD COLUMN workspace_id TEXT NOT NULL DEFAULT 'default' REFERENCES workspaces(id);

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
ALTER TABLE comments ADD COLUMN workspace_id TEXT NOT NULL DEFAULT 'default' REFERENCES workspaces(id);

-- New indices
CREATE INDEX IF NOT EXISTS idx_issues_workspace ON issues(workspace_id);
CREATE INDEX IF NOT EXISTS idx_issues_workspace_team ON issues(workspace_id, team_key);
CREATE INDEX IF NOT EXISTS idx_comments_workspace ON comments(workspace_id);
";
```

Note: `chunks` and `issue_relations` are already foreign-keyed to `issues(id)` with `ON DELETE CASCADE`, so they don't need a `workspace_id` column themselves — they inherit workspace scoping via JOINs. This deviates from the spec (which listed them for `workspace_id` columns) but is the correct approach to avoid redundant data.

- [ ] **Step 2: Register migration 7 in `run_migrations`**

Add after the migration 6 block in `run_migrations`:

```rust
    if current_version < 7 {
        conn.execute_batch(MIGRATION_7)?;
        conn.execute("INSERT INTO schema_version (version) VALUES (7)", [])?;
    }
```

- [ ] **Step 3: Run migration on a test DB to verify**

Run: `cd /Users/pieter/Documents/rectilinear && cargo test -p rectilinear-core --lib db::tests -- --nocapture`
Expected: Existing tests still pass (the migration adds columns with defaults, so existing test helpers still work).

- [ ] **Step 4: Commit**

```bash
git add crates/rectilinear-core/src/db/schema.rs
git commit -m "Add migration 7: workspaces table and workspace_id columns

Adds workspace registry table and workspace_id to issues, sync_state,
and comments. Recreates sync_state with composite PK. Existing data
tagged as workspace 'default'."
```

---

## Task 3: Database — Workspace-Scoped Query Methods

**Files:**
- Modify: `crates/rectilinear-core/src/db/mod.rs`

This is the largest task. Every query method that reads or writes issues/sync_state/comments needs to accept and use `workspace_id`. We keep the old signatures working by adding new workspace-aware variants.

- [ ] **Step 1: Add workspace CRUD methods**

Add these methods to the `impl Database` block in `crates/rectilinear-core/src/db/mod.rs`:

```rust
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
                   linear_org_id = COALESCE(excluded.linear_org_id, workspaces.linear_org_id),
                   display_name = COALESCE(excluded.display_name, workspaces.display_name)",
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
            let mut stmt =
                conn.prepare("SELECT id, linear_org_id, display_name, created_at FROM workspaces ORDER BY id")?;
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

    pub fn delete_workspace(&self, id: &str) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute("DELETE FROM workspaces WHERE id = ?1", rusqlite::params![id])?;
            Ok(())
        })
    }
```

Add the `WorkspaceRow` data type alongside the other data types:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceRow {
    pub id: String,
    pub linear_org_id: Option<String>,
    pub display_name: Option<String>,
    pub created_at: String,
}
```

- [ ] **Step 2: Update `upsert_issue` to include workspace_id**

Add `workspace_id` to the `Issue` struct:

```rust
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

fn default_workspace_id() -> String {
    "default".to_string()
}
```

Update `Issue::from_row` to read `workspace_id` as column 17 (after branch_name at 16):

```rust
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
```

Update every SQL SELECT that reads issues to include `workspace_id` as the 18th column. This applies to:
- `upsert_issue` — add `workspace_id` to INSERT and ON CONFLICT
- `get_issue` — add `workspace_id` to SELECT
- `get_unprioritized_issues` — add `workspace_id` to SELECT, add workspace filter
- `get_issues_by_state_types` — add `workspace_id` to SELECT
- `count_issues` — add workspace_id filter
- `get_field_completeness` — add workspace_id filter
- `list_all_issues` — add `workspace_id` to SELECT
- `get_issues_needing_embedding` — add `workspace_id` to SELECT
- `fts_search` — add `workspace_id` to SELECT (via JOIN)
- `get_all_chunks` — add workspace scope via JOIN
- `get_chunks_for_team` — add workspace scope via JOIN

The pattern for each is the same: add `, workspace_id` to the SELECT column list, and add `AND workspace_id = ?N` to WHERE clauses that accept a workspace parameter.

For brevity, here's the updated `upsert_issue`:

```rust
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
```

And the updated `get_issue` that now takes an optional workspace filter:

```rust
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
```

For `get_unprioritized_issues`, add a `workspace_id` parameter:

```rust
    pub fn get_unprioritized_issues(
        &self,
        team_key: Option<&str>,
        include_completed: bool,
        workspace_id: &str,
    ) -> Result<Vec<Issue>> {
        self.with_conn(|conn| {
            let state_filter = if include_completed {
                ""
            } else {
                " AND state_type NOT IN ('completed', 'canceled')"
            };
            let (sql, params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(team) = team_key {
                (
                    format!(
                        "SELECT id, identifier, team_key, title, description, state_name, state_type, priority, assignee_name, project_name, labels_json, created_at, updated_at, content_hash, synced_at, url, branch_name, workspace_id
                         FROM issues WHERE priority = 0{} AND team_key = ?1 AND workspace_id = ?2
                         ORDER BY created_at DESC", state_filter
                    ),
                    vec![
                        Box::new(team.to_string()) as Box<dyn rusqlite::types::ToSql>,
                        Box::new(workspace_id.to_string()),
                    ],
                )
            } else {
                (
                    format!(
                        "SELECT id, identifier, team_key, title, description, state_name, state_type, priority, assignee_name, project_name, labels_json, created_at, updated_at, content_hash, synced_at, url, branch_name, workspace_id
                         FROM issues WHERE priority = 0{} AND workspace_id = ?1
                         ORDER BY created_at DESC", state_filter
                    ),
                    vec![Box::new(workspace_id.to_string())],
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
```

- [ ] **Step 3: Update sync_state methods for composite key**

Update `get_sync_cursor`, `set_sync_cursor`, `is_full_sync_done`, `get_last_synced_at` to use `(workspace_id, team_key)`:

```rust
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

    pub fn set_sync_cursor(&self, workspace_id: &str, team_key: &str, last_updated_at: &str) -> Result<()> {
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
```

- [ ] **Step 4: Update remaining query methods**

Apply the same pattern to `count_issues`, `get_field_completeness`, `list_all_issues`, `list_synced_teams`, `fts_search`, `get_all_chunks`, `get_chunks_for_team`, `count_embedded_issues`, `get_issues_needing_embedding`. Each gets a `workspace_id: &str` parameter and adds `AND workspace_id = ?N` (or `WHERE workspace_id = ?N`) to its query.

For `list_synced_teams`, add workspace_id filter:

```rust
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
```

For `fts_search`, add workspace scoping via JOIN:

```rust
    pub fn fts_search(&self, query: &str, limit: usize, workspace_id: &str) -> Result<Vec<FtsResult>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT i.id, i.identifier, i.title, i.state_name, i.priority, bm25(issues_fts) as rank
                 FROM issues_fts f
                 JOIN issues i ON f.rowid = i.rowid
                 WHERE issues_fts MATCH ?1 AND i.workspace_id = ?3
                 ORDER BY rank
                 LIMIT ?2"
            )?;
            let rows = stmt.query_map(rusqlite::params![query, limit, workspace_id], |row| {
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
```

- [ ] **Step 5: Update test helpers**

In `crates/rectilinear-core/src/db/test_helpers.rs`, update `make_issue` to include `workspace_id`:

```rust
    pub fn make_issue(identifier: &str, team: &str) -> Issue {
        Issue {
            // ... existing fields ...
            workspace_id: "default".to_string(),
        }
    }
```

- [ ] **Step 6: Run tests**

Run: `cd /Users/pieter/Documents/rectilinear && cargo test -p rectilinear-core -- --nocapture`
Expected: All tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/rectilinear-core/src/db/
git commit -m "Add workspace_id to all database query methods

Issue struct gains workspace_id field. Sync state uses composite
(workspace_id, team_key) key. All query methods accept workspace_id
parameter for scoping."
```

---

## Task 4: LinearClient — Workspace-Aware Construction

**Files:**
- Modify: `crates/rectilinear-core/src/linear/mod.rs`

- [ ] **Step 1: Change `LinearClient::new` to accept API key directly**

Replace the current `new` method:

```rust
    pub fn new(api_key: &str) -> Self {
        let client = reqwest::Client::new();
        Self {
            client,
            api_key: api_key.to_string(),
        }
    }
```

Remove or deprecate `with_api_key` since `new` now does the same thing. Keep `with_http_client` as-is.

- [ ] **Step 2: Add `fetch_organization` method**

Add this method to the `impl LinearClient` block:

```rust
    /// Fetch the Linear organization details for this API key.
    pub async fn fetch_organization(&self) -> Result<(String, String)> {
        #[derive(Debug, Deserialize)]
        struct OrgData {
            organization: OrgNode,
        }
        #[derive(Debug, Deserialize)]
        struct OrgNode {
            id: String,
            name: String,
        }

        let data: OrgData = self
            .query(
                "query { organization { id name } }",
                serde_json::json!({}),
            )
            .await?;
        Ok((data.organization.id, data.organization.name))
    }
```

- [ ] **Step 3: Update `sync_team` to accept workspace_id**

The `sync_team` method calls `db.get_sync_cursor`, `db.set_sync_cursor`, `db.is_full_sync_done`, and `db.upsert_issue`. All of these now require `workspace_id`. Update the signature:

```rust
    pub async fn sync_team(
        &self,
        db: &Database,
        team_key: &str,
        workspace_id: &str,
        full: bool,
        include_archived: bool,
        progress_cb: Option<&dyn Fn(usize)>,
    ) -> Result<usize> {
```

Inside the method, replace:
- `db.get_sync_cursor(team_key)` → `db.get_sync_cursor(workspace_id, team_key)`
- `db.set_sync_cursor(team_key, ...)` → `db.set_sync_cursor(workspace_id, team_key, ...)`
- `db.is_full_sync_done(team_key)` → `db.is_full_sync_done(workspace_id, team_key)`
- When constructing `Issue` from `LinearIssue`, set `workspace_id: workspace_id.to_string()`

- [ ] **Step 4: Fix all compilation errors**

Every call site that calls `LinearClient::new(config)` needs to change to `LinearClient::new(api_key)`. This will be handled in later tasks when we update CLI commands and MCP tools. For now, the core library compiles.

- [ ] **Step 5: Run library tests**

Run: `cd /Users/pieter/Documents/rectilinear && cargo test -p rectilinear-core -- --nocapture`
Expected: All tests pass (LinearClient tests are integration tests that don't run in CI).

- [ ] **Step 6: Commit**

```bash
git add crates/rectilinear-core/src/linear/mod.rs
git commit -m "Make LinearClient workspace-aware

Accept API key directly in constructor. Add fetch_organization method.
Thread workspace_id through sync_team for scoped database writes."
```

---

## Task 5: Search — Thread workspace_id

**Files:**
- Modify: `crates/rectilinear-core/src/search/mod.rs`

- [ ] **Step 1: Update search function signatures**

Add `workspace_id: &str` to `search`, `fts_search` (the module-level one), `vector_search`, and `find_duplicates`:

```rust
pub async fn search(
    db: &Database,
    query: &str,
    mode: SearchMode,
    team_key: Option<&str>,
    state_filter: Option<&str>,
    limit: usize,
    embedder: Option<&Embedder>,
    rrf_k: u32,
    workspace_id: &str,
) -> Result<Vec<SearchResult>> {
    let results = match mode {
        SearchMode::Fts => fts_search(db, query, limit * 2, workspace_id)?,
        SearchMode::Vector => {
            let embedder =
                embedder.ok_or_else(|| anyhow::anyhow!("Embedder required for vector search"))?;
            vector_search(db, query, team_key, limit * 2, embedder, workspace_id).await?
        }
        SearchMode::Hybrid => {
            let fts_results = fts_search(db, query, limit * 3, workspace_id)?;
            if let Some(embedder) = embedder {
                let vec_results = vector_search(db, query, team_key, limit * 3, embedder, workspace_id).await?;
                reciprocal_rank_fusion(fts_results, vec_results, rrf_k, 0.3, 0.7)
            } else {
                fts_results
            }
        }
    };
    // ... rest unchanged
}
```

Update `fts_search` to call `db.fts_search(query, limit, workspace_id)`.

Update `vector_search` to call workspace-scoped chunk methods.

Update `find_duplicates` similarly.

- [ ] **Step 2: Run tests**

Run: `cd /Users/pieter/Documents/rectilinear && cargo test -p rectilinear-core -- --nocapture`
Expected: All tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/rectilinear-core/src/search/mod.rs
git commit -m "Thread workspace_id through search functions

All search operations now scope to a specific workspace."
```

---

## Task 6: CLI — Workspace Subcommand and Global Flag

**Files:**
- Create: `src/cli/workspace_cmd.rs`
- Modify: `src/cli/mod.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Create workspace_cmd.rs**

Write `src/cli/workspace_cmd.rs`:

```rust
use anyhow::Result;
use colored::Colorize;

use crate::config::Config;

pub fn handle_assume(name: &str) -> Result<()> {
    let config = Config::load()?;
    let names = config.workspace_names();
    if !names.contains(&name.to_string()) {
        anyhow::bail!(
            "Workspace '{}' not found. Available: {}",
            name,
            names.join(", ")
        );
    }

    Config::set_active_workspace(name)?;
    println!(
        "{} Active workspace set to {}",
        "Done!".green().bold(),
        name.bold()
    );
    Ok(())
}

pub fn handle_list() -> Result<()> {
    let config = Config::load()?;
    let names = config.workspace_names();
    let active = config.resolve_active_workspace().ok();

    if names.is_empty() {
        println!("{}", "No workspaces configured.".dimmed());
        println!(
            "Run {} to add one.",
            "rectilinear config add-workspace".bold()
        );
        return Ok(());
    }

    println!("{}", "Configured workspaces:".bold());
    for name in &names {
        let marker = if active.as_deref() == Some(name) {
            " (active)".green().bold().to_string()
        } else {
            String::new()
        };
        let ws = config.workspace_config(name)?;
        let team = ws
            .default_team
            .as_deref()
            .unwrap_or("(no default team)");
        println!("  {}{} — default team: {}", name.bold(), marker, team);
    }

    Ok(())
}

pub fn handle_current() -> Result<()> {
    let config = Config::load()?;
    match config.resolve_active_workspace() {
        Ok(name) => println!("{}", name),
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    }
    Ok(())
}
```

- [ ] **Step 2: Register module and add commands to main.rs**

In `src/cli/mod.rs`, add:

```rust
pub mod workspace_cmd;
```

In `src/main.rs`, add the `Workspace` variant to the `Commands` enum:

```rust
    /// Manage workspace context
    Workspace {
        #[command(subcommand)]
        action: WorkspaceAction,
    },
```

Add the `WorkspaceAction` enum:

```rust
#[derive(Subcommand)]
enum WorkspaceAction {
    /// Set the active workspace
    Assume {
        /// Workspace name
        name: String,
    },
    /// List configured workspaces
    List,
    /// Show current active workspace
    Current,
}
```

Add a global `--workspace` flag to the `Cli` struct:

```rust
#[derive(Parser)]
#[command(
    name = "rectilinear",
    about = "Linear issue intelligence tool",
    version
)]
struct Cli {
    /// Override the active workspace
    #[arg(long, global = true)]
    workspace: Option<String>,
    #[command(subcommand)]
    command: Commands,
}
```

Add the dispatch in `main()`:

```rust
        Commands::Workspace { action } => match action {
            WorkspaceAction::Assume { name } => cli::workspace_cmd::handle_assume(&name)?,
            WorkspaceAction::List => cli::workspace_cmd::handle_list()?,
            WorkspaceAction::Current => cli::workspace_cmd::handle_current()?,
        },
```

- [ ] **Step 3: Add workspace resolution helper to main.rs**

Add a helper function that resolves the workspace from the CLI flag or config:

```rust
/// Resolve the active workspace name from CLI flag or config.
fn resolve_workspace(cli_flag: Option<&str>, config: &Config) -> Result<String> {
    if let Some(ws) = cli_flag {
        // Validate the workspace exists
        let names = config.workspace_names();
        if !names.contains(&ws.to_string()) {
            anyhow::bail!(
                "Workspace '{}' not found. Available: {}",
                ws,
                names.join(", ")
            );
        }
        return Ok(ws.to_string());
    }
    config.resolve_active_workspace()
}
```

- [ ] **Step 4: Run build check**

Run: `cd /Users/pieter/Documents/rectilinear && cargo build 2>&1 | head -40`
Expected: May have compilation errors from changed signatures in downstream commands — that's expected and will be fixed in Task 7.

- [ ] **Step 5: Commit**

```bash
git add src/cli/workspace_cmd.rs src/cli/mod.rs src/main.rs
git commit -m "Add workspace subcommand and global --workspace flag

Supports 'assume', 'list', and 'current' subcommands. Global
--workspace flag overrides the active workspace for any command."
```

---

## Task 7: CLI — Update All Commands for Workspace Context

**Files:**
- Modify: `src/main.rs`
- Modify: `src/cli/sync_cmd.rs`
- Modify: `src/cli/search_cmd.rs`
- Modify: `src/cli/show_cmd.rs`
- Modify: `src/cli/create_cmd.rs`
- Modify: `src/cli/append_cmd.rs`
- Modify: `src/cli/embed_cmd.rs`
- Modify: `src/cli/triage_cmd.rs`
- Modify: `src/cli/mark_triaged_cmd.rs`
- Modify: `src/cli/teams_cmd.rs`

Every CLI command that uses the database or Linear API needs the workspace context. The pattern is:
1. Resolve workspace name from `cli.workspace` flag or config
2. Get API key from `config.workspace_api_key(workspace)`
3. Create `LinearClient::new(&api_key)`
4. Pass `workspace` to all DB methods

- [ ] **Step 1: Update sync_cmd.rs**

```rust
pub async fn handle_sync(
    db: &Database,
    config: &Config,
    team: Option<&str>,
    full: bool,
    embed: bool,
    include_archived: bool,
    workspace: &str,
) -> Result<()> {
    let api_key = config.workspace_api_key(workspace)?;
    let client = LinearClient::new(&api_key);

    let default_team = config.workspace_default_team(workspace)?;
    let team_key = team
        .or(default_team.as_deref())
        .ok_or_else(|| {
            anyhow::anyhow!("No team specified. Use --team or set default_team in workspace config")
        })?;

    // Ensure workspace exists in DB
    db.upsert_workspace(workspace, None, None)?;

    let is_first = !db.is_full_sync_done(workspace, team_key)?;

    if is_first && !full {
        println!(
            "{} First sync for team {} in workspace {} — performing full sync",
            "Info:".blue().bold(),
            team_key.bold(),
            workspace.bold()
        );
    }

    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.green} {msg}")
            .unwrap(),
    );
    pb.set_message(format!("Syncing team {} ({})...", team_key, workspace));
    pb.enable_steady_tick(std::time::Duration::from_millis(100));

    let do_full = full || is_first;
    let progress_cb = |total: usize| {
        pb.set_message(format!("{} issues synced", total));
    };
    let count = client
        .sync_team(db, team_key, workspace, do_full, include_archived, Some(&progress_cb))
        .await?;

    // Fetch org info on first sync
    if is_first {
        if let Ok((org_id, org_name)) = client.fetch_organization().await {
            db.upsert_workspace(workspace, Some(&org_id), Some(&org_name))?;
        }
    }

    pb.finish_with_message(format!(
        "{} Synced {} issues for team {} ({})",
        "Done!".green().bold(),
        count,
        team_key.bold(),
        workspace,
    ));

    let total = db.count_issues(Some(team_key), workspace)?;
    println!("Total issues in database for {} ({}): {}", team_key.bold(), workspace, total);

    if count == 0 && is_first {
        eprintln!(
            "\n{} No issues found for team \"{}\". Is the team key correct?",
            "Warning:".yellow().bold(),
            team_key
        );
        eprintln!(
            "Run {} to see available team keys.",
            "rectilinear teams".bold()
        );
    }

    if embed {
        println!();
        crate::cli::embed_cmd::handle_embed(db, config, Some(team_key), false, workspace).await?;
    }

    Ok(())
}
```

- [ ] **Step 2: Update search_cmd.rs**

Add `workspace: &str` parameter to `handle_search` and `handle_find_similar`. Pass it through to `search::search(...)` and `search::find_duplicates(...)`.

- [ ] **Step 3: Update create_cmd.rs**

Add `workspace: &str` parameter. Use `config.workspace_api_key(workspace)` to create the client. Set `workspace_id` on created issues.

- [ ] **Step 4: Update append_cmd.rs**

Add `workspace: &str` parameter. Use workspace API key for client.

- [ ] **Step 5: Update show_cmd.rs**

Add `workspace: &str` parameter. Use workspace API key when falling back to Linear API fetch.

- [ ] **Step 6: Update embed_cmd.rs**

Add `workspace: &str` parameter. Pass to `get_issues_needing_embedding`.

- [ ] **Step 7: Update triage_cmd.rs**

Add `workspace: &str` parameter. Pass to all DB and search calls.

- [ ] **Step 8: Update mark_triaged_cmd.rs**

Add `workspace: &str` parameter. Use workspace API key for client.

- [ ] **Step 9: Update teams_cmd.rs**

Change to accept workspace context and create client with workspace API key:

```rust
pub async fn handle_teams(config: &Config, workspace: &str) -> Result<()> {
    let api_key = config.workspace_api_key(workspace)?;
    let client = LinearClient::new(&api_key);
    let teams = client.list_teams().await?;

    if teams.is_empty() {
        println!("{}", "No teams found.".dimmed());
        return Ok(());
    }

    println!("{} (workspace: {})", "Available teams:".bold(), workspace);
    for team in &teams {
        println!("  {} — {}", team.key.bold(), team.name);
    }

    Ok(())
}
```

- [ ] **Step 10: Update main.rs dispatch to pass workspace**

In `main()`, resolve workspace after loading config, then pass to each command:

```rust
        _ => {
            let db = db::Database::open(&config::Config::db_path()?)?;
            let workspace = resolve_workspace(cli.workspace.as_deref(), &config)?;

            match cli.command {
                Commands::Sync { team, full, embed, include_archived } => {
                    cli::sync_cmd::handle_sync(&db, &config, team.as_deref(), full, embed, include_archived, &workspace).await?;
                }
                Commands::Search { query, team, state, mode, limit, json } => {
                    let mode = mode.parse()?;
                    let limit = limit.unwrap_or(config.search.default_limit);
                    cli::search_cmd::handle_search(&db, &config, &query, team.as_deref(), state.as_deref(), mode, limit, json, &workspace).await?;
                }
                // ... all other commands get &workspace passed
            }
        }
```

- [ ] **Step 11: Build and verify**

Run: `cd /Users/pieter/Documents/rectilinear && cargo build 2>&1 | tail -5`
Expected: Build succeeds.

- [ ] **Step 12: Commit**

```bash
git add src/
git commit -m "Thread workspace context through all CLI commands

Every command resolves the active workspace and passes it to
LinearClient and database methods. Sync fetches org info on first run."
```

---

## Task 8: MCP — Workspace Parameter on All Tools

**Files:**
- Modify: `src/mcp/mod.rs`

- [ ] **Step 1: Add `workspace` field to all tool args structs**

Add to `SearchArgs`, `FindDuplicatesArgs`, `GetIssueArgs`, `CreateIssueArgs`, `UpdateIssueArgs`, `AppendArgs`, `SyncTeamArgs`, `IssueContextArgs`, `GetTriageQueueArgs`, `MarkTriagedArgs`, `ManageRelationArgs`:

```rust
    /// Workspace name (required). Use list_workspaces to see available workspaces.
    workspace: Option<String>,
```

- [ ] **Step 2: Add workspace validation helper**

Add a helper method to `RectilinearMcp`:

```rust
impl RectilinearMcp {
    fn require_workspace(&self, workspace: &Option<String>) -> Result<String, String> {
        match workspace {
            Some(ws) if !ws.is_empty() => {
                // Validate workspace exists in config
                let names = self.config.workspace_names();
                if !names.contains(ws) {
                    return Err(format!(
                        "Workspace '{}' not found. Use list_workspaces to see available workspaces. Available: {}",
                        ws,
                        names.join(", ")
                    ));
                }
                Ok(ws.clone())
            }
            _ => Err(
                "workspace is required. Use list_workspaces to see available workspaces.".to_string()
            ),
        }
    }

    fn client_for_workspace(&self, workspace: &str) -> Result<LinearClient, String> {
        let api_key = self
            .config
            .workspace_api_key(workspace)
            .map_err(|e| e.to_string())?;
        Ok(LinearClient::new(&api_key))
    }
}
```

- [ ] **Step 3: Add `list_workspaces` tool**

Add a new args struct and tool method:

```rust
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct ListWorkspacesArgs {}

// In the #[tool(tool_box)] impl block:
    #[tool(
        name = "list_workspaces",
        description = "List all configured workspaces. Use this to discover available workspace names before calling other tools."
    )]
    async fn list_workspaces(
        &self,
        #[tool(aggr)] _args: ListWorkspacesArgs,
    ) -> Result<String, String> {
        let names = self.config.workspace_names();
        let active = self.config.resolve_active_workspace().ok();

        let mut workspaces = Vec::new();
        for name in &names {
            let ws = self
                .config
                .workspace_config(name)
                .map_err(|e| e.to_string())?;
            let db_info = self.db.get_workspace(name).map_err(|e| e.to_string())?;

            workspaces.push(serde_json::json!({
                "name": name,
                "active": active.as_deref() == Some(name.as_str()),
                "default_team": ws.default_team,
                "org_name": db_info.as_ref().and_then(|w| w.display_name.clone()),
            }));
        }

        serde_json::to_string_pretty(&serde_json::json!({
            "workspaces": workspaces,
            "instruction": "Pass the workspace name to all other tools."
        }))
        .map_err(|e| e.to_string())
    }
```

- [ ] **Step 4: Update all existing tools to require workspace**

For each tool, add workspace validation at the top. Example for `search_issues`:

```rust
    async fn search_issues(&self, #[tool(aggr)] args: SearchArgs) -> Result<String, String> {
        let workspace = self.require_workspace(&args.workspace)?;

        let mode: SearchMode = args
            .mode
            .as_deref()
            .unwrap_or("hybrid")
            .parse()
            .map_err(|e: anyhow::Error| e.to_string())?;

        let limit = args.limit.unwrap_or(self.config.search.default_limit);

        let embedder = if mode != SearchMode::Fts {
            Embedder::new(&self.config).ok()
        } else {
            None
        };

        let results = search::search(
            &self.db,
            &args.query,
            mode,
            args.team.as_deref(),
            args.state.as_deref(),
            limit,
            embedder.as_ref(),
            self.config.search.rrf_k,
            &workspace,
        )
        .await
        .map_err(|e| e.to_string())?;

        serde_json::to_string_pretty(&results).map_err(|e| e.to_string())
    }
```

Apply the same pattern to all other tools:
- `find_duplicates` — add `&workspace` to `search::find_duplicates` call
- `get_issue` — use `self.client_for_workspace(&workspace)` instead of `LinearClient::new(&self.config)`
- `create_issue` — use workspace client, set workspace_id on created issue
- `update_issue` — use workspace client
- `append_to_issue` — use workspace client
- `sync_team` — use workspace client, pass workspace_id to `sync_team`
- `issue_context` — pass workspace to search calls
- `get_triage_queue` — use workspace client, pass workspace to DB calls
- `mark_triaged` — use workspace client
- `manage_relation` — use workspace client

- [ ] **Step 5: Update MCP server instructions**

In `get_info()`, update the `instructions` string to mention the workspace requirement. Add a note at the beginning:

```
"## Workspace Selection\n\
All tools (except list_workspaces) require a `workspace` parameter. Call list_workspaces first to discover available workspaces.\n\n"
```

- [ ] **Step 6: Build and verify**

Run: `cd /Users/pieter/Documents/rectilinear && cargo build 2>&1 | tail -5`
Expected: Build succeeds.

- [ ] **Step 7: Commit**

```bash
git add src/mcp/mod.rs
git commit -m "Require workspace parameter on all MCP tools

All tools except list_workspaces now require an explicit workspace name.
Returns error with guidance if omitted. Adds list_workspaces tool for
workspace discovery."
```

---

## Task 9: Config CLI — Add/Remove Workspace Commands

**Files:**
- Modify: `src/cli/config_cmd.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Add new ConfigAction variants**

In `src/main.rs`, update the `ConfigAction` enum:

```rust
#[derive(Subcommand)]
enum ConfigAction {
    /// Set a config value
    Set {
        /// Config key
        key: String,
        /// Config value
        value: String,
    },
    /// Get a config value
    Get {
        /// Config key
        key: String,
    },
    /// Show all config
    Show,
    /// Add a new workspace
    AddWorkspace,
    /// Remove a workspace
    RemoveWorkspace {
        /// Workspace name to remove
        name: String,
    },
}
```

Update the dispatch in `main()`:

```rust
        Commands::Config { action } => match action {
            Some(ConfigAction::Set { key, value }) => cli::config_cmd::handle_set(&key, &value)?,
            Some(ConfigAction::Get { key }) => cli::config_cmd::handle_get(&key)?,
            Some(ConfigAction::Show) => cli::config_cmd::handle_show()?,
            Some(ConfigAction::AddWorkspace) => cli::config_cmd::handle_add_workspace()?,
            Some(ConfigAction::RemoveWorkspace { name }) => cli::config_cmd::handle_remove_workspace(&name)?,
            None => cli::config_cmd::handle_interactive()?,
        },
```

- [ ] **Step 2: Implement `handle_add_workspace`**

In `src/cli/config_cmd.rs`:

```rust
pub fn handle_add_workspace() -> Result<()> {
    let mut config = Config::load()?;

    println!("{}", "Add a new workspace".bold());
    println!();

    let name = prompt_string("  Workspace name (e.g., personal, work)", None)?
        .ok_or_else(|| anyhow::anyhow!("Workspace name is required"))?;

    if config.workspaces.contains_key(&name) {
        anyhow::bail!("Workspace '{}' already exists", name);
    }

    let api_key = prompt_secret("  Linear API key", None)?
        .ok_or_else(|| anyhow::anyhow!("API key is required"))?;

    let default_team = prompt_string("  Default team key (e.g., ENG, optional)", None)?;

    let set_default = if config.workspaces.is_empty() && config.default_workspace.is_none() {
        true
    } else {
        prompt_string(
            &format!("  Set as default workspace? (y/n)"),
            Some("n"),
        )?
        .map(|v| v.to_lowercase().starts_with('y'))
        .unwrap_or(false)
    };

    config.workspaces.insert(
        name.clone(),
        crate::config::WorkspaceConfig {
            api_key: Some(api_key),
            default_team,
        },
    );

    if set_default {
        config.default_workspace = Some(name.clone());
    }

    // Migrate away from legacy [linear] if adding first workspace
    if config.linear.api_key.is_some() && !config.workspaces.contains_key("default") {
        println!(
            "\n{} Migrating legacy [linear] config to workspace 'default'",
            "Info:".blue().bold()
        );
        config.workspaces.insert(
            "default".to_string(),
            crate::config::WorkspaceConfig {
                api_key: config.linear.api_key.take(),
                default_team: config.linear.default_team.take(),
            },
        );
    }

    config.save()?;
    println!(
        "\n{} Workspace '{}' added.",
        "Done!".green().bold(),
        name.bold()
    );
    if set_default {
        println!("Set as default workspace.");
    }
    println!(
        "Run {} to sync issues.",
        format!("rectilinear sync --workspace {}", name).bold()
    );

    Ok(())
}
```

- [ ] **Step 3: Implement `handle_remove_workspace`**

```rust
pub fn handle_remove_workspace(name: &str) -> Result<()> {
    let mut config = Config::load()?;

    if !config.workspaces.contains_key(name) {
        anyhow::bail!("Workspace '{}' not found", name);
    }

    let is_active = config.resolve_active_workspace().ok().as_deref() == Some(name);
    if is_active {
        println!(
            "{} '{}' is the active workspace.",
            "Warning:".yellow().bold(),
            name
        );
        let confirm = prompt_string("  Remove anyway? (y/n)", Some("n"))?;
        if !confirm.map(|v| v.to_lowercase().starts_with('y')).unwrap_or(false) {
            println!("Cancelled.");
            return Ok(());
        }
    }

    config.workspaces.remove(name);
    if config.default_workspace.as_deref() == Some(name) {
        config.default_workspace = None;
    }

    config.save()?;
    println!(
        "{} Workspace '{}' removed.",
        "Done!".green().bold(),
        name.bold()
    );

    Ok(())
}
```

- [ ] **Step 4: Update `handle_show` for workspaces**

Update `handle_show` in `config_cmd.rs` to display workspaces:

```rust
pub fn handle_show() -> Result<()> {
    let config = Config::load()?;
    let path = Config::config_path()?;

    println!("{} {}", "Config file:".bold(), path.display());
    println!("{} {}", "Database:".bold(), Config::db_path()?.display());
    println!();

    // Show workspaces if configured
    if !config.workspaces.is_empty() {
        let active = config.resolve_active_workspace().ok();
        println!("{}", "[workspaces]".bold());
        if let Some(ref default) = config.default_workspace {
            println!("  default-workspace: {}", default);
        }
        for (name, ws) in &config.workspaces {
            let marker = if active.as_deref() == Some(name.as_str()) {
                " (active)".green().to_string()
            } else {
                String::new()
            };
            println!("  {}{}", name.bold(), marker);
            println!(
                "    api-key: {}",
                ws.api_key
                    .as_ref()
                    .map(|k| if k.len() > 8 {
                        format!("{}...{}", &k[..4], &k[k.len() - 4..])
                    } else {
                        "****".to_string()
                    })
                    .unwrap_or_else(|| "(not set)".dimmed().to_string())
            );
            println!(
                "    default-team: {}",
                ws.default_team
                    .as_deref()
                    .unwrap_or(&"(not set)".dimmed().to_string())
            );
        }
    } else if config.linear.api_key.is_some() {
        println!("{} {}", "[linear]".bold(), "(legacy — run 'rectilinear config add-workspace' to migrate)".dimmed());
        println!(
            "  api-key: {}",
            config.linear.api_key.as_ref().map(|k| if k.len() > 8 {
                format!("{}...{}", &k[..4], &k[k.len() - 4..])
            } else {
                "****".to_string()
            }).unwrap_or_else(|| "(not set)".dimmed().to_string())
        );
        println!(
            "  default-team: {}",
            config.linear.default_team.as_deref().unwrap_or(&"(not set)".dimmed().to_string())
        );
    }

    println!();
    println!("{}", "[anthropic]".bold());
    println!(
        "  api-key: {}",
        config.anthropic.api_key.as_ref().map(|k| if k.len() > 8 {
            format!("{}...{}", &k[..4], &k[k.len() - 4..])
        } else {
            "****".to_string()
        }).unwrap_or_else(|| "(not set)".dimmed().to_string())
    );

    println!();
    println!("{}", "[embedding]".bold());
    println!("  backend: {}", format!("{:?}", config.embedding.backend).to_lowercase());
    println!(
        "  gemini-api-key: {}",
        config.embedding.gemini_api_key.as_ref().map(|k| if k.len() > 8 {
            format!("{}...{}", &k[..4], &k[k.len() - 4..])
        } else {
            "****".to_string()
        }).unwrap_or_else(|| "(not set)".dimmed().to_string())
    );

    println!();
    println!("{}", "[search]".bold());
    println!("  default-limit: {}", config.search.default_limit);
    println!("  duplicate-threshold: {}", config.search.duplicate_threshold);

    println!();
    println!("{}", "[triage]".bold());
    println!("  mode: {}", config.triage.mode);

    Ok(())
}
```

- [ ] **Step 5: Build and verify**

Run: `cd /Users/pieter/Documents/rectilinear && cargo build 2>&1 | tail -5`
Expected: Build succeeds.

- [ ] **Step 6: Commit**

```bash
git add src/cli/config_cmd.rs src/main.rs
git commit -m "Add config add-workspace and remove-workspace commands

Interactive workspace creation with API key, default team, and optional
default flag. Legacy config migration on first workspace add."
```

---

## Task 10: Integration Verification

**Files:** None new — verification only.

- [ ] **Step 1: Run full test suite**

Run: `cd /Users/pieter/Documents/rectilinear && cargo test --all 2>&1 | tail -20`
Expected: All tests pass.

- [ ] **Step 2: Run clippy**

Run: `cd /Users/pieter/Documents/rectilinear && cargo clippy --all 2>&1 | tail -20`
Expected: No errors (warnings acceptable).

- [ ] **Step 3: Verify CLI help**

Run: `cd /Users/pieter/Documents/rectilinear && cargo run -- --help`
Expected: Shows `--workspace` flag and `workspace` subcommand.

Run: `cd /Users/pieter/Documents/rectilinear && cargo run -- workspace --help`
Expected: Shows `assume`, `list`, `current` subcommands.

Run: `cd /Users/pieter/Documents/rectilinear && cargo run -- config --help`
Expected: Shows `add-workspace`, `remove-workspace` subcommands.

- [ ] **Step 4: Verify backward compatibility with existing DB**

Run: `cd /Users/pieter/Documents/rectilinear && cargo run -- search "test" --workspace default`
Expected: Searches existing data (which was tagged `workspace_id = 'default'` by migration 7).

- [ ] **Step 5: Commit any fixes**

If any issues were found, fix and commit.

- [ ] **Step 6: Final commit — bump version**

Update `crates/rectilinear-core/Cargo.toml` version to `0.2.0` (breaking change — new required params on public API methods).

```bash
git add crates/rectilinear-core/Cargo.toml Cargo.toml
git commit -m "Bump rectilinear-core to 0.2.0 for multi-workspace support"
```
