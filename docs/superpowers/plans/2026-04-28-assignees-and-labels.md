# Assignees and First-Class Labels — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add assignee parameters to issue-mutating MCP tools (with a `"me"` shortcut), sync a workspace label catalog into a local SQLite table, expose `list_labels`, and let `search_issues` and `get_triage_queue` filter issues by label (AND, case-insensitive).

**Architecture:** Two new tables in a single migration: `labels` (workspace-scoped catalog of `id`, `name`, `color`, `parent_id`) and `issue_labels` (join). `LinearClient::sync_team` is extended to fetch the workspace's labels first, then sync issues — issue upserts now also write `issue_labels` rows. `labels_json` on `issues` is retained as a derived field to keep FTS5 triggers untouched. Assignees are resolved on-demand via Linear's `viewer` and `users` queries; no local users catalog. Migration 8 also clears `sync_state` so the next `sync_team` performs a full re-sync.

**Tech Stack:** Rust 2021, rusqlite 0.32 (SQLite + FTS5), reqwest (Linear GraphQL), rmcp 0.1.5 (MCP server), tempfile (test fixtures).

**Spec:** `docs/superpowers/specs/2026-04-28-assignees-and-labels-design.md`

**Commit style:** Plain English. No conventional-commit prefixes (`feat:`/`fix:`/etc.).

---

## File Map

**Modify:**
- `crates/rectilinear-core/src/db/schema.rs` — add `MIGRATION_8`, bump migration list.
- `crates/rectilinear-core/src/db/mod.rs` — add `Label` struct, label CRUD, `issue_labels` CRUD, `resolve_label_ids_local`, extend `get_unprioritized_issues` and `fts_search` to accept a label-id filter. Tests at the bottom (existing pattern).
- `crates/rectilinear-core/src/db/test_helpers.rs` — add `make_label` helper.
- `crates/rectilinear-core/src/linear/mod.rs` — add `id` to `LinearLabel`, change GraphQL fragments, add `LabelCatalogEntry` type, `fetch_labels`, `sync_labels_catalog`, integrate into `sync_team`, add `assignee_id` param to `create_issue`/`update_issue`, add `resolve_assignee_id`, cache viewer id.
- `crates/rectilinear-core/src/search/mod.rs` — add `label_ids: Option<&[String]>` to `SearchParams`, propagate through `fts_search` (vector path uses post-filter via `db.get_issue`).
- `src/mcp/mod.rs` — extend `CreateIssueArgs`/`UpdateIssueArgs`/`MarkTriagedArgs` with `assignee`; extend `CreateIssueArgs` with `labels`; extend `SearchArgs`/`GetTriageQueueArgs` with `labels`; add `ListLabelsArgs` and `list_labels` tool; wire resolution through to the Linear client.

**Create:**
- (none — all changes fit in existing files following the repo's monolithic-module convention).

---

## Task 1: Migration 8 — schema + sync state reset

**Files:**
- Modify: `crates/rectilinear-core/src/db/schema.rs`
- Test: `crates/rectilinear-core/src/db/mod.rs` (test module at bottom, ~line 1300+)

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` block in `crates/rectilinear-core/src/db/mod.rs`:

```rust
#[test]
fn migration_8_creates_label_tables_and_resets_sync_state() {
    use super::test_helpers::test_db;
    let (db, _dir) = test_db();
    db.with_conn(|conn| {
        // Seed sync_state as if a prior sync had completed
        conn.execute(
            "INSERT INTO sync_state (workspace_id, team_key, last_updated_at, full_sync_done, last_synced_at)
             VALUES ('default', 'ENG', '2026-04-01T00:00:00Z', 1, '2026-04-01T00:00:00Z')",
            [],
        )?;
        Ok(())
    })
    .unwrap();

    // Run migrations again — idempotent; the 8th should reset sync_state.
    // (Database::open already ran them; we re-run to confirm idempotency.)
    db.with_conn(|conn| crate::db::schema::run_migrations(conn)).unwrap();

    db.with_conn(|conn| {
        // Tables exist
        let labels_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='labels'",
            [], |r| r.get(0))?;
        assert_eq!(labels_count, 1);
        let join_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='issue_labels'",
            [], |r| r.get(0))?;
        assert_eq!(join_count, 1);

        // sync_state reset
        let full_done: i64 = conn.query_row(
            "SELECT full_sync_done FROM sync_state WHERE workspace_id='default' AND team_key='ENG'",
            [], |r| r.get(0))?;
        assert_eq!(full_done, 0);
        let last_updated: String = conn.query_row(
            "SELECT last_updated_at FROM sync_state WHERE workspace_id='default' AND team_key='ENG'",
            [], |r| r.get(0))?;
        assert_eq!(last_updated, "1970-01-01T00:00:00Z");
        Ok(())
    })
    .unwrap();
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p rectilinear-core migration_8_creates_label_tables_and_resets_sync_state`
Expected: FAIL — `no such table: labels` (the migration doesn't exist yet).

- [ ] **Step 3: Add the migration**

In `crates/rectilinear-core/src/db/schema.rs`, after the existing migration-7 block in `run_migrations` (around line 49–52):

```rust
    if current_version < 8 {
        conn.execute_batch(MIGRATION_8)?;
        conn.execute("INSERT INTO schema_version (version) VALUES (8)", [])?;
    }
```

Then add the migration constant at the top of the file (above `MIGRATION_7`):

```rust
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
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p rectilinear-core migration_8_creates_label_tables_and_resets_sync_state`
Expected: PASS.

Also run the full test suite to confirm no regression:
Run: `cargo test -p rectilinear-core`
Expected: all existing tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/rectilinear-core/src/db/schema.rs crates/rectilinear-core/src/db/mod.rs
git commit -m "Add migration 8 for labels catalog and issue_labels join

Forces a one-time full re-sync of every team so issue_labels can be
populated from a real Linear sync."
```

---

## Task 2: Database — Label struct, test helper, label CRUD

**Files:**
- Modify: `crates/rectilinear-core/src/db/mod.rs`
- Modify: `crates/rectilinear-core/src/db/test_helpers.rs`

- [ ] **Step 1: Add `Label` struct and `make_label` helper**

In `crates/rectilinear-core/src/db/mod.rs`, in the `// --- Data types ---` section near the bottom (after the existing `Issue` struct, around line 900):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Label {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub color: Option<String>,
    pub parent_id: Option<String>,
}
```

In `crates/rectilinear-core/src/db/test_helpers.rs`, append:

```rust
use super::Label;

