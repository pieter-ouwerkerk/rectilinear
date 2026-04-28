# Assignees and First-Class Labels

## Problem

The `create_issue` MCP tool can't set an assignee, so users have to assign themselves in the Linear UI after creation. Labels are stored on each issue as `labels_json` and indexed in FTS, but there's no label catalog and no way to filter queries by label — so workflow queries like "all untriaged Vanta issues" aren't possible without scanning every issue.

## Goals

- Let `create_issue`, `update_issue`, and `mark_triaged` set the assignee, including a `"me"` shortcut for self-assignment.
- Sync Linear's workspace label catalog into a local `labels` table and surface it via a `list_labels` MCP tool.
- Add a normalized `issue_labels` join table so `search_issues` and `get_triage_queue` can filter issues by label using SQL.

## Non-goals

- Local users catalog or `list_users` tool — out of scope; resolved on demand against Linear.
- Label filtering on `find_duplicates`, `issue_context`, or any tool other than `search_issues` and `get_triage_queue`.
- OR-semantics for multi-label filters; AND only.
- Launch-time toast or update-check mechanism (filed as follow-up).

## Design

### Data model

Two new tables, added in a single migration. `labels_json` on `issues` is kept as a derived denormalization that feeds the FTS5 `labels_text` column — it's not the source of truth.

```sql
CREATE TABLE labels (
    id TEXT PRIMARY KEY,                 -- Linear label id
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    name TEXT NOT NULL,
    color TEXT,
    parent_id TEXT,                      -- Linear's label group parent, if any
    UNIQUE (workspace_id, name COLLATE NOCASE)
);
CREATE INDEX idx_labels_workspace ON labels(workspace_id);

CREATE TABLE issue_labels (
    issue_id TEXT NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    label_id TEXT NOT NULL REFERENCES labels(id) ON DELETE CASCADE,
    PRIMARY KEY (issue_id, label_id)
);
CREATE INDEX idx_issue_labels_label ON issue_labels(label_id);
```

The migration also clears every row's sync state so the next `sync_team` call performs a full re-sync:

```sql
UPDATE sync_state SET full_sync_done = 0, last_updated_at = '1970-01-01T00:00:00Z';
```

This is acceptable because (a) Rectilinear has very few users today, (b) Linear sync is fast, and (c) it gives the cleanest invariant: `issue_labels` is always populated from a real Linear sync. The first `sync_team` after upgrading prints a one-line notice that a full re-sync is happening.

### Sync flow

`LinearClient::sync_team` gains a label-sync step that runs before issue fetching:

1. Paginated `issueLabels(first: 250)` query for the workspace, requesting `id`, `name`, `color`, and `parent { id }`.
2. Upsert each label into `labels` keyed by Linear id; rename in place if `name` changed.
3. Delete any local label for this workspace that wasn't returned. Cascades clean up `issue_labels`.

Issue sync is unchanged externally. Internally:

- The GraphQL fragment changes from `labels { nodes { name } }` to `labels { nodes { id name } }`.
- On issue upsert, the issue's labels are written to `issue_labels` keyed by `(issue_id, label_id)`. Existing rows for that issue are replaced (delete-then-insert in a transaction).
- `labels_json` is still computed from the same label list and written to the issue row, preserving FTS behavior.

**Edge case — issue references a label not present in the catalog:** can occur if a label was created between the labels page and the issues page. Skip the `issue_labels` insert for unknown ids and log a warning. The next sync corrects it.

**Multi-workspace concern:** labels live in the workspace, so multiple `sync_team` calls for different teams in the same workspace will refetch the same labels. One paginated query per call is cheap; no per-workspace caching for now.

### Resolution layer

A new local resolver becomes the primary path for label name → id, with the existing remote `get_label_ids` retained as a fallback for the empty-catalog case.

```rust
// Primary path: local SQL on the labels catalog.
// Returns (resolved_ids, unknown_names). Caller decides how to surface the unknowns.
fn resolve_label_ids_local(
    db: &Database,
    workspace_id: &str,
    names: &[String],
) -> Result<(Vec<String>, Vec<String>)>;

// "me" → cached viewer.id. "none" → empty string (matches project_id convention).
// Anything else → users name lookup, case-insensitive.
async fn resolve_assignee_id(client: &LinearClient, input: &str) -> Result<String>;
```

`resolve_assignee_id` rules:

