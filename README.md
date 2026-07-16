# Rectilinear

Local-first Linear issue intelligence. Maintains a search-optimized SQLite mirror of your Linear issues, projects, and project milestones with hybrid full-text + vector search. Find duplicates before filing, search across teams instantly, and manage complete project hierarchies from the terminal, native clients, or MCP.

## Why

Linear teams accumulate hundreds of issues. Duplicate detection is hard, search is scattered, and context lives across many views. Rectilinear keeps a local copy with embeddings so you can:

- **Find duplicates** before creating new issues — semantic similarity, not just keyword matching
- **Search fast** — hybrid FTS5 + vector search with Reciprocal Rank Fusion, all local
- **Manage issues** from the CLI or let Claude Code do it through MCP tools
- **Preserve project structure** with first-class project/milestone metadata and portable imports that include linked issues

Linear remains the source of truth. The local database is a read-optimized cache. Writes go to Linear first, then sync back.

> Just want to wire this up to Claude Code and start filing/triaging issues by voice? See [QUICKSTART.md](QUICKSTART.md).

## Architecture

```
┌─────────────────────────────────────────────────┐
│                  rectilinear                     │
│                                                  │
│  CLI (clap)              MCP Server (rmcp)       │
│  ┌──────────┐            ┌──────────────────┐    │
│  │ sync     │            │ search_issues    │    │
│  │ projects │            │ import_project   │    │
│  │ milestone│            │ project CRUD     │    │
│  │ search   │            │ find_duplicates  │    │
│  │ find     │            │ get_issue        │    │
│  │ show     │            │ create_issue     │    │
│  │ create   │            │ update_issue     │    │
│  │ append   │            │ append_to_issue  │    │
│  │ embed    │            │ sync_team        │    │
│  │ config   │            │ issue_context    │    │
│  └────┬─────┘            └────────┬─────────┘    │
│       │                           │              │
│  ┌────┴───────────────────────────┴──────────┐   │
│  │              Core Engine                   │   │
│  │  Search (FTS5 + Vector + RRF)             │   │
│  │  Embedding (Gemini API / local GGUF)      │   │
│  │  Linear GraphQL Client                    │   │
│  │  SQLite (WAL, FTS5, blob embeddings)      │   │
│  └───────────────────────────────────────────┘   │
└─────────────────────────────────────────────────┘
         │                          │
         ▼                          ▼
   Linear API                 Gemini API
   (source of truth)          (embeddings)
```

| Component | Choice |
|---|---|
| Language | Rust — fast startup, single binary |
| CLI | clap (derive) |
| Database | rusqlite (bundled, FTS5) |
| Vector storage | f32 blobs + cosine similarity in Rust |
| Embeddings | Gemini API (768-dim), or local GGUF with `local-embeddings` feature (EmbeddingGemma, 256-dim) |
| Linear API | reqwest + GraphQL |
| MCP server | rmcp, stdio transport |
| Config | TOML at `~/.config/rectilinear/config.toml` |

## Install

### From crates.io

```sh
cargo install rectilinear
```

To include the local GGUF embedding backend (EmbeddingGemma-300M, requires cmake):

```sh
cargo install rectilinear --features local-embeddings
```

### From source

```sh
git clone https://github.com/pieter-ouwerkerk/rectilinear.git && cd rectilinear
cargo build --release
cp target/release/rectilinear ~/.local/bin/
```

### Prerequisites