/// Create a minimal test label.
pub fn make_label(id: &str, name: &str, workspace_id: &str) -> Label {
    Label {
        id: id.to_string(),
        workspace_id: workspace_id.to_string(),
        name: name.to_string(),
        color: Some("#abcdef".to_string()),
        parent_id: None,
    }
}
```

- [ ] **Step 2: Write failing tests for label CRUD**

Append to the test module in `crates/rectilinear-core/src/db/mod.rs`:

```rust
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
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p rectilinear-core upsert_label_inserts_and_renames_in_place list_labels_is_workspace_scoped_and_sorted delete_labels_for_workspace_not_in_removes_orphans`
Expected: FAIL — methods don't exist.

- [ ] **Step 4: Implement label CRUD on `Database`**

In `crates/rectilinear-core/src/db/mod.rs`, add a new section before `// --- Data types ---` (or after the existing `// --- Workspace CRUD ---` block, around line 137):

```rust
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
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p rectilinear-core upsert_label list_labels delete_labels_for_workspace_not_in`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/rectilinear-core/src/db/mod.rs crates/rectilinear-core/src/db/test_helpers.rs
git commit -m "Add Label struct and CRUD on Database

upsert_label, list_labels (workspace-scoped, alphabetical),
delete_labels_for_workspace_not_in for sync-time pruning."
```

---

## Task 3: Database — `issue_labels` join CRUD

**Files:**
- Modify: `crates/rectilinear-core/src/db/mod.rs`

- [ ] **Step 1: Write failing tests**

Append to the test module:

```rust
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
```

- [ ] **Step 2: Run the tests to confirm they fail**

Run: `cargo test -p rectilinear-core replace_issue_labels deleting_issue_cascades deleting_label_cascades`
Expected: FAIL.

- [ ] **Step 3: Implement `replace_issue_labels` and `get_issue_label_ids`**

In the new label CRUD section (after `delete_labels_for_workspace_not_in`):

```rust
    /// Replace the label set for an issue. Atomic via transaction.
    /// Skips any label_ids not present in the `labels` table (logged at warn level).
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
```

Note: rusqlite's `Connection::unchecked_transaction()` is required because the connection is held behind a `Mutex` and the transaction must not require a `&mut Connection`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rectilinear-core replace_issue_labels deleting_issue_cascades deleting_label_cascades`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/rectilinear-core/src/db/mod.rs
git commit -m "Add issue_labels join CRUD with cascade tests

replace_issue_labels (transactional, skips unknown ids with warning) and
get_issue_label_ids. Cascade deletes verified for both directions."
```

---

## Task 4: Database — local label-name resolver

**Files:**
- Modify: `crates/rectilinear-core/src/db/mod.rs`

- [ ] **Step 1: Write failing tests**

Append to the test module:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rectilinear-core resolve_label_ids_local`
Expected: FAIL.

- [ ] **Step 3: Implement the resolver**

After `get_issue_label_ids`:

```rust
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rectilinear-core resolve_label_ids_local`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/rectilinear-core/src/db/mod.rs
git commit -m "Add resolve_label_ids_local for case-insensitive name lookup"
```

---

## Task 5: Database — extend triage queue and FTS to filter by labels

**Files:**
- Modify: `crates/rectilinear-core/src/db/mod.rs`

`get_unprioritized_issues` and `fts_search` both need to filter on "issue has ALL of these label ids". Build a helper and add an optional `label_ids: Option<&[String]>` parameter to both.

- [ ] **Step 1: Write failing tests**

Append to the test module:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rectilinear-core get_unprioritized_issues_filters_by_labels fts_search_with_label_filter`
Expected: FAIL — methods don't exist.

- [ ] **Step 3: Implement the filtered variants**

Strategy: keep existing `get_unprioritized_issues` and `fts_search` as thin wrappers that delegate to new `_filtered` variants with `None` label_ids. New variants build the WHERE clause with the AND-label sub-select when label_ids is `Some`.

In `crates/rectilinear-core/src/db/mod.rs`, add a private helper above `get_unprioritized_issues`:

```rust
    /// Build a SQL fragment "issues.id IN (SELECT issue_id FROM issue_labels ...)"
    /// for AND-matching all of `label_ids`. Returns the fragment + bound params.
    /// Caller is responsible for prepending " AND " before splicing in.
    fn label_filter_fragment(
        label_ids: &[String],
        param_offset: usize,
    ) -> (String, Vec<Box<dyn rusqlite::types::ToSql>>) {
        let n = label_ids.len();
        let placeholders = (0..n)
            .map(|i| format!("?{}", param_offset + i))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "issues.id IN (\
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
```

Replace the body of `get_unprioritized_issues` with a delegation, and add the filtered variant. Find the existing `pub fn get_unprioritized_issues(` (line 178) and change it to:

```rust
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
                let (frag, mut lp) = Self::label_filter_fragment(ids, params.len() + 1);
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
```

Find the existing `pub fn fts_search(` (line 843) and change it the same way:

```rust
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
                let (frag, mut lp) = Self::label_filter_fragment(ids, params.len() + 1);
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
```

Note: in the FTS query, `i.id` is referenced, but the label-filter fragment uses `issues.id IN (...)`. SQLite resolves `issues.id` against the aliased table `issues i` — confirm this works. If not, change the fragment to use the alias, OR rewrite the fragment to use `i.id` for FTS callers. Safer alternative: change the fragment to take a `table_alias: &str` parameter. Apply this fix proactively:

Replace the helper:

```rust
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
```

In `get_unprioritized_issues_filtered`, call with `"issues"`. In `fts_search_filtered`, call with `"i"`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rectilinear-core get_unprioritized_issues_filters_by_labels fts_search_with_label_filter`
Expected: PASS.

Also run the full suite:
Run: `cargo test -p rectilinear-core`
Expected: all pass (including the original `get_unprioritized` and `fts_search` tests, which now go through the wrapper).

- [ ] **Step 5: Commit**

```bash
git add crates/rectilinear-core/src/db/mod.rs
git commit -m "Filter triage queue and FTS results by label ids (AND semantics)

Adds get_unprioritized_issues_filtered and fts_search_filtered. Existing
unfiltered methods become thin wrappers."
```

---

## Task 6: Linear — extract label ids on issue conversion

**Files:**
- Modify: `crates/rectilinear-core/src/linear/mod.rs`

The GraphQL fragment for issues currently requests `labels { nodes { name } }`. We need the id too. We also need a way to get the resolved label-id list out of `convert_linear_issue` so the caller (sync flow) can write `issue_labels` rows.

- [ ] **Step 1: Add `id` to `LinearLabel` and bubble it up**

In `crates/rectilinear-core/src/linear/mod.rs`, change the struct (around line 119–122):

```rust
#[derive(Debug, Deserialize)]
struct LinearLabel {
    id: String,
    name: String,
}
```

Update every GraphQL `labels { nodes { name } }` to `labels { nodes { id name } }`. Locations:
- `fetch_issues` (around line 355)
- `fetch_single_issue` (around line 578)
- `fetch_issue_by_identifier` (around line 626)

Change `convert_linear_issue` to also return label ids. Modify its signature and body (around line 645):

```rust
    fn convert_linear_issue(i: LinearIssue) -> (db::Issue, Vec<db::Relation>, Vec<String>) {
        let labels: Vec<String> = i.labels.nodes.iter().map(|l| l.name.clone()).collect();
        let label_ids: Vec<String> = i.labels.nodes.iter().map(|l| l.id.clone()).collect();
        let labels_json = serde_json::to_string(&labels).unwrap_or_else(|_| "[]".to_string());

        let mut hasher = Sha256::new();
        hasher.update(&i.title);
        hasher.update(i.description.as_deref().unwrap_or(""));
        hasher.update(&labels_json);
        let content_hash = hex::encode(hasher.finalize());

        let relations = Self::extract_relations(&i.id, &i);

        let issue = db::Issue {
            id: i.id,
            identifier: i.identifier,
            url: i.url,
            team_key: i.team.key,
            title: i.title,
            description: i.description,
            state_name: i.state.name,
            state_type: i.state.state_type,
            priority: i.priority,
            assignee_name: i.assignee.map(|a| a.name),
            project_name: i.project.map(|p| p.name),
            labels_json,
            created_at: i.created_at,
            updated_at: i.updated_at,
            content_hash,
            synced_at: None,
            branch_name: i.branch_name,
            workspace_id: "default".to_string(),
        };

        (issue, relations, label_ids)
    }