- `"me"` (case-insensitive) → `viewer { id }`, cached per `LinearClient` instance.
- `"none"` (case-insensitive) → empty string. Mirrors the existing project-removal convention (`update_issue.rs` already uses `Some(String::new())` to clear `projectId`). The MCP layer rejects `"none"` on `create_issue` before reaching this resolver.
- Otherwise → `users(filter: { name: { eqIgnoreCase: $name } })`. The `name` field matches the `LinearUser { name }` already deserialized during issue sync, so what the agent sees in stored `assignee_name` is what it can pass back. Zero matches → error. Multiple matches → error listing the matched names.

**Stale-catalog fallback for labels:** if `resolve_label_ids_local` returns no rows for the workspace (catalog never synced), fall back to the existing remote `LinearClient::get_label_ids` so `create_issue` still works before the first sync. Logged when it happens.

`LinearClient::create_issue` and `update_issue` gain an `assignee_id: Option<&str>` parameter, passed through as `assigneeId` on the GraphQL `IssueCreateInput` / `IssueUpdateInput`.

### MCP tool surface

**`create_issue`** — adds two optional fields:

```rust
struct CreateIssueArgs {
    // ... existing fields ...
    /// Set labels by name (case-insensitive). Use list_labels to discover.
    labels: Option<Vec<String>>,
    /// Assignee. Pass "me" to assign to the authenticated user, or a name (case-insensitive).
    assignee: Option<String>,
}
```

**`update_issue`** — adds `assignee: Option<String>` (labels already exist). `"none"` clears the assignee.

**`mark_triaged`** — adds `assignee: Option<String>` with the same semantics as `update_issue`.

**`search_issues` and `get_triage_queue`** — add `labels: Option<Vec<String>>` with AND semantics, case-insensitive. Implementation:

```sql
WHERE issues.id IN (
    SELECT issue_id FROM issue_labels
    WHERE label_id IN (?, ?, ...)
    GROUP BY issue_id
    HAVING COUNT(DISTINCT label_id) = N
)
```

where `N` is the number of resolved label ids. Unknown label names cause the tool to error before running the query, listing the unknowns and suggesting close matches via substring scan against the local catalog.

**`list_labels`** — new tool:

```rust
#[tool(name = "list_labels", description = "List all labels in the workspace, grouped by parent.")]
async fn list_labels(workspace: Option<String>) -> Result<String, String>;
```

Returns `{ name, color, parent }` per label, sorted alphabetically with parented labels nested under their group. Pure local SQL on `labels` — no Linear API call.

### Errors and edge cases

Surfaced to the MCP caller as plain text in the tool result:

- Unknown label: `"Label 'foo' not found. Did you mean: 'Foo Bar', 'Food'? Run list_labels for the full set."`
- Unknown assignee: `"Assignee 'jane' not found in Linear users."`
- Ambiguous assignee: `"Assignee 'john' matched multiple users: 'John Smith', 'John Doe'. Use a more specific name."`
- `"none"` on create: `"Cannot use 'none' on create_issue; omit the parameter to leave unassigned."`
- Empty catalog on label-filter query: `"No labels synced yet for workspace 'foo'. Run sync_team first."`

### Testing

- `db` unit tests: upsert/delete labels (including rename); upsert issue with labels writes both `labels_json` and `issue_labels`; cascade delete of issue removes `issue_labels` rows; case-insensitive name resolution; AND-multi-label filter SQL.
- `linear` resolver unit tests: `"me"` shortcut hits `viewer` and is cached; ambiguous-name error; case-insensitive name match.
- Migration test: seed a DB at the previous schema version, run migrations, verify `sync_state.full_sync_done = 0` for all teams, both new tables created, existing issue/comment rows untouched.
- MCP integration: `create_issue` with `labels` + `assignee` returns the new identifier and the issue is persisted with both; `search_issues` with `labels: ["A", "B"]` returns only issues that have both.

## Follow-ups

- **Launch-time toast / update-check mechanism.** On `rectilinear` startup, check for a newer release and, if a migration like this one was applied, encourage the user to run a full re-sync. Worth its own small spec.
- **Local users catalog + `list_users` tool.** Symmetric with this work but no current driver.
- **OR-semantics label filter** via an explicit `match: "any" | "all"` param.
- **Label filtering on `find_duplicates` and `issue_context`** if usage justifies it.
