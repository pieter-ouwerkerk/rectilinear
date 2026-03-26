# Multi-Workspace Support for Rectilinear-Core

**Date:** 2026-03-25
**Status:** Approved

## Problem

Rectilinear is fundamentally single-workspace. One Linear API key in config, no workspace identity in the database, and all operations assume a single Linear workspace. Users who belong to multiple Linear workspaces (e.g., different orgs or clients) cannot search, triage, or manage issues across them from one rectilinear instance.

## Goal

Support multiple Linear workspaces in a single rectilinear installation with context-based switching (like `aws sts assume-role`), backed by a single shared SQLite database.

## Design

### Config Structure

Multiple named workspaces in `config.toml`:

```toml
default_workspace = "personal"

[workspaces.personal]
api_key = "lin_api_..."
default_team = "CUT"

[workspaces.work]
api_key = "lin_api_..."
default_team = "ENG"

[embedding]
# unchanged, shared across workspaces

[search]
# unchanged

[triage]
# unchanged
```

- Each workspace has a user-chosen name (the TOML key), an API key, and an optional default team.
- `default_workspace` sets which workspace is active by default.
- The old flat `[linear]` section is deprecated but supported as a single workspace named `"default"` for backward compatibility.
- Embedding, search, triage, and anthropic config remain global.

### Config CLI

- `rectilinear config` — prints current config (workspaces listed with their teams, active workspace highlighted).
- `rectilinear config add-workspace` — interactive flow: prompts for workspace name, API key, default team, and whether to set as default.
- `rectilinear config remove-workspace <name>` — removes a workspace entry (with confirmation if it's the active one).
- `rectilinear config show` — explicit alias for bare `rectilinear config`.

API key input is masked. The command reads/writes `config.toml` directly.

### Active Workspace Resolution

Highest priority wins:

1. `RECTILINEAR_WORKSPACE` environment variable
2. Persisted state from `rectilinear workspace assume <name>` (stored in `~/.local/share/rectilinear/active_workspace`)
3. `default_workspace` in config.toml

### Workspace CLI Commands

- `rectilinear workspace assume <name>` — sets the active workspace (persisted to state file).
- `rectilinear workspace list` — lists configured workspaces, marks the active one.
- `rectilinear workspace current` — prints the active workspace name.

### Database Schema Changes

**New table** — `workspaces`:

```sql
CREATE TABLE workspaces (
    id TEXT PRIMARY KEY,
    linear_org_id TEXT,
    display_name TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

- `id` is the user-chosen name from config.
- `linear_org_id` and `display_name` are populated on first sync from the Linear API's `organization` query.

**Column additions** — `workspace_id TEXT NOT NULL` added to:

- `issues` — unique constraint updated to include workspace_id.
- `sync_state` — composite PK becomes `(workspace_id, team_key)`.
- `chunks` — follows issue foreign key.
- `issue_relations` — follows issue foreign key.

**FTS5** — the `issues_fts` table is rebuilt from `issues` via triggers. No structural change needed; queries pre-filter by workspace before ranking.

**Migration strategy:**

- New migration (migration 7) adds the `workspaces` table, adds `workspace_id` columns with default `"default"`, then creates new composite indices.
- Existing data gets tagged as workspace `"default"` — upgrades are non-destructive.

### Query Scoping

All `Database` read methods gain a `workspace_id` parameter. Search (FTS + vector) filters by `workspace_id` before ranking.

### Linear Client Changes

Construction changes from config-level to workspace-level:

```rust
// Before: LinearClient::new(&config)  — pulls api_key from config.linear
// After:  LinearClient::new(api_key)   — receives the key for a specific workspace
```

The caller resolves the active workspace, looks up its API key, and passes it to `LinearClient::new`.

**Workspace identity discovery:** On first sync, the client queries Linear's `organization` field for org ID and display name, stored in the `workspaces` table. If two workspace configs point to the same org, a warning is issued.

No changes to GraphQL queries — they already scope by team. The workspace boundary is the API key, handled server-side by Linear.

### CLI Changes

**Global `--workspace` flag** on all commands that touch Linear or the database:

- `rectilinear sync --workspace work`
- `rectilinear search --workspace personal "auth bug"`
- If omitted, uses the resolved active workspace.

**Sync:**

- `rectilinear sync` syncs the active workspace's default team.
- `rectilinear sync --all-workspaces` syncs default teams across all configured workspaces (sequentially).
- Team flag still works: `rectilinear sync --team ENG --workspace work`.

**Single workspace shortcut:** If only one workspace is configured and `--workspace` is omitted, it's used implicitly. If multiple exist and no active workspace is set, CLI errors with guidance to run `rectilinear workspace assume <name>`.

### MCP Changes

**Workspace parameter required on all tools except `list_workspaces`:**

- All tools (read and write) require the `workspace` parameter.
- If omitted, return an error: `"workspace is required. Use list_workspaces to see available workspaces."`
- No implicit defaults in MCP context — explicit is better when an LLM is driving actions.

**New tool:**

- `list_workspaces` — returns configured workspaces with active indicator, so the caller knows what's available. Does not require a workspace parameter.

### Backward Compatibility

**Old config format:**

```toml
[linear]
api_key = "lin_api_..."
default_team = "CUT"
```

When detected (no `[workspaces]` section), treated as a single workspace named `"default"`. A deprecation warning is printed. `rectilinear config` offers to migrate to the new format.

**Database:** Existing data gets `workspace_id = "default"`. A `"default"` entry is added to the `workspaces` table. Everything works without config changes.

**MCP:** Even with a single workspace, the `workspace` parameter is still required (no special cases).

## Out of Scope

- **Cross-workspace search** — schema supports it, but no UX for querying all workspaces at once.
- **Cross-workspace duplicate detection** — same reasoning.
- **Per-workspace embedding/search config** — stays global.
- **Concurrent sync across workspaces** — `--all-workspaces` syncs sequentially.