```

- [ ] **Step 2: Update every caller of `convert_linear_issue`**

Three callers, plus their callers. Look for the existing pattern `Self::convert_linear_issue` and update:

In `fetch_issues` (around line 366), the `.map` call now produces a 3-tuple — change the return type to match. Update the function signature:

```rust
    pub async fn fetch_issues(
        &self,
        team_key: &str,
        after_cursor: Option<&str>,
        updated_after: Option<&str>,
        include_archived: bool,
    ) -> Result<(Vec<(db::Issue, Vec<db::Relation>, Vec<String>)>, bool, Option<String>)> {
```

Update its body to use the new tuple shape — the `.map(Self::convert_linear_issue)` already produces the right tuple.

Update `fetch_single_issue` signature:

```rust
    pub async fn fetch_single_issue(
        &self,
        issue_id: &str,
    ) -> Result<(db::Issue, Vec<db::Relation>, Vec<String>)> {
```

Update `fetch_issue_by_identifier` signature:

```rust
    pub async fn fetch_issue_by_identifier(
        &self,
        identifier: &str,
    ) -> Result<Option<(db::Issue, Vec<db::Relation>, Vec<String>)>> {
```

- [ ] **Step 3: Update `sync_team` and all MCP callers to consume the new tuple**

In `sync_team` (around line 410), the loop now destructures three fields and writes `issue_labels`:

```rust
            for (mut issue, relations, label_ids) in issues {
                issue.workspace_id = workspace_id.to_string();
                if max_updated.is_none() || Some(&issue.updated_at) > max_updated.as_ref() {
                    max_updated = Some(issue.updated_at.clone());
                }
                db.upsert_issue(&issue)?;
                db.upsert_relations(&issue.id, &relations)?;
                db.replace_issue_labels(&issue.id, &label_ids)?;
            }
```

In `src/mcp/mod.rs`, every call site of `fetch_single_issue` and `fetch_issue_by_identifier` needs the 3-tuple:

```bash
grep -n "fetch_single_issue\|fetch_issue_by_identifier" src/mcp/mod.rs
```

For each call site, update the destructure from `let (issue, relations) = ...` to `let (issue, relations, label_ids) = ...` and add a `db.replace_issue_labels(&issue.id, &label_ids)` call right after the existing `upsert_relations`.

The known call sites (verify with grep — line numbers may have shifted):
- `get_issue` fallback (around line 495): `let result = client.fetch_issue_by_identifier(...)`. Match on `Some((issue, relations, label_ids)) =>`.
- `create_issue` post-create fetch (around line 575): destructure and persist label_ids.
- `update_issue` description-preservation fetch (around line 645): destructure as `let (latest, _, _) = client.fetch_single_issue(...)`.
- `update_issue` post-update fetch (around line 673): destructure and persist label_ids.
- `mark_triaged` modified-since check (around line 990): destructure (just for the comparison; no need to persist label_ids since the next post-update fetch handles it).
- `mark_triaged` post-update fetch (around line 1109): destructure and persist label_ids.
- `manage_relation` post-add and post-remove fetches: destructure and persist label_ids.

- [ ] **Step 4: Build to verify everything compiles**

Run: `cargo build`
Expected: clean build, no errors.

- [ ] **Step 5: Run the full test suite**

Run: `cargo test`
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add crates/rectilinear-core/src/linear/mod.rs src/mcp/mod.rs
git commit -m "Extract label ids on Linear issue conversion and persist via issue_labels

Linear GraphQL fragments now request label ids; convert_linear_issue
returns label_ids alongside Issue and relations. All call sites updated
to write issue_labels via db.replace_issue_labels."
```

---

## Task 7: Linear — fetch and sync the workspace label catalog

**Files:**
- Modify: `crates/rectilinear-core/src/linear/mod.rs`

- [ ] **Step 1: Add `LabelCatalogEntry` and `fetch_labels`**

After the existing `get_label_ids` method (around line 789), add:

```rust
    /// Fetch the full label catalog for the workspace (all pages).
    pub async fn fetch_labels(&self) -> Result<Vec<LabelCatalogEntry>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let after_param = match cursor {
                Some(ref c) => format!(", after: \"{}\"", c),
                None => String::new(),
            };
            let query = format!(
                r#"query {{
                    issueLabels(first: 250{}) {{
                        nodes {{ id name color parent {{ id }} }}
                        pageInfo {{ hasNextPage endCursor }}
                    }}
                }}"#,
                after_param
            );
            let data: serde_json::Value = self.query(&query, serde_json::json!({})).await?;
            let nodes = data["issueLabels"]["nodes"]
                .as_array()
                .context("No issueLabels.nodes in response")?;
            for n in nodes {
                let id = n["id"].as_str().context("label has no id")?.to_string();
                let name = n["name"].as_str().unwrap_or("").to_string();
                let color = n["color"].as_str().map(|s| s.to_string());
                let parent_id = n["parent"]["id"].as_str().map(|s| s.to_string());
                out.push(LabelCatalogEntry { id, name, color, parent_id });
            }
            let has_next = data["issueLabels"]["pageInfo"]["hasNextPage"].as_bool().unwrap_or(false);
            if !has_next { break; }
            cursor = data["issueLabels"]["pageInfo"]["endCursor"].as_str().map(|s| s.to_string());
            if cursor.is_none() { break; }
        }
        Ok(out)
    }

    /// Sync the workspace's label catalog into the local database.
    /// Upserts labels by id and removes labels that no longer exist remotely.
    pub async fn sync_labels_catalog(&self, db: &Database, workspace_id: &str) -> Result<usize> {
        let entries = self.fetch_labels().await?;
        let keep_ids: Vec<String> = entries.iter().map(|e| e.id.clone()).collect();
        for e in &entries {
            db.upsert_label(&db::Label {
                id: e.id.clone(),
                workspace_id: workspace_id.to_string(),
                name: e.name.clone(),
                color: e.color.clone(),
                parent_id: e.parent_id.clone(),
            })?;
        }
        db.delete_labels_for_workspace_not_in(workspace_id, &keep_ids)?;
        Ok(entries.len())
    }
```

Add the type near the other public types (after `TeamNode`, around line 142):

```rust
#[derive(Debug, Clone)]
pub struct LabelCatalogEntry {
    pub id: String,
    pub name: String,
    pub color: Option<String>,
    pub parent_id: Option<String>,
}
```

- [ ] **Step 2: Build to verify it compiles**

Run: `cargo build`
Expected: clean build.

- [ ] **Step 3: Commit**

```bash
git add crates/rectilinear-core/src/linear/mod.rs
git commit -m "Fetch workspace label catalog from Linear and sync into local table

fetch_labels paginates issueLabels(first: 250). sync_labels_catalog
upserts each label by id and removes any label not returned by the API."
```

---

## Task 8: Linear — call `sync_labels_catalog` from `sync_team`

**Files:**
- Modify: `crates/rectilinear-core/src/linear/mod.rs`

- [ ] **Step 1: Add the labels-sync step to `sync_team`**

In `sync_team` (around line 380), insert at the very top of the function body (before the existing cursor logic):

```rust
        // Refresh workspace label catalog before syncing issues so issue_labels
        // can be populated. Linear labels are workspace-scoped, so this runs
        // per-call (cheap: one paginated query).
        if let Err(e) = self.sync_labels_catalog(db, workspace_id).await {
            eprintln!("warning: failed to sync label catalog for workspace '{}': {}", workspace_id, e);
        }
```

This is intentionally a soft-failure: if Linear's labels endpoint is briefly unavailable, issue sync should still proceed using whatever labels are already cached. Issues that reference an unknown label id will be skipped by `replace_issue_labels` with a warning.

- [ ] **Step 2: Build and run all tests**

Run: `cargo build && cargo test`
Expected: clean build, all existing tests pass. (Sync behavior change is exercised by the manual smoke test in Task 13.)

- [ ] **Step 3: Commit**

```bash
git add crates/rectilinear-core/src/linear/mod.rs
git commit -m "Sync label catalog at the start of every sync_team call

Soft-fails so issue sync can still proceed if the labels endpoint is
unavailable. Unknown label ids on issues are logged and skipped by
replace_issue_labels."
```

---

## Task 9: Linear — `assignee_id` parameter on `create_issue` and `update_issue`

**Files:**
- Modify: `crates/rectilinear-core/src/linear/mod.rs`

- [ ] **Step 1: Add `assignee_id` to `LinearClient::create_issue`**

Find `pub async fn create_issue(` (around line 437). Add the parameter before `parent_id`:

```rust
    pub async fn create_issue(
        &self,
        team_id: &str,
        title: &str,
        description: Option<&str>,
        priority: Option<i32>,
        label_ids: &[String],
        assignee_id: Option<&str>,
        parent_id: Option<&str>,
    ) -> Result<(String, String)> {
```

In the input building block, after the `label_ids` branch and before `parent_id`:

```rust
        if let Some(aid) = assignee_id {
            input["assigneeId"] = serde_json::Value::String(aid.to_string());
        }
```

- [ ] **Step 2: Add `assignee_id` to `LinearClient::update_issue`**

Find `pub async fn update_issue(` (around line 510). Add the parameter at the end:

```rust
    pub async fn update_issue(
        &self,
        issue_id: &str,
        title: Option<&str>,
        description: Option<&str>,
        priority: Option<i32>,
        state_id: Option<&str>,
        label_ids: Option<&[String]>,
        project_id: Option<&str>,
        assignee_id: Option<&str>,
    ) -> Result<()> {
```

In the input building block, after the `project_id` branch:

```rust
        if let Some(aid) = assignee_id {
            // Empty string is the convention used elsewhere (project_id) to clear the field.
            input.insert("assigneeId".into(), serde_json::Value::String(aid.to_string()));
        }
```

- [ ] **Step 3: Update every caller in `src/mcp/mod.rs`**

Find every call to `client.create_issue(` and `client.update_issue(` and add `None` as the new arg. (We'll wire real values in Tasks 12–14.)

```bash
grep -n "client$.create_issue\|client$.update_issue" src/mcp/mod.rs
# (the regex above won't actually match — use:)
grep -n "\.create_issue(\|\.update_issue(" src/mcp/mod.rs
```

Each existing call adds `None` for the new positional parameter. For example, in `create_issue` (around line 564):

```rust
        let (issue_id, identifier) = client
            .create_issue(
                &team_id,
                &args.title,
                args.description.as_deref(),
                args.priority,
                &[],
                None,                  // assignee_id (wired in Task 12)
                parent_id.as_deref(),
            )
            .await
            .map_err(|e| e.to_string())?;
```

In `update_issue` (around line 660):

```rust
        client
            .update_issue(
                &issue.id,
                args.title.as_deref(),
                safe_description.as_deref(),
                args.priority,
                state_id.as_deref(),
                label_ids.as_deref(),
                project_id.as_deref(),
                None,                  // assignee_id (wired in Task 13)
            )
            .await
            .map_err(|e| e.to_string())?;
```

In `mark_triaged` (around line 1089):

```rust
        client
            .update_issue(
                &issue.id,
                args.title.as_deref(),
                safe_description.as_deref(),
                Some(args.priority),
                state_id.as_deref(),
                label_ids.as_deref(),
                project_id.as_deref(),
                None,                  // assignee_id (wired in Task 14)
            )
            .await
            .map_err(|e| e.to_string())?;
```

- [ ] **Step 4: Build and test**

Run: `cargo build && cargo test`
Expected: clean build, all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/rectilinear-core/src/linear/mod.rs src/mcp/mod.rs
git commit -m "Plumb assignee_id through Linear create_issue and update_issue

Threaded as Option<&str>; existing call sites pass None pending the MCP
layer wiring."
```

---

## Task 10: Linear — `resolve_assignee_id` with `"me"` shortcut

**Files:**
- Modify: `crates/rectilinear-core/src/linear/mod.rs`

- [ ] **Step 1: Cache viewer id on `LinearClient`**

Change the struct (around line 11):

```rust
use std::sync::{Arc, RwLock};

#[derive(Clone)]
pub struct LinearClient {
    client: reqwest::Client,
    api_key: String,
    viewer_id: Arc<RwLock<Option<String>>>,
}
```

Update every constructor (`new`, `with_api_key`, `with_http_client`, around lines 229–252) to initialise it:

```rust
    pub fn new(config: &Config) -> Result<Self> {
        let api_key = config.linear_api_key()?.to_string();
        let client = reqwest::Client::new();
        Ok(Self { client, api_key, viewer_id: Arc::new(RwLock::new(None)) })
    }

    pub fn with_api_key(api_key: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.to_string(),
            viewer_id: Arc::new(RwLock::new(None)),
        }
    }

    pub fn with_http_client(client: reqwest::Client, api_key: &str) -> Self {
        Self {
            client,
            api_key: api_key.to_string(),
            viewer_id: Arc::new(RwLock::new(None)),
        }
    }
```

- [ ] **Step 2: Add `resolve_assignee_id`**

After `get_label_ids` (or grouped near `get_state_id`):

```rust
    /// Resolve an assignee identifier to a Linear user id.
    ///
    /// - `"me"` (case-insensitive) → cached `viewer.id`.
    /// - `"none"` (case-insensitive) → empty string (caller decides whether that's allowed).
    /// - Anything else → case-insensitive `name` lookup. Errors if zero or multiple matches.
    pub async fn resolve_assignee_id(&self, input: &str) -> Result<String> {
        let trimmed = input.trim();
        if trimmed.eq_ignore_ascii_case("none") {
            return Ok(String::new());
        }
        if trimmed.eq_ignore_ascii_case("me") {
            if let Some(cached) = self.viewer_id.read().unwrap().clone() {
                return Ok(cached);
            }
            let data: serde_json::Value = self
                .query("query { viewer { id } }", serde_json::json!({}))
                .await?;
            let id = data["viewer"]["id"]
                .as_str()
                .context("viewer query returned no id")?
                .to_string();
            *self.viewer_id.write().unwrap() = Some(id.clone());
            return Ok(id);
        }

        // Name lookup. Linear's `users` query has no `eqIgnoreCase`; fetch and filter locally.
        let data: serde_json::Value = self
            .query(
                "query { users(first: 250) { nodes { id name } } }",
                serde_json::json!({}),
            )
            .await?;
        let nodes = data["users"]["nodes"]
            .as_array()
            .context("users query returned no nodes")?;
        let matches: Vec<(String, String)> = nodes
            .iter()
            .filter_map(|n| {
                let name = n["name"].as_str()?;
                if name.eq_ignore_ascii_case(trimmed) {
                    Some((n["id"].as_str()?.to_string(), name.to_string()))
                } else {
                    None
                }
            })
            .collect();

        match matches.len() {
            0 => anyhow::bail!("Assignee '{}' not found in Linear users.", trimmed),
            1 => Ok(matches.into_iter().next().unwrap().0),
            _ => {
                let names: Vec<&str> = matches.iter().map(|(_, n)| n.as_str()).collect();
                anyhow::bail!(
                    "Assignee '{}' matched multiple users: {}. Use a more specific name.",
                    trimmed,
                    names.join(", ")
                )
            }
        }
    }
```

- [ ] **Step 3: Build and test**

Run: `cargo build && cargo test`
Expected: clean build, all tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/rectilinear-core/src/linear/mod.rs
git commit -m "Resolve assignee identifiers via 'me' shortcut, name, or 'none'

viewer.id cached per LinearClient via Arc<RwLock>. Name lookup is
case-insensitive; ambiguous matches return an actionable error."
```

---

## Task 11: Search — propagate `label_ids` through `SearchParams`

**Files:**
- Modify: `crates/rectilinear-core/src/search/mod.rs`

The vector search path returns issues by similarity, then post-filters. The FTS path goes straight through `db.fts_search`. Plumb a `label_ids` filter into both.

- [ ] **Step 1: Extend `SearchParams`**

In `crates/rectilinear-core/src/search/mod.rs` (around line 43):

```rust
pub struct SearchParams<'a> {
    pub query: &'a str,
    pub mode: SearchMode,
    pub team_key: Option<&'a str>,
    pub state_filter: Option<&'a str>,
    pub label_ids: Option<&'a [String]>,
    pub limit: usize,
    pub embedder: Option<&'a Embedder>,
    pub rrf_k: u32,
    pub workspace_id: &'a str,
}
```

- [ ] **Step 2: Wire `label_ids` through `fts_search` and `vector_search`**

Update the `fts_search` private function in `search/mod.rs` (line 112) to accept and forward `label_ids`:

```rust
fn fts_search(
    db: &Database,
    query: &str,
    limit: usize,
    workspace_id: &str,
    label_ids: Option<&[String]>,
) -> Result<Vec<SearchResult>> {
    let fts_query = build_fts_query(query);
    let fts_results = db.fts_search_filtered(&fts_query, limit, workspace_id, label_ids)?;
    // ... rest unchanged
```

Update `vector_search` (line 139) similarly:

```rust
async fn vector_search(
    db: &Database,
    query: &str,
    team_key: Option<&str>,
    limit: usize,
    embedder: &Embedder,
    workspace_id: &str,
    label_ids: Option<&[String]>,
) -> Result<Vec<SearchResult>> {
```

For vector search, post-filter results by checking each candidate issue's label set. After the existing `let results: Vec<_> = results.into_iter().take(limit).enumerate().filter_map(...)` block produces `SearchResult`s, add a label filter step:

```rust
    let results = if let Some(required_ids) = label_ids.filter(|ids| !ids.is_empty()) {
        results
            .into_iter()
            .filter(|r| {
                let issue_labels = db.get_issue_label_ids(&r.issue_id).unwrap_or_default();
                required_ids.iter().all(|req| issue_labels.contains(req))
            })
            .collect()
    } else {
        results
    };
```

Update `pub async fn search` to extract `label_ids` from params and pass it through:

```rust
    let SearchParams {
        query,
        mode,
        team_key,
        state_filter,
        label_ids,
        limit,
        embedder,
        rrf_k,
        workspace_id,
    } = params;
    let results = match mode {
        SearchMode::Fts => fts_search(db, query, limit * 2, workspace_id, label_ids)?,
        SearchMode::Vector => {
            let embedder =
                embedder.ok_or_else(|| anyhow::anyhow!("Embedder required for vector search"))?;
            vector_search(db, query, team_key, limit * 2, embedder, workspace_id, label_ids).await?
        }
        SearchMode::Hybrid => {
            let fts_results = fts_search(db, query, limit * 3, workspace_id, label_ids)?;

            if let Some(embedder) = embedder {
                let vec_results =
                    vector_search(db, query, team_key, limit * 3, embedder, workspace_id, label_ids).await?;
                reciprocal_rank_fusion(fts_results, vec_results, rrf_k, 0.3, 0.7)
            } else {
                fts_results
            }
        }
    };
```

- [ ] **Step 3: Update `find_duplicates` to forward `None`**

`pub async fn find_duplicates` calls `search(...)` — add `label_ids: None` to its `SearchParams`:

```rust
    let mut results = search(
        db,
        SearchParams {
            query: text,
            mode: SearchMode::Hybrid,
            team_key,
            state_filter: None,
            label_ids: None,
            limit,
            embedder: Some(embedder),
            rrf_k,
            workspace_id,
        },
    )
    .await?;
```

Same for the inline `vector_search` call inside `find_duplicates` — pass `None` for `label_ids`.

- [ ] **Step 4: Update every external caller of `SearchParams`**

```bash
grep -rn "SearchParams" src/ crates/
```

The MCP `search_issues` (`src/mcp/mod.rs` around line 427) constructs `SearchParams` — add `label_ids: None` there for now. Same for any other call sites (CLI search command, etc.). Tasks 15 and 16 will wire real values.

```bash
grep -rn "SearchParams {" src/ crates/
```

For each match, add `label_ids: None,`.

- [ ] **Step 5: Build and run all tests**

Run: `cargo build && cargo test`
Expected: clean build, all tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/rectilinear-core/src/search/mod.rs src/ crates/rectilinear-core/src/cli
git commit -m "Plumb label_ids through SearchParams to FTS and vector paths

FTS uses fts_search_filtered. Vector path post-filters via
db.get_issue_label_ids. Existing call sites pass None pending MCP wiring."
```

---

## Task 12: MCP — `assignee` and `labels` on `create_issue`

**Files:**
- Modify: `src/mcp/mod.rs`

- [ ] **Step 1: Add fields to `CreateIssueArgs`**

Find `struct CreateIssueArgs` (line 247):

```rust
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct CreateIssueArgs {
    /// Workspace name (required). Use list_workspaces to see available workspaces.
    workspace: Option<String>,
    /// Team key (e.g., "ENG")
    team: String,
    /// Issue title
    title: String,
    /// Issue description
    description: Option<String>,
    /// Priority: 1=Urgent, 2=High, 3=Medium, 4=Low
    priority: Option<i32>,
    /// Parent issue identifier to create as sub-issue (e.g., "CUT-42")
    parent: Option<String>,
    /// Set labels by name (case-insensitive). Use list_labels to discover.
    labels: Option<Vec<String>>,
    /// Assignee. Pass "me" to assign to the authenticated user, or a name (case-insensitive).
    assignee: Option<String>,
}
```

- [ ] **Step 2: Wire resolution in `create_issue`**

In the `create_issue` MCP method body (around line 543), after computing `parent_id` and before calling `client.create_issue(...)`:

```rust
        // Resolve labels (if provided) — local catalog first, fall back to remote on empty cache.
        let label_ids: Vec<String> = if let Some(ref names) = args.labels {
            let (resolved, unknown) = self.db
                .resolve_label_ids_local(&workspace, names)
                .map_err(|e| e.to_string())?;
            if !unknown.is_empty() {
                let suggestions = suggest_label_names(&self.db, &workspace, &unknown);
                return Err(format!(
                    "Label{} {} not found. {}Run list_labels for the full set.",
                    if unknown.len() == 1 { "" } else { "s" },
                    unknown.iter().map(|s| format!("'{}'", s)).collect::<Vec<_>>().join(", "),
                    if suggestions.is_empty() { String::new() }
                    else { format!("Did you mean: {}? ", suggestions.join(", ")) }
                ));
            }
            // Stale-catalog fallback: if local catalog is empty, fall through to remote resolution.
            if resolved.is_empty() && !names.is_empty() {
                client.get_label_ids(names).await.map_err(|e| e.to_string())?
            } else {
                resolved
            }
        } else {
            Vec::new()
        };

        // Resolve assignee.
        let assignee_id: Option<String> = if let Some(ref a) = args.assignee {
            if a.eq_ignore_ascii_case("none") {
                return Err("Cannot use 'none' on create_issue; omit the parameter to leave unassigned.".to_string());
            }
            Some(client.resolve_assignee_id(a).await.map_err(|e| e.to_string())?)
        } else {
            None
        };
```

Replace the existing `client.create_issue(...)` call to pass these:

```rust
        let (issue_id, identifier) = client
            .create_issue(
                &team_id,
                &args.title,
                args.description.as_deref(),
                args.priority,
                &label_ids,
                assignee_id.as_deref(),
                parent_id.as_deref(),
            )
            .await
            .map_err(|e| e.to_string())?;
```

- [ ] **Step 3: Add the `suggest_label_names` helper**

Anywhere convenient near the top of `src/mcp/mod.rs` (e.g. after `extract_code_hints`):

```rust
/// Suggest up to 3 label names from the local catalog matching any unknown name as a substring.
fn suggest_label_names(db: &Database, workspace: &str, unknown: &[String]) -> Vec<String> {
    let Ok(catalog) = db.list_labels(workspace) else { return Vec::new() };
    let mut hits: Vec<String> = Vec::new();
    for u in unknown {
        let needle = u.to_lowercase();
        for label in &catalog {
            let hay = label.name.to_lowercase();
            if hay.contains(&needle) || needle.contains(&hay) {
                if !hits.contains(&label.name) {
                    hits.push(label.name.clone());
                    if hits.len() >= 3 { return hits; }
                }
            }
        }
    }
    hits
}
```

You may need to add `use rectilinear_core::db::Database;` if it isn't already imported (check the existing imports).

- [ ] **Step 4: Update the tool description**

In the `#[tool(...)] async fn create_issue` attribute (line 532), append a note to the description:

```rust
    #[tool(
        name = "create_issue",
        description = "Create a new issue in Linear. Specify team (key like 'ENG'), title, and optionally description, priority (1=Urgent, 2=High, 3=Medium, 4=Low), labels (list of names), and assignee ('me' for self-assign, or a user's display name).

IMPORTANT — Before calling this tool, you MUST:

1. **Disambiguate the request.** Ask the user 2-4 clarifying questions to sharpen scope, acceptance criteria, and edge cases. Think like a principal engineer: what assumptions are you making? What could go wrong? What's in vs. out of scope? Do not create the issue until the user has answered.

2. **Check for duplicates.** Call find_duplicates with the intended title/description to verify this issue doesn't already exist. If a match is found (>0.8 similarity), show it to the user and ask whether to proceed, update the existing issue, or cancel.

3. **Write a clear title and description.** The title should be imperative and specific (e.g. 'Add rate limiting to /api/upload endpoint' not 'rate limiting'). The description should include: what the desired behavior is, why it matters, and any constraints or acceptance criteria surfaced during disambiguation."
    )]
```

- [ ] **Step 5: Build and test**

Run: `cargo build && cargo test`
Expected: clean build, all tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/mcp/mod.rs
git commit -m "Expose labels and assignee on create_issue MCP tool

Local catalog resolution with did-you-mean suggestions and a
stale-catalog fallback to the remote issueLabels query. assignee
supports 'me' shortcut; 'none' on create is rejected."
```

---

## Task 13: MCP — `assignee` on `update_issue`

**Files:**
- Modify: `src/mcp/mod.rs`

- [ ] **Step 1: Add `assignee` to `UpdateIssueArgs`**

Find `struct UpdateIssueArgs` (line 263):

```rust
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct UpdateIssueArgs {
    /// Workspace name (required). Use list_workspaces to see available workspaces.
    workspace: Option<String>,
    /// Issue ID or identifier
    id: String,
    /// New title
    title: Option<String>,
    /// New description
    description: Option<String>,
    /// New priority
    priority: Option<i32>,
    /// Set issue state by name (e.g., "Done", "Cancelled", "In Progress")
    state: Option<String>,
    /// Set labels by name (replaces all existing labels)
    labels: Option<Vec<String>>,
    /// Set project by name (or "none" to remove from project)
    project: Option<String>,
    /// Assignee. Pass "me" for self-assign, "none" to clear, or a name (case-insensitive).
    assignee: Option<String>,
}
```

- [ ] **Step 2: Wire resolution in `update_issue`**

In the `update_issue` MCP method body, after the existing `project_id` block and before the description-preservation logic:

```rust
        let assignee_id: Option<String> = if let Some(ref a) = args.assignee {
            Some(client.resolve_assignee_id(a).await.map_err(|e| e.to_string())?)
        } else {
            None
        };
```

Update the `client.update_issue(...)` call to pass `assignee_id.as_deref()` instead of `None`.

- [ ] **Step 3: Build and test**

Run: `cargo build && cargo test`
Expected: clean build, all tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/mcp/mod.rs
git commit -m "Expose assignee on update_issue MCP tool

'me' / 'none' / display name; 'none' clears via empty assigneeId."
```

---

## Task 14: MCP — `assignee` on `mark_triaged`

**Files:**
- Modify: `src/mcp/mod.rs`

- [ ] **Step 1: Add `assignee` to `MarkTriagedArgs`**

Find `struct MarkTriagedArgs` (line 332):

```rust
    /// Set project by name (or "none" to remove from project)
    project: Option<String>,
    /// Assignee. Pass "me" for self-assign, "none" to clear, or a name (case-insensitive).
    assignee: Option<String>,
}
```

- [ ] **Step 2: Wire resolution and pass through**

In the `mark_triaged` MCP method body, after the `project_id` block:

```rust
        let assignee_id: Option<String> = if let Some(ref a) = args.assignee {
            Some(client.resolve_assignee_id(a).await.map_err(|e| e.to_string())?)
        } else {
            None
        };
```

Replace the `None` placeholder in the `client.update_issue(...)` call with `assignee_id.as_deref()`.

- [ ] **Step 3: Build and test**

Run: `cargo build && cargo test`
Expected: clean build, all tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/mcp/mod.rs
git commit -m "Expose assignee on mark_triaged MCP tool"
```

---

## Task 15: MCP — `labels` filter on `search_issues` and `get_triage_queue`

**Files:**
- Modify: `src/mcp/mod.rs`

- [ ] **Step 1: Add `labels` field to `SearchArgs` and `GetTriageQueueArgs`**

In `SearchArgs` (line 205):

```rust
    /// Filter to issues that have ALL of these labels (case-insensitive).
    labels: Option<Vec<String>>,
```

In `GetTriageQueueArgs` (line 315):

```rust
    /// Filter to issues that have ALL of these labels (case-insensitive).
    labels: Option<Vec<String>>,
```

- [ ] **Step 2: Wire resolution into `search_issues`**

In the `search_issues` MCP method body, after computing `embedder`:

```rust
        let label_ids = if let Some(ref names) = args.labels {
            let (resolved, unknown) = self.db
                .resolve_label_ids_local(&workspace, names)
                .map_err(|e| e.to_string())?;
            if !unknown.is_empty() {
                if resolved.is_empty() && self.db.list_labels(&workspace).map_err(|e| e.to_string())?.is_empty() {
                    return Err(format!(
                        "No labels synced yet for workspace '{}'. Run sync_team first.",
                        workspace
                    ));
                }
                let suggestions = suggest_label_names(&self.db, &workspace, &unknown);
                return Err(format!(
                    "Label{} {} not found. {}Run list_labels for the full set.",
                    if unknown.len() == 1 { "" } else { "s" },
                    unknown.iter().map(|s| format!("'{}'", s)).collect::<Vec<_>>().join(", "),
                    if suggestions.is_empty() { String::new() }
                    else { format!("Did you mean: {}? ", suggestions.join(", ")) }
                ));
            }
            Some(resolved)
        } else {
            None
        };
```

Pass it to `SearchParams`:

```rust
        let results = search::search(
            &self.db,
            search::SearchParams {
                query: &args.query,
                mode,
                team_key: args.team.as_deref(),
                state_filter: args.state.as_deref(),
                label_ids: label_ids.as_deref(),
                limit,
                embedder: embedder.as_ref(),
                rrf_k: self.config.search.rrf_k,
                workspace_id: &workspace,
            },
        )
```

- [ ] **Step 3: Wire resolution into `get_triage_queue`**

In the `get_triage_queue` MCP method body, after the incremental `sync_team` call:

```rust
        let label_ids = if let Some(ref names) = args.labels {
            let (resolved, unknown) = self.db
                .resolve_label_ids_local(&workspace, names)
                .map_err(|e| e.to_string())?;
            if !unknown.is_empty() {
                if resolved.is_empty() && self.db.list_labels(&workspace).map_err(|e| e.to_string())?.is_empty() {
                    return Err(format!(
                        "No labels synced yet for workspace '{}'. Run sync_team first.",
                        workspace
                    ));
                }
                let suggestions = suggest_label_names(&self.db, &workspace, &unknown);
                return Err(format!(
                    "Label{} {} not found. {}Run list_labels for the full set.",
                    if unknown.len() == 1 { "" } else { "s" },
                    unknown.iter().map(|s| format!("'{}'", s)).collect::<Vec<_>>().join(", "),
                    if suggestions.is_empty() { String::new() }
                    else { format!("Did you mean: {}? ", suggestions.join(", ")) }
                ));
            }
            Some(resolved)
        } else {
            None
        };
```

Replace the call to `get_unprioritized_issues` with the filtered variant:

```rust
        let all_issues = self
            .db
            .get_unprioritized_issues_filtered(
                Some(&args.team),
                args.include_completed.unwrap_or(false),
                &workspace,
                label_ids.as_deref(),
            )
            .map_err(|e| e.to_string())?;
```

- [ ] **Step 4: Build and test**

Run: `cargo build && cargo test`
Expected: clean build, all tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/mcp/mod.rs
git commit -m "Filter search_issues and get_triage_queue by labels (AND, case-insensitive)"
```

---

## Task 16: MCP — `list_labels` tool

**Files:**
- Modify: `src/mcp/mod.rs`

- [ ] **Step 1: Add `ListLabelsArgs` and the tool method**

Add the args struct near the others (e.g. after `ListWorkspacesArgs`):

```rust
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct ListLabelsArgs {
    /// Workspace name (required). Use list_workspaces to see available workspaces.
    workspace: Option<String>,
}
```

Add the tool inside `impl RectilinearMcp` (somewhere alongside `list_workspaces`):

```rust
    #[tool(
        name = "list_labels",
        description = "List all labels in the workspace, grouped by parent. Pure local read — no Linear API call. If empty, run sync_team to refresh the catalog."
    )]
    async fn list_labels(
        &self,
        #[tool(aggr)] args: ListLabelsArgs,
    ) -> Result<String, String> {
        let workspace = self.require_workspace(&args.workspace)?;
        let labels = self.db.list_labels(&workspace).map_err(|e| e.to_string())?;

        // Index by id for parent name lookup.
        let by_id: std::collections::HashMap<&str, &str> =
            labels.iter().map(|l| (l.id.as_str(), l.name.as_str())).collect();

        // Group: top-level (no parent) and grouped-by-parent.
        let mut top_level: Vec<&_> = labels.iter().filter(|l| l.parent_id.is_none()).collect();
        top_level.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

        let mut groups: std::collections::BTreeMap<String, Vec<&_>> = std::collections::BTreeMap::new();
        for l in labels.iter().filter(|l| l.parent_id.is_some()) {
            let parent_name = l.parent_id.as_deref()
                .and_then(|pid| by_id.get(pid).copied())
                .unwrap_or("(unknown group)")
                .to_string();
            groups.entry(parent_name).or_default().push(l);
        }
        for v in groups.values_mut() {
            v.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        }

        let payload = serde_json::json!({
            "workspace": workspace,
            "count": labels.len(),
            "top_level": top_level.iter().map(|l| serde_json::json!({
                "name": l.name,
                "color": l.color,
            })).collect::<Vec<_>>(),
            "groups": groups.iter().map(|(parent, members)| serde_json::json!({
                "parent": parent,
                "labels": members.iter().map(|l| serde_json::json!({
                    "name": l.name,
                    "color": l.color,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "note": if labels.is_empty() {
                "Catalog is empty. Run sync_team to populate labels."
            } else { "" },
        });

        serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())
    }
```

- [ ] **Step 2: Build and test**

Run: `cargo build && cargo test`
Expected: clean build, all tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/mcp/mod.rs
git commit -m "Add list_labels MCP tool

Returns workspace label catalog grouped by parent, sorted alphabetically.
Pure local SQL; encourages sync_team if catalog is empty."
```

---

## Task 17: End-to-end manual smoke test

**Files:**
- (none — verification only)

This task verifies the full pipeline against a real Linear workspace. It is the only thing exercising the actual GraphQL queries.

- [ ] **Step 1: Build the release binary**

Run: `cargo build --release`
Expected: clean build.

- [ ] **Step 2: Start the MCP server (or use existing CLI)**

If a running Claude Code session has `rectilinear` MCP wired in, restart it so it picks up the new binary. Otherwise:

Run: `./target/release/rectilinear sync --team <YOUR_TEAM>`
Expected: full sync runs (because migration 8 reset `sync_state`); progress shows total issues; exits 0.

- [ ] **Step 3: Confirm the labels catalog populated**

Run a quick SQL check via the CLI or sqlite3:

```bash
sqlite3 ~/.local/share/rectilinear/rectilinear.db "SELECT COUNT(*) FROM labels;"
sqlite3 ~/.local/share/rectilinear/rectilinear.db "SELECT COUNT(*) FROM issue_labels;"
```

Expected: both > 0.

- [ ] **Step 4: Exercise `list_labels` via MCP**

In Claude Code, call:

```
mcp__rectilinear__list_labels
```

Expected: returns the workspace's label catalog grouped by parent.

- [ ] **Step 5: Exercise `create_issue` with labels and `assignee: "me"`**

In Claude Code, call:

```
mcp__rectilinear__create_issue
  team: <YOUR_TEAM>
  title: "rectilinear smoke test — assignees and labels"
  priority: 4
  labels: ["<some-real-label>"]
  assignee: "me"
```

Expected: returns identifier; opening the issue in Linear shows you assigned and the label set.

- [ ] **Step 6: Exercise label filtering**

Pick a label that scopes a meaningful set of issues. Call:

```
mcp__rectilinear__get_triage_queue
  team: <YOUR_TEAM>
  labels: ["<that-label>"]
  limit: 5
```

Expected: returns only issues that carry that label.

```
mcp__rectilinear__search_issues
  query: "<some keyword>"
  labels: ["<that-label>"]
```

Expected: only label-scoped results.

- [ ] **Step 7: Exercise assignee error paths**

Try `assignee: "definitely-not-a-real-user"` on a `create_issue` or `update_issue` call.

Expected error: `"Assignee 'definitely-not-a-real-user' not found in Linear users."`

Try `assignee: "none"` on `create_issue`.

Expected error: `"Cannot use 'none' on create_issue; omit the parameter to leave unassigned."`

- [ ] **Step 8: Clean up the smoke-test issue**

In Linear, delete or cancel the smoke-test issue you created in Step 5.

- [ ] **Step 9: Final commit (if any tweaks needed)**

If the smoke test surfaced bugs, fix them, re-run the relevant unit tests, and commit per-fix with a descriptive message.

---

## Self-Review Notes (from plan author)

**Spec coverage check:** every requirement in the design spec maps to a task —

| Spec section | Tasks |
|---|---|
| Data model: `labels` + `issue_labels` tables, FK cascade, sync_state reset | Task 1 |
| `labels_json` retained on issues | Tasks 1, 6 (labels_json still computed in `convert_linear_issue`) |
| Sync flow: labels first, then issues, with delete-not-in pruning | Tasks 7, 8, 9 |
| GraphQL fragment change to include label `id` | Task 6 |
| Edge case: unknown label id during issue upsert | Task 3 (`replace_issue_labels` warns + skips) |
| `resolve_label_ids_local` | Task 4 |
| `resolve_assignee_id` (`me`/`none`/name with viewer cache) | Task 10 |
| Stale-catalog fallback for labels | Task 12 |
| `LinearClient::create_issue` / `update_issue` `assignee_id` param | Task 9 |
| MCP `create_issue`: `labels` + `assignee` | Task 12 |
| MCP `update_issue`: `assignee` | Task 13 |
| MCP `mark_triaged`: `assignee` | Task 14 |
| MCP `search_issues` + `get_triage_queue`: `labels` filter (AND, case-insensitive) | Tasks 5, 11, 15 |
| `list_labels` tool with parent grouping | Task 16 |
| Error messages match spec verbatim | Tasks 12, 13, 15 |
| Tests: db CRUD, cascades, AND-filter SQL, name resolution | Tasks 2, 3, 4, 5 |
| Tests: migration runs and resets sync_state | Task 1 |
| Manual end-to-end smoke test | Task 17 |

**Out-of-scope follow-ups (per spec):** launch-time toast/update-check, local users catalog, OR-semantics filter, label filter on `find_duplicates`/`issue_context`. None should creep into this plan.