- A Linear API key (https://linear.app/settings/api)
- Optional: a Gemini API key for embeddings (https://aistudio.google.com/apikey)

### Configure

#### Connect a Linear workspace

Rectilinear is multi-tenant: you connect one or more Linear orgs as named *workspaces*, and pass the workspace name on each MCP call (or set a default). The interactive flow keeps the API key out of shell history:

```sh
rectilinear config add-workspace
```

You'll be prompted for:

- **Workspace name** — a short label you'll reference later, e.g. `home`, `work`, `oss`. Agents pass this as `workspace` in MCP calls.
- **Linear API key** — get one at https://linear.app/settings/api. Linear API keys are *org-scoped*, so one key covers every team in that org. If your new workspace is in a Linear org you've already connected, you can reuse the existing key.
- **Default team** — the team prefix (e.g. `ENG`, `SFO`) Rectilinear should use when you don't pass `--team` explicitly.
- **Set as default workspace?** — `Y` if this is your primary; `N` otherwise.

The config is written to `~/.config/rectilinear/config.toml` with mode `0600` (owner read/write only).

Useful follow-ups:

```sh
rectilinear workspace list      # show configured workspaces
rectilinear workspace current   # show the active default
rectilinear workspace assume X  # switch the active default to workspace X
rectilinear config show         # full config dump (keys masked)
```

#### Optional: Gemini API key for embeddings

Embeddings power vector / hybrid search and duplicate detection. Configure once:

```sh
rectilinear config set embedding.gemini-api-key AIza...
rectilinear config set embedding.backend api
```

Or set `GEMINI_API_KEY` in your environment — it overrides the config value.

#### Single-workspace shortcut (legacy)

If you only ever work with one Linear org, the older single-tenant flow still works and skips the workspace concept entirely:

```sh
rectilinear config set linear-api-key lin_api_XXXX
rectilinear config set default-team ENG
```

`LINEAR_API_KEY` env var works too. New users should prefer `config add-workspace`.

### Sync issues

```sh
# First sync (automatically does a full sync)
rectilinear sync --team ENG

# Sync and generate embeddings in one step
rectilinear sync --team ENG --embed

# Force full re-sync
rectilinear sync --team ENG --full

# Include archived issues on an incremental sync. Full syncs include archived issues automatically.
rectilinear sync --team ENG --include-archived
```

### Generate embeddings

Embeddings power vector search and duplicate detection. Uses the Gemini API if `GEMINI_API_KEY` is set. If you installed with `--features local-embeddings`, it can also use a local GGUF model (EmbeddingGemma-300M, auto-downloaded on first use) as a fallback.

```sh
# Embed issues that don't have embeddings yet
rectilinear embed --team ENG

# Regenerate all embeddings (e.g. after changing backend)
rectilinear embed --force
```

## Usage

### Search

```sh
# Hybrid search (FTS + vector, default)
rectilinear search "login timeout on mobile"

# FTS-only (no embeddings needed)
rectilinear search "login timeout" --mode fts

# Filter by team and state
rectilinear search "auth" --team ENG --state "In Progress"

# JSON output for scripting
rectilinear search "auth" --json --limit 5
```

### Find duplicates

```sh
# Check if an issue already exists before filing
rectilinear find --similar "Users can't reset password on Safari"

# Lower the threshold to cast a wider net
rectilinear find --similar "password reset bug" --threshold 0.5
```

### View issues

```sh
rectilinear show ENG-123
rectilinear show ENG-123 --comments
rectilinear show ENG-123 --json
```

When comments are requested through MCP, Rectilinear returns `comments` with
`comments_status`, `comments_synced_at`, and `comments_sync_error`. Treat
`comments: []` as meaningful only with the status:

- `synced` means comments were fetched and at least one comment was found.
- `none_found` means Linear returned no comments for the issue.
- `not_synced` means comments have not been fetched yet.
- `permission_denied` or `unavailable` means Linear could not provide comments; see `comments_sync_error`.

### Create and update issues

```sh
# Create an issue (writes to Linear, syncs back locally)
rectilinear create --team ENG --title "Fix Safari password reset" \
  --description "Users on Safari 17+ can't complete the reset flow" \
  --priority 2

# Add a comment
rectilinear append ENG-123 --comment "Reproduced on Safari 17.4"

# Append to description
rectilinear append ENG-123 --description "Also affects Safari 17.3"
```

### Projects and milestones

Projects and milestones are first-class cached resources. Linear remains the source of truth; `sync`, `projects sync`, and the MCP refresh operations update the relational mirror.

```sh
# Query project metadata and milestones
rectilinear projects list
rectilinear projects show "API Reliability"
rectilinear milestones list --project "API Reliability"

# Create and update the hierarchy
rectilinear projects create --name "API Reliability" --teams ENG \
  --description "Improve service resilience and incident response" --priority 2 \
  --labels Infrastructure
rectilinear milestones create --project "API Reliability" --name "Request tracing" \
  --target-date 2026-09-01
rectilinear milestones update "Request tracing" \
  --description "Instrument critical request paths"

# Export one complete relationship graph as JSON
rectilinear projects import "API Reliability"
rectilinear milestones import "Request tracing" --project "API Reliability"
```

Project imports contain the complete project metadata, ordered milestones, and all linked issues across the project’s teams. Milestone imports contain the owning project, milestone metadata, and every issue assigned to that milestone. This is the preferred downstream-client boundary when the relationship graph matters; consumers no longer need to copy or reconstruct individual issues.

The MCP server exposes matching `list/get/create/update/delete_project`, `*_project_milestone`, `import_project`, and `import_project_milestone` tools. Project CRUD preserves teams, members, labels, status, lead, priority, dates, content, and visual metadata. `create_issue`, `update_issue`, and `mark_triaged` accept `project_milestone` so an issue can be created or moved within the hierarchy. The UniFFI `RectilinearEngine` exposes the same local reads, CRUD calls, hierarchy imports, and `set_issue_project_context` for Swift clients.

### Use with AI agents (MCP)

Rectilinear ships an MCP server (`rectilinear serve`, stdio transport) that any MCP-aware agent can connect to. Register it once at user scope and every project on your machine gets access — there's nothing per-repo to configure for the server itself, only for *which workspace* a given repo should use (see [Per-project guidance](#per-project-guidance) below).

#### Claude Code

User-scope (recommended — available in every project automatically):

```sh
claude mcp add rectilinear -s user -- rectilinear serve
```

Verify it's connected:

```sh
claude mcp list  # should show: rectilinear: ... ✓ Connected
```

Per-project alternative — add to the project's `.mcp.json`:

```json
{
  "mcpServers": {
    "rectilinear": {
      "command": "rectilinear",
      "args": ["serve"]
    }
  }
}
```

#### Codex CLI

Add to `~/.codex/config.toml`:

```toml
[mcp_servers.rectilinear]
command = "rectilinear"
args = ["serve"]
enabled = true
```

(Use an absolute path to the binary if `rectilinear` isn't on your `$PATH` when Codex spawns the server.)

#### Per-project guidance

The server exposes the same tools to every repo on your machine, so agents need a hint to pick the right workspace. Drop a short section in the repo's `AGENTS.md` (read by Codex, Cursor, and Claude Code) or `CLAUDE.md`:

```markdown
## Linear / Rectilinear

Issues for this repo live in Linear team `SFO`. Use the Rectilinear MCP
with workspace `home` — it's already configured with `SFO` as the default
team, so you don't need to pass `team` explicitly.
```

Without this hint, agents have to call `list_workspaces` and guess; with it, they go straight to the right one.

#### Tools exposed

This exposes 25 tools to MCP clients:

| Tool | Purpose |
|---|---|
| `list_workspaces` | Discover configured Linear workspaces |
| `list_labels` | Read the cached workspace label catalog |
| `list_projects` | Refresh and list project metadata |
| `get_project` | Read a project, its milestones, and optionally all issues |
| `create_project` | Create a project with teams and metadata |
| `update_project` | Update project metadata and relationships |
| `delete_project` | Archive a project and remove its cached hierarchy |
| `import_project` | Return a portable project + milestones + issues bundle |
| `list_project_milestones` | List ordered milestones for a project |
| `get_project_milestone` | Read a milestone and optionally all issues |
| `create_project_milestone` | Create a milestone inside a project |
| `update_project_milestone` | Update or move a milestone |
| `delete_project_milestone` | Delete a milestone |
| `import_project_milestone` | Return a portable project + milestone + issues bundle |
| `search_issues` | Hybrid search with team/state filters |
| `find_duplicates` | Semantic duplicate detection given title + description |
| `get_issue` | Full issue details with optional comments and comment sync diagnostics |
| `create_issue` | Create in Linear with optional project/milestone + sync back |
| `update_issue` | Update title, description, priority, state, labels, project, and milestone |
| `append_to_issue` | Add comment or extend description |
| `sync_team` | Trigger sync for a team; full syncs include archived issues and refresh comments |
| `issue_context` | Issue + its N most similar issues, comments, and comment sync diagnostics |
| `get_triage_queue` | Batch of unprioritized issues enriched with similar issues and code search hints |
| `mark_triaged` | Set priority, state, labels, project/milestone + update title/description + add comment in one call |
| `manage_relation` | Add or remove issue relations |

### Triage workflow

The MCP server includes built-in instructions that teach Claude Code how to triage issues conversationally. Setup:

```sh
# 1. Sync and embed your team's issues (needed once, then incremental)
rectilinear sync --team CUT --embed

# 2. Add rectilinear to your Claude Code MCP config (see above)

# 3. In Claude Code, just say:
#    "triage CUT issues"
#    "let's triage some random CUT issues"  (uses shuffle for variety)
```

**What happens:** Claude calls `get_triage_queue`, which syncs from Linear to get fresh data. For each issue, Claude:

1. Explores the codebase using extracted `code_search_hints` (file paths, identifiers, labels) to understand the current implementation
2. Presents the issue with code findings and similar issues, then asks clarifying questions from the perspective of a staff engineer who would implement it
3. Proposes priority, improved title/description (with code references), state changes, labels, and project assignment
4. After you confirm, calls `mark_triaged` to apply all changes to Linear in one call

Issues are presented **one at a time** — Claude waits for your input and applies changes before moving to the next.

**Staleness protection:** `mark_triaged` re-fetches the issue from Linear before applying changes. If someone else already prioritized it, Claude skips it. If the content changed since the queue was fetched, Claude shows what changed and re-evaluates. Embeddings are automatically updated when content changes.

**Best results:** Run triage from within your project directory so Claude can explore the actual codebase. If you use [Cuttlefish](https://github.com/pieter-ouwerkerk/cuttlefish), its MCP tools (`get_symbols`, `find_references`) give Claude even richer code context.

You can also add project-specific guidance in your `CLAUDE.md`:

```markdown
## Triage

When triaging Linear issues, present and resolve one issue at a time
before moving to the next. Explore the codebase to understand each
issue's context before asking questions.
```

## Data storage

| Path | Contents |
|---|---|
| `~/.config/rectilinear/config.toml` | API keys, defaults, preferences |
| `~/.local/share/rectilinear/rectilinear.db` | SQLite database (issues, FTS index, embeddings) |
| `~/.local/share/rectilinear/models/` | Local GGUF models (auto-downloaded) |

## Search modes

**FTS** — BM25 keyword search via SQLite FTS5 with Porter stemming. Fast, no embeddings needed.

**Vector** — Embeds the query via Gemini API, computes cosine similarity against stored issue chunks, returns max similarity per issue.

**Hybrid** (default) — Runs both FTS and vector search, combines results with Reciprocal Rank Fusion (`score = Σ 1/(k + rank)`). For duplicate detection, vector results are weighted 0.7 vs FTS 0.3.
