# Rectilinear

Local-first Linear issue intelligence. Maintains a search-optimized SQLite mirror of your Linear issues with hybrid full-text + vector search. Find duplicates before filing, search across teams instantly, and manage issues from the terminal or through Claude Code via MCP.

## Why

Linear teams accumulate hundreds of issues. Duplicate detection is hard, search is scattered, and context lives across many views. Rectilinear keeps a local copy with embeddings so you can:

- **Find duplicates** before creating new issues — semantic similarity, not just keyword matching
- **Search fast** — hybrid FTS5 + vector search with Reciprocal Rank Fusion, all local
- **Manage issues** from the CLI or let Claude Code do it through MCP tools

Linear remains the source of truth. The local database is a read-optimized cache. Writes go to Linear first, then sync back.

## Architecture

```
┌─────────────────────────────────────────────────┐
│                  rectilinear                     │
│                                                  │
│  CLI (clap)              MCP Server (rmcp)       │
│  ┌──────────┐            ┌──────────────────┐    │
│  │ sync     │            │ search_issues    │    │
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
| Embeddings | Gemini API (768-dim) or local GGUF (planned) |
| Linear API | reqwest + GraphQL |
| MCP server | rmcp, stdio transport |
| Config | TOML at `~/.config/rectilinear/config.toml` |

## Setup

### Prerequisites

- Rust toolchain (`rustup` — https://rustup.rs)
- A Linear API key (https://linear.app/settings/api)
- Optional: a Gemini API key for embeddings (https://aistudio.google.com/apikey)

### Build

```sh
git clone <repo-url> && cd rectilinear
cargo build --release
```

The binary is at `target/release/rectilinear`. Copy it somewhere on your `$PATH`:

```sh
cp target/release/rectilinear ~/.local/bin/
```

### Configure

```sh
# Required: Linear API key
rectilinear config set linear-api-key lin_api_XXXX

# Recommended: set a default team so you don't have to pass --team every time
rectilinear config set default-team ENG

# Optional: Gemini API key for semantic search / embeddings
rectilinear config set embedding.gemini-api-key AIza...
rectilinear config set embedding.backend api
```

Environment variables `LINEAR_API_KEY` and `GEMINI_API_KEY` also work and override config values.

View your config:

```sh
rectilinear config show
```

### Sync issues

```sh
# First sync (automatically does a full sync)
rectilinear sync --team ENG

# Sync and generate embeddings in one step
rectilinear sync --team ENG --embed

# Force full re-sync
rectilinear sync --team ENG --full

# Include archived issues
rectilinear sync --team ENG --include-archived
```

### Generate embeddings

Embeddings power vector search and duplicate detection. Requires a Gemini API key (or will use the local GGUF backend once implemented).

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

### MCP server (Claude Code integration)

Start the MCP server for use with Claude Code:

```sh
rectilinear serve
```

Add to your Claude Code MCP config (`~/.claude/claude_desktop_config.json` or project `.mcp.json`):

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

This exposes 8 tools to Claude Code:

| Tool | Purpose |
|---|---|
| `search_issues` | Hybrid search with team/state filters |
| `find_duplicates` | Semantic duplicate detection given title + description |
| `get_issue` | Full issue details with optional comments |
| `create_issue` | Create in Linear + sync back |
| `update_issue` | Update title, description, priority |
| `append_to_issue` | Add comment or extend description |
| `sync_team` | Trigger sync for a team |
| `issue_context` | Issue + its N most similar issues |

## Data storage

| Path | Contents |
|---|---|
| `~/.config/rectilinear/config.toml` | API keys, defaults, preferences |
| `~/.local/share/rectilinear/rectilinear.db` | SQLite database (issues, FTS index, embeddings) |
| `~/.local/share/rectilinear/models/` | Local GGUF models (when implemented) |

## Search modes

**FTS** — BM25 keyword search via SQLite FTS5 with Porter stemming. Fast, no embeddings needed.

**Vector** — Embeds the query via Gemini API, computes cosine similarity against stored issue chunks, returns max similarity per issue.

**Hybrid** (default) — Runs both FTS and vector search, combines results with Reciprocal Rank Fusion (`score = Σ 1/(k + rank)`). For duplicate detection, vector results are weighted 0.7 vs FTS 0.3.
